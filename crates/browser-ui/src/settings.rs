use crate::native::{
    button, button_if, commit, field, heading, para, setting, sheet, text, title, Initial, Ink, Key, Node, Pop, Style, Weight, HOME_PAGE,
    PASSWORD_SEARCH, PROFILE_EDIT, PROFILE_NAME, SETTINGS_QUERY, SHELL, TERM_FONT, UI_FONT, USER_AGENT,
};
use crate::window::{profile_badge_node, paint, BrowserWindow, Paint};
use browser_core::{app_support_dir, Settings, TabState, SEARCH_ENGINES};
use browser_engine::{Shortcut, WebEngine, SHORTCUTS};
use crate::terminal::is_terminal;
use browser_engine::LogicalRect;
use gpui::Context;
use gpuikit::theme::parse_hex;

pub const PROFILE_ICONS: [&str; 17] =
    ["", "😀", "🦊", "🐱", "🐼", "🚀", "💼", "🏠", "🎮", "🎨", "📚", "🌙", "⭐", "🔥", "🌿", "☕", "🎵"];
pub const PROFILE_SVG_ICONS: [&str; 16] = [
    "icons/radix/home.svg", "icons/radix/person.svg", "icons/radix/rocket.svg", "icons/radix/star.svg",
    "icons/radix/heart.svg", "icons/radix/gear.svg", "icons/radix/globe.svg", "icons/radix/lightning-bolt.svg",
    "icons/radix/moon.svg", "icons/radix/sun.svg", "icons/radix/code.svg", "icons/radix/backpack.svg",
    "icons/radix/camera.svg", "icons/radix/laptop.svg", "icons/radix/reader.svg", "icons/radix/target.svg",
];
pub const PROFILE_COLORS: [&str; 11] = [
    "", "#e5484d", "#f76b15", "#ffc53d", "#46a758", "#12a594", "#0090ff", "#3e63dd", "#8e4ec6", "#d6409f", "#8b8d98",
];
const SUSPEND_OPTIONS: [(&str, i64); 6] =
    [("30 s", 30), ("1 min", 60), ("2 min", 120), ("5 min", 300), ("10 min", 600), ("30 min", 1800)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    Profiles,
    General,
    Shortcuts,
    Appearance,
    Search,
    Privacy,
    Passwords,
    Content,
    Downloads,
    Terminal,
    Performance,
    Developer,
    About,
}

impl SettingsSection {
    const ALL: [Self; 13] = [
        Self::Profiles,
        Self::General,
        Self::Shortcuts,
        Self::Appearance,
        Self::Search,
        Self::Privacy,
        Self::Passwords,
        Self::Content,
        Self::Downloads,
        Self::Terminal,
        Self::Performance,
        Self::Developer,
        Self::About,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Profiles => "Profiles",
            Self::General => "General",
            Self::Shortcuts => "Keyboard shortcuts",
            Self::Appearance => "Appearance",
            Self::Search => "Search engine",
            Self::Privacy => "Privacy & security",
            Self::Passwords => "Passwords",
            Self::Content => "Site content",
            Self::Downloads => "Downloads",
            Self::Terminal => "Terminal",
            Self::Performance => "Performance",
            Self::Developer => "Developer",
            Self::About => "About",
        }
    }

    /// Its SF Symbol in the sidebar.
    fn symbol(self) -> &'static str {
        match self {
            Self::Profiles => "person.2",
            Self::General => "gearshape",
            Self::Shortcuts => "keyboard",
            Self::Appearance => "paintbrush",
            Self::Search => "magnifyingglass",
            Self::Privacy => "hand.raised",
            Self::Passwords => "key",
            Self::Content => "globe",
            Self::Downloads => "arrow.down.circle",
            Self::Terminal => "terminal",
            Self::Performance => "gauge.with.dots.needle.67percent",
            Self::Developer => "hammer",
            Self::About => "info.circle",
        }
    }

    /// Row titles of the section, so search finds a setting by name.
    fn keywords(self) -> &'static str {
        match self {
            Self::Profiles => "new profile icon color rename delete switch export import backup",
            Self::General => "on startup restore session new tab page home focus windows hover click default browser links other apps profile",
            Self::Shortcuts => "keyboard keys reset",
            Self::Appearance => "theme dark light system font typeface page zoom accent color size line height reduce motion site zoom background wallpaper liquid glass",
            Self::Search => "address bar google duckduckgo",
            Self::Privacy => "save browsing history https ads trackers block pop-ups popups fraudulent warnings clear cookies site data permissions",
            Self::Passwords => "offer save suggest autofill logins vault",
            Self::Content => "javascript custom user agent",
            Self::Downloads => "location folder ask where save file screenshot",
            Self::Terminal => "font size line height cursor scrollback shell",
            Self::Performance => "suspend background tabs maximum live tabs memory",
            Self::Developer => "web inspector inspect element",
            Self::About => "version update check restart engine webkit data folder",
        }
    }

    /// Whether search `query` (already lowercased) should list this section.
    fn matches(self, query: &str) -> bool {
        query.is_empty()
            || self.label().to_lowercase().contains(query)
            || self.keywords().contains(query)
            || (self == Self::Shortcuts && SHORTCUTS.iter().any(|binding| crate::shortcuts::matches(binding, query)))
    }
}

type Apply<T> = fn(&mut Settings, T);

impl BrowserWindow {
    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.end_recording();
        self.settings_open = !self.settings_open;
        self.settings_note = None;
        self.passwords.lock_view();
        for field in [HOME_PAGE, USER_AGENT, SHELL, SETTINGS_QUERY, PASSWORD_SEARCH, UI_FONT, TERM_FONT] {
            self.popover_texts.remove(&field);
        }
        if self.settings_open {
            let active = self.model.lock().active_profile_id.clone();
            self.select_settings_profile(active, cx);
        } else {
            // Keys go back to the page, which may have been opened or activated while settings held them.
            self.focus_active_content();
        }
        cx.notify();
    }

    /// Opens settings on one profile.
    pub(crate) fn open_profile_settings(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.settings_open {
            self.toggle_settings(cx);
        }
        self.settings_section = SettingsSection::Profiles;
        self.select_settings_profile(id, cx);
        cx.notify();
    }

    pub(crate) fn open_new_profile(&mut self, cx: &mut Context<Self>) {
        self.popover_texts.remove(&PROFILE_NAME);
        self.new_profile = Some((String::new(), String::new()));
        cx.notify();
    }

    pub(crate) fn create_new_profile(&mut self, cx: &mut Context<Self>) {
        let Some((icon, color)) = self.new_profile.take() else {
            return;
        };
        let typed = self.typed(PROFILE_NAME).trim().to_string();
        let name = if typed.is_empty() {
            // First free number, so a deleted "Profile 2" doesn't make the next one a duplicate.
            let taken = self.model.lock().profiles.iter().map(|profile| profile.name.clone()).collect::<Vec<_>>();
            (1..).map(|n| format!("Profile {n}")).find(|name| !taken.contains(name)).unwrap_or_default()
        } else {
            typed
        };
        let id = self.with_model(|model| {
            let id = model.create_profile(&name).ok()?;
            let _ = model.set_profile_appearance(&id, &icon, &color);
            Some(id)
        });
        if let (Some(id), true) = (id, self.settings_open) {
            self.select_settings_profile(id, cx);
        }
        self.refresh_url(cx);
        cx.notify();
    }

    /// Name, icon and color for a profile before it is created.
    pub(crate) fn profile_sheet(&self, pop: &mut Pop, cx: &mut Context<Self>) -> Node {
        let paint = paint(cx);
        let (icon, color) = self.new_profile.clone().unwrap_or_default();
        let typed = self.typed(PROFILE_NAME).trim().to_string();
        let preview = browser_core::Profile {
            id: String::new(),
            name: if typed.is_empty() { "Profile".into() } else { typed },
            storage_path: Default::default(),
            data_store_id: String::new(),
            created_at: 0,
            icon: icon.clone(),
            color: color.clone(),
        };
        let create = pop.act(|this, cx| this.create_new_profile(cx));
        let cancel = pop.act(|this, _| this.new_profile = None);
        let (icons, colors) = appearance_swatches(pop, &preview.name, &icon, &color, paint, |this, icon, color| this.new_profile = Some((icon, color)));
        let mut body = vec![
            para("Each profile keeps its own tabs, history, cookies and site data.", 13., Ink::Secondary),
            Node::Row { gap: 12., children: vec![profile_badge_node(&preview, false, 44., paint), field(PROFILE_NAME, "", "Profile name", create)] },
            text("Icon", 13., Weight::Semibold, Ink::Primary),
        ];
        body.extend(swatch_rows(icons));
        body.push(text("Color", 13., Weight::Semibold, Ink::Primary));
        body.extend(swatch_rows(colors));
        sheet("New profile", body, vec![button("Cancel", cancel, Style::Plain, Key::Escape), button("Create profile", create, Style::Primary, Key::Return)])
    }

    pub(crate) fn delete_profile_now(&mut self, id: &str, cx: &mut Context<Self>) {
        let store = self.with_model(|model| model.delete_profile(id).ok().flatten());
        if let Some(store) = store {
            self.engine.remove_data_store(&store);
        }
        self.delete_prompt = None;
        if self.settings_profile.as_deref() == Some(id) {
            let active = self.model.lock().active_profile_id.clone();
            self.select_settings_profile(active, cx);
        }
        self.refresh_url(cx);
        cx.notify();
    }

    /// Confirmation for deleting a profile, stacked above everything else.
    pub(crate) fn delete_sheet(&self, id: &str, pop: &mut Pop) -> Node {
        let name = self.model.lock().profiles.iter().find(|profile| profile.id == id).map(|profile| profile.name.clone()).unwrap_or_default();
        let id = id.to_string();
        let cancel = pop.act(|this, _| this.delete_prompt = None);
        let delete = pop.act(move |this, cx| this.delete_profile_now(&id, cx));
        sheet(
            format!("Delete “{name}”?"),
            vec![para("Its tabs, sessions, history, bookmarks, cookies and site data will be removed. This can't be undone.", 13., Ink::Secondary)],
            vec![button("Cancel", cancel, Style::Plain, Key::Escape), button("Delete profile", delete, Style::Danger, Key::Return)],
        )
    }

    /// Which profile links from other apps open in.
    pub(crate) fn link_sheet(&self, pop: &mut Pop, cx: &mut Context<Self>) -> Node {
        let paint = paint(cx);
        let (profiles, active) = {
            let model = self.model.lock();
            (model.profiles.clone(), model.active_profile_id.clone())
        };
        let heading = match self.pending_links.len() {
            1 => "Open link in".to_string(),
            n => format!("Open {n} links in"),
        };
        let mut body = vec![crate::native::title(self.pending_links.first().cloned().unwrap_or_default(), 13., Weight::Regular, Ink::Secondary)];
        let mut rows = Vec::new();
        for profile in profiles {
            let id = profile.id.clone();
            let open = pop.act(move |this, cx| {
                let urls = std::mem::take(&mut this.pending_links);
                this.open_links_in(&id, urls, cx);
            });
            let mut children = vec![profile_badge_node(&profile, profile.id == active, 28., paint), crate::native::title(profile.name.clone(), 13., Weight::Regular, Ink::Primary), Node::Spacer];
            if profile.id == active {
                children.push(text("Active", 13., Weight::Regular, Ink::Secondary));
            }
            rows.push(Node::Item { height: 40., click: Some(open), drag: None, children });
        }
        body.push(Node::Column { gap: 2., pad: (0., 0.), children: rows });
        let cancel = pop.act(|this, _| this.pending_links.clear());
        sheet(heading, body, vec![button("Cancel", cancel, Style::Plain, Key::Escape)])
    }

    pub(crate) fn open_rail_menu(&mut self, id: String, index: usize, position: gpui::Point<gpui::Pixels>) {
        let count = self.model.lock().profiles.len().max(1);
        let rows = vec![
            menu_item("Move up", index > 0, { let id = id.clone(); move |this, _| this.with_model(|model| { let _ = model.move_profile(&id, index - 1); }) }),
            menu_item("Move down", index + 1 < count, { let id = id.clone(); move |this, _| this.with_model(|model| { let _ = model.move_profile(&id, index + 1); }) }),
            None,
            menu_item("Edit profile…", true, { let id = id.clone(); move |this, cx| this.open_profile_settings(id.clone(), cx) }),
            menu_item("Delete profile…", count > 1, move |this, _| this.delete_prompt = Some(id.clone())),
        ];
        self.show_menu(rows, position);
    }

    /// Right-click menu on a terminal's screen.
    pub(crate) fn open_term_menu(&mut self, id: String, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let view = crate::terminal::get(&id, cx);
        let selected = view.as_ref().is_some_and(|view| view.read(cx).has_selection());
        let on = |run: fn(&mut crate::terminal::TerminalView, &mut Context<crate::terminal::TerminalView>)| {
            let view = view.clone();
            move |_: &mut Self, cx: &mut Context<Self>| {
                if let Some(view) = &view {
                    view.update(cx, run);
                }
            }
        };
        let rows = vec![
            menu_item("Copy", selected, on(|view, cx| view.copy(cx))),
            menu_item("Paste", true, on(|view, cx| view.paste(cx))),
            None,
            menu_item("Clear", true, on(|view, cx| view.clear(cx))),
        ];
        self.show_menu(rows, position);
    }

    /// The title bar's screenshot button menu.
    pub(crate) fn open_shot_menu(&mut self, id: String, position: gpui::Point<gpui::Pixels>) {
        let url = self.model.lock().session_tabs().into_iter().find(|tab| tab.id == id).map(|tab| tab.url.clone()).unwrap_or_default();
        let web = !url.is_empty() && !is_terminal(&url);
        let rows = vec![
            menu_item("Whole Page", web, |this, cx| this.on_shortcut(Shortcut::Screenshot, cx)),
            menu_item("Select Element", web, |this, cx| this.on_shortcut(Shortcut::ScreenshotElement, cx)),
        ];
        self.show_menu(rows, position);
    }

    /// Right-click menu on a tab pill; "other" and "to the right" stay within the tab's window.
    pub(crate) fn open_tab_menu(&mut self, id: String, position: gpui::Point<gpui::Pixels>) {
        let row = self.tab_rows().into_iter().map(|(_, ids)| ids).find(|ids| ids.contains(&id)).unwrap_or_default();
        let at = row.iter().position(|other| *other == id).unwrap_or(0);
        let (others, right) = (row.iter().filter(|other| **other != id).cloned().collect::<Vec<_>>(), row[(at + 1).min(row.len())..].to_vec());
        let (url, title, suspended, bookmark, reopenable) = {
            let model = self.model.lock();
            let tab = model.session_tabs().into_iter().find(|tab| tab.id == id).cloned();
            let (url, title) = tab.as_ref().map(|tab| (tab.url.clone(), tab.title.clone())).unwrap_or_default();
            let bookmark = model.bookmarks().unwrap_or_default().into_iter().find(|bookmark| bookmark.url == url);
            (url, title, tab.is_some_and(|tab| tab.state == TabState::Suspended), bookmark, !model.closed_urls().is_empty())
        };
        let web = !url.is_empty() && !is_terminal(&url);
        let pinned = bookmark.as_ref().is_some_and(|bookmark| bookmark.pinned);
        let muted = self.engine.is_muted(&id);
        let rows = vec![
            menu_item("New Tab", true, { let id = id.clone(); move |this, cx| this.new_tab_in(&id, None, cx) }),
            menu_item("Duplicate Tab", true, { let (id, url) = (id.clone(), url.clone()); move |this, cx| this.new_tab_in(&id, Some(&url), cx) }),
            menu_item("Reload Tab", true, { let id = id.clone(); move |this, _| this.engine.reload(&id) }),
            menu_item(if muted { "Unmute Tab" } else { "Mute Tab" }, true, { let id = id.clone(); move |this, _| this.engine.set_muted(&id, !muted) }),
            None,
            menu_item("Copy Link", web, { let url = url.clone(); move |this, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                this.status = "Copied page address".into();
            } }),
            menu_item(if bookmark.is_some() { "Remove Bookmark" } else { "Add Bookmark" }, web, {
                let (url, title, bookmark) = (url.clone(), title.clone(), bookmark.as_ref().map(|bookmark| bookmark.id.clone()));
                move |this, _| this.with_model(|model| {
                    let _ = match &bookmark {
                        Some(id) => model.delete_bookmark(id),
                        None => model.save_bookmark(None, &url, &title, false),
                    };
                })
            }),
            menu_item(if pinned { "Unpin from Start Page" } else { "Pin to Start Page" }, web, {
                let bookmark = bookmark.as_ref().map(|bookmark| bookmark.id.clone());
                move |this, _| this.with_model(|model| { let _ = model.save_bookmark(bookmark.as_deref(), &url, &title, !pinned); })
            }),
            None,
            menu_item("Move to New Window", self.group_len(&id) > 1, { let id = id.clone(); move |this, cx| this.detach_tab(&id, position, cx) }),
            // A tab its window shows would go blank; only ones hidden behind another tab can sleep.
            menu_item("Suspend Tab", web && !suspended && !self.is_front(&id), { let id = id.clone(); move |this, _| this.with_model(|model| { let _ = model.mark_suspended(&id); }) }),
            None,
            menu_item("Close Tab", true, move |this, cx| this.close_tabs(vec![id.clone()], cx)),
            menu_item("Close Other Tabs", !others.is_empty(), move |this, cx| this.close_tabs(others.clone(), cx)),
            menu_item("Close Tabs to the Right", !right.is_empty(), move |this, cx| this.close_tabs(right.clone(), cx)),
            menu_item("Reopen Closed Tab", reopenable, |this, cx| this.on_shortcut(Shortcut::ReopenTab, cx)),
        ];
        self.show_menu(rows, position);
    }

    /// Shows `rows` as a native menu at `position`; `EngineEvent::MenuPick` runs the picked row.
    pub(crate) fn show_menu(&mut self, rows: Vec<MenuRow>, position: gpui::Point<gpui::Pixels>) {
        let items = rows.iter().map(|row| row.as_ref().map(|(label, enabled, _)| (label.as_ref(), *enabled))).collect::<Vec<_>>();
        self.engine.show_menu(&items, (f64::from(position.x), f64::from(position.y)));
        self.menu_actions = rows.into_iter().map(|row| row.map(|(_, _, run)| run)).collect();
    }

    pub(crate) fn set_pref(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let _ = self.model.lock().update_settings(f);
        self.apply_settings();
        let settings = self.model.lock().settings.clone();
        crate::terminal::set_prefs(&settings, cx);
        cx.notify();
    }

    fn select_settings_profile(&mut self, id: String, _cx: &mut Context<Self>) {
        let name = self.model.lock().profiles.iter().find(|profile| profile.id == id).map(|profile| profile.name.clone()).unwrap_or_default();
        // The field is rebuilt with this name; a stale edit of another profile must not rename this one.
        self.popover_texts.insert(PROFILE_EDIT, name);
        self.settings_profile = Some(id);
    }

    /// The native Settings window: sections in a glass sidebar, the chosen one's controls beside it.
    pub(crate) fn settings_window(&self, pop: &mut Pop, cx: &mut Context<Self>) -> Node {
        let paint = paint(cx);
        let query = self.typed(SETTINGS_QUERY).trim().to_lowercase();
        let shown = SettingsSection::ALL.into_iter().filter(|section| section.matches(&query)).collect::<Vec<_>>();
        let current = if shown.contains(&self.settings_section) { Some(self.settings_section) } else { shown.first().copied() };
        let search = pop.act(|_, _| {});
        // Room for the traffic lights, which sit over the sidebar.
        let mut nav = vec![Node::Item { height: 30., click: None, drag: None, children: Vec::new() }, field(SETTINGS_QUERY, "", "Search", search), Node::Item { height: 6., click: None, drag: None, children: Vec::new() }];
        if shown.is_empty() {
            nav.push(Node::Item { height: 28., click: None, drag: None, children: vec![text("No matches", 13., Weight::Regular, Ink::Secondary)] });
        }
        for section in shown {
            let open = pop.act(move |this, _| {
                this.end_recording();
                this.settings_section = section;
            });
            let selected = current == Some(section);
            // A fixed-width icon column, so the labels line up whatever each symbol's width.
            let icon = Node::Width(18., Box::new(Node::Center(Box::new(Node::Glyph { name: section.symbol(), size: 13., ink: Ink::Accent }))));
            let row = Node::Item { height: 28., click: Some(open), drag: None, children: vec![icon, text(section.label(), 13., if selected { Weight::Medium } else { Weight::Regular }, Ink::Primary)] };
            nav.push(if selected { Node::Card { tint: Ink::Accent, child: Box::new(row) } } else { row });
        }
        let mut page = Vec::new();
        if let Some(section) = current {
            page.push(text(section.label(), 17., Weight::Semibold, Ink::Primary));
            if let Some(note) = &self.settings_note {
                page.push(para(note.to_string(), 12., Ink::Secondary));
            }
            page.extend(self.settings_page(section, &query, pop, paint, cx));
        }
        Node::Split {
            sidebar_width: 180.,
            height: 560.,
            sidebar: Box::new(Node::Column { gap: 1., pad: (10., 10.), children: nav }),
            content: Box::new(Node::Column { gap: 0., pad: (28., 20.), children: page }),
        }
    }

    fn settings_page(&self, section: SettingsSection, query: &str, pop: &mut Pop, paint: Paint, cx: &gpui::App) -> Vec<Node> {
        let settings = self.model.lock().settings.clone();
        match section {
            SettingsSection::Profiles => self.profiles_settings(pop, paint),
            SettingsSection::General => {
                let home = pop.act(|this, cx| {
                    let value = this.typed(HOME_PAGE).trim().to_string();
                    this.set_pref(cx, |s| s.home_page = value);
                });
                let make_default = pop.act(|this, _| {
                    if let Err(error) = browser_engine_macos::make_default_browser() {
                        this.settings_note = Some(error.to_string().into());
                    }
                });
                vec![
                    setting("On startup", None, choice(pop, vec![("Restore last session", true), ("Open new tab page", false)], settings.restore_session, |s, v| s.restore_session = v)),
                    setting("New tab page", Some("Address opened for new tabs. Press Return to save."), commit(HOME_PAGE, settings.home_page.clone(), "nxbrowser://home", home)),
                    setting(
                        "Focus windows",
                        Some("Hover focuses the window under the pointer; click waits for a click."),
                        choice(pop, vec![("Click", false), ("Hover", true)], settings.focus_on_hover, |s, v| s.focus_on_hover = v),
                    ),
                    setting(
                        "Links from other apps",
                        Some("Which profile a link clicked in another app opens in."),
                        choice(pop, vec![("Ask each time", true), ("Active profile", false)], settings.ask_link_profile, |s, v| s.ask_link_profile = v),
                    ),
                    setting("Default browser", Some("Open web links from every app in NxBrowser. macOS asks you to confirm."), button("Make default", make_default, Style::Plain, Key::None)),
                ]
            }
            SettingsSection::Shortcuts => self.shortcuts_settings(&settings, query, pop),
            SettingsSection::Appearance => {
                let fonts = std::iter::once("System".to_string()).chain(crate::terminal::all_fonts().iter().cloned()).collect();
                let font = pop.act(|this, cx| {
                    let typed = this.typed(UI_FONT).trim().to_string();
                    // "System" is the interface's own font, stored as none.
                    let font = if typed == "System" { String::new() } else { typed };
                    this.set_pref(cx, |s| s.ui_font = font);
                });
                let shown = if settings.ui_font.is_empty() { "System".to_string() } else { settings.ui_font.clone() };
                let mut accents = Vec::new();
                for option in PROFILE_COLORS {
                    // Empty means the theme's own accent, so show that rather than the current override.
                    let fill = if option.is_empty() { paint.muted } else { parse_hex(option) };
                    let pick = pop.act(move |this, cx| this.set_pref(cx, |s| s.accent = option.to_string()));
                    accents.push(Node::Swatch { size: 18., fill: Some(rgba(fill)), selected: settings.accent == option, action: Some(pick), child: None });
                }
                let mut backgrounds = Vec::new();
                if !settings.wallpaper.is_empty() {
                    backgrounds.push(button("Use Liquid Glass", pop.act(|this, cx| this.set_pref(cx, |s| s.wallpaper.clear())), Style::Plain, Key::None));
                }
                backgrounds.push(button("Choose Image…", pop.act(|_, cx| pick_path(cx, false, |s, path| s.wallpaper = path)), Style::Plain, Key::None));
                let mut page = vec![
                    setting("Theme", None, choice(pop, vec![("System", "system".to_string()), ("Light", "light".into()), ("Dark", "dark".into())], settings.theme.clone(), |s, v| s.theme = v)),
                    setting("Font", Some("Used across the browser's interface. Type to search, or type any font name."), Node::Combo { field: UI_FONT, value: Initial(shown), items: fonts, submit: font }),
                    setting("Use font for web pages", Some("Pages that don't choose their own font use it."), switch(pop, settings.ui_font_pages, |s, v| s.ui_font_pages = v)),
                    setting(
                        "Font size",
                        Some("Size of the browser's interface text."),
                        stepper(pop, format!("{:.0} pt", settings.ui_font_size), |s| s.ui_font_size = (s.ui_font_size - 1.0).max(10.0), |s| s.ui_font_size = (s.ui_font_size + 1.0).min(18.0)),
                    ),
                    setting(
                        "Line height",
                        Some("Line height as a multiple of the font size."),
                        stepper(
                            pop,
                            format!("{:.2}", settings.ui_line_height),
                            |s| s.ui_line_height = ((s.ui_line_height - 0.05) * 100.0).round().max(100.0) / 100.0,
                            |s| s.ui_line_height = ((s.ui_line_height + 0.05) * 100.0).round().min(200.0) / 100.0,
                        ),
                    ),
                    setting(
                        "Page zoom",
                        Some("Default zoom for web pages."),
                        stepper(pop, format!("{:.0}%", settings.page_zoom * 100.0), |s| s.page_zoom = (s.page_zoom - 0.1).max(0.5), |s| s.page_zoom = (s.page_zoom + 0.1).min(2.0)),
                    ),
                    setting("Accent color", Some("Highlights, focus rings and the active window border."), Node::Row { gap: 4., children: accents }),
                    setting("Background", Some(if settings.wallpaper.is_empty() { "Liquid Glass" } else { settings.wallpaper.as_str() }), Node::Row { gap: 6., children: backgrounds }),
                    setting("Reduce motion", Some("Panels, windows and messages appear without animating."), switch(pop, settings.reduce_motion, |s, v| s.reduce_motion = v)),
                    heading("Site zoom"),
                ];
                if settings.site_zoom.is_empty() {
                    page.push(para("Zoom a site with ⌘+ or ⌘− and it is remembered here", 13., Ink::Secondary));
                }
                for (host, zoom) in &settings.site_zoom {
                    let reset = {
                        let host = host.clone();
                        pop.act(move |this, cx| {
                            this.set_pref(cx, |s| {
                                s.site_zoom.remove(&host);
                            })
                        })
                    };
                    page.push(Node::Row {
                        gap: 8.,
                        children: vec![
                            title(host.clone(), 13., Weight::Regular, Ink::Primary),
                            Node::Spacer,
                            text(format!("{:.0}%", zoom * 100.0), 13., Weight::Regular, Ink::Secondary),
                            button("Reset", reset, Style::Plain, Key::None),
                        ],
                    });
                }
                page
            }
            SettingsSection::Search => vec![setting(
                "Search engine used in the address bar",
                None,
                choice(pop, SEARCH_ENGINES.iter().map(|(name, _)| (*name, name.to_string())).collect(), settings.search_engine.clone(), |s, v| s.search_engine = v),
            )],
            SettingsSection::Privacy => {
                let clear_history = pop.act(|this, _| {
                    let _ = this.model.lock().clear_data(true, false);
                    this.settings_note = Some("History cleared".into());
                });
                let clear_downloads = pop.act(|this, _| {
                    let _ = this.model.lock().clear_data(false, true);
                    this.settings_note = Some("Download list cleared".into());
                });
                let clear_site_data = pop.act(|this, _| {
                    let store = Some(this.model.lock()).and_then(|model| model.active_profile().map(|p| p.data_store_id.clone()));
                    if let Some(store) = store {
                        this.engine.clear_site_data(&store);
                    }
                    this.settings_note = Some("Cookies, cache and site data cleared".into());
                });
                let mut page = vec![
                    setting("Save browsing history", None, switch(pop, settings.save_history, |s, v| s.save_history = v)),
                    setting("HTTPS-only mode", Some("Upgrade http:// pages to https://."), switch(pop, settings.https_only, |s, v| s.https_only = v)),
                    setting("Block ads and trackers", Some("Blocks requests to known ad and tracking hosts."), switch(pop, settings.block_ads, |s, v| s.block_ads = v)),
                    setting("Block pop-ups", Some("Stops pages opening windows on their own. Applies to new tabs."), switch(pop, settings.block_popups, |s, v| s.block_popups = v)),
                    setting("Fraudulent website warnings", Some("Applies to new tabs."), switch(pop, settings.fraud_warnings, |s, v| s.fraud_warnings = v)),
                    heading("Clear browsing data (current profile)"),
                    Node::Row {
                        gap: 8.,
                        children: vec![
                            button("Clear history", clear_history, Style::Plain, Key::None),
                            button("Clear download list", clear_downloads, Style::Plain, Key::None),
                            button("Clear cookies & site data", clear_site_data, Style::Plain, Key::None),
                            Node::Spacer,
                        ],
                    },
                    heading("Site permissions (current profile)"),
                ];
                page.extend(self.permission_list(pop));
                page
            }
            SettingsSection::Passwords => {
                let mut page = vec![
                    setting("Offer to save passwords", Some("Asks after you sign in. Passwords stay on this Mac."), switch(pop, settings.offer_passwords, |s, v| s.offer_passwords = v)),
                    setting("Suggest saved passwords", Some("Lists saved logins when you click a sign-in field."), switch(pop, settings.autofill_passwords, |s, v| s.autofill_passwords = v)),
                ];
                page.extend(self.passwords_settings(pop));
                page
            }
            SettingsSection::Content => {
                let agent = pop.act(|this, cx| {
                    let value = this.typed(USER_AGENT).trim().to_string();
                    this.set_pref(cx, |s| s.user_agent = value);
                });
                vec![
                    setting("JavaScript", Some("Applies to new tabs."), switch(pop, settings.javascript, |s, v| s.javascript = v)),
                    setting("Custom user agent", Some("Leave empty for the default. Press Return to save."), commit(USER_AGENT, settings.user_agent.clone(), "Default", agent)),
                ]
            }
            SettingsSection::Downloads => {
                let downloads = pop.act(|_, cx| pick_path(cx, true, |s, dir| s.download_dir = dir));
                let shots = pop.act(|_, cx| pick_path(cx, true, |s, dir| s.screenshot_dir = dir));
                let mut shot_buttons = Vec::new();
                if !settings.screenshot_dir.is_empty() {
                    shot_buttons.push(button("Use Downloads", pop.act(|this, cx| this.set_pref(cx, |s| s.screenshot_dir.clear())), Style::Plain, Key::None));
                }
                shot_buttons.push(button("Change…", shots, Style::Plain, Key::None));
                vec![
                    setting("Location", Some(settings.download_dir.as_str()), button("Change…", downloads, Style::Plain, Key::None)),
                    setting(
                        "Screenshot location",
                        Some(if settings.screenshot_dir.is_empty() { "Same as downloads" } else { settings.screenshot_dir.as_str() }),
                        Node::Row { gap: 6., children: shot_buttons },
                    ),
                    setting("Ask where to save each file", None, switch(pop, settings.ask_download_path, |s, v| s.ask_download_path = v)),
                ]
            }
            SettingsSection::Terminal => {
                let mut fonts = crate::terminal::monospace_fonts().to_vec();
                if !fonts.contains(&settings.terminal_font) {
                    // A font typed in earlier (or one CoreText doesn't flag as monospace) stays selectable.
                    fonts.insert(0, settings.terminal_font.clone());
                }
                let font = pop.act(|this, cx| {
                    let font = this.typed(TERM_FONT).trim().to_string();
                    if !font.is_empty() {
                        this.set_pref(cx, |s| s.terminal_font = font);
                    }
                });
                let shell = pop.act(|this, cx| {
                    let value = this.typed(SHELL).trim().to_string();
                    this.set_pref(cx, |s| s.terminal_shell = value);
                });
                let login_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                vec![
                    setting(
                        "Font",
                        Some("Installed monospace fonts. Type to search, or type any font name."),
                        Node::Combo { field: TERM_FONT, value: Initial(settings.terminal_font.clone()), items: fonts, submit: font },
                    ),
                    setting(
                        "Font size",
                        None,
                        stepper(
                            pop,
                            format!("{:.0} pt", settings.terminal_font_size),
                            |s| s.terminal_font_size = (s.terminal_font_size - 1.0).max(8.0),
                            |s| s.terminal_font_size = (s.terminal_font_size + 1.0).min(32.0),
                        ),
                    ),
                    setting(
                        "Line height",
                        Some("Row height as a multiple of the font size."),
                        stepper(
                            pop,
                            format!("{:.2}", settings.terminal_line_height),
                            |s| s.terminal_line_height = ((s.terminal_line_height - 0.05) * 100.0).round().max(100.0) / 100.0,
                            |s| s.terminal_line_height = ((s.terminal_line_height + 0.05) * 100.0).round().min(200.0) / 100.0,
                        ),
                    ),
                    setting(
                        "Cursor",
                        Some("Programs like vim may still change it while they run."),
                        choice(pop, vec![("Block", "block".to_string()), ("Bar", "beam".into()), ("Underline", "underline".into())], settings.terminal_cursor.clone(), |s, v| {
                            s.terminal_cursor = v
                        }),
                    ),
                    setting(
                        "Scrollback",
                        Some("Lines of history kept per terminal."),
                        choice(pop, vec![("1,000", 1_000), ("10,000", 10_000), ("50,000", 50_000), ("100,000", 100_000)], settings.terminal_scrollback, |s, v| {
                            s.terminal_scrollback = v
                        }),
                    ),
                    setting(
                        "Shell",
                        Some("Program started in new terminals. Leave empty for your login shell. Press Return to save."),
                        commit(SHELL, settings.terminal_shell.clone(), &login_shell, shell),
                    ),
                ]
            }
            SettingsSection::Performance => vec![
                setting(
                    "Suspend background tabs after",
                    Some("Frees memory from tabs you haven't looked at."),
                    choice(pop, SUSPEND_OPTIONS.to_vec(), settings.suspend_after_secs, |s, v| s.suspend_after_secs = v),
                ),
                setting(
                    "Maximum live tabs",
                    Some("Older background tabs are suspended past this count."),
                    stepper(pop, settings.max_live_tabs.to_string(), |s| s.max_live_tabs = s.max_live_tabs.saturating_sub(1).max(1), |s| s.max_live_tabs = (s.max_live_tabs + 1).min(30)),
                ),
            ],
            SettingsSection::Developer => vec![setting(
                "Allow Web Inspector",
                Some(&match crate::shortcuts::hint(&settings, browser_engine::Shortcut::Inspect) {
                    keys if keys.is_empty() => "Right-click a page and choose Inspect Element.".to_string(),
                    keys => format!("Right-click a page and choose Inspect Element, or press {keys}."),
                }),
                switch(pop, settings.inspector, |s, v| s.inspector = v),
            )],
            SettingsSection::About => vec![
                setting("Version", None, text(env!("CARGO_PKG_VERSION"), 13., Weight::Regular, Ink::Secondary)),
                self.update_row(pop, cx),
                setting(
                    "Check for updates automatically",
                    Some("New versions download in the background and install when you restart."),
                    switch(pop, settings.auto_update, |s, v| s.auto_update = v),
                ),
                setting("Engine", None, text("WebKit (WKWebView)", 13., Weight::Regular, Ink::Secondary)),
                setting("Data folder", Some(app_support_dir().display().to_string().as_str()), Node::Spacer),
            ],
        }
    }

    fn update_row(&self, pop: &mut Pop, cx: &gpui::App) -> Node {
        use crate::update::Status;
        let status = crate::update::status(cx).clone();
        let detail = match &status {
            Status::Unavailable => "This build doesn't update itself.".to_string(),
            Status::Idle => "Updates come from GitHub releases.".to_string(),
            Status::Checking => "Checking…".to_string(),
            Status::Downloading(version) => format!("Downloading NxBrowser {version}…"),
            Status::Ready { version, .. } => format!("NxBrowser {version} is ready to install."),
            Status::Failed(message) => message.clone(),
        };
        let ready = matches!(status, Status::Ready { .. });
        let enabled = matches!(status, Status::Idle | Status::Failed(_) | Status::Ready { .. });
        let run = pop.act(move |_, cx| {
            if ready {
                crate::update::restart(cx);
            } else {
                crate::update::check(true, cx);
            }
        });
        let control = Node::Button {
            title: if ready { "Restart to Update" } else { "Check for Updates" }.into(),
            action: run,
            style: if ready { Style::Primary } else { Style::Plain },
            key: Key::None,
            enabled,
        };
        setting("Updates", Some(&detail), control)
    }

    fn profiles_settings(&self, pop: &mut Pop, paint: Paint) -> Vec<Node> {
        let (profiles, active) = {
            let model = self.model.lock();
            (model.profiles.clone(), model.active_profile_id.clone())
        };
        let only_one = profiles.len() <= 1;
        let new_profile = pop.act(|this, cx| this.open_new_profile(cx));
        let mut page = vec![Node::Row {
            gap: 8.,
            children: vec![title("Click a profile to edit it.", 12., Weight::Regular, Ink::Secondary), Node::Spacer, button("New profile…", new_profile, Style::Primary, Key::None)],
        }];
        let mut list = Vec::new();
        for profile in &profiles {
            let id = profile.id.clone();
            let is_active = profile.id == active;
            let expanded = self.settings_profile.as_deref() == Some(profile.id.as_str());
            let toggle = pop.act(move |this, cx| {
                if this.settings_profile.as_deref() == Some(id.as_str()) {
                    this.settings_profile = None;
                } else {
                    this.select_settings_profile(id.clone(), cx);
                }
            });
            let state = if is_active {
                text("Active", 11., Weight::Regular, Ink::Secondary)
            } else {
                let switch_id = profile.id.clone();
                let switch_to = pop.act(move |this, cx| {
                    this.with_model(|model| {
                        let _ = model.switch_profile(&switch_id);
                    });
                    this.refresh_url(cx);
                });
                button("Switch", switch_to, Style::Plain, Key::None)
            };
            let row = Node::Item {
                height: 40.,
                click: Some(toggle),
                drag: None,
                children: vec![profile_badge_node(profile, is_active, 28., paint), title(profile.name.clone(), 13., Weight::Regular, Ink::Primary), Node::Spacer, state],
            };
            list.push(if expanded { Node::Card { tint: Ink::Secondary, child: Box::new(row) } } else { row });
            if !expanded {
                continue;
            }
            let save = pop.act(|this, cx| this.save_profile_name(cx));
            let mut editor = vec![commit(PROFILE_EDIT, profile.name.clone(), "Profile name", save), text("Icon", 12., Weight::Semibold, Ink::Secondary)];
            let (icons, colors) = appearance_swatches(pop, &profile.name, &profile.icon, &profile.color, paint, |this, icon, color| {
                if let Some(id) = this.settings_profile.clone() {
                    let _ = this.model.lock().set_profile_appearance(&id, &icon, &color);
                }
            });
            editor.extend(swatch_rows(icons));
            editor.push(text("Color", 12., Weight::Semibold, Ink::Secondary));
            editor.extend(swatch_rows(colors));
            let delete_id = profile.id.clone();
            let delete = pop.act(move |this, _| this.delete_prompt = Some(delete_id.clone()));
            editor.push(Node::Row {
                gap: 0.,
                children: vec![Node::Spacer, Node::Button { title: "Delete profile…".into(), action: delete, style: Style::Danger, key: Key::None, enabled: !only_one }],
            });
            list.push(Node::Column { gap: 8., pad: (46., 8.), children: editor });
        }
        page.push(Node::Column { gap: 2., pad: (0., 8.), children: list });
        let export = pop.act(|this, cx| this.export_profile(cx));
        let import = pop.act(|this, cx| this.import_profile(cx));
        page.push(setting(
            "Backup",
            Some("Export the active profile, or import one from a file."),
            Node::Row { gap: 8., children: vec![button("Export active", export, Style::Plain, Key::None), button("Import…", import, Style::Plain, Key::None)] },
        ));
        page
    }

    pub(crate) fn save_profile_name(&mut self, cx: &mut Context<Self>) {
        let name = self.typed(PROFILE_EDIT).trim().to_string();
        if let (false, Some(id)) = (name.is_empty(), self.settings_profile.clone()) {
            let mut model = self.model.lock();
            // Blur saves too, so leaving the field untouched must not rename or note anything.
            if model.profiles.iter().any(|profile| profile.id == id && profile.name != name) {
                let _ = model.rename_profile(&id, &name);
                drop(model);
                self.settings_note = Some("Profile renamed".into());
            }
        }
        cx.notify();
    }

    fn permission_list(&self, pop: &mut Pop) -> Vec<Node> {
        let entries = self.model.lock().permissions_map().unwrap_or_default();
        if entries.is_empty() {
            return vec![para("No sites have asked for permissions yet", 13., Ink::Secondary)];
        }
        let mut rows = Vec::new();
        for (origin, kind, decision) in entries {
            let mut children = vec![Node::Column {
                gap: 2.,
                pad: (0., 0.),
                children: vec![title(origin.clone(), 13., Weight::Regular, Ink::Primary), title(format!("{kind}: {decision}"), 11., Weight::Regular, Ink::Secondary)],
            }];
            for (action, label) in [("allow", "Allow"), ("deny", "Deny"), ("reset", "Reset")] {
                let (origin, kind) = (origin.clone(), kind.clone());
                let run = pop.act(move |this, _| {
                    let model = this.model.lock();
                    let _ = if action == "reset" { model.remove_permission(&origin, &kind) } else { model.set_permission(&origin, &kind, action) };
                    drop(model);
                    this.sync_permissions();
                });
                children.push(button_if(action != decision, label, run));
            }
            rows.push(Node::Row { gap: 8., children });
        }
        vec![Node::Column { gap: 6., pad: (0., 0.), children: rows }]
    }

}

/// Icon and colour swatches for a profile named `name`; `pick` receives the chosen (icon, color).
fn appearance_swatches(pop: &mut Pop, name: &str, icon: &str, color: &str, paint: Paint, pick: fn(&mut BrowserWindow, String, String)) -> (Vec<Node>, Vec<Node>) {
    let mut icons = Vec::new();
    for option in PROFILE_ICONS.into_iter().chain(PROFILE_SVG_ICONS) {
        let glyph = if option.ends_with(".svg") {
            crate::native::asset(option).map(|data| Node::Svg { data, size: 15., ink: Ink::Primary })
        } else {
            Some(text(if option.is_empty() { crate::window::rail_mark(name) } else { option.to_string() }, 15., Weight::Regular, Ink::Primary))
        };
        let color = color.to_string();
        let choose = pop.act(move |this, _| pick(this, option.to_string(), color.clone()));
        icons.push(Node::Swatch { size: 30., fill: Some(rgba(paint.secondary)), selected: icon == option, action: Some(choose), child: glyph.map(Box::new) });
    }
    let mut colors = Vec::new();
    for option in PROFILE_COLORS {
        let icon = icon.to_string();
        let choose = pop.act(move |this, _| pick(this, icon.clone(), option.to_string()));
        let fill = if option.is_empty() { paint.accent } else { parse_hex(option) };
        colors.push(Node::Swatch { size: 30., fill: Some(rgba(fill)), selected: color == option, action: Some(choose), child: None });
    }
    (icons, colors)
}

/// Swatches 11 to a row, which fills 400pt.
fn swatch_rows(swatches: Vec<Node>) -> Vec<Node> {
    let mut swatches = swatches.into_iter().peekable();
    let mut rows = Vec::new();
    while swatches.peek().is_some() {
        // The spacer keeps a short last row's final swatch from stretching across the rest.
        rows.push(Node::Row { gap: 6., children: swatches.by_ref().take(11).chain([Node::Spacer]).collect() });
    }
    rows
}

fn switch(pop: &mut Pop, on: bool, apply: Apply<bool>) -> Node {
    Node::Switch { on, action: pop.act(move |this, cx| this.set_pref(cx, |s| apply(s, !on))) }
}

/// A pop-up menu of `options`, showing the one equal to `current`.
fn choice<T: Clone + PartialEq + 'static>(pop: &mut Pop, options: Vec<(&'static str, T)>, current: T, apply: Apply<T>) -> Node {
    let selected = options.iter().position(|(_, value)| *value == current).unwrap_or(0);
    let mut first = None;
    let mut items = Vec::new();
    for (label, value) in options {
        // Consecutive ids: the pop-up runs `first + index`.
        let id = pop.act(move |this, cx| {
            let value = value.clone();
            this.set_pref(cx, |s| apply(s, value));
        });
        first.get_or_insert(id);
        items.push(label.to_string());
    }
    Node::Popup { items, selected, first: first.unwrap_or_default() }
}

fn stepper(pop: &mut Pop, value: String, minus: fn(&mut Settings), plus: fn(&mut Settings)) -> Node {
    // `plus` must follow `minus`: the stepper's up arrow runs `down + 1`.
    let down = pop.act(move |this, cx| this.set_pref(cx, minus));
    pop.act(move |this, cx| this.set_pref(cx, plus));
    Node::Stepper { value, down }
}


/// A colour as RGBA for a native swatch.
fn rgba(color: gpui::Hsla) -> [f64; 4] {
    let [r, g, b] = crate::native::rgb(color);
    [r, g, b, f64::from(color.a)]
}

pub(crate) fn logical(bounds: gpui::Bounds<gpui::Pixels>) -> LogicalRect {
    LogicalRect {
        x: f64::from(bounds.origin.x),
        y: f64::from(bounds.origin.y),
        width: f64::from(bounds.size.width),
        height: f64::from(bounds.size.height),
    }
}

/// A native menu row: (label, enabled, what picking it runs); `None` is a separator.
pub(crate) type MenuRow = Option<(std::borrow::Cow<'static, str>, bool, MenuAction)>;
pub(crate) type MenuAction = Box<dyn Fn(&mut BrowserWindow, &mut Context<BrowserWindow>)>;

pub(crate) fn menu_item(label: impl Into<std::borrow::Cow<'static, str>>, enabled: bool, run: impl Fn(&mut BrowserWindow, &mut Context<BrowserWindow>) + 'static) -> MenuRow {
    Some((label.into(), enabled, Box::new(run)))
}

/// Asks for a folder (or a file), then stores it with `set`.
fn pick_path(cx: &mut Context<BrowserWindow>, folder: bool, set: fn(&mut Settings, String)) {
    // gpui's sheet, not runModal: a modal loop inside this click lets gpui's timers re-enter and abort.
    let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: !folder, directories: folder, multiple: false, prompt: None });
    cx.spawn(async move |this, cx| {
        let Ok(Ok(Some(paths))) = paths.await else { return };
        let Some(path) = paths.into_iter().next() else { return };
        let _ = this.update(cx, |this, cx| this.set_pref(cx, |s| set(s, path.display().to_string())));
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_finds_sections_by_setting_and_shortcut() {
        let hits = |query: &str| SettingsSection::ALL.into_iter().filter(|section| section.matches(query)).collect::<Vec<_>>();
        assert_eq!(hits("https"), [SettingsSection::Privacy]);
        assert!(hits("new tab").contains(&SettingsSection::Shortcuts));
        assert_eq!(hits("").len(), SettingsSection::ALL.len());
        assert!(hits("zzzz").is_empty());
    }
}
