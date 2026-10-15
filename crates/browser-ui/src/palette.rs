//! Cmd+K command palette: actions, settings, tabs, spaces, extensions, history, bangs and a calculator,
//! modelled on Arc's command bar, Vivaldi's Quick Commands and Zen's command bar.
use crate::settings::measure_overlay;
use crate::tabs::tab_label;
use crate::window::{favicon_path, paint, site_host, site_icon, BrowserWindow};
use browser_core::{bang_home, calc, find_bang, normalize_url, spawn_favicon, Settings, BANGS};
use browser_engine::Shortcut;
use gpui::{div, prelude::*, px, ClickEvent, Context, MouseDownEvent, SharedString};
use gpuikit::elements::input::input;
use gpuikit::input::bindings;
use gpuikit::layout::{h_stack, v_stack};

const MAX_ROWS: usize = 10;

/// Vivaldi-style scope filters, in Chrome (`@tabs`) and Firefox (`%`) spellings.
const SCOPES: [(&str, &str, &str); 5] = [
    ("@tabs", "%", "Tabs"),
    ("@bookmarks", "*", "Bookmarks"),
    ("@history", "^", "History"),
    ("@actions", ">", "Actions"),
    ("@spaces", "", "Spaces"),
];

const ACTIONS: [(&str, &str, Shortcut); 19] = [
    ("New tab", "⌘T", Shortcut::NewTab),
    ("New terminal", "⌥⌘T", Shortcut::NewTerminal),
    ("Close tab", "⌘W", Shortcut::CloseTab),
    ("Reopen closed tab", "⇧⌘T", Shortcut::ReopenTab),
    ("Reload page", "⌘R", Shortcut::Reload),
    ("Go back", "⌘[", Shortcut::Back),
    ("Go forward", "⌘]", Shortcut::Forward),
    ("Find in page", "⌘F", Shortcut::Find),
    ("Bookmark page", "⌘D", Shortcut::Bookmark),
    ("Open settings", "⌘,", Shortcut::Settings),
    ("Print page", "⌘P", Shortcut::Print),
    ("Inspect page", "⌥⌘I", Shortcut::Inspect),
    ("Zoom in", "⌘+", Shortcut::ZoomIn),
    ("Zoom out", "⌘-", Shortcut::ZoomOut),
    ("Reset zoom", "⌘0", Shortcut::ZoomReset),
    ("Next tab", "⌃⇥", Shortcut::NextTab),
    ("Previous tab", "⌃⇧⇥", Shortcut::PrevTab),
    ("Focus address bar", "⌘L", Shortcut::FocusUrl),
    ("Go to last tab", "⌘9", Shortcut::SelectTab(9)),
];

#[derive(Clone)]
pub(crate) enum Run {
    Shortcut(Shortcut),
    Downloads,
    Extensions,
    CopyUrl,
    Duplicate,
    CloseOthers,
    CloseAll,
    NewSpace,
    Pref(fn(&mut Settings)),
    Tab(String),
    Closed(String),
    Space(String),
    Profile(String),
    Extension(String),
    Open(String),
    Copy(String),
    /// Replaces the typed text, e.g. `!yt ` or `@tabs `.
    Fill(String),
}

#[derive(Clone)]
pub(crate) struct Item {
    section: &'static str,
    label: String,
    hint: String,
    run: Run,
    /// A URL whose site icon leads the row.
    icon: Option<&'static str>,
}

fn item(section: &'static str, label: impl Into<String>, hint: impl Into<String>, run: Run) -> Item {
    Item { section, label: label.into(), hint: hint.into(), run, icon: None }
}

/// Splits a leading scope (`@tabs foo`, `%foo`) off the query.
fn scope(query: &str) -> (Option<&'static str>, &str) {
    for (word, symbol, section) in SCOPES {
        if let Some(rest) = query.strip_prefix(word).filter(|rest| rest.is_empty() || rest.starts_with(' ')) {
            return (Some(section), rest.trim_start());
        }
        if !symbol.is_empty() {
            if let Some(rest) = query.strip_prefix(symbol) {
                return (Some(section), rest.trim_start());
            }
        }
    }
    (None, query)
}

/// Lower is better: 0 prefix, 1 word prefix, 2 substring, 3 in-order letters. `None` is no match.
fn score(text: &str, query: &str) -> Option<u8> {
    let (text, query) = (text.to_lowercase(), query.to_lowercase());
    if query.is_empty() || text.starts_with(&query) {
        return Some(0);
    }
    if text.split(|c: char| !c.is_alphanumeric()).any(|word| word.starts_with(&query)) {
        return Some(1);
    }
    if text.contains(&query) {
        return Some(2);
    }
    let mut letters = text.chars();
    query.chars().filter(|c| !c.is_whitespace()).all(|c| letters.any(|t| t == c)).then_some(3)
}

/// Pretty-prints a calculator result: whole numbers without `.0`, others to 10 significant places.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let text = format!("{value:.10}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl BrowserWindow {
    pub(crate) fn open_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_open = !self.palette_open;
        self.palette_index = 0;
        self.palette_bang = None;
        self.palette_input.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
        if self.palette_open {
            self.focus_palette = true;
            // shortcut: /favicon.ico only; sites without one keep the coloured-initial tile.
            for (_, _, template) in BANGS {
                let path = favicon_path(&site_host(template));
                if !path.exists() {
                    spawn_favicon(format!("{}/favicon.ico", bang_home(template)), path);
                }
            }
        } else {
            self.focus_active_content();
        }
        cx.notify();
    }

    /// Turns a finished `!trigger ` in the input into the pill, leaving only the query in the input.
    pub(crate) fn absorb_bang(&mut self, cx: &mut Context<Self>) {
        if self.palette_bang.is_some() {
            return;
        }
        let raw = self.palette_input.read(cx).content().to_string();
        let Some((bang, query)) = find_bang(&raw) else {
            return;
        };
        // Only once the trigger is complete: `!g` on the way to `!gi` stays text.
        let done = raw.to_lowercase().split(' ').rev().skip(1).any(|word| word == format!("!{}", bang.0));
        if !done {
            return;
        }
        self.palette_bang = Some(bang);
        self.palette_input.update(cx, |input, cx| {
            input.set_content_silent(query.clone(), cx);
            input.set_selected_range(query.len()..query.len());
        });
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        if self.palette_open {
            self.palette_open = false;
            cx.notify();
        }
    }

    fn palette_items(&self, cx: &Context<Self>) -> Vec<Item> {
        let typed = self.palette_input.read(cx).content().trim().to_string();
        let (only, query) = scope(&typed);
        let wants = |section: &str| only.is_none_or(|only| only == section);
        let model = self.model.lock();
        let settings = model.settings.clone();
        // An active bang is the whole intent: show only what Enter will do, with the site's icon.
        if let Some((trigger, site, template)) = self.palette_bang {
            let url = normalize_url(&format!("!{trigger} {typed}"), settings.search_template(), settings.https_only);
            let label = if typed.is_empty() { format!("Open {site}") } else { format!("Search {site} for “{typed}”") };
            let mut go = item(site, label, "↵ open · ⌘↵ new tab", Run::Open(url));
            go.icon = Some(template);
            return vec![go];
        }
        let mut ranked: Vec<(u8, Item)> = Vec::new();
        if typed == "@" {
            for (word, symbol, section) in SCOPES {
                let hint = if symbol.is_empty() { String::new() } else { format!("or {symbol}") };
                ranked.push((0, item("Search in", format!("{word}  {section}"), hint, Run::Fill(format!("{word} ")))));
            }
        }
        if let Some(bang) = typed.strip_prefix('!').filter(|bang| !bang.contains(' ')) {
            for (trigger, site, template) in BANGS {
                if trigger.starts_with(&bang.to_ascii_lowercase()) || score(site, bang).is_some_and(|rank| rank < 3) {
                    let mut row = item("Bangs", format!("!{trigger}"), format!("Search {site}"), Run::Fill(format!("!{trigger} ")));
                    row.icon = Some(template);
                    ranked.push((0, row));
                }
            }
        }
        let mut add = |text: &str, entry: Item| {
            if let Some(rank) = score(text, query) {
                ranked.push((rank, entry));
            }
        };

        if wants("Tabs") {
            let active = model.active_tab().map(|tab| tab.id.clone());
            for tab in model.session_tabs() {
                let hint = if Some(&tab.id) == active.as_ref() { "Current tab" } else { "Switch to tab" };
                let text = format!("{} {}", tab.title, tab.url);
                add(&text, item("Tabs", tab_label(&tab.title, &tab.url), hint, Run::Tab(tab.id.clone())));
            }
            for url in model.closed_urls().into_iter().take(5) {
                add(&url, item("Recently closed", url.clone(), "Reopen", Run::Closed(url.clone())));
            }
        }
        if wants("Actions") {
            for (label, hint, shortcut) in ACTIONS {
                add(label, item("Actions", label, hint, Run::Shortcut(shortcut)));
            }
            for (label, run) in [
                ("Copy page URL", Run::CopyUrl),
                ("Duplicate tab", Run::Duplicate),
                ("Close other tabs", Run::CloseOthers),
                ("Close all tabs", Run::CloseAll),
                ("New space", Run::NewSpace),
                ("Show downloads", Run::Downloads),
                ("Manage extensions", Run::Extensions),
            ] {
                add(label, item("Actions", label, "", run));
            }
            // Zen-style toggles; the label says what picking it will do.
            let on_off = |on: bool| if on { "Turn off" } else { "Turn on" };
            let toggles: [(String, fn(&mut Settings)); 7] = [
                ("Use dark theme".into(), |s| s.theme = "dark".into()),
                ("Use light theme".into(), |s| s.theme = "light".into()),
                ("Use system theme".into(), |s| s.theme = "system".into()),
                (format!("{} compact mode", on_off(settings.compact())), |s| {
                    s.density = if s.compact() { "comfortable" } else { "compact" }.into();
                }),
                (format!("{} reduced motion", on_off(settings.reduce_motion)), |s| s.reduce_motion = !s.reduce_motion),
                (format!("{} HTTPS-only mode", on_off(settings.https_only)), |s| s.https_only = !s.https_only),
                (format!("{} JavaScript", on_off(settings.javascript)), |s| s.javascript = !s.javascript),
            ];
            for (label, change) in toggles {
                add(&label, item("Settings", label.clone(), "", Run::Pref(change)));
            }
        }
        if wants("Spaces") {
            // Torn-off windows show one space and never switch.
            let detached = [model.settings.detached_spaces.as_slice(), model.settings.detached_windows.as_slice()].concat();
            let listed = self.model.detached.is_none();
            for (index, session) in model.profile_sessions().into_iter().filter(|s| listed && !detached.contains(&s.id)).enumerate() {
                let name = if session.name.is_empty() { format!("Space {}", index + 1) } else { session.name.clone() };
                let hint = if session.active { "Current space" } else { "Switch to space" };
                add(&name, item("Spaces", name.clone(), hint, Run::Space(session.id.clone())));
            }
            for profile in &model.profiles {
                let hint = if profile.id == model.active_profile_id { "Current profile" } else { "Switch to profile" };
                add(&profile.name, item("Profiles", profile.name.clone(), hint, Run::Profile(profile.id.clone())));
            }
        }
        if only.is_none() || only == Some("Actions") {
            for extension in model.extensions.iter().filter(|extension| extension.enabled) {
                let name = self.ext_meta.get(&extension.id).map_or(extension.name.clone(), |meta| meta.name.clone());
                add(&name, item("Extensions", name.clone(), "Open extension", Run::Extension(extension.id.clone())));
            }
        }
        if !query.is_empty() {
            let mut links = Vec::new();
            if only == Some("Bookmarks") {
                links = model.bookmarks().unwrap_or_default().into_iter().map(|b| (b.url, b.title)).collect();
            } else if only == Some("History") {
                links = model.history().unwrap_or_default().into_iter().map(|h| (h.url, h.title)).collect();
            } else if only.is_none() {
                links = model.suggest(query, 6).unwrap_or_default();
            }
            let open = model.session_tabs().iter().map(|tab| tab.url.clone()).collect::<Vec<_>>();
            for (url, title) in links.into_iter().filter(|(url, _)| !open.contains(url)).take(6) {
                let label = if title.is_empty() { url.clone() } else { title };
                add(&format!("{label} {url}"), item(only.unwrap_or("History"), label, url.clone(), Run::Open(url)));
            }
        }
        drop(model);

        ranked.sort_by_key(|(rank, _)| *rank);
        let mut items = ranked.into_iter().map(|(_, entry)| entry).collect::<Vec<_>>();
        if typed.is_empty() {
            return items.into_iter().take(MAX_ROWS).collect();
        }
        if let Some(value) = calc(&typed) {
            let result = format_number(value);
            items.insert(0, item("Calculator", format!("= {result}"), "Copy result", Run::Copy(result)));
        }
        if only.is_none() && !typed.starts_with('@') {
            let url = normalize_url(&typed, settings.search_template(), settings.https_only);
            let looks_like_url = !url.starts_with(settings.search_template().split("%s").next().unwrap_or_default());
            let label = if looks_like_url { format!("Open {typed}") } else { format!("Search for “{typed}”") };
            let go = item("Go", label, "⌘↵ new tab", Run::Open(url));
            // A URL is clearly meant to be opened; otherwise matches come first.
            if looks_like_url || items.is_empty() {
                items.insert(0, go);
            } else {
                items.push(go);
            }
        }
        items.truncate(MAX_ROWS);
        items
    }

    fn run_palette(&mut self, entry: Item, new_tab: bool, cx: &mut Context<Self>) {
        self.palette_open = false;
        let mut keep_focus = false;
        match entry.run {
            Run::Shortcut(shortcut) => {
                keep_focus = matches!(shortcut, Shortcut::Find | Shortcut::Settings | Shortcut::FocusUrl | Shortcut::NewTab);
                self.on_shortcut(shortcut, cx);
            }
            Run::Downloads => (self.downloads_open, keep_focus) = (true, true),
            Run::Extensions => (self.extensions_open, keep_focus) = (true, true),
            Run::CopyUrl => {
                let url = self.model.lock().active_tab().map(|tab| tab.url.clone()).unwrap_or_default();
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                self.status = "Copied page URL".into();
            }
            Run::Duplicate => {
                let tab = self.model.lock().active_tab().map(|tab| (tab.id.clone(), tab.url.clone()));
                if let Some((id, url)) = tab {
                    self.new_tab_in(&id, Some(&url), cx);
                }
            }
            Run::CloseOthers => {
                let active = self.active_tab_id();
                let others = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone())
                    .filter(|id| Some(id) != active.as_ref()).collect::<Vec<_>>();
                self.close_tabs(others, cx);
            }
            Run::CloseAll => {
                let all = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect();
                self.close_tabs(all, cx);
            }
            Run::NewSpace => self.with_model(|model| { let _ = model.create_session(); }),
            Run::Pref(change) => self.set_pref(cx, change),
            Run::Tab(id) => self.focus_window(&id, cx),
            Run::Closed(url) => match self.with_model(|model| model.reopen_closed_url(Some(&url))) {
                Ok(Some(id)) => self.focus_window(&id, cx),
                _ => self.status = "That tab is no longer in recently closed".into(),
            },
            Run::Space(id) => self.with_model(|model| { let _ = model.switch_session(&id); }),
            Run::Profile(id) => self.with_model(|model| { let _ = model.switch_profile(&id); }),
            Run::Extension(id) => {
                let at = self.stage_bounds.map_or(0., |bounds| f32::from(bounds.center().x));
                self.open_extension(&id, at, cx);
                keep_focus = true;
            }
            Run::Open(url) => match self.active_tab_id().filter(|_| new_tab) {
                Some(id) => self.new_tab_in(&id, Some(&url), cx),
                None => self.with_model(|model| { let _ = model.navigate_active(&url); }),
            },
            Run::Copy(text) => {
                self.status = format!("Copied {text}").into();
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            }
            Run::Fill(text) => {
                self.palette_open = true;
                self.palette_index = 0;
                self.palette_input.update(cx, |input, cx| {
                    input.set_content_silent(text.clone(), cx);
                    input.set_selected_range(text.len()..text.len());
                });
                self.absorb_bang(cx);
                cx.notify();
                return;
            }
        }
        if !keep_focus {
            self.focus_active_content();
        }
        self.refresh_url(cx);
        cx.notify();
    }

    /// Handles a list key; anything it doesn't use (→ mid-text) goes on to the input.
    fn palette_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let items = self.palette_items(cx);
        let count = items.len();
        let at = self.palette_index.min(count.saturating_sub(1));
        let caret_at_end = {
            let input = self.palette_input.read(cx);
            input.selected_range().start == input.content().len()
        };
        match key {
            "down" if count > 0 => self.palette_index = (at + 1) % count,
            "up" if count > 0 => self.palette_index = (at + count - 1) % count,
            "enter" | "cmd-enter" => {
                if let Some(entry) = items.get(at).cloned() {
                    self.run_palette(entry, key == "cmd-enter", cx);
                }
            }
            "escape" => self.open_palette(cx),
            // Vivaldi-style autocomplete: Tab (or → at the end) copies the selected row into the input.
            "tab" | "right" if count > 0 && (key == "tab" || caret_at_end) => {
                let entry = &items[at];
                let text = match &entry.run {
                    Run::Fill(text) => text.clone(),
                    Run::Open(url) => url.clone(),
                    _ => entry.label.clone(),
                };
                self.palette_index = 0;
                self.palette_input.update(cx, |input, cx| {
                    input.set_content_silent(text.clone(), cx);
                    input.set_selected_range(text.len()..text.len());
                });
                self.absorb_bang(cx);
            }
            // Backspace in an empty input drops the pill, as in Arc and Kagi.
            "backspace" if self.palette_bang.is_some() && self.palette_input.read(cx).content().is_empty() => self.palette_bang = None,
            "up" | "down" | "tab" => {}
            _ => return cx.propagate(),
        }
        // Capture-phase actions keep going unless stopped; otherwise the input also inserts `\t`, etc.
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn palette_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let items = self.palette_items(cx);
        let bang = self.palette_bang;
        let selected = self.palette_index.min(items.len().saturating_sub(1));
        let mut list = v_stack().w_full().p(px(6.)).gap(px(1.));
        let mut last_section = "";
        for (index, entry) in items.iter().cloned().enumerate() {
            if entry.section != last_section {
                last_section = entry.section;
                list = list.child(div().px(px(10.)).pt(px(if index == 0 { 2. } else { 8. })).pb(px(2.)).text_size(px(11.)).text_color(paint.muted).child(entry.section));
            }
            let (label, hint, icon) = (entry.label.clone(), entry.hint.clone(), entry.icon);
            list = list.child(
                h_stack()
                    .id(SharedString::from(format!("palette-{index}")))
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(index == selected, |row| row.bg(paint.accent_bg))
                    .hover(|row| row.bg(paint.secondary))
                    .items_center()
                    .children(icon.map(|url| site_icon(url, 16., false)))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().max_w(px(220.)).truncate().text_size(px(11.)).text_color(paint.muted).child(hint))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        let new_tab = event.modifiers().platform;
                        this.run_palette(entry.clone(), new_tab, cx);
                    })),
            );
        }
        let card = v_stack()
            .id("command-palette")
            .relative()
            .w(px(560.))
            .max_w_full()
            .rounded(px(12.))
            .overflow_hidden()
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_palette(cx)))
            // The input binds these keys to its own actions, and gpui runs bindings before any key-down
            // listener, so the list has to catch the actions in the capture phase, ahead of the input.
            .capture_action(cx.listener(|this, _: &bindings::Down, _, cx| this.palette_key("down", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Up, _, cx| this.palette_key("up", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Enter, _, cx| this.palette_key("enter", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Submit, _, cx| this.palette_key("cmd-enter", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Tab, _, cx| this.palette_key("tab", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Right, _, cx| this.palette_key("right", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Escape, _, cx| this.palette_key("escape", cx)))
            .capture_action(cx.listener(|this, _: &bindings::Backspace, _, cx| this.palette_key("backspace", cx)))
            .child(measure_overlay(12., cx))
            .child(
                div().h(px(44.)).px(px(14.)).gap(px(8.)).flex().items_center().border_b_1().border_color(paint.border)
                    .children(bang.map(|(_, site, template)| {
                        h_stack()
                            .flex_none()
                            .gap(px(6.))
                            .items_center()
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(6.))
                            .bg(paint.accent_bg)
                            .text_color(paint.accent)
                            .text_size(px(12.))
                            .child(site_icon(template, 14., false))
                            .child(*site)
                    }))
                    .child(
                    input(&self.palette_input, cx)
                        .placeholder(match bang {
                            Some((_, site, _)) => format!("Search {site}"),
                            None => "Search tabs, actions, history, or type !bang, @scope, 2+2".into(),
                        })
                        .cursor_color(paint.accent)
                        .flex_1()
                        .text_size(px(14.))
                        .text_color(paint.fg),
                ),
            )
            .when(!items.is_empty(), |card| card.child(list));
        div().absolute().top(px(72.)).left_0().right_0().flex().justify_center().px(px(16.)).child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_and_scores() {
        assert_eq!(scope("@tabs foo"), (Some("Tabs"), "foo"));
        assert_eq!(scope("%foo"), (Some("Tabs"), "foo"));
        assert_eq!(scope("> zoom"), (Some("Actions"), "zoom"));
        assert_eq!(scope("@tabsfoo"), (None, "@tabsfoo"));
        assert_eq!(scope("rust"), (None, "rust"));
        assert_eq!(score("Reload page", "rel"), Some(0));
        assert_eq!(score("Go back", "back"), Some(1));
        assert_eq!(score("Reload page", "oad"), Some(2));
        assert_eq!(score("Turn on compact mode", "tcm"), Some(3));
        assert_eq!(score("Reload", "xyz"), None);
        assert_eq!(format_number(42.0), "42");
        assert_eq!(format_number(1.0 / 3.0), "0.3333333333");
    }
}
