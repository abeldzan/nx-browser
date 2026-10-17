use crate::window::{paint, profile_badge, BrowserWindow, Paint};
use browser_core::{app_support_dir, Settings, SEARCH_ENGINES};
use browser_engine::{WebEngine, SHORTCUTS};
use browser_engine::LogicalRect;
use gpui::{FontWeight, anchored, canvas, deferred, div, prelude::*, px, ClickEvent, Context, KeyDownEvent, MouseButton, SharedString};
use crate::ui::{action_button, Kind};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{parse_hex, ActiveTheme, Themeable};
use gpuikit::traits::control_sized::ControlSized;

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

    /// Row titles of the section, so search finds a setting by name.
    fn keywords(self) -> &'static str {
        match self {
            Self::Profiles => "new profile icon color rename delete switch export import backup",
            Self::General => "on startup restore session new tab page home focus windows hover click default browser links other apps profile",
            Self::Shortcuts => "keyboard keys reset",
            Self::Appearance => "theme dark light system page zoom accent color density compact reduce motion site zoom",
            Self::Search => "address bar google duckduckgo",
            Self::Privacy => "save browsing history https ads trackers block pop-ups popups fraudulent warnings clear cookies site data permissions",
            Self::Passwords => "offer save suggest autofill logins vault",
            Self::Content => "javascript custom user agent",
            Self::Downloads => "location folder ask where save file",
            Self::Terminal => "font size line height cursor scrollback shell",
            Self::Performance => "suspend background tabs maximum live tabs memory",
            Self::Developer => "web inspector inspect element",
            Self::About => "version engine webkit data folder",
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
        if self.settings_open {
            let (settings, active) = {
                let model = self.model.lock();
                (model.settings.clone(), model.active_profile_id.clone())
            };
            self.home_input.update(cx, |input, cx| input.set_content_silent(settings.home_page, cx));
            self.ua_input.update(cx, |input, cx| input.set_content_silent(settings.user_agent, cx));
            // Rebuilt on show, so it lists fonts installed since and selects the saved one.
            self.term_font_picker = None;
            self.term_shell_input.update(cx, |input, cx| input.set_content_silent(settings.terminal_shell, cx));
            self.settings_query.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
            self.select_settings_profile(active, cx);
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
        self.new_profile_input.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
        self.new_profile = Some((String::new(), String::new()));
        self.rail_menu = None;
        cx.notify();
    }

    pub(crate) fn create_new_profile(&mut self, cx: &mut Context<Self>) {
        let Some((icon, color)) = self.new_profile.take() else {
            return;
        };
        let typed = self.new_profile_input.read(cx).content().trim().to_string();
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
    pub(crate) fn new_profile_modal(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let (icon, color) = self.new_profile.clone().unwrap_or_default();
        let typed = self.new_profile_input.read(cx).content().trim().to_string();
        let preview = browser_core::Profile {
            id: String::new(),
            name: if typed.is_empty() { "Profile".into() } else { typed },
            storage_path: Default::default(),
            data_store_id: String::new(),
            created_at: 0,
            icon: icon.clone(),
            color: color.clone(),
        };
        let (icons, colors) = appearance_pickers("new", &preview.name, &icon, &color, |this, icon, color| {
            this.new_profile = Some((icon, color));
        }, paint, cx);
        let card = v_stack()
            .w(px(440.))
            .max_w_full()
            .gap(px(14.))
            .p(px(24.))
            .child(title("New profile"))
            .child(div().text_color(paint.muted).child("Each profile keeps its own tabs, history, cookies and site data."))
            .child(
                h_stack()
                    .gap(px(12.))
                    .items_center()
                    .child(profile_badge(&preview, false, 44., paint))
                    .child(div().flex_1().child(text_field(&self.new_profile_input, cx).placeholder("Profile name").full_width(true))),
            )
            .child(heading("Icon", paint))
            .child(icons)
            .child(heading("Color", paint))
            .child(colors)
            .child(
                h_stack()
                    .pt(px(12.))
                    .gap(px(10.))
                    .justify_end()
                    .child(action_button("new-profile-cancel", "Cancel", Kind::Secondary, true, cx, |this, cx| {
                        this.new_profile = None;
                        cx.notify();
                    }))
                    .child(action_button("new-profile-create", "Create profile", Kind::Primary, true, cx, |this, cx| {
                        this.create_new_profile(cx);
                    })),
            );
        self.modal_shell("new-profile-modal", card, |this, cx| {
            this.new_profile = None;
            cx.notify();
        }, cx)
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
    pub(crate) fn delete_modal(&self, id: &str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let name = self.model.lock().profiles.iter().find(|profile| profile.id == id).map(|profile| profile.name.clone()).unwrap_or_default();
        let id = id.to_string();
        let card = v_stack()
            .w(px(380.))
            .max_w_full()
            .gap(px(10.))
            .p(px(24.))
            .child(title(format!("Delete “{name}”?")))
            .child(div().text_color(paint.muted).child(
                "Its tabs, sessions, history, bookmarks, cookies and site data will be removed. This can't be undone.",
            ))
            .child(
                h_stack()
                    .pt(px(12.))
                    .gap(px(10.))
                    .justify_end()
                    .child(action_button("delete-cancel", "Cancel", Kind::Secondary, true, cx, |this, cx| {
                        this.delete_prompt = None;
                        cx.notify();
                    }))
                    .child(action_button("delete-confirm", "Delete profile", Kind::Danger, true, cx, move |this, cx| this.delete_profile_now(&id, cx),
                    )),
            );
        self.modal_shell("delete-modal", card, |this, cx| {
            this.delete_prompt = None;
            cx.notify();
        }, cx)
    }

    /// Which profile links from other apps open in.
    pub(crate) fn link_profile_modal(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let (profiles, active) = {
            let model = self.model.lock();
            (model.profiles.clone(), model.active_profile_id.clone())
        };
        let heading = match self.pending_links.len() {
            1 => "Open link in".to_string(),
            n => format!("Open {n} links in"),
        };
        let mut list = v_stack().gap(px(2.));
        for profile in profiles {
            let id = profile.id.clone();
            list = list.child(
                h_stack()
                    .id(SharedString::from(format!("link-profile-{id}")))
                    .gap(px(10.))
                    .items_center()
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .hover(|style| style.bg(paint.secondary))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let urls = std::mem::take(&mut this.pending_links);
                        this.open_links_in(&id, urls, cx);
                    }))
                    .child(profile_badge(&profile, profile.id == active, 28., paint))
                    .child(div().flex_1().child(profile.name.clone()))
                    .when(profile.id == active, |row| row.child(div().text_color(paint.muted).child("Active"))),
            );
        }
        let card = v_stack()
            .w(px(380.))
            .max_w_full()
            .gap(px(10.))
            .p(px(24.))
            .child(title(heading))
            .child(div().text_color(paint.muted).truncate().child(self.pending_links.first().cloned().unwrap_or_default()))
            .child(list);
        self.modal_shell("link-profile-modal", card, |this, cx| {
            this.pending_links.clear();
            cx.notify();
        }, cx)
    }

    /// Dimmed backdrop + centred card. Reports the card's bounds so web pages under it are cut out.
    pub(crate) fn modal_shell(
        &self,
        id: &'static str,
        card: gpui::Div,
        dismiss: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let watch = cx.entity();
        let card = card
            .relative()
            .rounded(px(12.))
            .overflow_hidden()
            .bg(paint.bg)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                canvas(
                    move |bounds: gpui::Bounds<gpui::Pixels>, _, cx| {
                        watch.update(cx, |view, _| {
                            view.overlay_rects.push((logical(bounds), 12.))
                        })
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        div()
            .id(id)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.))
            .bg(gpui::black().opacity(0.45))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| dismiss(this, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    dismiss(this, cx);
                }
            }))
            .child(card)
    }

    pub(crate) fn rail_menu_view(&self, (id, index, position): (String, usize, gpui::Point<gpui::Pixels>), cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let danger = cx.theme().danger();
        let count = self.model.lock().profiles.len().max(1);
        let watch = cx.entity();
        let item = |label: &'static str, enabled: bool, destructive: bool, run: fn(&mut Self, &str, usize, &mut Context<Self>), cx: &mut Context<Self>| {
            let id = id.clone();
            div()
                .id(label)
                .px(px(10.))
                .py(px(5.))
                .rounded(px(5.))
                .text_color(if !enabled { paint.muted } else if destructive { danger } else { paint.fg })
                .when(enabled, |row| {
                    row.cursor_pointer().hover(|style| style.bg(paint.secondary)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.rail_menu = None;
                        run(this, &id, index, cx);
                        cx.notify();
                    }))
                })
                .child(label)
        };
        let menu = v_stack()
            .id("rail-menu")
            .relative()
            .min_w(px(170.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.rail_menu = None;
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds: gpui::Bounds<gpui::Pixels>, _, cx| watch.update(cx, |view, _| view.overlay_rects.push((logical(bounds), 8.))),
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(item("Move up", index > 0, false, |this, id, index, _| this.with_model(|model| { let _ = model.move_profile(id, index - 1); }), cx))
            .child(item("Move down", index + 1 < count, false, |this, id, index, _| this.with_model(|model| { let _ = model.move_profile(id, index + 1); }), cx))
            .child(div().my(px(4.)).h(px(1.)).bg(paint.border))
            .child(item("Edit profile…", true, false, |this, id, _, cx| this.open_profile_settings(id.to_string(), cx), cx))
            .child(item("Delete profile…", count > 1, true, |this, id, _, _| this.delete_prompt = Some(id.to_string()), cx));
        deferred(anchored().position(position).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(1)
    }

    pub(crate) fn set_pref(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let _ = self.model.lock().update_settings(f);
        self.apply_settings();
        let settings = self.model.lock().settings.clone();
        crate::terminal::set_prefs(&settings, cx);
        cx.notify();
    }

    fn select_settings_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let name = self.model.lock().profiles.iter().find(|profile| profile.id == id).map(|profile| profile.name.clone()).unwrap_or_default();
        self.name_input.update(cx, |input, cx| input.set_content_silent(name, cx));
        self.settings_profile = Some(id);
    }

    pub(crate) fn settings_modal(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let query = self.settings_query.read(cx).content().trim().to_lowercase();
        let shown = SettingsSection::ALL.into_iter().filter(|section| section.matches(&query)).collect::<Vec<_>>();
        let current = if shown.contains(&self.settings_section) { Some(self.settings_section) } else { shown.first().copied() };
        let mut nav = v_stack()
            .w(px(168.))
            .h_full()
            .gap(px(1.))
            .p(px(8.))
            .bg(paint.surface)
            .border_r_1()
            .border_color(paint.border)
            .child(div().pb(px(6.)).child(text_field(&self.settings_query, cx).placeholder("Search").full_width(true)))
            .when(shown.is_empty(), |nav| nav.child(div().px(px(10.)).py(px(6.)).text_size(px(13.)).text_color(paint.muted).child("No matches")));
        for section in shown {
            let selected = current == Some(section);
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("settings-nav-{}", section.label())))
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .bg(if selected { paint.accent_bg } else { gpui::transparent_black() })
                    .text_color(if selected { paint.accent } else { paint.fg })
                    .hover(|style| style.bg(paint.secondary))
                    .child(section.label())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.end_recording();
                        this.settings_section = section;
                        cx.notify();
                    })),
            );
        }
        let card = v_stack()
            .w(px(780.))
            .h(px(580.))
            .max_w_full()
            .max_h_full()
            .child(
                h_stack()
                    .w_full()
                    .px(px(20.))
                    .py(px(10.))
                    .justify_between()
                    .items_center()
                    .border_b_1()
                    .border_color(paint.border)
                    .child(title("Settings"))
                    .child(icon_button("settings-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_settings(cx),
                    ))),
            )
            .child(
                h_stack().flex_1().min_h(px(0.)).w_full().child(nav).child(
                    div().flex_1().h_full().overflow_hidden().child(
                        scroll_area("settings-content")
                            .vertical()
                            .full_height(true)
                            .child(v_stack().w_full().px(px(28.)).py(px(24.)).children(current.map(|section| self.settings_page(section, &query, paint, cx)))),
                    ),
                ),
            )
            .when_some(self.settings_note.clone(), |card, note| {
                card.child(
                    div()
                        .px(px(20.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(paint.border)
                        .text_size(px(12.))
                        .text_color(paint.muted)
                        .child(note),
                )
            });
        self.modal_shell("settings-backdrop", card, Self::toggle_settings, cx)
    }

    fn settings_page(&self, section: SettingsSection, query: &str, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let settings = self.model.lock().settings.clone();
        let page = v_stack()
            .w_full()
            .gap(px(4.))
            .child(div().pb(px(8.)).text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(section.label()));
        match section {
            SettingsSection::Profiles => page.child(self.profiles_settings(paint, cx)),
            SettingsSection::General => page
                .child(row(
                    "On startup",
                    None,
                    choice(
                        "startup",
                        vec![("Restore last session", true), ("Open new tab page", false)],
                        settings.restore_session,
                        |s, v| s.restore_session = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "New tab page",
                    Some("Address opened for new tabs. Press Return to save."),
                    div().w(px(260.)).child(text_field(&self.home_input, cx).placeholder("nxbrowser://home").full_width(true)),
                    paint,
                ))
                .child(row(
                    "Focus windows",
                    Some("Hover focuses the window under the pointer; click waits for a click."),
                    choice("window-focus", vec![("Click", false), ("Hover", true)], settings.focus_on_hover, |s, v| s.focus_on_hover = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Links from other apps",
                    Some("Which profile a link clicked in another app opens in."),
                    choice("link-profile", vec![("Ask each time", true), ("Active profile", false)], settings.ask_link_profile, |s, v| s.ask_link_profile = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Default browser",
                    Some("Open web links from every app in NxBrowser. macOS asks you to confirm."),
                    action_button("make-default", "Make default", Kind::Secondary, true, cx, |this, cx| {
                        if let Err(error) = browser_engine_macos::make_default_browser() {
                            this.settings_note = Some(error.to_string().into());
                        }
                        cx.notify();
                    }),
                    paint,
                )),
            SettingsSection::Shortcuts => page.child(self.shortcuts_settings(&settings, query, paint, cx)),
            SettingsSection::Appearance => page
                .child(row(
                    "Theme",
                    None,
                    choice(
                        "theme",
                        vec![("System", "system".to_string()), ("Light", "light".into()), ("Dark", "dark".into())],
                        settings.theme.clone(),
                        |s, v| s.theme = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Page zoom",
                    Some("Default zoom for web pages."),
                    stepper(
                        "zoom",
                        format!("{:.0}%", settings.page_zoom * 100.0),
                        |s| s.page_zoom = (s.page_zoom - 0.1).max(0.5),
                        |s| s.page_zoom = (s.page_zoom + 0.1).min(2.0),
                        cx,
                    ),
                    paint,
                ))
                .child(row("Accent color", Some("Highlights, focus rings and the active window border."), self.accent_picker(&settings.accent, paint, cx), paint))
                .child(row(
                    "Density",
                    Some("Height of the title bar and window bars."),
                    choice(
                        "density",
                        vec![("Comfortable", "comfortable".to_string()), ("Compact", "compact".into())],
                        settings.density.clone(),
                        |s, v| s.density = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Reduce motion",
                    Some("Panels, windows and messages appear without animating."),
                    switch("reduce-motion", settings.reduce_motion, |s, v| s.reduce_motion = v, paint, cx),
                    paint,
                ))
                .child(heading("Site zoom", paint))
                .child(self.site_zoom_list(&settings, paint, cx)),
            SettingsSection::Search => page.child(row(
                "Search engine used in the address bar",
                None,
                choice(
                    "engine",
                    SEARCH_ENGINES.iter().map(|(name, _)| (*name, name.to_string())).collect(),
                    settings.search_engine.clone(),
                    |s, v| s.search_engine = v,
                    paint,
                    cx,
                ),
                paint,
            )),
            SettingsSection::Privacy => page
                .child(row("Save browsing history", None, switch("history", settings.save_history, |s, v| s.save_history = v, paint, cx), paint))
                .child(row(
                    "HTTPS-only mode",
                    Some("Upgrade http:// pages to https://."),
                    switch("https", settings.https_only, |s, v| s.https_only = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Block ads and trackers",
                    Some("Blocks requests to known ad and tracking hosts."),
                    switch("ads", settings.block_ads, |s, v| s.block_ads = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Block pop-ups",
                    Some("Stops pages opening windows on their own. Applies to new tabs."),
                    switch("popups", settings.block_popups, |s, v| s.block_popups = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Fraudulent website warnings",
                    Some("Applies to new tabs."),
                    switch("fraud", settings.fraud_warnings, |s, v| s.fraud_warnings = v, paint, cx),
                    paint,
                ))
                .child(heading("Clear browsing data (current profile)", paint))
                .child(
                    h_stack()
                        .gap(px(8.))
                        .flex_wrap()
                        .child(action_button("clear-history", "Clear history", Kind::Secondary, true, cx, |this, cx| {
                            let _ = this.model.lock().clear_data(true, false);
                            this.settings_note = Some("History cleared".into());
                            cx.notify();
                        }))
                        .child(action_button("clear-downloads", "Clear download list", Kind::Secondary, true, cx, |this, cx| {
                            let _ = this.model.lock().clear_data(false, true);
                            this.settings_note = Some("Download list cleared".into());
                            cx.notify();
                        }))
                        .child(action_button("clear-site-data", "Clear cookies & site data", Kind::Secondary, true, cx, |this, cx| {
                            let store = Some(this.model.lock()).and_then(|model| model.active_profile().map(|p| p.data_store_id.clone()));
                            if let Some(store) = store {
                                this.engine.clear_site_data(&store);
                            }
                            this.settings_note = Some("Cookies, cache and site data cleared".into());
                            cx.notify();
                        })),
                )
                .child(heading("Site permissions (current profile)", paint))
                .child(self.permission_list(paint, cx)),
            SettingsSection::Passwords => page
                .child(row(
                    "Offer to save passwords",
                    Some("Asks after you sign in. Passwords stay on this Mac."),
                    switch("offer-passwords", settings.offer_passwords, |s, v| s.offer_passwords = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Suggest saved passwords",
                    Some("Lists saved logins when you click a sign-in field."),
                    switch("autofill-passwords", settings.autofill_passwords, |s, v| s.autofill_passwords = v, paint, cx),
                    paint,
                ))
                .child(self.passwords_settings(paint, cx)),
            SettingsSection::Content => page
                .child(row(
                    "JavaScript",
                    Some("Applies to new tabs."),
                    switch("javascript", settings.javascript, |s, v| s.javascript = v, paint, cx),
                    paint,
                ))
                .child(row(
                    "Custom user agent",
                    Some("Leave empty for the default. Press Return to save."),
                    div().w(px(260.)).child(text_field(&self.ua_input, cx).placeholder("Default").full_width(true)),
                    paint,
                )),
            SettingsSection::Downloads => page
                .child(row(
                    "Location",
                    Some(settings.download_dir.as_str()),
                    action_button("download-dir", "Change…", Kind::Secondary, true, cx, |_, cx| {
                        // gpui's sheet, not runModal: a modal loop inside this click lets gpui's timers re-enter and abort.
                        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                        cx.spawn(async move |this, cx| {
                            let Ok(Ok(Some(paths))) = paths.await else { return };
                            let Some(path) = paths.into_iter().next() else { return };
                            let _ = this.update(cx, |this, cx| this.set_pref(cx, |s| s.download_dir = path.display().to_string()));
                        })
                        .detach();
                    }),
                    paint,
                ))
                .child(row(
                    "Ask where to save each file",
                    None,
                    switch("ask-download", settings.ask_download_path, |s, v| s.ask_download_path = v, paint, cx),
                    paint,
                )),
            SettingsSection::Terminal => page
                .child(row(
                    "Font",
                    Some("Installed monospace fonts. Type to search, or type any font name."),
                    div().w(px(260.)).children(self.term_font_picker.clone()),
                    paint,
                ))
                .child(row(
                    "Font size",
                    None,
                    stepper(
                        "term-size",
                        format!("{:.0} pt", settings.terminal_font_size),
                        |s| s.terminal_font_size = (s.terminal_font_size - 1.0).max(8.0),
                        |s| s.terminal_font_size = (s.terminal_font_size + 1.0).min(32.0),
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Line height",
                    Some("Row height as a multiple of the font size."),
                    stepper(
                        "term-line",
                        format!("{:.2}", settings.terminal_line_height),
                        |s| s.terminal_line_height = ((s.terminal_line_height - 0.05) * 100.0).round().max(100.0) / 100.0,
                        |s| s.terminal_line_height = ((s.terminal_line_height + 0.05) * 100.0).round().min(200.0) / 100.0,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Cursor",
                    Some("Programs like vim may still change it while they run."),
                    choice(
                        "term-cursor",
                        vec![("Block", "block".to_string()), ("Bar", "beam".into()), ("Underline", "underline".into())],
                        settings.terminal_cursor.clone(),
                        |s, v| s.terminal_cursor = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Scrollback",
                    Some("Lines of history kept per terminal."),
                    choice(
                        "term-scrollback",
                        vec![("1,000", 1_000), ("10,000", 10_000), ("50,000", 50_000), ("100,000", 100_000)],
                        settings.terminal_scrollback,
                        |s, v| s.terminal_scrollback = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Shell",
                    Some("Program started in new terminals. Leave empty for your login shell. Press Return to save."),
                    div().w(px(260.)).child(text_field(&self.term_shell_input, cx).placeholder(std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())).full_width(true)),
                    paint,
                )),
            SettingsSection::Performance => page
                .child(row(
                    "Suspend background tabs after",
                    Some("Frees memory from tabs you haven't looked at."),
                    choice(
                        "suspend",
                        SUSPEND_OPTIONS.to_vec(),
                        settings.suspend_after_secs,
                        |s, v| s.suspend_after_secs = v,
                        paint,
                        cx,
                    ),
                    paint,
                ))
                .child(row(
                    "Maximum live tabs",
                    Some("Older background tabs are suspended past this count."),
                    stepper(
                        "live-tabs",
                        settings.max_live_tabs.to_string(),
                        |s| s.max_live_tabs = s.max_live_tabs.saturating_sub(1).max(1),
                        |s| s.max_live_tabs = (s.max_live_tabs + 1).min(30),
                        cx,
                    ),
                    paint,
                )),
            SettingsSection::Developer => page.child(row(
                "Allow Web Inspector",
                Some(&match crate::shortcuts::hint(&settings, browser_engine::Shortcut::Inspect) {
                    keys if keys.is_empty() => "Right-click a page and choose Inspect Element.".to_string(),
                    keys => format!("Right-click a page and choose Inspect Element, or press {keys}."),
                }),
                switch("inspector", settings.inspector, |s, v| s.inspector = v, paint, cx),
                paint,
            )),
            SettingsSection::About => page
                .child(row("Version", None, div().child(env!("CARGO_PKG_VERSION")), paint))
                .child(row("Engine", None, div().child("WebKit (WKWebView)"), paint))
                .child(row("Data folder", Some(app_support_dir().display().to_string().as_str()), div(), paint)),
        }
    }

    fn profiles_settings(&self, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let (profiles, active) = {
            let model = self.model.lock();
            (model.profiles.clone(), model.active_profile_id.clone())
        };
        let only_one = profiles.len() <= 1;
        let mut list = v_stack().w_full().p(px(4.)).gap(px(2.)).rounded(px(10.)).border_1().border_color(paint.border);
        for profile in &profiles {
            let id = profile.id.clone();
            let is_active = profile.id == active;
            let expanded = self.settings_profile.as_deref() == Some(profile.id.as_str());
            let switch_id = profile.id.clone();
            list = list.child(
                h_stack()
                    .id(SharedString::from(format!("settings-profile-{id}")))
                    .w_full()
                    .gap(px(10.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .items_center()
                    .cursor_pointer()
                    .bg(if expanded { paint.secondary } else { gpui::transparent_black() })
                    .hover(|style| style.bg(paint.secondary))
                    .child(profile_badge(profile, is_active, 28., paint))
                    .child(div().flex_1().child(profile.name.clone()))
                    .child(if is_active {
                        div().px(px(8.)).text_size(px(11.)).text_color(paint.muted).child("Active")
                    } else {
                        // Its own click must not also expand the row.
                        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                            action_button(SharedString::from(format!("settings-switch-{id}")), "Switch", Kind::Secondary, true, cx, move |this, cx| {
                                this.with_model(|model| {
                                    let _ = model.switch_profile(&switch_id);
                                });
                                this.refresh_url(cx);
                                cx.notify();
                            }),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if this.settings_profile.as_deref() == Some(id.as_str()) {
                            this.settings_profile = None;
                        } else {
                            this.select_settings_profile(id.clone(), cx);
                        }
                        cx.notify();
                    })),
            );
            if !expanded {
                continue;
            }
            let (icons, colors) = appearance_pickers("edit", &profile.name, &profile.icon, &profile.color, |this, icon, color| {
                if let Some(id) = this.settings_profile.clone() {
                    let _ = this.model.lock().set_profile_appearance(&id, &icon, &color);
                }
            }, paint, cx);
            let delete_id = profile.id.clone();
            list = list.child(
                v_stack()
                    .w_full()
                    .gap(px(8.))
                    .pl(px(46.))
                    .pr(px(8.))
                    .pt(px(6.))
                    .pb(px(12.))
                    .child(text_field(&self.name_input, cx).placeholder("Profile name").full_width(true))
                    .child(heading("Icon", paint))
                    .child(icons)
                    .child(heading("Color", paint))
                    .child(colors)
                    .child(h_stack().pt(px(8.)).justify_end().child(action_button(
                        "settings-delete-profile",
                        "Delete profile…",
                        Kind::Danger,
                        !only_one,
                        cx,
                        move |this, cx| {
                            this.delete_prompt = Some(delete_id.clone());
                            cx.notify();
                        },
                    ))),
            );
        }
        v_stack()
            .w_full()
            .gap(px(12.))
            .child(
                h_stack()
                    .justify_between()
                    .items_center()
                    .child(div().text_size(px(12.)).text_color(paint.muted).child("Click a profile to edit it."))
                    .child(action_button("settings-new-profile", "New profile…", Kind::Primary, true, cx, |this, cx| {
                        this.open_new_profile(cx);
                    })),
            )
            .child(list)
            .child(row(
                "Backup",
                Some("Export the active profile, or import one from a file."),
                h_stack()
                    .gap(px(8.))
                    .child(action_button("settings-export", "Export active", Kind::Secondary, true, cx, |this, cx| {
                        this.export_profile(cx);
                        cx.notify();
                    }))
                    .child(action_button("settings-import", "Import…", Kind::Secondary, true, cx, |this, cx| {
                        this.import_profile(cx);
                    })),
                paint,
            ))
    }

    pub(crate) fn save_profile_name(&mut self, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).content().trim().to_string();
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

    fn accent_picker(&self, current: &str, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let mut colors = h_stack().flex_wrap().justify_end().gap(px(6.)).max_w(px(360.));
        for option in PROFILE_COLORS {
            colors = colors.child(
                swatch(format!("accent-{option}"), current == option, paint)
                    .size(px(24.))
                    // Empty means the theme's own accent, so show that rather than the current override.
                    .bg(if option.is_empty() { cx.theme().fg_muted() } else { parse_hex(option) })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_pref(cx, |s| s.accent = option.to_string()))),
            );
        }
        colors
    }

    fn site_zoom_list(&self, settings: &Settings, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        if settings.site_zoom.is_empty() {
            return div().text_color(paint.muted).child("Zoom a site with ⌘+ or ⌘− and it is remembered here");
        }
        let mut list = v_stack().w_full().gap(px(4.));
        for (index, (host, zoom)) in settings.site_zoom.iter().enumerate() {
            let host = host.clone();
            list = list.child(
                h_stack()
                    .w_full()
                    .gap(px(8.))
                    .items_center()
                    .child(div().flex_1().child(host.clone()))
                    .child(div().text_color(paint.muted).child(format!("{:.0}%", zoom * 100.0)))
                    .child(action_button(SharedString::from(format!("zoom-reset-{index}")), "Reset", Kind::Secondary, true, cx, move |this, cx| {
                        this.set_pref(cx, |s| {
                            s.site_zoom.remove(&host);
                        });
                    })),
            );
        }
        list
    }

    fn permission_list(&self, paint: Paint, cx: &mut Context<Self>) -> gpui::Div {
        let entries = self.model.lock().permissions_map().unwrap_or_default();
        if entries.is_empty() {
            return div().text_color(paint.muted).child("No sites have asked for permissions yet");
        }
        let mut list = v_stack().w_full().gap(px(4.));
        for (index, (origin, kind, decision)) in entries.into_iter().enumerate() {
            let actions = ["allow", "deny", "reset"].map(|action| {
                let (origin, kind) = (origin.clone(), kind.clone());
                let label = match action {
                    "allow" => "Allow",
                    "deny" => "Deny",
                    _ => "Reset",
                };
                action_button(SharedString::from(format!("perm-{index}-{action}")), label, Kind::Secondary, action != decision, cx, move |this, cx| {
                    let model = this.model.lock();
                    let _ = if action == "reset" {
                        model.remove_permission(&origin, &kind)
                    } else {
                        model.set_permission(&origin, &kind, action)
                    };
                    drop(model);
                    this.sync_permissions();
                    cx.notify();
                })
            });
            list = list.child(
                h_stack()
                    .w_full()
                    .gap(px(8.))
                    .items_center()
                    .child(
                        v_stack()
                            .flex_1()
                            .child(origin.clone())
                            .child(div().text_size(px(11.)).text_color(paint.muted).child(format!("{kind}: {decision}"))),
                    )
                    .children(actions),
            );
        }
        list
    }
}

pub(crate) fn heading(label: &'static str, paint: Paint) -> gpui::Div {
    div().pt(px(14.)).pb(px(4.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(paint.muted).child(label)
}

pub(crate) fn title(text: impl Into<SharedString>) -> gpui::Div {
    div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(text.into())
}

pub(crate) fn row(title: &'static str, detail: Option<&str>, control: impl IntoElement, paint: Paint) -> gpui::Div {
    h_stack()
        .w_full()
        .gap(px(16.))
        .items_center()
        .justify_between()
        .py(px(12.))
        .border_b_1()
        .border_color(paint.border.opacity(0.5))
        .child(
            v_stack()
                .flex_1()
                .min_w(px(0.))
                .gap(px(3.))
                .child(title)
                .when_some(detail.map(str::to_string), |column, detail| {
                    column.child(div().text_size(px(11.)).text_color(paint.muted).child(detail))
                }),
        )
        .child(control)
}

/// Icon and color swatch grids; `pick` receives the chosen (icon, color).
fn appearance_pickers(
    prefix: &'static str,
    name: &str,
    icon: &str,
    color: &str,
    pick: fn(&mut BrowserWindow, String, String),
    paint: Paint,
    cx: &mut Context<BrowserWindow>,
) -> (gpui::Div, gpui::Div) {
    let mut icons = h_stack().flex_wrap().gap(px(6.));
    for option in PROFILE_ICONS.into_iter().chain(PROFILE_SVG_ICONS) {
        let label = crate::window::profile_glyph(option, name, 15., paint.fg);
        let color = color.to_string();
        icons = icons.child(
            swatch(format!("{prefix}-icon-{option}"), icon == option, paint)
                .bg(paint.secondary)
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick(this, option.to_string(), color.clone());
                    cx.notify();
                })),
        );
    }
    let mut colors = h_stack().flex_wrap().gap(px(6.));
    for option in PROFILE_COLORS {
        let icon = icon.to_string();
        colors = colors.child(
            swatch(format!("{prefix}-color-{option}"), color == option, paint)
                .bg(if option.is_empty() { paint.accent } else { parse_hex(option) })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    pick(this, icon.clone(), option.to_string());
                    cx.notify();
                })),
        );
    }
    (icons, colors)
}

fn swatch(id: String, selected: bool, paint: Paint) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .size(px(30.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .border_2()
        .border_color(if selected { paint.fg } else { gpui::transparent_black() })
}

fn switch(id: &'static str, on: bool, apply: Apply<bool>, paint: Paint, cx: &mut Context<BrowserWindow>) -> impl IntoElement {
    toggle(id, on, paint).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_pref(cx, |s| apply(s, !on))))
}

/// An on/off track; the caller adds the click handler.
pub(crate) fn toggle(id: impl Into<gpui::ElementId>, on: bool, paint: Paint) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id.into())
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .when(on, |track| track.justify_end())
        .bg(if on { paint.accent } else { paint.border })
        .child(div().size(px(16.)).rounded_full().bg(gpui::white()))
}

/// A segmented control.
fn choice<T: Clone + PartialEq + 'static>(
    id: &'static str,
    options: Vec<(&'static str, T)>,
    current: T,
    apply: Apply<T>,
    paint: Paint,
    cx: &mut Context<BrowserWindow>,
) -> gpui::Div {
    let mut group = h_stack().flex_wrap().justify_end().gap(px(4.)).max_w(px(360.));
    for (label, value) in options {
        let selected = value == current;
        group = group.child(
            div()
                .id(SharedString::from(format!("{id}-{label}")))
                .h(px(30.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(7.))
                .cursor_pointer()
                .border_1()
                .border_color(if selected { paint.accent } else { paint.border })
                .bg(if selected { paint.accent_bg } else { gpui::transparent_black() })
                .text_color(if selected { paint.accent } else { paint.fg })
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let value = value.clone();
                    this.set_pref(cx, |s| apply(s, value));
                })),
        );
    }
    group
}

fn stepper(
    id: &'static str,
    value: String,
    minus: fn(&mut Settings),
    plus: fn(&mut Settings),
    cx: &mut Context<BrowserWindow>,
) -> impl IntoElement {
    h_stack()
        .gap(px(6.))
        .items_center()
        .child(icon_button(SharedString::from(format!("{id}-minus")), Icons::minus()).small().on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| this.set_pref(cx, minus)),
        ))
        .child(div().min_w(px(44.)).flex().justify_center().child(value))
        .child(icon_button(SharedString::from(format!("{id}-plus")), Icons::plus()).small().on_click(
            cx.listener(move |this, _: &ClickEvent, _, cx| this.set_pref(cx, plus)),
        ))
}

/// Reports its parent's bounds each frame so web pages are cut out under that overlay.
pub(crate) fn measure_overlay(radius: f64, cx: &mut Context<BrowserWindow>) -> impl IntoElement {
    let watch = cx.entity();
    canvas(
        move |bounds: gpui::Bounds<gpui::Pixels>, _, cx| watch.update(cx, |view, _| view.overlay_rects.push((logical(bounds), radius))),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn logical(bounds: gpui::Bounds<gpui::Pixels>) -> LogicalRect {
    LogicalRect {
        x: f64::from(bounds.origin.x),
        y: f64::from(bounds.origin.y),
        width: f64::from(bounds.size.width),
        height: f64::from(bounds.size.height),
    }
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
