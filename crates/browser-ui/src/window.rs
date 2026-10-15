use crate::profiles::profile_label;
use crate::settings::SettingsSection;
use crate::sidebar::SidebarPage;
use crate::tabs::tab_label;
use crate::SharedModel;
use browser_core::{
    app_support_dir, downloads_dir, normalize_url, snapshot, spawn_download, spawn_favicon, DownloadUpdate, Effect,
    Model,
};
use browser_engine::{EngineEvent, EngineSettings, LogicalRect, Shortcut, WebEngine};
use browser_engine_macos::{choose_folder_path, choose_open_path, choose_save_path, MacEngine, EXTENSION_SCHEME};
use browser_extensions::{discover, install_from_store, local_id, patch_missing_apis, read_manifest, store_id, Manifest};
use browser_storage::Storage;
use gpui::{
    canvas, div, img, prelude::*, px, relative, rems, ClickEvent, Context, CursorStyle, DispatchPhase, Entity, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString,
    Subscription, Window,
};
use crate::extensions::extension_icon;
use crate::settings::measure_overlay;
use crate::ui::{action_button, Kind};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::input::input;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::input::{InputState, InputStateEvent, SubmitOn};
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{parse_hex, ActiveTheme, GlobalTheme, Theme, ThemeVariant, Themeable};
use gpuikit::traits::control_sized::ControlSized;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct BrowserWindow {
    pub(crate) model: SharedModel,
    pub(crate) engine: MacEngine,
    /// Store URL or extension id typed in the extensions panel.
    pub(crate) ext_input: Entity<InputState>,
    /// What each installed extension's manifest says, by id.
    pub(crate) ext_meta: HashMap<String, Manifest>,
    /// Permission requests from extensions: (prompt, extension id, what it asks for).
    pub(crate) ext_prompts: Vec<(u64, String, String)>,
    /// Store ids being downloaded, and where finished installs report back.
    pub(crate) installing: Vec<String>,
    installs_tx: Sender<(String, Result<std::path::PathBuf, String>)>,
    installs_rx: Receiver<(String, Result<std::path::PathBuf, String>)>,
    /// Last tab reported to extensions as active.
    ext_active: Option<String>,
    address: Entity<InputState>,
    find_input: Entity<InputState>,
    rename_input: Entity<InputState>,
    pub(crate) home_input: Entity<InputState>,
    pub(crate) ua_input: Entity<InputState>,
    pub(crate) name_input: Entity<InputState>,
    pub(crate) settings_open: bool,
    pub(crate) settings_section: SettingsSection,
    /// Confirmation shown in the settings footer after an action.
    pub(crate) settings_note: Option<SharedString>,
    pub(crate) settings_profile: Option<String>,
    pub(crate) delete_prompt: Option<String>,
    /// Bounds of open modal cards this frame; web pages are cut out beneath them.
    /// Open overlays this frame (bounds, corner radius); web pages are cut out beneath them.
    pub(crate) overlay_rects: Vec<(LogicalRect, f64)>,
    /// Right-click menu on a rail profile: (profile id, index, click position).
    pub(crate) downloads_open: bool,
    pub(crate) extensions_open: bool,
    /// Icon and color picked in the new-profile modal while it is open.
    pub(crate) new_profile: Option<(String, String)>,
    pub(crate) new_profile_input: Entity<InputState>,
    /// Live (received, total) bytes per download id, fed by the download threads.
    pub(crate) download_progress: HashMap<String, (u64, Option<u64>)>,
    pub(crate) rail_menu: Option<(String, usize, gpui::Point<Pixels>)>,
    address_focused: bool,
    focus_address: bool,
    /// The active window shows the editable field (focus requested or held); otherwise a plain pill.
    address_editing: bool,
    /// A page was clicked while the address was being edited; end the edit on the next render.
    blur_address: bool,
    find_open: bool,
    panel: Option<SidebarPage>,
    chrome: HashMap<String, TabChrome>,
    gesture: Option<Gesture>,
    suppress_window_drag: bool,
    stage_bounds: Option<gpui::Bounds<Pixels>>,
    layout_stage: Option<(i32, i32)>,
    order: Vec<String>,
    subscriptions: Vec<Subscription>,
    pub(crate) status: SharedString,
    renaming: bool,
    confirm_delete: bool,
    permission: Option<(String, String)>,
    downloads_tx: Sender<DownloadUpdate>,
    downloads_rx: Receiver<DownloadUpdate>,
    attached: bool,
    loading: bool,
    started: Instant,
    metrics: SharedString,
    self_test: Option<SelfTest>,
    cookies_saved: Instant,
    /// Window dock under the title bar.
    dock: Tween,
    /// Pointer over the dock; it stays shut while a window drag is in progress.
    dock_hovered: bool,
    /// A window carried up to the spaces bar shrinks under the pointer (open = small).
    lift: Tween,
    /// Space chips laid out this frame, so a carried window can be dropped on one.
    space_chips: Vec<(String, gpui::Bounds<Pixels>)>,
    /// A dropped carried window growing back into place: (tab id, rect it was dropped at, started).
    landing: Option<(String, Frame, Instant)>,
    /// Tab pill being dragged and the pointer; drawn here so pages can be cut out beneath it.
    tab_drag: Option<(browser_core::Tab, gpui::Point<Pixels>)>,
    find: Tween,
    panel_tween: Tween,
    /// Sidebar page still drawn while its panel slides shut.
    shown_panel: Option<SidebarPage>,
    /// Status message shown as a toast, and when it changed.
    toast: (SharedString, Instant),
    /// Load bar width for the active tab, and when its load finished (the bar then fades).
    load_bar: f32,
    load_done: Option<Instant>,
    /// New windows growing in.
    opened: HashMap<String, Instant>,
    /// Address bar matches (url, title) and the one picked with the arrow keys.
    suggestions: Vec<(String, String)>,
    suggestion: Option<usize>,
    address_bounds: Option<gpui::Bounds<Pixels>>,
    pub(crate) compact: bool,
    reduce_motion: bool,
    /// Theme variant and accent last applied, so the global theme is only swapped on change.
    applied_theme: String,
}

/// Eased 0..=1 progress toward open (1) or shut (0), restarting from wherever it is when flipped.
#[derive(Clone, Copy)]
struct Tween {
    open: bool,
    from: f32,
    at: Instant,
    dur: Duration,
}

impl Tween {
    fn new(open: bool) -> Self {
        Self { open, from: if open { 1.0 } else { 0.0 }, at: Instant::now(), dur: Duration::ZERO }
    }

    fn progress(&self) -> f32 {
        let target = if self.open { 1.0 } else { 0.0 };
        if self.dur.is_zero() {
            return target;
        }
        self.from + (target - self.from) * ease(self.at.elapsed().as_secs_f32() / self.dur.as_secs_f32())
    }

    fn set(&mut self, open: bool, dur: Duration) -> bool {
        if open == self.open {
            return false;
        }
        self.from = self.progress();
        self.open = open;
        self.at = Instant::now();
        self.dur = dur;
        true
    }

    fn running(&self) -> bool {
        self.at.elapsed() < self.dur
    }
}

struct SelfTest {
    stage: u8,
    tab_a: String,
    tab_b: String,
    value_a: Option<String>,
    value_b: Option<String>,
    sum: Option<String>,
    /// Whether the example extension's content script ran in tab A.
    ext: Option<String>,
    started: Instant,
}

impl BrowserWindow {
    pub fn new(self_test: bool, started: Instant, cx: &mut Context<Self>) -> Self {
        let storage = Storage::open(&app_support_dir().join("nxbrowser.sqlite"))
            .expect("open browser database");
        let mut model = Model::open(storage).expect("open browser model");
        if !model.settings.restore_session {
            // Start fresh: closing the last tab leaves one home tab, then open the new tab page in it.
            for id in model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>() {
                let _ = model.close_tab(&id);
            }
            let _ = model.navigate_active(&model.settings.new_tab_url());
        }
        model.request_show_active();
        let model = Arc::new(Mutex::new(model));
        let (installs_tx, installs_rx) = mpsc::channel();
        let ext_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let (downloads_tx, downloads_rx) = mpsc::channel();
        let address = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let find_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let rename_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let home_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let ua_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let name_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let new_profile_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let subscriptions = vec![
            cx.subscribe(&address, |this, input, event, cx| match event {
                InputStateEvent::Submit => {
                    let raw = input.read(cx).content().to_string();
                    this.commit_navigation(&raw, cx);
                }
                InputStateEvent::Focus => {
                    this.address_focused = true;
                    this.engine.focus_chrome();
                }
                InputStateEvent::Blur => {
                    this.address_focused = false;
                    this.address_editing = false;
                    this.refresh_url(cx);
                }
                InputStateEvent::TextChanged => {
                    let typed = input.read(cx).content().to_string();
                    this.suggestions = this.model.lock().expect("model").suggest(&typed, 6).unwrap_or_default();
                    this.suggestion = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe(&find_input, |this, input, event, cx| match event {
                InputStateEvent::Submit => {
                    let query = input.read(cx).content().to_string();
                    if let Some(id) = this.active_tab_id() {
                        this.engine.find(&id, &query);
                    }
                }
                InputStateEvent::Focus => this.engine.focus_chrome(),
                _ => {}
            }),
            cx.subscribe(&rename_input, |this, input, event, cx| {
                if matches!(event, InputStateEvent::Focus) {
                    this.engine.focus_chrome();
                }
                if matches!(event, InputStateEvent::Submit) {
                    let name = input.read(cx).content().to_string();
                    let id = this.model.lock().expect("model").active_profile_id.clone();
                    let _ = this.model.lock().expect("model").rename_profile(&id, &name);
                    this.renaming = false;
                }
            }),
            cx.subscribe(&home_input, |this, input, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit | InputStateEvent::Blur => {
                    let value = input.read(cx).content().trim().to_string();
                    this.set_pref(cx, |s| s.home_page = value);
                }
                _ => {}
            }),
            cx.subscribe(&ua_input, |this, input, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit | InputStateEvent::Blur => {
                    let value = input.read(cx).content().trim().to_string();
                    this.set_pref(cx, |s| s.user_agent = value);
                }
                _ => {}
            }),
            cx.subscribe(&new_profile_input, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.create_new_profile(cx),
                // Keeps the badge preview in step with the typed name.
                _ => cx.notify(),
            }),
            cx.subscribe(&ext_input, |this, input, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => {
                    let raw = input.read(cx).content().to_string();
                    this.install_from_input(&raw, cx);
                }
                _ => {}
            }),
            cx.subscribe(&name_input, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_profile_name(cx),
                _ => {}
            }),
        ];
        let mut view = Self {
            model,
            engine: MacEngine::new(),
            ext_input,
            ext_meta: HashMap::new(),
            ext_prompts: Vec::new(),
            installing: Vec::new(),
            installs_tx,
            installs_rx,
            ext_active: None,
            address,
            find_input,
            rename_input,
            home_input,
            ua_input,
            name_input,
            settings_open: false,
            settings_section: SettingsSection::Profiles,
            settings_note: None,
            settings_profile: None,
            delete_prompt: None,
            overlay_rects: Vec::new(),
            rail_menu: None,
            downloads_open: false,
            extensions_open: false,
            new_profile: None,
            new_profile_input,
            download_progress: HashMap::new(),
            address_focused: false,
            focus_address: false,
            address_editing: false,
            blur_address: false,
            find_open: false,
            panel: None,
            chrome: HashMap::new(),
            gesture: None,
            suppress_window_drag: false,
            stage_bounds: None,
            layout_stage: None,
            order: Vec::new(),
            subscriptions,
            status: "Ready".into(),
            renaming: false,
            confirm_delete: false,
            permission: None,
            downloads_tx,
            downloads_rx,
            attached: false,
            loading: false,
            started,
            metrics: "Measuring…".into(),
            self_test: self_test.then(|| SelfTest {
                stage: 0,
                tab_a: String::new(),
                tab_b: String::new(),
                value_a: None,
                value_b: None,
                sum: None,
                ext: None,
                started: Instant::now(),
            }),
            cookies_saved: Instant::now(),
            dock: Tween::new(false),
            dock_hovered: false,
            lift: Tween::new(false),
            space_chips: Vec::new(),
            landing: None,
            tab_drag: None,
            find: Tween::new(false),
            panel_tween: Tween::new(false),
            shown_panel: None,
            toast: ("Ready".into(), Instant::now() - TOAST_LIFE),
            load_bar: 0.0,
            load_done: None,
            opened: HashMap::new(),
            suggestions: Vec::new(),
            suggestion: None,
            address_bounds: None,
            compact: false,
            reduce_motion: false,
            applied_theme: String::new(),
        };
        view.apply_settings();
        if view.self_test.is_none() {
            let (restore, tab_ids) = {
                let model = view.model.lock().expect("model");
                (model.settings.restore_session, model.tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>())
            };
            view.engine.set_session_cookie_dir(app_support_dir().join("session-cookies"), restore);
            view.engine.hold_media(tab_ids);
        }
        view.reload_extensions();
        view.sync_permissions();
        let url = view
            .model
            .lock()
            .ok()
            .and_then(|model| model.active_tab().map(|tab| tab.url.clone()))
            .unwrap_or_else(|| "nxbrowser://home".into());
        view.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        view
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.drain(cx);
        self.warm_tabs();
        while let Ok((id, result)) = self.installs_rx.try_recv() {
            self.installing.retain(|pending| pending != &id);
            match result {
                Ok(path) => self.add_extension(&id, &path, "store"),
                Err(error) => self.status = format!("Install failed: {error}").into(),
            }
        }
        let active = self.active_tab_id();
        if active.is_some() && active != self.ext_active {
            self.engine.extensions().activated(active.as_deref().unwrap_or_default());
            self.ext_active = active;
        }
        if let Some(id) = self.engine.take_page_click() {
            // Page clicks never reach GPUI, so the address input would stay focused and expanded.
            if self.address_editing || self.address_focused {
                self.blur_address = true;
                cx.notify();
            }
            let already = self.order.last().map(String::as_str) == Some(id.as_str())
                && self.active_tab_id().as_deref() == Some(id.as_str());
            if !already {
                self.focus_window(&id, cx);
            }
        }
        if self.started.elapsed() > Duration::from_secs(2) && self.metrics == "Measuring…" {
            let sample = snapshot(self.started);
            self.metrics = format!(
                "startup {} ms · app {:.1} MB · WebKit {:.1} MB · cpu {:.2}s",
                sample.startup.as_millis(),
                sample.rss_bytes as f64 / 1_048_576.0,
                sample.webkit_rss_bytes as f64 / 1_048_576.0,
                sample.cpu_seconds
            )
            .into();
        }
        // Catches logins done without a page load (XHR); finished loads save right away.
        if self.cookies_saved.elapsed() > Duration::from_secs(10) {
            self.save_cookies();
        }
        self.run_self_test();
        cx.notify();
    }

    fn save_cookies(&mut self) {
        self.cookies_saved = Instant::now();
        self.engine.save_session_cookies();
    }

    fn drain(&mut self, cx: &mut Context<Self>) {
        let effects = self.model.lock().expect("model").take_effects();
        self.apply(effects);
        while let Ok(update) = self.downloads_rx.try_recv() {
            self.download_progress.insert(update.id.clone(), (update.received, update.total));
            if update.status != "running" {
                let _ = self.model.lock().expect("model").update_download(&update.id, &update.status, &update.path);
                self.status = format!("Download {}", update.status).into();
            }
        }
        let mut events = Vec::new();
        while let Ok(event) = self.engine.events().try_recv() {
            events.push(event);
        }
        for event in events {
            self.on_event(event, cx);
        }
        let suspend = self.model.lock().expect("model").tabs_due_for_suspension();
        let tabs = self.model.lock().expect("model").session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        for tab_id in suspend {
            // Every window on the stage is showing its page; suspending one makes it reload in place.
            let entry = self.chrome.get(&tab_id);
            let hidden = entry.is_some_and(|chrome| chrome.minimized) || (maximized && !entry.is_some_and(|chrome| chrome.maximized)) || !self.is_front(&tab_id);
            let mut model = self.model.lock().expect("model");
            if hidden {
                let _ = model.mark_suspended(&tab_id);
            } else {
                model.keep_alive(&tab_id);
            }
        }
        let effects = self.model.lock().expect("model").take_effects();
        self.apply(effects);
    }

    fn on_event(&mut self, event: EngineEvent, cx: &mut Context<Self>) {
        match event {
            EngineEvent::Navigated { tab_id, url, title, loading } => {
                self.loading = loading;
                if !loading {
                    let _ = self.model.lock().expect("model").note_navigation(&tab_id, &url, &title);
                } else if !title.is_empty() {
                    let _ = self.model.lock().expect("model").set_loading_title(&tab_id, &title);
                }
                let active = self.model.lock().expect("model").active_tab().map(|tab| tab.id.clone());
                if active.as_deref() == Some(tab_id.as_str()) && !self.address_focused && !url.is_empty() {
                    self.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
                }
                if is_web(&url) {
                    let zoom = self.model.lock().expect("model").settings.zoom_for(&site_glyph(&url).1);
                    self.engine.set_zoom(&tab_id, zoom);
                }
                if !loading {
                    self.save_cookies();
                    let (_, host) = site_glyph(&url);
                    if self.self_test.is_none() && is_web(&url) && !favicon_path(&host).exists() {
                        self.engine.eval(&tab_id, "favicon", FAVICON_JS);
                    }
                }
            }
            EngineEvent::Failed { message, .. } => self.status = message.into(),
            EngineEvent::Download { url, .. } => self.start_download(&url),
            EngineEvent::Permission { origin, kind, .. } => {
                self.permission = Some((origin.clone(), kind.clone()));
                self.status = format!("Permission request: {kind} for {origin}").into();
            }
            EngineEvent::OpenInNewTab { url, from } => {
                // `chrome.sidePanel.open()` arrives here as a plain `tabs.create` of the panel page.
                let panel = self.ext_meta.keys().find(|id| self.panel_url(id).as_deref() == Some(url.as_str())).cloned();
                // A side panel's links get a regular window, not a tab squeezed into the panel.
                let from = from.filter(|id| self.chrome.get(id).is_some_and(|chrome| chrome.panel.is_none()));
                match (panel, from) {
                    (Some(id), _) => self.open_extension_panel(&id, &url, cx),
                    (None, Some(from)) => self.new_tab_in(&from, Some(&url), cx),
                    (None, None) => self.with_model(|model| {
                        let _ = model.create_tab(&url);
                    }),
                }
            }
            EngineEvent::Alert { message } => self.status = message.into(),
            EngineEvent::Eval { token, value, .. } if token == "favicon" => {
                // shortcut: icons are cached per host forever; refetch on age if sites change icons.
                if let Some((page, icon)) = value.split_once('\n') {
                    if is_web(page) && is_web(icon) {
                        spawn_favicon(icon.to_string(), favicon_path(&site_glyph(page).1));
                    }
                }
            }
            EngineEvent::Eval { token, value, .. } => {
                if let Some(test) = &mut self.self_test {
                    match token.as_str() {
                        "a" => test.value_a = Some(value),
                        "b" => test.value_b = Some(value),
                        "sum" => test.sum = Some(value),
                        "ext" => test.ext = Some(value),
                        _ => {}
                    }
                }
            }
            EngineEvent::Shortcut(shortcut) => self.on_shortcut(shortcut, cx),
            EngineEvent::ExtensionChanged { .. } => cx.notify(),
            EngineEvent::ExtensionPrompt { prompt, id, request } => {
                self.ext_prompts.push((prompt, id, request));
                self.extensions_open = true;
            }
            EngineEvent::ActivateTab { tab_id } => self.focus_window(&tab_id, cx),
            EngineEvent::CloseTab { tab_id } => self.close_tabs(vec![tab_id], cx),
        }
    }

    fn on_shortcut(&mut self, shortcut: Shortcut, cx: &mut Context<Self>) {
        match shortcut {
            Shortcut::NewTab => match self.active_tab_id() {
                Some(id) => self.new_tab_in(&id, None, cx),
                None => self.with_model(|model| { let _ = model.create_tab(&model.settings.new_tab_url()); }),
            },
            Shortcut::CloseTab => {
                if let Some(id) = self.active_tab_id() {
                    self.close_tabs(vec![id], cx);
                }
            }
            Shortcut::FocusUrl => self.focus_address = true,
            Shortcut::Reload => self.reload(),
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::Find => self.find_open = true,
            Shortcut::Bookmark => self.bookmark(),
            Shortcut::Settings => self.toggle_settings(cx),
            Shortcut::ReopenTab => match self.with_model(|model| model.reopen_closed()) {
                Ok(Some(id)) => self.focus_window(&id, cx),
                _ => self.status = "No recently closed tabs".into(),
            },
            Shortcut::ZoomIn => self.zoom(1),
            Shortcut::ZoomOut => self.zoom(-1),
            Shortcut::ZoomReset => self.zoom(0),
            Shortcut::SelectTab(n) => {
                let ids = self.model.lock().expect("model").session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
                let pick = if n == 9 { ids.last() } else { ids.get(n - 1) };
                if let Some(id) = pick.cloned() {
                    self.focus_window(&id, cx);
                }
            }
            Shortcut::NextTab | Shortcut::PrevTab => {
                let ids = self.model.lock().expect("model").session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
                let at = self.active_tab_id().and_then(|id| ids.iter().position(|other| *other == id)).unwrap_or(0);
                let step = if matches!(shortcut, Shortcut::NextTab) { 1 } else { ids.len().saturating_sub(1) };
                if let Some(id) = ids.get((at + step) % ids.len().max(1)).cloned() {
                    self.focus_window(&id, cx);
                }
            }
            Shortcut::Print => {
                if let Some(id) = self.active_tab_id() {
                    self.engine.print(&id);
                }
            }
        }
        self.refresh_url(cx);
    }

    pub(crate) fn with_model<T>(&mut self, f: impl FnOnce(&mut Model) -> T) -> T {
        let (value, effects) = {
            let mut model = self.model.lock().expect("model");
            let value = f(&mut model);
            let effects = model.take_effects();
            (value, effects)
        };
        self.apply(effects);
        value
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::EnsureTab { tab_id, data_store_id, url } => {
                    let _ = self.engine.ensure_tab(&tab_id, &data_store_id, &url);
                }
                Effect::CloseTab { tab_id } => self.engine.close_tab(&tab_id),
                Effect::SuspendTab { tab_id } => self.engine.suspend_tab(&tab_id),
                Effect::ShowTab { tab_id } => {
                    if self.self_test.is_some() {
                        self.engine.show_tab(tab_id.as_deref());
                    }
                }
                Effect::Navigate { tab_id, url } => {
                    let _ = self.engine.navigate(&tab_id, &url);
                }
            }
        }
    }

    fn warm_tabs(&mut self) {
        if self.self_test.is_some() {
            return;
        }
        let (store, tabs) = {
            let model = self.model.lock().expect("model");
            let Some(store) = model.active_profile().map(|profile| profile.data_store_id.clone()) else {
                return;
            };
            let tabs = model.session_tabs().into_iter().cloned().collect::<Vec<_>>();
            (store, tabs)
        };
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        for tab in tabs {
            let entry = self.chrome.get(&tab.id);
            let minimized = entry.is_some_and(|chrome| chrome.minimized);
            let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
            if minimized || covered {
                continue;
            }
            let _ = self.engine.ensure_tab(&tab.id, &store, &tab.url);
        }
    }

    /// Registers the example extensions, then loads every enabled extension into WebKit.
    pub(crate) fn reload_extensions(&mut self) {
        let mut dirs = vec![std::env::current_dir().unwrap_or_default().join("extensions/examples")];
        if let Some(parent) = std::env::current_exe().ok().and_then(|path| path.parent().map(|path| path.to_path_buf())) {
            dirs.push(parent.join("../../../extensions/examples"));
        }
        for path in discover(&dirs) {
            let path = path.canonicalize().unwrap_or(path);
            let name = read_manifest(&path).map(|manifest| manifest.name).unwrap_or_default();
            let _ = self.model.lock().expect("model").upsert_extension(&local_id(&path), &name, &path, "bundled");
        }
        // Every profile's extensions, so background profiles keep running theirs.
        let ids = self.model.lock().expect("model").installed_extensions().unwrap_or_default();
        for (id, ..) in ids {
            self.sync_extension(&id);
        }
    }

    /// Re-reads one extension's manifest and starts or stops it in each profile that has it enabled.
    pub(crate) fn sync_extension(&mut self, id: &str) {
        let (installed, stores) = {
            let model = self.model.lock().expect("model");
            let installed = model.installed_extensions().unwrap_or_default().into_iter().find(|(found, ..)| found == id);
            (installed, model.extension_stores(id).unwrap_or_default())
        };
        let Some((_, path, source)) = installed else {
            self.ext_meta.remove(id);
            self.engine.extensions().unload(id);
            return;
        };
        if source == "store" {
            if let Err(error) = patch_missing_apis(&path) {
                eprintln!("could not add API polyfill to {id}: {error}");
            }
        }
        self.ext_meta.insert(id.to_string(), read_manifest(&path).unwrap_or_default());
        if !stores.is_empty() && path.join("manifest.json").is_file() {
            self.engine.extensions().load(id, &path, stores);
        } else {
            self.engine.extensions().unload(id);
        }
    }

    fn add_extension(&mut self, id: &str, path: &std::path::Path, source: &str) {
        let manifest = match read_manifest(path) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = format!("Not an extension: {error}").into();
                return;
            }
        };
        let _ = self.model.lock().expect("model").upsert_extension(id, &manifest.name, path, source);
        self.sync_extension(id);
        self.status = format!("Installed {}", manifest.name).into();
        self.extensions_open = true;
    }

    /// Installs from a Chrome Web Store URL or extension id, downloading on a background thread.
    pub(crate) fn install_from_input(&mut self, raw: &str, cx: &mut Context<Self>) {
        let Some(id) = store_id(raw) else {
            self.status = "Paste a Chrome Web Store link or a 32-letter extension id".into();
            cx.notify();
            return;
        };
        self.ext_input.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
        self.install_from_store(id);
        cx.notify();
    }

    pub(crate) fn install_from_store(&mut self, id: String) {
        if self.installing.contains(&id) {
            return;
        }
        self.installing.push(id.clone());
        self.status = "Downloading extension…".into();
        let tx = self.installs_tx.clone();
        std::thread::spawn(move || {
            let result = install_from_store(&id, &app_support_dir().join("extensions")).map_err(|error| format!("{error:#}"));
            let _ = tx.send((id, result));
        });
    }

    pub(crate) fn load_unpacked(&mut self) {
        let Some(path) = choose_folder_path() else {
            return;
        };
        if !path.join("manifest.json").is_file() {
            self.status = "That folder has no manifest.json".into();
            return;
        }
        self.add_extension(&local_id(&path), &path, "unpacked");
    }

    pub(crate) fn remove_extension(&mut self, id: &str) {
        let record = self.model.lock().expect("model").extensions.iter().find(|extension| extension.id == id).cloned();
        let last = self.model.lock().expect("model").remove_extension(id).unwrap_or(false);
        self.sync_extension(id);
        self.engine.extensions().remove_data(id, &self.active_store());
        // Only folders we unpacked ourselves are deleted, once no profile uses them; picked folders and examples stay.
        let root = app_support_dir().join("extensions");
        if let Some(record) = record.filter(|record| last && record.source == "store" && record.path.starts_with(&root)) {
            let _ = std::fs::remove_dir_all(&record.path);
        }
    }

    pub(crate) fn active_store(&self) -> String {
        self.model.lock().expect("model").active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default()
    }

    /// What clicking an extension's button at window x `at` does: its popup (a popover under the
    /// title bar there) or `onClicked` via WebKit, or its side panel in a slim window.
    pub(crate) fn open_extension(&mut self, id: &str, at: f32, cx: &mut Context<Self>) {
        let meta = self.ext_meta.get(id).cloned().unwrap_or_default();
        match self.panel_url(id).filter(|_| !meta.popup) {
            Some(url) => self.open_extension_panel(id, &url, cx),
            None => {
                let (store, tab) = (self.active_store(), self.active_tab_id());
                let anchor = [f64::from(at) - 13., 0., 26., f64::from(self.titlebar_h())];
                self.engine.extensions().perform_action(id, &store, tab.as_deref(), anchor);
            }
        }
    }

    pub(crate) fn open_extension_options(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.engine.extensions().options_url(id, &self.active_store()) {
            Some(url) => self.open_extension_window(&url, cx),
            None => self.status = "This extension has no options page".into(),
        }
    }

    /// Options pages open as a regular floating window.
    fn open_extension_window(&mut self, url: &str, cx: &mut Context<Self>) {
        if let Ok(id) = self.with_model(|model| model.create_tab(url)) {
            self.focus_window(&id, cx);
        }
    }

    fn panel_url(&self, id: &str) -> Option<String> {
        let path = self.ext_meta.get(id)?.side_panel.clone()?;
        Some(format!("{EXTENSION_SCHEME}{id}/{}", path.trim_start_matches('/')))
    }

    /// A side panel: a narrow window docked right, titled with just the extension. One per space.
    fn open_extension_panel(&mut self, id: &str, url: &str, cx: &mut Context<Self>) {
        let open = self
            .model
            .lock()
            .expect("model")
            .session_tabs()
            .into_iter()
            .find(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.panel.as_deref() == Some(id)))
            .map(|tab| tab.id.clone());
        let Some(tab_id) = open.or_else(|| self.with_model(|model| model.create_tab(url)).ok()) else {
            return;
        };
        let entry = self.chrome.entry(tab_id.clone()).or_default();
        if entry.panel.is_none() {
            entry.panel = Some(id.to_string());
            if let Some(bounds) = self.stage_bounds {
                let (sw, sh) = (f64::from(bounds.size.width) as f32, f64::from(bounds.size.height) as f32);
                let w = 380f32.min(sw - 20.0);
                entry.frame = Some(Frame { x: sw - w - 10.0, y: 10.0, w, h: sh - 20.0 });
            }
        }
        self.focus_window(&tab_id, cx);
    }

    pub(crate) fn apply_settings(&mut self) {
        let settings = self.model.lock().expect("model").settings.clone();
        self.engine.set_settings(EngineSettings {
            javascript: settings.javascript,
            block_popups: settings.block_popups,
            fraud_warnings: settings.fraud_warnings,
            https_only: settings.https_only,
            inspectable: settings.inspector,
            user_agent: settings.user_agent.clone(),
            page_zoom: settings.page_zoom,
        });
        self.compact = settings.compact();
        self.reduce_motion = settings.reduce_motion;
        // set_settings reset every page to the default zoom.
        let tabs = self.model.lock().expect("model").tabs.iter().map(|tab| (tab.id.clone(), tab.url.clone())).collect::<Vec<_>>();
        for (id, url) in tabs.iter().filter(|(_, url)| is_web(url)) {
            self.engine.set_zoom(id, settings.zoom_for(&site_glyph(url).1));
        }
    }

    /// Steps the active site's zoom through Chrome's levels; 0 resets it to the default.
    fn zoom(&mut self, direction: i32) {
        let Some(tab) = self.model.lock().expect("model").active_tab().cloned() else {
            return;
        };
        if !is_web(&tab.url) {
            return;
        }
        let host = site_glyph(&tab.url).1;
        let (current, default) = {
            let model = self.model.lock().expect("model");
            (model.settings.zoom_for(&host), model.settings.page_zoom)
        };
        let next = match direction {
            0 => default,
            d if d > 0 => ZOOM_LEVELS.iter().copied().find(|level| *level > current + 0.001).unwrap_or(ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]),
            _ => ZOOM_LEVELS.iter().rev().copied().find(|level| *level < current - 0.001).unwrap_or(ZOOM_LEVELS[0]),
        };
        let _ = self.model.lock().expect("model").update_settings(|s| {
            if (next - default).abs() < 0.001 {
                s.site_zoom.remove(&host);
            } else {
                s.site_zoom.insert(host.clone(), next);
            }
        });
        self.engine.set_zoom(&tab.id, next);
        self.status = format!("Zoom {:.0}%", next * 100.0).into();
    }

    fn anim(&self, duration: Duration) -> Duration {
        if self.reduce_motion { Duration::ZERO } else { duration }
    }

    /// Closes tabs; a whole window shrinks away first, then its tabs close.
    pub(crate) fn close_tabs(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(first) = ids.first() else {
            return;
        };
        let group = self.group_id(first);
        // The model would activate the space's first tab; focus the rest of this window, else the window on top.
        let current = self.model.lock().expect("model").session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let open = |id: &&String| current.contains(id) && !ids.contains(id);
        let survivor = self.order.iter().rev().filter(open).find(|id| self.group_id(id) == group).cloned().or_else(|| {
            self.order
                .iter()
                .rev()
                .filter(open)
                .find(|id| self.is_front(id) && !self.chrome.get(*id).is_some_and(|chrome| chrome.minimized))
                .cloned()
        });
        self.with_model(|model| {
            for id in &ids {
                let _ = model.close_tab(id);
            }
        });
        self.refresh_url(cx);
        if let Some(survivor) = survivor {
            self.focus_window(&survivor, cx);
        }
        cx.notify();
    }

    /// Scale and opacity for a window opening; (1, 1) when at rest.
    fn window_motion(&self, id: &str) -> (f32, f32) {
        let Some(at) = self.opened.get(id) else {
            return (1.0, 1.0);
        };
        let p = ease(at.elapsed().as_secs_f32() / WINDOW_ANIM.as_secs_f32());
        (WINDOW_FROM_SCALE + (1.0 - WINDOW_FROM_SCALE) * p, p)
    }

    pub(crate) fn titlebar_h(&self) -> f32 {
        if self.compact { 30.0 } else { TITLEBAR_H }
    }

    fn title_h(&self) -> f64 {
        if self.compact { 28.0 } else { TITLE_H }
    }

    fn apply_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (theme, accent) = {
            let model = self.model.lock().expect("model");
            (model.settings.theme.clone(), model.settings.accent.clone())
        };
        let dark = match theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark),
        };
        let key = format!("{dark}{accent}");
        if key != self.applied_theme || (cx.theme().variant == ThemeVariant::Dark) != dark {
            let mut next = if dark { Theme::default() } else { Theme::gruvbox_light() };
            if !accent.is_empty() {
                next.accent_color = parse_hex(&accent);
            }
            cx.set_global(GlobalTheme(Arc::new(next)));
            self.applied_theme = key;
        }
    }

    pub(crate) fn sync_permissions(&mut self) {
        if let Ok(entries) = self.model.lock().expect("model").permissions_map() {
            self.engine.set_permissions(entries);
        }
    }

    pub(crate) fn refresh_url(&mut self, cx: &mut Context<Self>) {
        if self.address_focused {
            return;
        }
        if let Some(url) = self.model.lock().ok().and_then(|model| model.active_tab().map(|tab| tab.url.clone())) {
            self.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
        }
    }

    fn commit_navigation(&mut self, raw: &str, cx: &mut Context<Self>) {
        let url = {
            let model = self.model.lock().expect("model");
            normalize_url(raw, model.settings.search_template(), model.settings.https_only)
        };
        self.address_focused = false;
        self.address_editing = false;
        self.suggestions.clear();
        self.suggestion = None;
        self.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
        self.with_model(|model| {
            let _ = model.navigate_active(&url);
        });
        self.engine.focus_page();
    }

    fn back(&mut self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.back(&id);
        }
    }

    fn forward(&mut self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.forward(&id);
        }
    }

    fn reload(&mut self) {
        if let Some(id) = self.active_tab_id() {
            self.engine.reload(&id);
        }
    }

    fn bookmark(&mut self) {
        let added = self.model.lock().expect("model").toggle_bookmark().unwrap_or(false);
        self.status = if added { "Bookmark added".into() } else { "Bookmark removed".into() };
    }

    pub(crate) fn active_tab_id(&self) -> Option<String> {
        self.model.lock().ok().and_then(|model| model.active_tab().map(|tab| tab.id.clone()))
    }

    fn start_download(&mut self, url: &str) {
        let id = Uuid::new_v4().to_string();
        let name = url.split('/').next_back().filter(|part| !part.is_empty()).unwrap_or("download");
        let (dir, ask) = {
            let settings = &self.model.lock().expect("model").settings;
            (settings.download_dir.clone(), settings.ask_download_path)
        };
        let path = if ask {
            let Some(path) = choose_save_path(name) else {
                return;
            };
            path
        } else if dir.is_empty() {
            downloads_dir().join(name)
        } else {
            std::path::PathBuf::from(dir).join(name)
        };
        let _ = self.model.lock().expect("model").add_download(&id, url, &path.display().to_string());
        self.download_progress.insert(id.clone(), (0, None));
        spawn_download(id, url.to_string(), path, self.downloads_tx.clone());
        self.status = "Download started".into();
        self.downloads_open = true;
    }

    pub(crate) fn export_profile(&mut self) {
        let bundle = self.model.lock().expect("model").export_active();
        let Ok(bundle) = bundle else {
            self.status = "Export failed".into();
            return;
        };
        let Some(path) = choose_save_path("profile.json") else {
            return;
        };
        match std::fs::write(&path, serde_json::to_string_pretty(&bundle).unwrap_or_default()) {
            Ok(()) => self.status = format!("Exported {}", path.display()).into(),
            Err(error) => self.status = error.to_string().into(),
        }
    }

    pub(crate) fn import_profile(&mut self) {
        let Some(path) = choose_open_path() else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                self.status = error.to_string().into();
                return;
            }
        };
        match serde_json::from_str(&text) {
            Ok(bundle) => {
                self.with_model(|model| {
                    let _ = model.import_bundle(bundle);
                });
                self.status = "Profile imported".into();
            }
            Err(error) => self.status = error.to_string().into(),
        }
    }

    fn attach(&mut self, window: &mut Window) {
        if self.attached {
            return;
        }
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        if self.engine.attach(handle.ns_view.as_ptr()).is_ok() {
            self.attached = true;
            let effects = self.model.lock().expect("model").take_effects();
            self.apply(effects);
        }
    }

    fn sync_tab_bounds(&mut self, tab_id: &str, bounds: gpui::Bounds<Pixels>, window: &mut Window) {
        self.attach(window);
        if self.self_test.is_some() {
            return;
        }
        self.engine.place_tab(
            tab_id,
            Some(LogicalRect {
                x: f64::from(bounds.origin.x),
                y: f64::from(bounds.origin.y),
                width: f64::from(bounds.size.width),
                height: f64::from(bounds.size.height),
            }),
        );
    }

    fn run_self_test(&mut self) {
        if self.self_test.is_none() || !self.attached {
            return;
        }
        let started = self.self_test.as_ref().unwrap().started;
        let stage = self.self_test.as_ref().unwrap().stage;
        if started.elapsed() > Duration::from_secs(20) {
            eprintln!("SELFTEST FAIL timeout");
            std::process::exit(1);
        }
        if stage == 0 {
            let waiting = self.model.lock().expect("model").extensions.iter().any(|extension| {
                extension.enabled && !self.engine.extensions().is_loaded(&extension.id) && self.engine.extensions().errors(&extension.id).is_empty()
            });
            if waiting {
                return;
            }
            let (tab_a, store_a) = {
                let model = self.model.lock().expect("model");
                (
                    model.active_tab().map(|tab| tab.id.clone()).unwrap_or_default(),
                    model.active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default(),
                )
            };
            self.with_model(|model| {
                let _ = model.create_profile("Profile B");
            });
            let (tab_b, store_b) = {
                let model = self.model.lock().expect("model");
                (
                    model.active_tab().map(|tab| tab.id.clone()).unwrap_or_default(),
                    model.active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default(),
                )
            };
            if store_a == store_b || tab_a.is_empty() || tab_b.is_empty() {
                eprintln!("SELFTEST FAIL profile ids were not isolated");
                std::process::exit(1);
            }
            let _ = self.engine.ensure_tab(&tab_a, &store_a, "nxbrowser://home");
            let _ = self.engine.ensure_tab(&tab_b, &store_b, "nxbrowser://home");
            let html_a = "<script>localStorage.setItem('nx','A')</script><title>A</title>";
            let html_b = "<script>localStorage.setItem('nx','B')</script><title>B</title>";
            self.engine.load_html(&tab_a, html_a, Some("https://nxbrowser.test/"));
            self.engine.load_html(&tab_b, html_b, Some("https://nxbrowser.test/"));
            self.engine.show_tab(Some(&tab_a));
            self.engine.set_bounds(LogicalRect { x: 10.0, y: 10.0, width: 640.0, height: 480.0 });
            if let Some(test) = self.self_test.as_mut() {
                test.tab_a = tab_a;
                test.tab_b = tab_b;
                test.stage = 1;
            }
            return;
        }
        if stage == 1 && started.elapsed() > Duration::from_millis(800) {
            let (tab_a, tab_b) = {
                let test = self.self_test.as_ref().unwrap();
                (test.tab_a.clone(), test.tab_b.clone())
            };
            self.engine.eval(&tab_a, "a", "localStorage.getItem('nx')");
            self.engine.eval(&tab_b, "b", "localStorage.getItem('nx')");
            self.engine.eval(&tab_a, "sum", "String(1+1)");
            self.engine.eval(&tab_a, "ext", "document.getElementById('nx-hello') ? 'banner' : 'none'");
            if let Some(test) = self.self_test.as_mut() {
                test.stage = 2;
            }
        }
        let outcome = self.self_test.as_ref().and_then(|test| {
            if test.stage != 2 {
                return None;
            }
            Some((
                test.value_a.clone()?,
                test.value_b.clone()?,
                test.sum.clone()?,
                test.tab_a.clone(),
                test.ext.clone()?,
            ))
        });
        if let Some((a, b, sum, tab_a, ext)) = outcome {
            let frame = self.engine.frame(&tab_a);
            let sample = snapshot(self.started);
            let isolated = a.contains('A') && b.contains('B') && !a.contains('B');
            let math = sum.contains('2');
            let sized = frame.map(|(width, _)| width > 100.0).unwrap_or(false);
            let hello = self.model.lock().expect("model").extensions.iter().any(|extension| extension.enabled && extension.name == "Hello World");
            let extension = !hello || ext.contains("banner");
            for record in self.model.lock().expect("model").extensions.iter().filter(|extension| extension.enabled) {
                println!("self-test extension {} loaded={} errors={:?}", record.name, self.engine.extensions().is_loaded(&record.id), self.engine.extensions().errors(&record.id));
            }
            println!(
                "self-test localStorage A={a:?} B={b:?} js={sum:?} extension={ext:?} frame={frame:?} startup_ms={} rss={}",
                sample.startup.as_millis(),
                sample.rss_bytes
            );
            if isolated && math && sized && extension {
                println!("SELFTEST OK");
                std::process::exit(0);
            }
            eprintln!("SELFTEST FAIL");
            std::process::exit(1);
        }
    }


    /// Collapses the expanded address field back to its tab pill, dropping any unsubmitted edit.
    fn end_address_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.address_editing && !self.address_focused {
            return;
        }
        window.blur();
        self.address_focused = false;
        self.address_editing = false;
        self.refresh_url(cx);
        cx.notify();
    }

    fn request_address_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.blur_address) && !self.focus_address {
            self.end_address_edit(window, cx);
        }
        if !self.focus_address {
            return;
        }
        self.focus_address = false;
        self.address_editing = true;
        self.suggestions.clear();
        self.suggestion = None;
        if let Some(id) = self.active_tab_id() {
            self.chrome.entry(id).or_default().minimized = false;
        }
        let address = self.address.clone();
        self.engine.focus_chrome();
        window.defer(cx, move |window, cx| {
            let handle = address.focus_handle(cx);
            window.focus(&handle, cx);
            address.update(cx, |input, cx| {
                let end = input.content().len();
                input.set_selected_range(0..end);
                cx.notify();
            });
        });
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Paint {
    pub(crate) bg: gpui::Hsla,
    pub(crate) fg: gpui::Hsla,
    pub(crate) muted: gpui::Hsla,
    pub(crate) surface: gpui::Hsla,
    pub(crate) secondary: gpui::Hsla,
    pub(crate) border: gpui::Hsla,
    pub(crate) accent: gpui::Hsla,
    pub(crate) accent_bg: gpui::Hsla,
}

pub(crate) fn paint(cx: &gpui::App) -> Paint {
    let theme = cx.theme();
    Paint {
        bg: theme.bg(),
        fg: theme.fg(),
        muted: theme.fg_muted(),
        surface: theme.surface(),
        secondary: theme.surface_secondary(),
        border: theme.border(),
        accent: theme.accent(),
        accent_bg: theme.accent_bg(),
    }
}

impl Focusable for BrowserWindow {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.address.focus_handle(cx)
    }
}

impl Render for BrowserWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach(window);
        self.overlay_rects.clear();
        self.space_chips.clear();
        self.apply_theme(window, cx);
        self.request_address_focus(window, cx);
        let _subscriptions = &self.subscriptions;
        let entity = cx.entity();
        let tabs = self.model.lock().expect("model").session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let profiles = self.model.lock().expect("model").profiles.clone();
        let active_profile = self.model.lock().expect("model").active_profile_id.clone();
        let sessions = self.model.lock().expect("model").profile_sessions().into_iter().cloned().collect::<Vec<_>>();
        let active_session = self.model.lock().expect("model").active_session_id.clone();
        let all_tabs = self.model.lock().expect("model").tabs.clone();
        let history = self.model.lock().expect("model").history().unwrap_or_default();
        let bookmarks = self.model.lock().expect("model").bookmarks().unwrap_or_default();
        let colors = paint(cx);
        if !cx.has_active_drag() {
            self.tab_drag = None;
        }
        // Keep layouts of every open tab, not just this session's, so switching profile and back keeps them.
        let live_ids = self.model.lock().expect("model").tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        self.chrome.retain(|id, _| live_ids.iter().any(|live| live == id));
        self.order.retain(|id| live_ids.iter().any(|live| live == id));
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        self.prepare_frames(&tabs);
        self.sync_groups();
        self.save_layouts(&tabs);
        self.step_motion();
        if self.animating() {
            window.request_animation_frame();
        }
        if self.self_test.is_none() {
            // Spaces are separate desktops: pages from the others stay hidden.
            for tab in all_tabs.iter().filter(|tab| tab.session_id != active_session) {
                self.engine.place_tab(&tab.id, None);
            }
            for tab in &tabs {
                let entry = self.chrome.get(&tab.id);
                let minimized = entry.is_some_and(|chrome| chrome.minimized);
                let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
                if minimized || covered || !self.is_front(&tab.id) {
                    self.engine.place_tab(&tab.id, None);
                }
            }
        }
        v_stack()
            .relative()
            .size_full()
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(px(13.))
            .child(self.titlebar(&sessions, &all_tabs, &active_session, cx))
            .child(
                h_stack()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(self.rail(&profiles, &active_profile, cx))
                    .child(
                v_stack()
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(self.stage(&tabs, entity, cx))
                    .when_some(self.shown_panel, |column, page| {
                        column.child(self.panel_view(
                            page,
                            &profiles,
                            &active_profile,
                            &sessions,
                            &history,
                            &bookmarks,
                            cx,
                        ))
                    }),
            ),
            )
            .child(self.dock(&tabs, cx))
            .children(self.carried_view(&tabs, cx))
            .children(self.tab_drag_view(cx))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<TabDrag>, _, cx| {
                this.tab_drag = Some((event.drag(cx).0.clone(), event.event.position));
                cx.notify();
            }))
            .when(self.address_focused && !self.suggestions.is_empty(), |root| root.child(self.suggestions_view(cx)))
            .child(self.toast_view(cx))
            .when(self.settings_open, |root| root.child(self.settings_modal(cx)))
            .when(self.new_profile.is_some(), |root| root.child(self.new_profile_modal(cx)))
            .when_some(self.delete_prompt.clone(), |root, id| root.child(self.delete_modal(&id, cx)))
            .when(self.downloads_open, |root| root.child(self.downloads_popover(cx)))
            .when(self.extensions_open, |root| root.child(self.extensions_popover(cx)))
            .when_some(self.rail_menu.clone(), |root, menu| root.child(self.rail_menu_view(menu, cx)))
    }
}

impl BrowserWindow {
    /// Moves every animation one frame along: tweens follow their targets, finished closes run.
    fn step_motion(&mut self) {
        let (find, panel) = (self.anim(PANEL_ANIM), self.anim(PANEL_ANIM));
        self.find.set(self.find_open, find);
        if self.panel.is_some() {
            self.shown_panel = self.panel;
        }
        self.panel_tween.set(self.panel.is_some(), panel);
        if self.panel.is_none() && self.panel_tween.progress() <= 0.0 {
            self.shown_panel = None;
        }
        if self.status != self.toast.0 {
            self.toast = (self.status.clone(), Instant::now());
        }
        let progress = self.active_tab_id().and_then(|id| self.engine.progress(&id));
        match progress {
            Some(target) => {
                let target = (target as f32).max(0.08);
                self.load_bar = if self.reduce_motion { target } else { self.load_bar + (target - self.load_bar) * 0.2 };
                self.load_done = None;
            }
            None if self.load_bar > 0.0 && self.load_done.is_none() => {
                self.load_bar = 1.0;
                self.load_done = Some(Instant::now());
            }
            None if self.load_done.is_some_and(|at| at.elapsed() >= LOAD_FADE) => {
                self.load_bar = 0.0;
                self.load_done = None;
            }
            None => {}
        }
        self.opened.retain(|_, at| at.elapsed() < WINDOW_ANIM);
        if self.landing.as_ref().is_some_and(|(_, _, at)| at.elapsed() >= LAND_ANIM) {
            self.landing = None;
        }
    }

    fn toast_view(&self, cx: &mut Context<Self>) -> gpui::Div {
        let age = self.toast.1.elapsed();
        if age >= TOAST_LIFE || self.toast.0.is_empty() || self.self_test.is_some() {
            return div();
        }
        let paint = paint(cx);
        let fade = TOAST_LIFE - TOAST_IN;
        let p = if self.reduce_motion {
            1.0
        } else if age < TOAST_IN {
            ease(age.as_secs_f32() / TOAST_IN.as_secs_f32())
        } else if age > fade {
            1.0 - ease((age - fade).as_secs_f32() / TOAST_IN.as_secs_f32())
        } else {
            1.0
        };
        // The pill stays opaque: pages are cut out beneath it, so only its contents fade.
        div().absolute().bottom(px(18. - (1. - p) * 14.)).left_0().right_0().flex().justify_center().child(
            div()
                .relative()
                .max_w(px(420.))
                .px(px(14.))
                .py(px(7.))
                .rounded(px(10.))
                .bg(paint.surface)
                .border_1()
                .border_color(paint.border)
                .shadow_md()
                .text_size(px(12.))
                .occlude()
                .child(measure_overlay(10., cx))
                .child(div().opacity(p).truncate().child(self.toast.0.clone())),
        )
    }

    fn suggestions_view(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some(bounds) = self.address_bounds else {
            return div();
        };
        let paint = paint(cx);
        let mut list = v_stack()
            .id("address-suggestions")
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y + bounds.size.height + px(4.))
            .w(bounds.size.width.max(px(320.)))
            .p(px(4.))
            .rounded(px(10.))
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_md()
            .occlude()
            .child(measure_overlay(10., cx));
        for (index, (url, title)) in self.suggestions.iter().enumerate() {
            let target = url.clone();
            let picked = self.suggestion == Some(index);
            list = list.child(
                h_stack()
                    .id(SharedString::from(format!("suggestion-{index}")))
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .when(picked, |row| row.bg(paint.accent_bg))
                    .hover(|row| row.bg(paint.secondary))
                    .child(div().flex_none().max_w(px(220.)).truncate().child(if title.is_empty() { display_url(url) } else { title.clone() }))
                    .child(div().flex_1().min_w_0().truncate().text_color(paint.muted).child(display_url(url)))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.commit_navigation(&target, cx);
                    })),
            );
        }
        div().child(list)
    }

    fn toggle_panel(&mut self, page: SidebarPage, cx: &mut Context<Self>) {
        self.panel = if self.panel == Some(page) { None } else { Some(page) };
        cx.notify();
    }

    fn rail(
        &self,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        let mut profile_list = v_stack().w_full().items_center().gap(px(6.)).py(px(4.));
        for (index, profile) in profiles.iter().enumerate() {
            let id = profile.id.clone();
            let active = profile.id == active_profile;
            let dragged = RailDrag(profile.clone());
            let badge = profile_badge(profile, active, 32., paint)
                .id(SharedString::from(format!("rail-{id}")))
                .cursor_pointer()
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_profile(&id); });
                        this.refresh_url(cx);
                        this.confirm_delete = false;
                    }
                }))
                .on_drag(dragged, move |drag, _, _, cx| cx.new(|_| RailDragPreview(drag.0.clone(), paint)))
                .drag_over::<RailDrag>(move |style, _, _, _| style.border_color(paint.accent))
                .on_drop(cx.listener(move |this, drag: &RailDrag, _, cx| {
                    this.with_model(|model| { let _ = model.move_profile(&drag.0.id, index); });
                    cx.notify();
                }));
            let badge = badge.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.rail_menu = Some((id.clone(), index, event.position));
                    cx.notify();
                }),
            );
            profile_list = profile_list.child(badge);
        }
        v_stack()
            .w(px(56.))
            .h_full()
            .py(px(8.))
            .items_center()
            .gap(px(8.))
            .bg(paint.surface)
            .border_r_1()
            .border_color(paint.border)
            .child(
                div().flex_1().w_full().overflow_hidden().child(
                    scroll_area("rail-profiles").vertical().full_height(true).child(profile_list),
                ),
            )
            .child(icon_button("new-profile-rail", Icons::plus()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.open_new_profile(cx);
            })))
            .child(icon_button("downloads-list", Icons::download()).small().selected(self.downloads_open).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                // Opens only: a click outside the popover (this one included) already closes it.
                this.downloads_open = true;
                cx.notify();
            })))
            .child(icon_button("open-settings", Icons::gear()).small().selected(self.settings_open).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_settings(cx);
            })))
            .child(icon_button("more-actions", Icons::dots_vertical()).small().selected(self.panel == Some(SidebarPage::Profiles)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_panel(SidebarPage::Profiles, cx);
            })))
    }

    fn prepare_frames(&mut self, tabs: &[browser_core::Tab]) {
        let Some(bounds) = self.stage_bounds else {
            return;
        };
        let sw = f64::from(bounds.size.width) as f32;
        let sh = f64::from(bounds.size.height) as f32;
        if sw < 80.0 || sh < 80.0 {
            return;
        }
        // Windows not seen this run come back from their saved layout, stacked in saved z-order.
        let mut seeded = Vec::new();
        for tab in tabs {
            if self.chrome.contains_key(&tab.id) {
                continue;
            }
            let Ok(saved) = serde_json::from_str::<SavedLayout>(&tab.layout) else {
                continue;
            };
            let mut chrome = saved.chrome;
            let (ow, oh) = saved.stage;
            if chrome.snap.is_some() {
                chrome.frame = chrome.frame.map(|frame| rescale_snapped(frame, ow, oh, sw, sh));
            }
            if chrome.frame.is_some_and(|frame| mostly_off(frame, sw, sh)) {
                chrome.frame = None;
            }
            self.chrome.insert(tab.id.clone(), chrome);
            seeded.push((saved.z, tab.id.clone()));
        }
        seeded.sort();
        for (_, id) in seeded {
            if !self.order.contains(&id) {
                self.order.push(id);
            }
        }
        let key = (sw.round() as i32, sh.round() as i32);
        if self.layout_stage != Some(key) {
            let old = self.layout_stage.replace(key);
            if self.gesture.is_none() {
                for entry in self.chrome.values_mut() {
                    if let Some(snap) = entry.snap {
                        // Snapped windows keep their share of the stage, including splits the user dragged.
                        entry.frame = Some(match (old, entry.frame) {
                            (Some((ow, oh)), Some(frame)) => rescale_snapped(frame, ow as f32, oh as f32, sw, sh),
                            _ => snap.frame(sw, sh),
                        });
                    }
                    if entry.frame.is_some_and(|frame| mostly_off(frame, sw, sh)) {
                        entry.frame = None;
                    }
                }
            }
        }
        let count = tabs.len().max(1);
        let cols = if count == 1 { 1 } else { 2 };
        let rows = (count + cols - 1) / cols;
        let gap = 10.0;
        let pad = 10.0;
        let cell_w = (sw - pad * 2.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        let cell_h = (sh - pad * 2.0 - gap * (rows as f32 - 1.0)) / rows as f32;
        for (index, tab) in tabs.iter().enumerate() {
            if !self.order.iter().any(|id| id == &tab.id) {
                self.order.push(tab.id.clone());
            }
            let entry = self.chrome.entry(tab.id.clone()).or_default();
            if entry.frame.is_some() {
                continue;
            }
            if self.layout_stage.is_some() && self.self_test.is_none() && !self.reduce_motion {
                self.opened.insert(tab.id.clone(), Instant::now());
            }
            let col = (index % cols) as f32;
            let row = (index / cols) as f32;
            entry.frame = Some(Frame {
                x: pad + col * (cell_w + gap),
                y: pad + row * (cell_h + gap),
                w: cell_w.max(160.0),
                h: cell_h.max(120.0),
            });
        }
    }

    /// Writes changed window layouts to the tab rows so they survive restarts.
    fn save_layouts(&mut self, tabs: &[browser_core::Tab]) {
        if self.gesture.is_some() || self.self_test.is_some() {
            return;
        }
        let Some((sw, sh)) = self.layout_stage else {
            return;
        };
        let changed = tabs
            .iter()
            .filter_map(|tab| {
                let chrome = self.chrome.get(&tab.id)?.clone();
                let z = self.order.iter().position(|id| id == &tab.id).unwrap_or(0);
                let json = serde_json::to_string(&SavedLayout { chrome, z, stage: (sw as f32, sh as f32) }).ok()?;
                (json != tab.layout).then(|| (tab.id.clone(), json))
            })
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            self.with_model(|model| {
                for (id, json) in &changed {
                    let _ = model.set_tab_layout(id, json);
                }
            });
        }
    }

    fn note_stage(&mut self, bounds: gpui::Bounds<Pixels>, cx: &mut Context<Self>) -> bool {
        let changed = self.stage_bounds.map(|old| (f64::from(old.size.width) - f64::from(bounds.size.width)).abs() > 1.0 || (f64::from(old.size.height) - f64::from(bounds.size.height)).abs() > 1.0).unwrap_or(true);
        self.stage_bounds = Some(bounds);
        if changed {
            cx.notify();
        }
        self.gesture.is_some()
    }

    fn listen_gesture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gesture.is_none() {
            return;
        }
        let entity = cx.entity();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            entity.update(cx, |view, cx| view.apply_gesture(event.position, cx));
        });
        let entity = cx.entity();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                return;
            }
            entity.update(cx, |view, cx| view.finish_gesture(cx));
        });
    }

    fn apply_gesture(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(mut gesture) = self.gesture.clone() else {
            return;
        };
        let Some(bounds) = self.stage_bounds else {
            return;
        };
        let sw = f64::from(bounds.size.width) as f32;
        let sh = f64::from(bounds.size.height) as f32;
        let dx = f64::from(position.x) as f32 - gesture.origin_x;
        let dy = f64::from(position.y) as f32 - gesture.origin_y;
        gesture.pointer = (f64::from(position.x) as f32, f64::from(position.y) as f32);
        if let Some(restore) = gesture.restore {
            // A click on a snapped title bar must not unsnap it; only a real drag does.
            if dx.abs() + dy.abs() < RESTORE_DRAG {
                return;
            }
            // Pop back to the floating size, keeping the grab point at the same spot along the title bar.
            let grab_x = gesture.origin_x - f64::from(bounds.origin.x) as f32;
            let along = ((grab_x - gesture.start.x) / gesture.start.w).clamp(0.0, 1.0);
            gesture.start = Frame { x: grab_x - along * restore.w, y: gesture.start.y, w: restore.w, h: restore.h };
            gesture.restore = None;
            if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                entry.snap = None;
                entry.restore = None;
            }
        }
        let group = self.group_id(&gesture.tab_id);
        let current = self.model.lock().expect("model").session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let others = self
            .chrome
            .iter()
            .filter(|(id, _)| self.group_id(id) != group && current.contains(id))
            // Partners move with the border, so they must not attract it back to where they were.
            .filter(|(id, entry)| **id != gesture.tab_id && !entry.minimized && !entry.maximized)
            .filter(|(id, _)| !gesture.partners.iter().any(|(partner, _)| partner == *id))
            .filter_map(|(_, entry)| entry.frame)
            .collect::<Vec<_>>();
        let mut frame = gesture.start;
        match gesture.kind {
            GestureKind::Move => {
                frame.x += dx;
                frame.y += dy;
            }
            GestureKind::Resize(edges) => {
                if edges.left {
                    frame.x += dx;
                    frame.w -= dx;
                }
                if edges.right {
                    frame.w += dx;
                }
                if edges.top {
                    frame.y += dy;
                    frame.h -= dy;
                }
                if edges.bottom {
                    frame.h += dy;
                }
            }
        }
        match gesture.kind {
            GestureKind::Move => {
                frame = magnet(frame, Edges { left: true, right: true, top: true, bottom: true }, &others, sw, sh);
                let x = f64::from(position.x - bounds.origin.x) as f32;
                let y = f64::from(position.y - bounds.origin.y) as f32;
                gesture.snap = snap_zone(x, y, sw, sh);
                // Up on the spaces bar the window is carried to another space instead of snapped.
                let over_bar = y < 0.0 && self.space_chips.len() > 1;
                self.lift.set(over_bar, self.anim(LIFT_ANIM));
                if over_bar {
                    gesture.snap = None;
                    let space = self.space_chips.iter().find(|(_, chip)| chip.contains(&position)).map(|(id, _)| id.clone());
                    let active = self.model.lock().expect("model").active_session_id.clone();
                    // Hovering a space opens it, taking the window along to be dragged down into it.
                    if let Some(space) = space.filter(|space| *space != active) {
                        let group = self.group_id(&gesture.tab_id);
                        let ids = self.chrome.keys().filter(|id| self.group_id(id) == group).cloned().collect::<Vec<_>>();
                        self.with_model(|model| {
                            let _ = model.move_tabs_to_session(&ids, &space);
                            let _ = model.switch_session(&space);
                        });
                        self.focus_window(&gesture.tab_id, cx);
                        self.refresh_url(cx);
                    }
                }
            }
            GestureKind::Resize(edges) => {
                frame = magnet(frame, edges, &others, sw, sh);
                frame.w = frame.w.max(MIN_W.min(gesture.start.w));
                frame.h = frame.h.max(MIN_H.min(gesture.start.h));
            }
        }
        frame = keep_on_stage(frame, sw, sh);
        if gesture.kind == GestureKind::Move {
            // The title bar is the only handle, so it must never be dragged above the stage.
            frame.y = frame.y.max(0.0);
        }
        if let GestureKind::Resize(edges) = gesture.kind {
            let starts = gesture.partners.iter().map(|(_, start)| *start).collect::<Vec<_>>();
            let (linked, followers) = link_resize(gesture.start, frame, edges, &starts);
            frame = linked;
            for ((id, _), follower) in gesture.partners.iter().zip(followers) {
                if let (Some(follower), Some(entry)) = (follower, self.chrome.get_mut(id)) {
                    entry.frame = Some(follower);
                }
            }
        }
        if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
            entry.maximized = false;
            entry.frame = Some(frame);
        }
        self.gesture = Some(gesture);
        // Native views follow in the canvas paint, in the same frame as the GPUI chrome.
        cx.notify();
    }

    fn finish_gesture(&mut self, cx: &mut Context<Self>) {
        let carried = self.carried();
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        self.lift = Tween::new(false);
        // Released still small: grow into its frame in whichever space is open now.
        if let Some((id, from)) = carried {
            self.landing = (!self.reduce_motion).then(|| (id, from, Instant::now()));
        }
        self.update_dock(cx);
        if let (Some(snap), Some(stage)) = (gesture.snap, self.stage_bounds) {
            let (sw, sh) = (f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32);
            // The size the window had when the drag began is what dragging it out of the snap brings back.
            if snap == Snap::Maximize {
                for chrome in self.chrome.values_mut() {
                    chrome.maximized = false;
                }
            }
            if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                if snap == Snap::Maximize {
                    entry.maximized = true;
                    entry.snap = None;
                    entry.restore = None;
                    entry.frame = Some(gesture.start);
                } else {
                    entry.snap = Some(snap);
                    entry.restore = Some(gesture.start);
                    entry.frame = Some(snap.frame(sw, sh));
                }
            }
        }
        cx.notify();
    }

    fn place_windows(&mut self) {
        if self.self_test.is_some() {
            return;
        }
        let modal = self.settings_open || self.delete_prompt.is_some() || self.new_profile.is_some();
        self.engine.set_modal(modal);
        let Some(stage) = self.stage_bounds else {
            return;
        };
        let active = self.active_tab_id();
        let origin_x = f64::from(stage.origin.x);
        let origin_y = f64::from(stage.origin.y);
        let stage_w = f64::from(stage.size.width);
        let stage_h = f64::from(stage.size.height);
        let ids = self.order.clone();
        // `order` spans every space; only this space's windows are on the stage.
        let current = self.model.lock().expect("model").session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let mut stack = Vec::new();
        let preview = self.gesture.as_ref().and_then(|gesture| Some((gesture.tab_id.clone(), gesture.snap?)));
        let carried = self.carried().map(|(id, _)| id);
        for id in ids {
            let Some(entry) = self.chrome.get(&id).cloned() else {
                continue;
            };
            if let Some((_, snap)) = preview.as_ref().filter(|(dragged, _)| *dragged == id) {
                // The preview hides pages behind it but has no page of its own.
                let frame = snap.frame(stage_w as f32, stage_h as f32);
                let shell = LogicalRect {
                    x: origin_x + f64::from(frame.x),
                    y: origin_y + f64::from(frame.y),
                    width: f64::from(frame.w),
                    height: f64::from(frame.h),
                };
                stack.push((String::new(), shell, LogicalRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }));
            }
            if entry.minimized || !self.is_front(&id) || !current.contains(&id) || carried.as_deref() == Some(id.as_str()) {
                self.engine.place_tab(&id, None);
                continue;
            }
            let frame = if entry.maximized {
                Frame { x: 8.0, y: 8.0, w: (stage_w as f32 - 16.0).max(1.0), h: (stage_h as f32 - 16.0).max(1.0) }
            } else if let Some(frame) = entry.frame {
                frame
            } else {
                self.engine.place_tab(&id, None);
                continue;
            };
            let find = if active.as_deref() == Some(id.as_str()) { FIND_H * f64::from(self.find.progress()) } else { 0.0 };
            let (scale, alpha) = self.window_motion(&id);
            let frame = scaled(frame, scale);
            let title_h = self.title_h();
            let rect = LogicalRect {
                x: origin_x + f64::from(frame.x) + PAGE_INSET,
                y: origin_y + f64::from(frame.y) + title_h + find,
                width: (f64::from(frame.w) - PAGE_INSET * 2.0).max(1.0),
                height: (f64::from(frame.h) - title_h - find - PAGE_INSET).max(1.0),
            };
            self.engine.place_tab(&id, Some(rect.clone()));
            self.engine.set_alpha(&id, f64::from(alpha));
            let shell = LogicalRect {
                x: origin_x + f64::from(frame.x),
                y: origin_y + f64::from(frame.y),
                width: f64::from(frame.w),
                height: f64::from(frame.h),
            };
            stack.push((id, shell, rect));
        }
        let ids = stack.iter().map(|(id, _, _)| id.clone()).filter(|id| !id.is_empty()).collect::<Vec<_>>();
        self.engine.stack_tabs(&ids);
        let stage = LogicalRect { x: origin_x, y: origin_y, width: stage_w, height: stage_h };
        let (mut holes, mut notches) = visible_pages(&stack, &stage);
        // Pages show through holes in the chrome, so cut modal cards and menus out of them.
        for (rect, radius) in &self.overlay_rects {
            cut_overlay(&mut holes, &mut notches, rect, *radius);
        }
        self.engine.set_page_holes(&holes, &notches);
    }

    fn begin_move(&mut self, id: &str, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.suppress_window_drag {
            self.suppress_window_drag = false;
            return;
        }
        // The title bar covers the top edge, so a press there has to resize instead of drag.
        if let Some(edges) = self.edges_under_pointer(id, position) {
            if edges.top {
                self.begin_resize(id, edges, position, cx);
                return;
            }
        }
        self.focus_window(id, cx);
        self.landing = None;
        let Some(start) = self.visual_frame(id) else {
            return;
        };
        // A maximized window keeps its floating frame; a snapped one keeps it in `restore`.
        let restore = self.chrome.get(id).and_then(|entry| if entry.maximized { entry.frame } else { entry.restore });
        self.gesture = Some(Gesture {
            tab_id: id.to_string(),
            kind: GestureKind::Move,
            origin_x: f64::from(position.x) as f32,
            origin_y: f64::from(position.y) as f32,
            start,
            restore,
            snap: None,
            partners: Vec::new(),
            pointer: (f64::from(position.x) as f32, f64::from(position.y) as f32),
        });
        self.engine.focus_chrome();
        cx.notify();
    }

    fn begin_resize(&mut self, id: &str, edges: Edges, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.focus_window(id, cx);
        let Some(start) = self.visual_frame(id) else {
            return;
        };
        let snapped = self.chrome.get(id).is_some_and(|entry| entry.snap.is_some());
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.maximized = false;
            entry.frame = Some(start);
        }
        // Snapped neighbours share borders with a snapped window, so they resize along with it.
        let current = self.model.lock().expect("model").session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let partners = if snapped {
            self.chrome
                .iter()
                .filter(|(other, entry)| current.contains(other) && self.group_id(other) != self.group_id(id) && entry.snap.is_some() && !entry.minimized && !entry.maximized)
                .filter_map(|(other, entry)| Some((other.clone(), entry.frame?)))
                .collect()
        } else {
            Vec::new()
        };
        self.gesture = Some(Gesture {
            tab_id: id.to_string(),
            kind: GestureKind::Resize(edges),
            origin_x: f64::from(position.x) as f32,
            origin_y: f64::from(position.y) as f32,
            start,
            restore: None,
            snap: None,
            partners,
            pointer: (f64::from(position.x) as f32, f64::from(position.y) as f32),
        });
        self.engine.focus_chrome();
        cx.notify();
    }

    fn stage(
        &self,
        tabs: &[browser_core::Tab],
        entity: Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let shown = self.shown_tabs(tabs);
        let watch = entity.clone();
        let paint_watch = entity.clone();
        let mut stage = div()
            .relative()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, _, cx| watch.update(cx, |view, cx| view.note_stage(bounds, cx)),
                    move |_bounds, listening, window, cx| {
                        paint_watch.update(cx, |view, cx| {
                            view.place_windows();
                            if listening {
                                view.listen_gesture(window, cx);
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        let mut ordered = shown;
        ordered.sort_by_key(|tab| self.order.iter().position(|id| id == &tab.id).unwrap_or(0));
        let preview = self.gesture.as_ref().and_then(|gesture| Some((gesture.tab_id.clone(), gesture.snap?)));
        let carried = self.carried().map(|(id, _)| id);
        // Windows occlude, so only a tab dropped on empty stage lands here.
        stage = stage.on_drop(cx.listener(|this, drag: &TabDrag, window, cx| this.detach_tab(&drag.0.id, window.mouse_position(), cx)));
        for tab in ordered {
            if let Some((_, snap)) = preview.as_ref().filter(|(dragged, _)| *dragged == tab.id) {
                stage = stage.child(self.snap_preview(*snap, cx));
            }
            if carried.as_deref() == Some(tab.id.as_str()) {
                continue;
            }
            let group = self.group_id(&tab.id);
            let members = tabs.iter().filter(|other| self.group_id(&other.id) == group).collect::<Vec<_>>();
            if let Some(window) = self.floating_window(tab, &members, cx) {
                stage = stage.child(window);
            }
        }
        stage
    }

    fn snap_preview(&self, snap: Snap, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let Some(stage) = self.stage_bounds else {
            return div();
        };
        let frame = snap.frame(f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32);
        div()
            .absolute()
            .left(px(frame.x))
            .top(px(frame.y))
            .w(px(frame.w))
            .h(px(frame.h))
            .rounded(px(WINDOW_RADIUS as f32))
            .border_2()
            .border_color(paint.accent.opacity(0.7))
            .bg(paint.accent.opacity(0.16))
    }

    fn edges_under_pointer(&self, id: &str, position: gpui::Point<Pixels>) -> Option<Edges> {
        let frame = self.visual_frame(id)?;
        let stage = self.stage_bounds?;
        let x = (f64::from(position.x) - f64::from(stage.origin.x)) as f32 - frame.x;
        let y = (f64::from(position.y) - f64::from(stage.origin.y)) as f32 - frame.y;
        grip_edges(x, y, frame.w, frame.h)
    }

    fn visual_frame(&self, id: &str) -> Option<Frame> {
        let entry = self.chrome.get(id)?;
        if entry.maximized {
            let stage = self.stage_bounds?;
            return Some(Frame {
                x: 8.0,
                y: 8.0,
                w: (f64::from(stage.size.width) as f32 - 16.0).max(1.0),
                h: (f64::from(stage.size.height) as f32 - 16.0).max(1.0),
            });
        }
        entry.frame
    }

    pub(crate) fn focus_window(&mut self, id: &str, cx: &mut Context<Self>) {
        self.order.retain(|existing| existing != id);
        self.order.push(id.to_string());
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.minimized = false;
        }
        if self.active_tab_id().as_deref() != Some(id) {
            self.with_model(|model| { let _ = model.activate_tab(id); });
            self.refresh_url(cx);
        }
        self.place_windows();
        cx.notify();
    }

    fn shown_tabs<'a>(&self, tabs: &'a [browser_core::Tab]) -> Vec<&'a browser_core::Tab> {
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        tabs.iter()
            .filter(|tab| {
                let entry = self.chrome.get(&tab.id);
                let minimized = entry.is_some_and(|chrome| chrome.minimized);
                let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
                !minimized && !covered && self.is_front(&tab.id)
            })
            .collect()
    }

    fn floating_window(&self, tab: &browser_core::Tab, members: &[&browser_core::Tab], cx: &mut Context<Self>) -> Option<gpui::Div> {
        let paint = paint(cx);
        let active = tab.active;
        let maximized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized);
        let frame = self.chrome.get(&tab.id).and_then(|chrome| chrome.frame)?;
        // Occlude so a press on this window never also reaches the windows stacked beneath it.
        let (scale, alpha) = self.window_motion(&tab.id);
        let mut shell = div().absolute().overflow_hidden().occlude().opacity(alpha);
        shell = if maximized {
            shell.left(px(8.)).top(px(8.)).right(px(8.)).bottom(px(8.))
        } else {
            let frame = scaled(frame, scale);
            shell.left(px(frame.x)).top(px(frame.y)).w(px(frame.w)).h(px(frame.h))
        };
        let target = tab.id.clone();
        let mut body = div().relative().size_full().child(
            v_stack()
                .id(SharedString::from(format!("window-{}", tab.id)))
                .size_full()
                .rounded(px(WINDOW_RADIUS as f32))
                .border_1()
                .border_color(if active { paint.accent } else { paint.border })
                .drag_over::<TabDrag>(move |style, _, _, _| style.border_color(paint.accent).bg(paint.accent.opacity(0.12)))
                .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| this.move_tab_to_window(&drag.0.id, &target, cx)))
                .shadow_md()
                .bg(paint.secondary)
                .overflow_hidden()
                .child(self.window_titlebar(tab, members, cx))
                .when(active && self.find.progress() > 0.0, |column| {
                    column.child(div().h(px(FIND_H as f32 * self.find.progress())).flex_none().overflow_hidden().child(self.find_bar(cx)))
                })
                .child(div().flex_1().min_h(px(0.))),
        );
        if !maximized {
            body = body
                .child(self.resize_grip(&tab.id, Edges::left(), cx))
                .child(self.resize_grip(&tab.id, Edges::right(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom(), cx))
                .child(self.resize_grip(&tab.id, Edges::top(), cx))
                .child(self.resize_grip(&tab.id, Edges::top_left(), cx))
                .child(self.resize_grip(&tab.id, Edges::top_right(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom_left(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom_right(), cx));
        }
        Some(shell.child(body))
    }

    fn resize_grip(&self, tab_id: &str, edges: Edges, cx: &mut Context<Self>) -> impl IntoElement {
        let id = tab_id.to_string();
        let name = format!(
            "grip-{}-{}{}{}{}",
            tab_id,
            if edges.left { "l" } else { "" },
            if edges.right { "r" } else { "" },
            if edges.top { "t" } else { "" },
            if edges.bottom { "b" } else { "" },
        );
        let mut grip = div()
            .id(SharedString::from(name))
            .absolute()
            .occlude()
            .cursor(resize_cursor(edges))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.begin_resize(&id, edges, event.position, cx);
            }));
        let corner = (edges.left || edges.right) && (edges.top || edges.bottom);
        let band = px(GRIP);
        let corner_size = px(GRIP_CORNER);
        if corner {
            grip = grip.size(corner_size);
            grip = if edges.left { grip.left(px(0.)) } else { grip.right(px(0.)) };
            grip = if edges.top { grip.top(px(0.)) } else { grip.bottom(px(0.)) };
        } else if edges.left || edges.right {
            grip = grip.w(band).top(corner_size).bottom(corner_size);
            grip = if edges.left { grip.left(px(0.)) } else { grip.right(px(0.)) };
        } else {
            grip = grip.h(band).left(corner_size).right(corner_size);
            grip = if edges.top { grip.top(px(0.)) } else { grip.bottom(px(0.)) };
        }
        grip
    }

    /// Safari compact style: one row of nav, tabs and tools, where the shown tab is the address field.
    fn window_titlebar(&self, tab: &browser_core::Tab, members: &[&browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        if let Some(ext) = self.chrome.get(&tab.id).and_then(|chrome| chrome.panel.clone()) {
            return self.panel_titlebar(&tab.id, &ext, cx);
        }
        let paint = paint(cx);
        let id = tab.id.clone();
        let nav = self.nav_controls(&tab.id, cx);
        let tools = self.window_tools(&tab.id, cx);
        let bar = (tab.active && self.load_bar > 0.0).then(|| {
            let fade = self.load_done.map_or(1.0, |at| 1.0 - (at.elapsed().as_secs_f32() / LOAD_FADE.as_secs_f32()).min(1.0));
            div().absolute().left_0().bottom_0().h(px(2.)).w(relative(self.load_bar)).bg(paint.accent).opacity(fade)
        });
        div().relative().children(bar).child(
            h_stack()
                .id(SharedString::from(format!("title-{id}")))
                .h(px(self.title_h() as f32))
                .px(px(6.))
                .items_center()
                .gap(px(8.))
                .bg(paint.surface)
                .border_b_1()
                .border_color(paint.border)
                .cursor(CursorStyle::OpenHand)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_move(&id, event.position, cx);
                }))
                // Buttons keep their size; the tabs shrink, and the empty space either side drags the window.
                .child(self.hit_guard(format!("nav-guard-{}", tab.id), nav, cx).flex_none())
                .child(div().flex_1().min_w(px(8.)).h_full())
                .child(self.compact_tabs(&tab.id, members, cx))
                .child(div().flex_1().min_w(px(8.)).h_full())
                .child(self.hit_guard(format!("tools-guard-{}", tab.id), tools, cx).flex_none()),
        )
    }

    /// Small centred tab pills; clicking the shown tab widens it into the address field.
    fn compact_tabs(&self, front: &str, members: &[&browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let mut row = h_stack().min_w(px(0.)).h_full().items_center().gap(px(2.)).text_size(px(12.));
        for tab in members {
            let shown = tab.id == front;
            if shown && tab.active && (self.address_editing || self.address_focused) {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("ctab-{}", tab.id)))
                        .w(px(520.))
                        .min_w(px(120.))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, _| this.suppress_window_drag = true))
                        .child(self.address_slot(tab, cx)),
                );
                continue;
            }
            let id = tab.id.clone();
            let close_id = tab.id.clone();
            let hover_group = SharedString::from(format!("ctab-{id}"));
            row = row.child(
                h_stack()
                    .id(SharedString::from(format!("ctab-{id}")))
                    .group(hover_group.clone())
                    .relative()
                    // Shrinks to just the icon when the window is narrow.
                    .min_w(px(28.))
                    .max_w(px(160.))
                    .h(px(28.))
                    .px(px(8.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(8.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(shown, |pill| pill.bg(paint.secondary).text_color(paint.fg))
                    .when(!shown, |pill| pill.text_color(paint.muted).hover(|style| style.bg(paint.secondary.opacity(0.6)).text_color(paint.fg)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, _| this.suppress_window_drag = true))
                    .on_drag(TabDrag((*tab).clone()), |_, _, _, cx| cx.new(|_| TabDragPreview))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        // Safari compact: a click selects a tab, a click on the selected one edits its address.
                        let edit = this.is_front(&id) && this.active_tab_id().as_deref() == Some(id.as_str());
                        this.focus_window(&id, cx);
                        if edit {
                            this.focus_address = true;
                        }
                    }))
                    // The close button takes the favicon's place on hover, as in Safari.
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().group_hover(hover_group.clone(), |style| style.opacity(0.)).child(favicon(tab, false)))
                            .child(
                                div()
                                    .id(SharedString::from(format!("ctab-close-{close_id}")))
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .opacity(0.)
                                    .group_hover(hover_group, |style| style.opacity(1.))
                                    .hover(|style| style.bg(paint.border))
                                    .child(Icons::cross_2().size(px(10.)).text_color(paint.fg))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.close_tabs(vec![close_id.clone()], cx);
                                    })),
                            ),
                    )
                    .child(div().min_w(px(0.)).truncate().child(tab_label(&tab.title, &tab.url))),
            );
        }
        row
    }

    /// `[icon] Name ──── [×]`: a side panel needs no address bar or navigation.
    fn panel_titlebar(&self, tab_id: &str, ext: &str, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let meta = self.ext_meta.get(ext).cloned().unwrap_or_default();
        let (move_id, close_id) = (tab_id.to_string(), tab_id.to_string());
        let close = self.tool_button(format!("close-{tab_id}"), Icons::cross_2(), false, cx, move |this, cx| {
            this.close_tabs(vec![close_id.clone()], cx);
        });
        div().child(
            h_stack()
                .id(SharedString::from(format!("title-{tab_id}")))
                .h(px(self.title_h() as f32))
                .px(px(10.))
                .items_center()
                .gap(px(8.))
                .bg(paint.surface)
                .border_b_1()
                .border_color(paint.border)
                .cursor(CursorStyle::OpenHand)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_move(&move_id, event.position, cx);
                }))
                .child(extension_icon(&meta.name, meta.icon.clone(), 16.))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).child(meta.name.clone()))
                .child(self.hit_guard(format!("tools-guard-{tab_id}"), close, cx)),
        )
    }

    fn hit_guard(&self, id: String, child: impl IntoElement, cx: &mut Context<Self>) -> gpui::Div {
        div().child(
            div()
                .id(SharedString::from(id))
                .flex()
                .items_center()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, _| {
                    this.suppress_window_drag = true;
                }))
                .child(child),
        )
    }

    fn address_slot(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let editing = tab.active && (self.address_editing || self.address_focused);
        let pill = div()
            .h(px(28.))
            .w_full()
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .overflow_hidden()
            .bg(paint.secondary)
            .border_1()
            .text_size(px(12.));
        if editing {
            let watch = cx.entity();
            return pill
                .relative()
                .border_color(paint.accent.opacity(0.6))
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| this.end_address_edit(window, cx)))
                // Runs before the input sees the key, so arrows move through suggestions instead of the caret.
                .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    let count = this.suggestions.len();
                    match event.keystroke.key.as_str() {
                        "down" if count > 0 => this.suggestion = Some(this.suggestion.map_or(0, |at| (at + 1) % count)),
                        "up" if count > 0 => this.suggestion = Some(this.suggestion.map_or(count - 1, |at| (at + count - 1) % count)),
                        "enter" => match this.suggestion.and_then(|at| this.suggestions.get(at)).map(|(url, _)| url.clone()) {
                            Some(url) => this.commit_navigation(&url, cx),
                            None => return,
                        },
                        "escape" if count > 0 => this.suggestions.clear(),
                        "escape" => this.end_address_edit(window, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(canvas(move |bounds, _, cx| watch.update(cx, |view, _| view.address_bounds = Some(bounds)), |_, _, _, _| {}).absolute().size_full())
                .child(
                input(&self.address, cx)
                    .placeholder("Search or enter an address")
                    .cursor_color(paint.accent)
                    .flex_1()
                    .h_full()
                    .text_size(px(12.))
                    .text_color(paint.fg),
            );
        }
        let id = tab.id.clone();
        let (label, color) = if tab.url.is_empty() || tab.url == "nxbrowser://home" {
            ("Search or enter an address".to_string(), paint.muted)
        } else {
            (display_url(&tab.url), if tab.active { paint.fg } else { paint.muted })
        };
        div().w_full().child(
            pill.id(SharedString::from(format!("address-{id}")))
                .border_color(gpui::transparent_black())
                .cursor_text()
                .text_color(color)
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.focus_window(&id, cx);
                    this.focus_address = true;
                    cx.notify();
                })),
        )
    }

    fn nav_controls(&self, tab_id: &str, cx: &mut Context<Self>) -> gpui::Div {
        let back_id = tab_id.to_string();
        let forward_id = tab_id.to_string();
        h_stack()
            .items_center()
            .gap(px(2.))
            .child(self.tool_button(format!("back-{back_id}"), Icons::arrow_left(), true, cx, move |this, cx| {
                this.focus_window(&back_id, cx);
                this.back();
            }))
            .child(self.tool_button(format!("forward-{forward_id}"), Icons::arrow_right(), true, cx, move |this, cx| {
                this.focus_window(&forward_id, cx);
                this.forward();
            }))
    }

    fn window_tools(&self, tab_id: &str, cx: &mut Context<Self>) -> gpui::Div {
        let reload_id = tab_id.to_string();
        let bookmark_id = tab_id.to_string();
        let new_tab_id = tab_id.to_string();
        let min_id = tab_id.to_string();
        let zoom_id = tab_id.to_string();
        let close_id = tab_id.to_string();
        let maximized = self.chrome.get(tab_id).is_some_and(|chrome| chrome.maximized);
        let zoom_icon = if maximized { Icons::exit_full_screen() } else { Icons::enter_full_screen() };
        h_stack()
            .items_center()
            .gap(px(2.))
            .child(self.tool_button(format!("reload-{reload_id}"), Icons::reload(), false, cx, move |this, cx| {
                this.focus_window(&reload_id, cx);
                this.reload();
            }))
            .child(self.tool_button(format!("bookmark-{bookmark_id}"), Icons::star(), false, cx, move |this, cx| {
                this.focus_window(&bookmark_id, cx);
                this.bookmark();
            }))
            .child(self.tool_button(format!("new-tab-{new_tab_id}"), Icons::plus(), false, cx, move |this, cx| {
                this.new_tab_in(&new_tab_id, None, cx);
            }))
            .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(paint(cx).border))
            .child(self.tool_button(format!("min-{min_id}"), Icons::minus(), false, cx, move |this, cx| {
                let entry = this.chrome.entry(min_id.clone()).or_default();
                entry.minimized = true;
                entry.maximized = false;
                cx.notify();
            }))
            .child(self.tool_button(format!("zoom-{zoom_id}"), zoom_icon, false, cx, move |this, cx| {
                let next = !maximized;
                for chrome in this.chrome.values_mut() {
                    chrome.maximized = false;
                }
                let entry = this.chrome.entry(zoom_id.clone()).or_default();
                entry.maximized = next;
                entry.minimized = false;
                this.focus_window(&zoom_id, cx);
            }))
            .child(self.tool_button(format!("close-{close_id}"), Icons::cross_2(), false, cx, move |this, cx| {
                let group = this.group_id(&close_id);
                let members = this.chrome.keys().filter(|id| this.group_id(id) == group).cloned().collect::<Vec<_>>();
                this.close_tabs(members, cx);
            }))
    }

    fn tool_button(
        &self,
        id: String,
        icon: gpui::Svg,
        prominent: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpuikit::elements::icon_button::IconButton {
        let button = if prominent {
            icon_button(id, icon).box_size(rems(1.875)).icon_size(rems(1.15))
        } else {
            icon_button(id, icon).box_size(rems(1.625)).icon_size(rems(1.0))
        };
        button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| action(this, cx)))
    }

    fn titlebar(
        &self,
        sessions: &[browser_core::Session],
        all_tabs: &[browser_core::Tab],
        active_session: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        let mut spaces = h_stack().flex_none().items_center().gap(px(6.));
        let closable = sessions.len() > 1;
        for session in sessions {
            let id = session.id.clone();
            let close_id = session.id.clone();
            let hover_group = SharedString::from(format!("space-{id}"));
            let active = session.id == active_session;
            let members = all_tabs.iter().filter(|tab| tab.session_id == session.id).collect::<Vec<_>>();
            let watch = cx.entity();
            let chip_id = id.clone();
            spaces = spaces.child(
                h_stack()
                    .id(SharedString::from(format!("space-{id}")))
                    .group(hover_group.clone())
                    .relative()
                    .h(px(28.))
                    .px(px(10.))
                    .gap(px(8.))
                    .items_center()
                    .rounded(px(8.))
                    .cursor_pointer()
                    .text_color(if active { paint.fg } else { paint.muted })
                    .when(active, |chip| chip.bg(paint.secondary))
                    .when(!active, |chip| chip.hover(|style| style.bg(paint.secondary.opacity(0.5)).text_color(paint.fg)))
                    .child(canvas(move |bounds, _, cx| watch.update(cx, |view, _| view.space_chips.push((chip_id, bounds))), |_, _, _, _| {}).absolute().size_full())
                    .child(favicon_stack(&members, if active { paint.secondary } else { paint.surface }))
                    .child(session.name.clone())
                    .when(closable, |chip| {
                        chip.child(
                            div()
                                .id(SharedString::from(format!("space-close-{close_id}")))
                                .absolute()
                                .top(px(-5.))
                                .left(px(-5.))
                                .size(px(16.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(paint.muted)
                                .border_1()
                                .border_color(paint.surface)
                                .opacity(0.)
                                .group_hover(hover_group, |style| style.opacity(1.))
                                .child(Icons::cross_2().size(px(9.)).text_color(paint.surface))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.with_model(|model| { let _ = model.delete_session(&close_id); });
                                    this.refresh_url(cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_session(&id); });
                        this.refresh_url(cx);
                    })),
            );
        }
        spaces = spaces.child(self.tool_button("new-space".to_string(), Icons::plus(), false, cx, |this, cx| {
            this.with_model(|model| { let _ = model.create_session(); });
            this.refresh_url(cx);
        }));
        h_stack()
            .h(px(self.titlebar_h()))
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .pl(px(80.))
            .pr(px(8.))
            .bg(paint.surface)
            .border_b_1()
            .border_color(paint.border)
            // Equal flex on both sides keeps the spaces centred in the bar.
            .child(drag_area().flex_1())
            .child(spaces)
            .child(
                h_stack()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .gap(px(2.))
                    .child(drag_area().flex_1())
                    .child(self.pinned_extensions(cx))
                    .child(self.panel_icon("find-toggle", Icons::magnifying_glass(), None, cx))
                    .child(self.panel_icon("history-toggle", Icons::file_text(), Some(SidebarPage::History), cx))
                    .child(self.panel_icon("bookmarks-toggle", Icons::bookmark(), Some(SidebarPage::Bookmarks), cx))
                    .child(
                        icon_button("extensions-toggle", gpui::svg().path(crate::PUZZLE_ICON))
                            .box_size(rems(1.625))
                            .icon_size(rems(1.0))
                            .selected(self.extensions_open)
                            // Opens only: a click outside the popover (this one included) already closes it.
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.extensions_open = true;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// Blob hanging from the title bar; hovering grows it into a shelf of this space's windows.
    fn dock(&self, tabs: &[browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        let p = self.dock_progress();
        let active = self.active_tab_id().map(|id| self.group_id(&id));
        let mut groups: Vec<(String, Vec<&browser_core::Tab>)> = Vec::new();
        for tab in tabs {
            let group = self.group_id(&tab.id);
            match groups.iter_mut().find(|(id, _)| *id == group) {
                Some((_, members)) => members.push(tab),
                None => groups.push((group, vec![tab])),
            }
        }
        // Dynamic Island: always black, light marks on top, in either theme.
        let island = gpui::black();
        let ink = gpui::white();
        let tile = |id: SharedString, width: f32| {
            div()
                .id(id)
                .relative()
                .flex_none()
                .w(px(width))
                .h(px(DOCK_TILE_H))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
        };
        let mut shelf = h_stack().flex_none().items_center().gap(px(DOCK_GAP));
        let mut count = 0;
        for (group, members) in &groups {
            let Some(front) = members.iter().find(|tab| self.is_front(&tab.id)).copied() else {
                continue;
            };
            count += 1;
            let id = front.id.clone();
            let minimized = self.chrome.get(&id).is_some_and(|chrome| chrome.minimized);
            let current = active.as_deref() == Some(group.as_str());
            shelf = shelf.child(
                tile(SharedString::from(format!("dock-{group}")), DOCK_ITEM_W)
                    .when(current, |item| item.bg(ink.opacity(0.16)))
                    .when(!current, |item| item.hover(|style| style.bg(ink.opacity(0.08))))
                    .when(minimized, |item| item.opacity(0.45))
                    .child(favicon_stack(members, island))
                    .when(members.len() > 1, |item| {
                        item.child(
                            div()
                                .absolute()
                                .top(px(0.))
                                .right(px(0.))
                                .min_w(px(14.))
                                .h(px(14.))
                                .px(px(3.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(ink.opacity(0.22))
                                .text_size(px(9.))
                                .text_color(ink)
                                .child(members.len().to_string()),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.focus_window(&id, cx))),
            );
        }
        let shelf = shelf
            .child(div().flex_none().w(px(1.)).h(px(18.)).bg(ink.opacity(0.18)))
            .child(
                tile("dock-new".into(), DOCK_NEW_W)
                    .hover(|style| style.bg(ink.opacity(0.08)))
                    .child(Icons::plus().size(px(14.)).text_color(ink.opacity(0.7)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.create_tab(&model.settings.new_tab_url()); });
                        this.focus_address = true;
                        this.refresh_url(cx);
                    })),
            );
        // Every child has a fixed width, so the island opens to exactly fit its content.
        let shelf_w = DOCK_PAD * 2.0 + count as f32 * (DOCK_ITEM_W + DOCK_GAP) + 1.0 + DOCK_GAP + DOCK_NEW_W;
        let lerp = |a: f32, b: f32| a + (b - a) * p;
        let height = lerp(DOCK_REST_H, DOCK_OPEN_H);
        // A capsule at every size, like the island. No shadow: it would darken the chrome around it
        // but not the pages, which show through holes in the chrome.
        let radius = height / 2.0;
        div()
            .absolute()
            .top(px(self.titlebar_h()))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                // Padding widens the hover target well past the resting pill.
                div()
                    .id("dock")
                    .pt(px(lerp(2., 6.)))
                    .px(px(28.))
                    .pb(px(12.))
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.set_dock(*hovered, cx)))
                    .child(
                        div()
                            .relative()
                            .w(px(lerp(DOCK_REST_W, shelf_w)))
                            .h(px(height))
                            .overflow_hidden()
                            .rounded(px(radius))
                            .bg(island)
                            // The island floats over window title bars: a press on it must not reach the buttons beneath.
                            // Not `occlude`, which would also unhover the hover zone around it and shut the dock.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(measure_overlay(f64::from(radius), cx))
                            .child(
                                // Laid out at full height and slid down into view, so items never squash while the island grows.
                                h_stack()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(px(lerp(-8., 0.)))
                                    .h(px(DOCK_OPEN_H))
                                    .items_center()
                                    .justify_center()
                                    .opacity(((p - 0.3) / 0.7).clamp(0., 1.))
                                    .child(shelf),
                            ),
                    ),
            )
    }

    /// Opens a new tab inside the window holding `id`, sharing its frame. `None` opens the new-tab page
    /// with the address field focused.
    fn new_tab_in(&mut self, id: &str, url: Option<&str>, cx: &mut Context<Self>) {
        let group = self.group_id(id);
        let mut chrome = self.chrome.get(id).cloned().unwrap_or_default();
        chrome.window = Some(group.clone());
        let created = self.with_model(|model| model.create_tab(&url.map_or_else(|| model.settings.new_tab_url(), str::to_string)));
        let Ok(new) = created else {
            return;
        };
        self.chrome.insert(new.clone(), chrome);
        self.focus_window(&new, cx);
        self.focus_address = url.is_none();
    }

    /// Moves a tab into the window holding `target`, in front, sharing its frame.
    fn move_tab_to_window(&mut self, id: &str, target: &str, cx: &mut Context<Self>) {
        let group = self.group_id(target);
        if self.group_id(id) == group {
            return;
        }
        let mut chrome = self.chrome.get(target).cloned().unwrap_or_default();
        chrome.window = Some(group);
        self.chrome.insert(id.to_string(), chrome);
        self.focus_window(id, cx);
    }

    /// Pulls a tab out of its window into one of its own, under the pointer.
    fn detach_tab(&mut self, id: &str, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.group_len(id) < 2 {
            return;
        }
        let (Some(stage), Some(start), Some(mut chrome)) = (self.stage_bounds, self.visual_frame(id), self.chrome.get(id).cloned()) else {
            return;
        };
        let (sw, sh) = (f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32);
        let x = f64::from(at.x - stage.origin.x) as f32 - 60.0;
        let y = f64::from(at.y - stage.origin.y) as f32 - TITLEBAR_H / 2.0;
        // A fresh key: the tabs left behind may still be keyed by this tab's id.
        chrome.window = Some(Uuid::new_v4().to_string());
        chrome.snap = None;
        chrome.restore = None;
        chrome.maximized = false;
        chrome.minimized = false;
        chrome.frame = Some(keep_on_stage(Frame { x, y, w: start.w, h: start.h }, sw, sh));
        self.chrome.insert(id.to_string(), chrome);
        if !self.reduce_motion {
            self.opened.insert(id.to_string(), Instant::now());
        }
        self.focus_window(id, cx);
    }

    /// The window being carried to another space and where it is drawn, in window coordinates:
    /// shrinking under the pointer over the spaces bar, or growing back into place after the drop.
    fn carried(&self) -> Option<(String, Frame)> {
        let stage = self.stage_bounds?;
        let (ox, oy) = (f64::from(stage.origin.x) as f32, f64::from(stage.origin.y) as f32);
        if let Some((id, from, at)) = &self.landing {
            let to = self.visual_frame(id)?;
            let to = Frame { x: to.x + ox, y: to.y + oy, ..to };
            return Some((id.clone(), lerp_frame(*from, to, ease(at.elapsed().as_secs_f32() / LAND_ANIM.as_secs_f32()))));
        }
        let gesture = self.gesture.as_ref().filter(|gesture| gesture.kind == GestureKind::Move)?;
        let p = self.lift.progress();
        if p <= 0.0 {
            return None;
        }
        let (px, py) = gesture.pointer;
        let start = gesture.start;
        let full = Frame { x: start.x + px - gesture.origin_x + ox, y: start.y + py - gesture.origin_y + oy, ..start };
        let small = (LIFT_W / full.w).min(1.0);
        Some((gesture.tab_id.clone(), shrink_toward(full, gesture.pointer, 1.0 + (small - 1.0) * p)))
    }

    /// A stand-in for the carried window: its page is hidden while it is too small to read.
    fn carried_view(&self, tabs: &[browser_core::Tab], cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (id, frame) = self.carried()?;
        let paint = paint(cx);
        let group = self.group_id(&id);
        let members = tabs.iter().filter(|tab| self.group_id(&tab.id) == group).collect::<Vec<_>>();
        let title = (self.title_h() as f32).min(frame.h * 0.2);
        Some(
            div()
                .absolute()
                .left(px(frame.x))
                .top(px(frame.y))
                .w(px(frame.w))
                .h(px(frame.h))
                .rounded(px(WINDOW_RADIUS as f32))
                .border_1()
                .border_color(paint.accent)
                .bg(paint.secondary)
                .overflow_hidden()
                .child(measure_overlay(WINDOW_RADIUS, cx))
                .child(
                    v_stack()
                        .size_full()
                        .child(div().h(px(title)).flex_none().bg(paint.surface).border_b_1().border_color(paint.border))
                        .child(div().flex_1().flex().items_center().justify_center().child(favicon_stack(&members, paint.secondary))),
                ),
        )
    }

    /// The tab pill following the pointer while it is dragged.
    fn tab_drag_view(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (tab, at) = self.tab_drag.as_ref()?;
        let paint = paint(cx);
        Some(
            h_stack()
                .absolute()
                .left(at.x - px(20.))
                .top(at.y - px(14.))
                .h(px(28.))
                .max_w(px(180.))
                .px(px(8.))
                .gap(px(6.))
                .items_center()
                .rounded(px(8.))
                .bg(paint.secondary)
                .border_1()
                .border_color(paint.accent)
                .text_size(px(12.))
                .text_color(paint.fg)
                .child(measure_overlay(8.0, cx))
                .child(favicon(tab, false))
                .child(div().min_w(px(0.)).truncate().child(tab_label(&tab.title, &tab.url))),
        )
    }

    /// The window a tab belongs to; a tab never added to another window is its own.
    fn group_id(&self, id: &str) -> String {
        self.chrome.get(id).and_then(|chrome| chrome.window.clone()).unwrap_or_else(|| id.to_string())
    }

    fn group_len(&self, id: &str) -> usize {
        let group = self.group_id(id);
        self.chrome.keys().filter(|other| self.group_id(other) == group).count()
    }

    /// Whether this tab is the one its window shows: the group member raised most recently.
    fn is_front(&self, id: &str) -> bool {
        let group = self.group_id(id);
        raised_last(&self.order, id, |other| self.group_id(other) == group)
    }

    /// Copies each window's frame and state from its shown tab to the hidden ones, so whichever is raised next sits in place.
    fn sync_groups(&mut self) {
        let fronts = self
            .chrome
            .iter()
            .filter(|(id, _)| self.is_front(id) && self.group_len(id) > 1)
            .map(|(id, chrome)| (self.group_id(id), id.clone(), chrome.clone()))
            .collect::<Vec<_>>();
        for (group, front, source) in fronts {
            for (id, entry) in self.chrome.iter_mut() {
                if *id != front && entry.window.as_deref().unwrap_or(id) == group {
                    let window = entry.window.take();
                    *entry = TabChrome { window, ..source.clone() };
                }
            }
        }
    }

    fn dock_progress(&self) -> f32 {
        self.dock.progress()
    }

    fn set_dock(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.dock_hovered = hovered;
        self.update_dock(cx);
    }

    fn update_dock(&mut self, cx: &mut Context<Self>) {
        // A window dragged to the top edge passes over the dock; it should not pop open.
        let open = self.dock_hovered && self.gesture.is_none();
        if self.dock.set(open, self.anim(DOCK_ANIM)) {
            cx.notify();
        }
    }

    fn animating(&self) -> bool {
        self.dock.running()
            || self.find.running()
            || self.panel_tween.running()
            || self.toast.1.elapsed() < TOAST_LIFE
            || self.load_bar > 0.0
            || !self.opened.is_empty()
            || self.lift.running()
            || self.landing.is_some()
    }

    fn panel_icon(
        &self,
        id: &'static str,
        icon: gpui::Svg,
        page: Option<SidebarPage>,
        cx: &mut Context<Self>,
    ) -> gpuikit::elements::icon_button::IconButton {
        let selected = match page {
            Some(page) => self.panel == Some(page),
            None => self.find_open,
        };
        icon_button(id, icon).box_size(rems(1.625)).icon_size(rems(1.0)).selected(selected).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            if page.is_none() {
                this.find_open = !this.find_open;
                cx.notify();
            } else if let Some(page) = page {
                this.toggle_panel(page, cx);
            }
        }))
    }

    fn panel_view(
        &self,
        page: SidebarPage,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        sessions: &[browser_core::Session],
        history: &[browser_core::HistoryEntry],
        bookmarks: &[browser_core::Bookmark],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        let p = self.panel_tween.progress();
        div()
            .h(px(260. * p))
            .flex_none()
            .opacity(p)
            .w_full()
            .overflow_hidden()
            .px(px(12.))
            .py(px(8.))
            .bg(paint.surface)
            .border_t_1()
            .border_color(paint.border)
            .child(
                scroll_area("library-panel")
                    .vertical()
                    .full_height(true)
                    .child(self.library_page(
                        page,
                        profiles,
                        active_profile,
                        sessions,
                        history,
                        bookmarks,
                        cx,
                    )),
            )
    }

    fn find_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        h_stack()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(6.))
            .bg(paint.surface)
            .border_b_1()
            .border_color(paint.border)
            .child(
                div().flex_1().child(
                    text_field(&self.find_input, cx)
                        .placeholder("Find in page")
                        .full_width(true),
                ),
            )
            .child(action_button("find-next", "Next", Kind::Secondary, true, cx, |this, cx| {
                let query = this.find_input.read(cx).content().to_string();
                if let Some(id) = this.active_tab_id() {
                    this.engine.find(&id, &query);
                }
            }))
            .child(action_button("find-close", "Close", Kind::Secondary, true, cx, |this, cx| {
                this.find_open = false;
                cx.notify();
            }))
    }

    fn library_page(
        &self,
        page: SidebarPage,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        sessions: &[browser_core::Session],
        history: &[browser_core::HistoryEntry],
        bookmarks: &[browser_core::Bookmark],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match page {
            SidebarPage::Profiles => self.profile_page(profiles, active_profile, sessions, cx),
            SidebarPage::History => self.link_list("No history yet", history.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
            SidebarPage::Bookmarks => self.link_list("No bookmarks yet", bookmarks.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
        }
    }

    fn profile_page(
        &self,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        sessions: &[browser_core::Session],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        let mut column = v_stack().gap(px(6.)).w_full();
        column = column.child(self.section_label("Profiles", paint));
        for profile in profiles {
            let id = profile.id.clone();
            let active = profile.id == active_profile;
            column = column.child(
                div()
                    .id(SharedString::from(format!("profile-{id}")))
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .bg(if active { paint.accent_bg } else { paint.secondary })
                    .text_color(if active { paint.accent } else { paint.fg })
                    .child(profile_label(&profile.name, active))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_profile(&id); });
                        this.refresh_url(cx);
                        this.confirm_delete = false;
                    })),
            );
        }
        column = column
            .child(action_button("new-profile", "New profile…", Kind::Secondary, true, cx, |this, cx| {
                this.open_new_profile(cx);
            }))
            .child(action_button("rename-profile", if self.renaming { "Save name" } else { "Rename" }, Kind::Secondary, true, cx, |this, cx| {
                if this.renaming {
                    let name = this.rename_input.read(cx).content().to_string();
                    let id = this.model.lock().expect("model").active_profile_id.clone();
                    let _ = this.model.lock().expect("model").rename_profile(&id, &name);
                    this.renaming = false;
                } else {
                    let name = this.model.lock().ok().and_then(|model| model.active_profile().map(|profile| profile.name.clone())).unwrap_or_default();
                    this.rename_input.update(cx, |input, cx| input.set_content_silent(name, cx));
                    this.renaming = true;
                }
                cx.notify();
            }));
        if self.renaming {
            column = column.child(
                text_field(&self.rename_input, cx)
                    .placeholder("Profile name")
                    .full_width(true),
            );
        }
        let only_one = profiles.len() <= 1;
        column = column.child(action_button("delete-profile", "Delete profile…", Kind::Danger, !only_one, cx, |this, cx| {
            this.delete_prompt = this.model.lock().ok().map(|model| model.active_profile_id.clone());
            cx.notify();
        }));
        column = column.child(self.section_label("Sessions", paint));
        for session in sessions {
            let id = session.id.clone();
            column = column.child(
                action_button(SharedString::from(format!("session-{id}")), session.name.clone(), Kind::Secondary, true, cx, move |this, cx| {
                    this.with_model(|model| { let _ = model.switch_session(&id); });
                    this.refresh_url(cx);
                }),
            );
        }
        column
            .child(action_button("new-session", "New session", Kind::Secondary, true, cx, |this, cx| {
                this.with_model(|model| { let _ = model.create_session(); });
                this.refresh_url(cx);
            }))
            .child(action_button("export-profile", "Export profile", Kind::Secondary, true, cx, |this, _| this.export_profile()))
            .child(action_button("import-profile", "Import profile", Kind::Secondary, true, cx, |this, cx| {
                this.import_profile();
                this.refresh_url(cx);
            }))
            .child(self.permission_row(cx))
    }

    fn link_list(&self, empty: &'static str, rows: Vec<(String, String)>, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        if rows.is_empty() {
            return div().py(px(8.)).text_color(paint.muted).child(empty);
        }
        let mut column = v_stack().gap(px(4.)).w_full();
        for (index, (title, url)) in rows.into_iter().enumerate() {
            let open = url.clone();
            let label = if title.is_empty() { url.clone() } else { title };
            column = column.child(
                v_stack()
                    .id(SharedString::from(format!("link-{index}")))
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(paint.secondary))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.address.update(cx, |input, cx| input.set_content_silent(open.clone(), cx));
                        this.commit_navigation(&open, cx);
                    }))
                    .child(label)
                    .child(div().text_size(px(11.)).text_color(paint.muted).child(url)),
            );
        }
        column
    }

    fn permission_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let Some((origin, kind)) = self.permission.clone() else {
            return div().text_color(paint.muted).child("No permission requests");
        };
        v_stack()
            .gap(px(6.))
            .child(self.section_label("Permission", paint))
            .child(format!("{kind} for {origin}"))
            .child(action_button("allow-permission", "Allow", Kind::Primary, true, cx, {
                let (origin, kind) = (origin.clone(), kind.clone());
                move |this, _| {
                    let _ = this.model.lock().expect("model").set_permission(&origin, &kind, "allow");
                    this.sync_permissions();
                    this.status = "Allowed for the next request".into();
                }
            }))
            .child(action_button("deny-permission", "Deny", Kind::Secondary, true, cx, move |this, _| {
                let _ = this.model.lock().expect("model").set_permission(&origin, &kind, "deny");
                this.sync_permissions();
                this.permission = None;
            }))
    }

    fn section_label(&self, label: &'static str, paint: Paint) -> gpui::Div {
        div()
            .mt(px(4.))
            .text_size(px(12.))
            .text_color(paint.muted)
            .child(label)
    }

}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct TabChrome {
    minimized: bool,
    maximized: bool,
    frame: Option<Frame>,
    /// Snapped to half or a quarter of the stage; `frame` follows the stage size.
    snap: Option<Snap>,
    /// Floating frame to bring back when a snapped window is dragged out.
    restore: Option<Frame>,
    /// Window (tab group) this tab lives in; `None` means its own, keyed by its id.
    window: Option<String>,
    /// Extension id when this window is that extension's side panel.
    #[serde(default)]
    panel: Option<String>,
}

#[derive(Clone)]
struct Gesture {
    tab_id: String,
    kind: GestureKind,
    origin_x: f32,
    origin_y: f32,
    start: Frame,
    /// Floating frame to pop back to once a snapped or maximized window is really dragged.
    restore: Option<Frame>,
    /// Snap target under the pointer, shown as a preview and applied on release.
    snap: Option<Snap>,
    /// Other snapped windows and their frames when a resize began; the adjacent ones follow the shared border.
    partners: Vec<(String, Frame)>,
    /// Pointer in window coordinates.
    pointer: (f32, f32),
}

/// The address as shown when not editing: no scheme, no trailing slash on a bare host.
fn display_url(url: &str) -> String {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    match rest.strip_suffix('/') {
        Some(host) if !host.contains('/') => host.to_string(),
        _ => rest.to_string(),
    }
}

/// What a tab row's `layout` column holds.
#[derive(Serialize, Deserialize)]
struct SavedLayout {
    chrome: TabChrome,
    /// Index in the window stacking order; higher is in front.
    z: usize,
    /// Stage size the frames were laid out for, so snapped frames can be rescaled.
    stage: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
enum Snap {
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Maximize,
}

impl Snap {
    /// Stage-local frame for this snap, laid out with the same inset as a maximized window.
    fn frame(self, sw: f32, sh: f32) -> Frame {
        let full_w = (sw - SNAP_INSET * 2.0).max(1.0);
        let full_h = (sh - SNAP_INSET * 2.0).max(1.0);
        let half_w = ((full_w - SNAP_INSET) / 2.0).max(1.0);
        let half_h = ((full_h - SNAP_INSET) / 2.0).max(1.0);
        let right = SNAP_INSET * 2.0 + half_w;
        let bottom = SNAP_INSET * 2.0 + half_h;
        let (x, y, w, h) = match self {
            Snap::Maximize => (SNAP_INSET, SNAP_INSET, full_w, full_h),
            Snap::Left => (SNAP_INSET, SNAP_INSET, half_w, full_h),
            Snap::Right => (right, SNAP_INSET, half_w, full_h),
            Snap::TopLeft => (SNAP_INSET, SNAP_INSET, half_w, half_h),
            Snap::TopRight => (right, SNAP_INSET, half_w, half_h),
            Snap::BottomLeft => (SNAP_INSET, bottom, half_w, half_h),
            Snap::BottomRight => (right, bottom, half_w, half_h),
        };
        Frame { x, y, w, h }
    }
}

/// Pointer distance from a stage edge that arms a snap.
const SNAP_EDGE: f32 = 14.0;
/// On a side edge, this close to the top or bottom snaps to a quarter.
const SNAP_CORNER: f32 = 72.0;
const SNAP_INSET: f32 = 8.0;
/// Window edges within this distance stick to the stage or a neighbour.
const MAGNET: f32 = 12.0;
const RESTORE_DRAG: f32 = 6.0;
/// Smallest size a resize shrinks a window to.
const MIN_W: f32 = 220.0;
const MIN_H: f32 = 120.0;

/// Moves the borders `frame` shares with snapped neighbours along with it, keeping the inset gap between them.
/// Adjacency is judged on the frames as they were when the resize began; returns the dragged frame,
/// held back so no neighbour shrinks below the minimum, and each neighbour's new frame if it follows.
fn link_resize(start: Frame, frame: Frame, edges: Edges, partners: &[Frame]) -> (Frame, Vec<Option<Frame>>) {
    let touches = |a: f32, b: f32| (b - a - SNAP_INSET).abs() <= 2.0;
    let beside_y = |o: &Frame| o.y < start.y + start.h && start.y < o.y + o.h;
    let beside_x = |o: &Frame| o.x < start.x + start.w && start.x < o.x + o.w;
    let left = |o: &Frame| edges.left && beside_y(o) && touches(o.x + o.w, start.x);
    let right = |o: &Frame| edges.right && beside_y(o) && touches(start.x + start.w, o.x);
    let top = |o: &Frame| edges.top && beside_x(o) && touches(o.y + o.h, start.y);
    let bottom = |o: &Frame| edges.bottom && beside_x(o) && touches(start.y + start.h, o.y);
    let mut frame = frame;
    for o in partners {
        if left(o) {
            let x = frame.x.max(o.x + MIN_W + SNAP_INSET);
            frame.w -= x - frame.x;
            frame.x = x;
        }
        if right(o) {
            frame.w = frame.w.min(o.x + o.w - MIN_W - SNAP_INSET - frame.x);
        }
        if top(o) {
            let y = frame.y.max(o.y + MIN_H + SNAP_INSET);
            frame.h -= y - frame.y;
            frame.y = y;
        }
        if bottom(o) {
            frame.h = frame.h.min(o.y + o.h - MIN_H - SNAP_INSET - frame.y);
        }
    }
    let followers = partners
        .iter()
        .map(|o| {
            let mut f = *o;
            if left(o) {
                f.w = frame.x - SNAP_INSET - o.x;
            }
            if right(o) {
                f.x = frame.x + frame.w + SNAP_INSET;
                f.w = o.x + o.w - f.x;
            }
            if top(o) {
                f.h = frame.y - SNAP_INSET - o.y;
            }
            if bottom(o) {
                f.y = frame.y + frame.h + SNAP_INSET;
                f.h = o.y + o.h - f.y;
            }
            (left(o) || right(o) || top(o) || bottom(o)).then_some(f)
        })
        .collect();
    (frame, followers)
}

/// Fits a snapped frame to a resized stage. Borders on the stage inset stay there; inner borders keep
/// their proportional place, measured at the middle of the gap so neighbours stay one inset apart.
fn rescale_snapped(frame: Frame, ow: f32, oh: f32, sw: f32, sh: f32) -> Frame {
    let lead = |v: f32, old: f32, new: f32| {
        if v <= SNAP_INSET + 0.5 { SNAP_INSET } else { (v - SNAP_INSET / 2.0) * new / old + SNAP_INSET / 2.0 }
    };
    let trail = |v: f32, old: f32, new: f32| {
        if v >= old - SNAP_INSET - 0.5 { new - SNAP_INSET } else { (v + SNAP_INSET / 2.0) * new / old - SNAP_INSET / 2.0 }
    };
    let (x, y) = (lead(frame.x, ow, sw), lead(frame.y, oh, sh));
    let (r, b) = (trail(frame.x + frame.w, ow, sw), trail(frame.y + frame.h, oh, sh));
    Frame { x, y, w: (r - x).max(1.0), h: (b - y).max(1.0) }
}

/// Snap target for a dragged window with the pointer at stage-local (x, y). Past the stage counts as on the edge.
fn snap_zone(x: f32, y: f32, sw: f32, sh: f32) -> Option<Snap> {
    let left = x <= SNAP_EDGE;
    let right = x >= sw - SNAP_EDGE;
    let near_top = y <= SNAP_CORNER;
    let near_bottom = y >= sh - SNAP_CORNER;
    match (left, right) {
        (true, _) if near_top => Some(Snap::TopLeft),
        (true, _) if near_bottom => Some(Snap::BottomLeft),
        (true, _) => Some(Snap::Left),
        (_, true) if near_top => Some(Snap::TopRight),
        (_, true) if near_bottom => Some(Snap::BottomRight),
        (_, true) => Some(Snap::Right),
        _ if y <= SNAP_EDGE => Some(Snap::Maximize),
        _ => None,
    }
}

/// Pulls the given edges of `frame` onto nearby stage insets or neighbouring windows' edges.
/// Moving all four edges shifts the frame; moving some resizes it.
fn magnet(frame: Frame, edges: Edges, others: &[Frame], sw: f32, sh: f32) -> Frame {
    // Neighbours only attract along an axis where they sit beside the frame.
    let beside_y = |o: &&Frame| o.y < frame.y + frame.h + MAGNET && frame.y < o.y + o.h + MAGNET;
    let beside_x = |o: &&Frame| o.x < frame.x + frame.w + MAGNET && frame.x < o.x + o.w + MAGNET;
    // Leading edges sit next to a neighbour's trailing edge, or line up with its leading edge, and vice versa.
    let lead_x = [SNAP_INSET].into_iter().chain(others.iter().filter(beside_y).flat_map(|o| [o.x + o.w + SNAP_INSET, o.x]));
    let trail_x = [sw - SNAP_INSET].into_iter().chain(others.iter().filter(beside_y).flat_map(|o| [o.x - SNAP_INSET, o.x + o.w]));
    let lead_y = [SNAP_INSET].into_iter().chain(others.iter().filter(beside_x).flat_map(|o| [o.y + o.h + SNAP_INSET, o.y]));
    let trail_y = [sh - SNAP_INSET].into_iter().chain(others.iter().filter(beside_x).flat_map(|o| [o.y - SNAP_INSET, o.y + o.h]));
    let pull = |edge: f32, targets: &mut dyn Iterator<Item = f32>| {
        targets.map(|t| t - edge).filter(|d| d.abs() <= MAGNET).min_by(|a, b| a.abs().total_cmp(&b.abs()))
    };
    let closest = |a: Option<f32>, b: Option<f32>| match (a, b) {
        (Some(a), Some(b)) => Some(if a.abs() <= b.abs() { a } else { b }),
        (a, b) => a.or(b),
    };
    let left = pull(frame.x, &mut lead_x.into_iter());
    let right = pull(frame.x + frame.w, &mut trail_x.into_iter());
    let top = pull(frame.y, &mut lead_y.into_iter());
    let bottom = pull(frame.y + frame.h, &mut trail_y.into_iter());
    let mut out = frame;
    if edges.left && edges.right {
        out.x += closest(left, right).unwrap_or(0.0);
    } else if edges.left {
        let d = left.unwrap_or(0.0);
        out.x += d;
        out.w -= d;
    } else if edges.right {
        out.w += right.unwrap_or(0.0);
    }
    if edges.top && edges.bottom {
        out.y += closest(top, bottom).unwrap_or(0.0);
    } else if edges.top {
        let d = top.unwrap_or(0.0);
        out.y += d;
        out.h -= d;
    } else if edges.bottom {
        out.h += bottom.unwrap_or(0.0);
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum GestureKind {
    Move,
    Resize(Edges),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Edges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl Edges {
    fn left() -> Self {
        Self { left: true, right: false, top: false, bottom: false }
    }
    fn right() -> Self {
        Self { left: false, right: true, top: false, bottom: false }
    }
    fn top() -> Self {
        Self { left: false, right: false, top: true, bottom: false }
    }
    fn bottom() -> Self {
        Self { left: false, right: false, top: false, bottom: true }
    }
    fn top_left() -> Self {
        Self { left: true, right: false, top: true, bottom: false }
    }
    fn top_right() -> Self {
        Self { left: false, right: true, top: true, bottom: false }
    }
    fn bottom_left() -> Self {
        Self { left: true, right: false, top: false, bottom: true }
    }
    fn bottom_right() -> Self {
        Self { left: false, right: true, top: false, bottom: true }
    }
}

const GRIP: f32 = 6.0;
const GRIP_CORNER: f32 = 12.0;

/// Which resize grip contains a point in window-local coordinates, if any.
fn grip_edges(x: f32, y: f32, w: f32, h: f32) -> Option<Edges> {
    if x < 0.0 || y < 0.0 || x > w || y > h {
        return None;
    }
    let left = x < GRIP_CORNER;
    let right = x > w - GRIP_CORNER;
    let top = y < GRIP_CORNER;
    let bottom = y > h - GRIP_CORNER;
    if (left || right) && (top || bottom) {
        return Some(Edges { left, right, top, bottom });
    }
    if x < GRIP {
        return Some(Edges::left());
    }
    if x > w - GRIP {
        return Some(Edges::right());
    }
    if y < GRIP {
        return Some(Edges::top());
    }
    if y > h - GRIP {
        return Some(Edges::bottom());
    }
    None
}

fn resize_cursor(edges: Edges) -> CursorStyle {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        // ↖↘ for the top-left and bottom-right corners; ↗↙ for the other pair.
        (true, _, true, _) | (_, true, _, true) => CursorStyle::ResizeUpLeftDownRight,
        (_, true, true, _) | (true, _, _, true) => CursorStyle::ResizeUpRightDownLeft,
        (true, _, _, _) | (_, true, _, _) => CursorStyle::ResizeLeftRight,
        _ => CursorStyle::ResizeUpDown,
    }
}

const TITLEBAR_H: f32 = 38.0;
const DOCK_ANIM: Duration = Duration::from_millis(260);
const LIFT_ANIM: Duration = Duration::from_millis(220);
const LAND_ANIM: Duration = Duration::from_millis(280);
/// Width a window shrinks to while carried over the spaces bar.
const LIFT_W: f32 = 140.0;
const PANEL_ANIM: Duration = Duration::from_millis(180);
const WINDOW_ANIM: Duration = Duration::from_millis(170);
const WINDOW_FROM_SCALE: f32 = 0.94;
const TOAST_IN: Duration = Duration::from_millis(180);
const TOAST_LIFE: Duration = Duration::from_millis(2800);
const LOAD_FADE: Duration = Duration::from_millis(300);
const ZOOM_LEVELS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
const DOCK_REST_W: f32 = 56.0;
/// At rest the pill sits in the inset gap above top-snapped windows (2px + 5px < SNAP_INSET).
const DOCK_REST_H: f32 = 5.0;
const DOCK_OPEN_H: f32 = 44.0;
const DOCK_GAP: f32 = 4.0;
const DOCK_PAD: f32 = 6.0;
/// Width of a window tile in the open dock (fits a three-icon stack), and of its new-window tile.
const DOCK_ITEM_W: f32 = 54.0;
const DOCK_NEW_W: f32 = 32.0;
const DOCK_TILE_H: f32 = 32.0;

/// The frame shrunk to `scale` around its centre.
fn scaled(frame: Frame, scale: f32) -> Frame {
    if scale >= 1.0 {
        return frame;
    }
    let (w, h) = (frame.w * scale, frame.h * scale);
    Frame { x: frame.x + (frame.w - w) / 2.0, y: frame.y + (frame.h - h) / 2.0, w, h }
}

/// Scales a frame about `point`, so whatever sat under it stays there.
fn shrink_toward(frame: Frame, point: (f32, f32), scale: f32) -> Frame {
    Frame {
        x: point.0 - (point.0 - frame.x) * scale,
        y: point.1 - (point.1 - frame.y) * scale,
        w: frame.w * scale,
        h: frame.h * scale,
    }
}

fn lerp_frame(a: Frame, b: Frame, t: f32) -> Frame {
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Frame { x: lerp(a.x, b.x), y: lerp(a.y, b.y), w: lerp(a.w, b.w), h: lerp(a.h, b.h) }
}

/// Ease-out cubic over t in 0..=1: quick start, soft landing.
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Empty title bar space that drags the app window and zooms it on double click.
fn drag_area() -> gpui::Div {
    div().h_full().on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, _| {
        if event.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

/// Whether `id` sits above every other tab in `order` (back to front) that `same` puts in its group.
fn raised_last(order: &[String], id: &str, same: impl Fn(&str) -> bool) -> bool {
    let later = match order.iter().position(|other| other == id) {
        Some(index) => &order[index + 1..],
        None => order,
    };
    !later.iter().any(|other| other != id && same(other))
}

/// Width of `favicon_stack` for `count` tabs.
fn stack_w(count: usize) -> f32 {
    20.0 + 12.0 * (count.clamp(1, 3) - 1) as f32
}

/// Up to three round favicons overlapping left to right, ringed in `ring` so each reads against the next.
fn favicon_stack(tabs: &[&browser_core::Tab], ring: gpui::Hsla) -> gpui::Div {
    let mut row = div().relative().flex_none().w(px(stack_w(tabs.len()))).h(px(20.));
    for (index, tab) in tabs.iter().take(3).enumerate() {
        row = row.child(
            div()
                .absolute()
                .top_0()
                .left(px(12. * index as f32))
                .size(px(20.))
                .rounded_full()
                .border_2()
                .border_color(ring)
                .bg(ring)
                .flex()
                .items_center()
                .justify_center()
                .child(favicon(tab, true)),
        );
    }
    row
}
const WINDOW_RADIUS: f64 = 10.0;
/// Page URL and its icon URL, newline separated.
const FAVICON_JS: &str = r#"(() => { const l = document.querySelector('link[rel~="icon" i], link[rel="apple-touch-icon"]'); return location.href + "\n" + (l ? l.href : location.origin + "/favicon.ico"); })()"#;
const TITLE_H: f64 = 36.0;
const PAGE_INSET: f64 = 6.0;
const FIND_H: f64 = 36.0;

/// Visible parts of each page, as disjoint rects: a page is hidden wherever a window in front of it sits.
/// `stack` is back to front: (tab id, window shell, page rect). Pages never show outside `stage`.
/// Also returns the slivers outside each front window's rounded corners where a back page shows,
/// as (outer corner, arc centre) points.
fn visible_pages(
    stack: &[(String, LogicalRect, LogicalRect)],
    stage: &LogicalRect,
) -> (Vec<(String, LogicalRect)>, Vec<((f64, f64), (f64, f64))>) {
    let mut holes = Vec::new();
    let mut notches = Vec::new();
    for (index, (id, _, page)) in stack.iter().enumerate() {
        let x = page.x.max(stage.x);
        let y = page.y.max(stage.y);
        let width = (page.x + page.width).min(stage.x + stage.width) - x;
        let height = (page.y + page.height).min(stage.y + stage.height) - y;
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let page = LogicalRect { x, y, width, height };
        let above = &stack[index + 1..];
        for (front, (_, shell, _)) in above.iter().enumerate() {
            let (x2, y2) = (shell.x + shell.width, shell.y + shell.height);
            let r = WINDOW_RADIUS;
            for (corner, center) in [
                ((shell.x, shell.y), (shell.x + r, shell.y + r)),
                ((x2, shell.y), (x2 - r, shell.y + r)),
                ((shell.x, y2), (shell.x + r, y2 - r)),
                ((x2, y2), (x2 - r, y2 - r)),
            ] {
                let square = LogicalRect {
                    x: corner.0.min(center.0),
                    y: corner.1.min(center.1),
                    width: r,
                    height: r,
                };
                let covered = above.iter().enumerate().any(|(other, (_, shell, _))| other != front && overlaps(&square, shell));
                if contains(&page, &square) && !covered {
                    notches.push((corner, center));
                }
            }
        }
        let mut parts = vec![page];
        for (_, shell, _) in above {
            parts = parts.iter().flat_map(|part| subtract(part, shell)).collect();
        }
        holes.extend(parts.into_iter().map(|part| (id.clone(), part)));
    }
    (holes, notches)
}

/// Removes `rect` from the page holes, giving back its rounded corners where a page lies beneath them.
fn cut_overlay(holes: &mut Vec<(String, LogicalRect)>, notches: &mut Vec<((f64, f64), (f64, f64))>, rect: &LogicalRect, radius: f64) {
    let (x2, y2) = (rect.x + rect.width, rect.y + rect.height);
    let corners = [
        ((rect.x, rect.y), (1.0, 1.0)),
        ((x2, rect.y), (-1.0, 1.0)),
        ((rect.x, y2), (1.0, -1.0)),
        ((x2, y2), (-1.0, -1.0)),
    ];
    let over_page = |(x, y): (f64, f64)| holes.iter().any(|(_, hole)| x > hole.x && y > hole.y && x < hole.x + hole.width && y < hole.y + hole.height);
    let rounded: Vec<_> = corners
        .iter()
        .filter(|&&(corner, (dx, dy))| over_page((corner.0 + dx * 0.5, corner.1 + dy * 0.5)))
        .map(|&(corner, (dx, dy))| (corner, (corner.0 + dx * radius, corner.1 + dy * radius)))
        .collect();
    *holes = std::mem::take(holes)
        .into_iter()
        .flat_map(|(id, hole)| subtract(&hole, rect).into_iter().map(move |part| (id.clone(), part)))
        .collect();
    notches.retain(|&((x, y), _)| !(x > rect.x && y > rect.y && x < x2 && y < y2));
    notches.extend(rounded);
}

fn overlaps(a: &LogicalRect, b: &LogicalRect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn contains(outer: &LogicalRect, inner: &LogicalRect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

/// `a` minus `b`, as up to four disjoint rects.
fn subtract(a: &LogicalRect, b: &LogicalRect) -> Vec<LogicalRect> {
    let (ax2, ay2) = (a.x + a.width, a.y + a.height);
    let (bx2, by2) = (b.x + b.width, b.y + b.height);
    if b.x >= ax2 || bx2 <= a.x || b.y >= ay2 || by2 <= a.y {
        return vec![a.clone()];
    }
    let top = b.y.max(a.y);
    let bottom = by2.min(ay2);
    let rect = |x: f64, y: f64, x2: f64, y2: f64| LogicalRect { x, y, width: x2 - x, height: y2 - y };
    [
        rect(a.x, a.y, ax2, top),
        rect(a.x, bottom, ax2, ay2),
        rect(a.x, top, b.x.max(a.x), bottom),
        rect(bx2.min(ax2), top, ax2, bottom),
    ]
    .into_iter()
    .filter(|part| part.width > 0.0 && part.height > 0.0)
    .collect()
}

fn mostly_off(frame: Frame, sw: f32, sh: f32) -> bool {
    let visible_w = (frame.x + frame.w).min(sw) - frame.x.max(0.0);
    let visible_h = (frame.y + frame.h).min(sh) - frame.y.max(0.0);
    visible_w < frame.w * 0.6 || visible_h < frame.h * 0.6 || frame.w > sw + 1.0 || frame.h > sh + 1.0
}

fn keep_on_stage(frame: Frame, sw: f32, sh: f32) -> Frame {
    let visible = 64.0;
    Frame {
        x: frame.x.clamp(visible - frame.w, (sw - visible).max(visible - frame.w)),
        y: frame.y.clamp(visible - frame.h, (sh - visible).max(visible - frame.h)),
        w: frame.w,
        h: frame.h,
    }
}

fn clamp_frame(frame: Frame, sw: f32, sh: f32) -> Frame {
    let min_w = 160.0_f32.min(sw.max(1.0));
    let min_h = 96.0_f32.min(sh.max(1.0));
    let w = frame.w.clamp(min_w, sw.max(min_w));
    let h = frame.h.clamp(min_h, sh.max(min_h));
    Frame {
        x: frame.x.clamp(0.0, (sw - w).max(0.0)),
        y: frame.y.clamp(0.0, (sh - h).max(0.0)),
        w,
        h,
    }
}

fn site_glyph(url: &str) -> (String, String) {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest).trim_start_matches("www.");
    let host = if host.is_empty() { "new tab" } else { host };
    let glyph = host.chars().next().unwrap_or('N').to_uppercase().to_string();
    (glyph, host.to_string())
}

/// The site's icon once cached, otherwise a coloured initial.
fn favicon(tab: &browser_core::Tab, round: bool) -> gpui::AnyElement {
    let (glyph, host) = site_glyph(&tab.url);
    let radius = if round { 8. } else { 4. };
    let tile = move || {
        div()
            .size(px(16.))
            .flex_none()
            .rounded(px(radius))
            .flex()
            .items_center()
            .justify_center()
            .bg(site_color(&host))
            .text_color(gpui::rgb(0xFFFFFF))
            .text_size(px(10.))
            .child(glyph.clone())
            .into_any_element()
    };
    let path = favicon_path(&site_glyph(&tab.url).1);
    if is_web(&tab.url) && path.exists() {
        img(path).size(px(16.)).flex_none().rounded(px(radius)).with_fallback(tile).into_any_element()
    } else {
        tile()
    }
}

fn favicon_path(host: &str) -> std::path::PathBuf {
    let name: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    app_support_dir().join("favicons").join(format!("icon-{name}"))
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub(crate) fn site_color(host: &str) -> gpui::Hsla {
    let mut hash = 0u32;
    for byte in host.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    gpui::hsla((hash % 360) as f32 / 360.0, 0.62, 0.38, 1.)
}

/// Drag payload for moving a tab to another window, or out into one of its own.
struct TabDrag(browser_core::Tab);

/// Draws nothing: the window draws the dragged pill itself, so pages are cut out beneath it.
struct TabDragPreview;

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Drag payload for reordering profiles in the rail.
struct RailDrag(browser_core::Profile);

struct RailDragPreview(browser_core::Profile, Paint);

impl Render for RailDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        profile_badge(&self.0, false, 32., self.1).opacity(0.85)
    }
}

/// Profile icon (emoji or initial) on its color; a ring marks the active profile.
pub(crate) fn profile_badge(profile: &browser_core::Profile, active: bool, size: f32, paint: Paint) -> gpui::Div {
    let custom = !profile.color.is_empty();
    let label = if profile.icon.is_empty() { rail_mark(&profile.name) } else { profile.icon.clone() };
    div()
        .size(px(size))
        .flex_none()
        .rounded(px(size / 4.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.45))
        .bg(if custom { parse_hex(&profile.color) } else if active { paint.accent } else { paint.secondary })
        .text_color(if custom { gpui::white() } else if active { paint.bg } else { paint.fg })
        .border_2()
        .border_color(if active && custom { paint.fg } else { gpui::transparent_black() })
        .child(label)
}

pub(crate) fn rail_mark(name: &str) -> String {
    let trimmed = name.trim();
    let word = trimmed.strip_prefix("Profile ").unwrap_or(trimmed);
    word.chars().next().unwrap_or('P').to_uppercase().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, width: f64, height: f64) -> LogicalRect {
        LogicalRect { x, y, width, height }
    }

    #[test]
    fn back_page_is_clipped_by_front_window() {
        let stack = vec![
            ("back".to_string(), r(0., 0., 100., 120.), r(0., 10., 100., 110.)),
            ("front".to_string(), r(60., 0., 100., 100.), r(60., 10., 100., 90.)),
        ];
        let (holes, notches) = visible_pages(&stack, &r(0., 0., 1000., 1000.));
        assert_eq!(visible_pages(&stack, &r(20., 0., 1000., 50.)).0[0].1, r(20., 10., 40., 40.));
        // Front window's bottom-left corner sits on the back page; its top corners are above it.
        assert_eq!(notches, vec![((60., 100.), (70., 90.))]);
        let back: Vec<_> = holes.iter().filter(|(id, _)| id == "back").map(|(_, rect)| rect.clone()).collect();
        assert_eq!(back, vec![r(0., 100., 100., 20.), r(0., 10., 60., 90.)]);
        assert!(holes.contains(&("front".to_string(), r(60., 10., 100., 90.))));
        let area: f64 = subtract(&r(0., 0., 10., 10.), &r(3., 3., 4., 4.)).iter().map(|p| p.width * p.height).sum();
        assert_eq!(area, 84.);
    }

    #[test]
    fn overlay_cut_rounds_only_corners_over_a_page() {
        // Page covers the right half; the card straddles its left edge.
        let mut holes = vec![("t".to_string(), r(100.0, 0.0, 100.0, 100.0))];
        let mut notches = Vec::new();
        cut_overlay(&mut holes, &mut notches, &r(50.0, 20.0, 100.0, 40.0), 8.0);
        let area: f64 = holes.iter().map(|(_, h)| h.width * h.height).sum();
        assert_eq!(area, 100.0 * 100.0 - 50.0 * 40.0);
        assert_eq!(notches, vec![((150.0, 20.0), (142.0, 28.0)), ((150.0, 60.0), (142.0, 52.0))]);
    }

    #[test]
    fn window_shows_its_most_recently_raised_tab() {
        let order = ["a", "x", "b", "y"].map(String::from);
        let group = |id: &str| id == "a" || id == "b";
        assert!(!raised_last(&order, "a", group));
        assert!(raised_last(&order, "b", group));
        assert!(raised_last(&order, "x", |id| id == "x"));
        // A tab not yet stacked only shows when nothing else in its window is.
        assert!(!raised_last(&order, "c", |id| id == "c" || id == "a"));
        assert!(raised_last(&order, "c", |id| id == "c"));
    }

    #[test]
    fn shrinking_keeps_the_grab_point_under_the_pointer() {
        let frame = Frame { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
        let small = shrink_toward(frame, (300.0, 60.0), 0.25);
        assert_eq!((small.w, small.h), (200.0, 150.0));
        // The grab point was a quarter across and 10px down; at a quarter scale it is 2.5px down.
        assert_eq!((small.x + small.w * 0.25, small.y + 2.5), (300.0, 60.0));
        let back = lerp_frame(small, frame, 1.0);
        assert_eq!((back.x, back.y, back.w, back.h), (frame.x, frame.y, frame.w, frame.h));
    }

    #[test]
    fn top_edge_is_a_resize_grip() {
        assert_eq!(grip_edges(80., 2., 400., 300.), Some(Edges::top()));
        assert_eq!(grip_edges(2., 2., 400., 300.), Some(Edges::top_left()));
        assert_eq!(grip_edges(398., 2., 400., 300.), Some(Edges::top_right()));
        assert_eq!(grip_edges(80., 20., 400., 300.), None);
    }

    #[test]
    fn pointer_at_edges_picks_snap() {
        assert_eq!(snap_zone(500., 300., 1000., 700.), None);
        assert_eq!(snap_zone(-20., 300., 1000., 700.), Some(Snap::Left));
        assert_eq!(snap_zone(995., 300., 1000., 700.), Some(Snap::Right));
        assert_eq!(snap_zone(500., 2., 1000., 700.), Some(Snap::Maximize));
        assert_eq!(snap_zone(500., 698., 1000., 700.), None);
        assert_eq!(snap_zone(2., 10., 1000., 700.), Some(Snap::TopLeft));
        assert_eq!(snap_zone(999., 690., 1000., 700.), Some(Snap::BottomRight));
    }

    #[test]
    fn snapped_halves_tile_the_stage() {
        let (l, r) = (Snap::Left.frame(1000., 700.), Snap::Right.frame(1000., 700.));
        assert_eq!((l.x, l.y, l.w, l.h), (8., 8., 488., 684.));
        assert_eq!((r.x, r.w), (504., 488.));
        assert_eq!(r.x + r.w, 1000. - 8.);
        let q = Snap::BottomRight.frame(1000., 700.);
        assert_eq!((q.x, q.y, q.w, q.h), (504., 354., 488., 338.));
    }

    #[test]
    fn display_url_drops_scheme_and_bare_trailing_slash() {
        assert_eq!(display_url("https://music.youtube.com/"), "music.youtube.com");
        assert_eq!(display_url("https://www.youtube.com/watch?v=x"), "youtube.com/watch?v=x");
        assert_eq!(display_url("http://example.com/a/"), "example.com/a/");
    }

    #[test]
    fn shared_border_resizes_both_snapped_windows() {
        let (l, r) = (Snap::Left.frame(1000., 700.), Snap::Right.frame(1000., 700.));
        let mut dragged = l;
        dragged.w += 100.;
        let (frame, followers) = link_resize(l, dragged, Edges::right(), &[r]);
        assert_eq!(frame.w, 588.);
        let f = followers[0].unwrap();
        assert_eq!((f.x, f.w, f.x + f.w), (604., 388., 992.));
        // The neighbour never shrinks below the minimum width.
        dragged.w = 900.;
        let (frame, followers) = link_resize(l, dragged, Edges::right(), &[r]);
        assert_eq!(followers[0].unwrap().w, MIN_W);
        assert_eq!(frame.x + frame.w + SNAP_INSET, followers[0].unwrap().x);
        // A quarter below shares the bottom border only; the far border of a half is not shared.
        let (tl, bl) = (Snap::TopLeft.frame(1000., 700.), Snap::BottomLeft.frame(1000., 700.));
        let (_, followers) = link_resize(tl, Frame { h: tl.h + 50., ..tl }, Edges::bottom(), &[bl, r]);
        assert_eq!(followers[0].map(|f| (f.y, f.h)), Some((bl.y + 50., bl.h - 50.)));
        assert!(followers[1].is_none());
    }

    #[test]
    fn stage_resize_keeps_snapped_splits() {
        for snap in [Snap::Left, Snap::Right, Snap::TopRight, Snap::BottomLeft] {
            let f = rescale_snapped(snap.frame(1000., 700.), 1000., 700., 1400., 900.);
            let want = snap.frame(1400., 900.);
            assert!((f.x - want.x).abs() < 0.01 && (f.w - want.w).abs() < 0.01 && (f.y - want.y).abs() < 0.01 && (f.h - want.h).abs() < 0.01, "{snap:?}");
        }
        // A dragged split at 60% stays at 60%, one inset gap apart.
        let l = Frame { x: 8., y: 8., w: 592., h: 684. };
        let r = Frame { x: 608., y: 8., w: 384., h: 684. };
        let (l, r) = (rescale_snapped(l, 1000., 700., 2000., 700.), rescale_snapped(r, 1000., 700., 2000., 700.));
        assert_eq!((l.x + l.w, r.x, r.x + r.w), (1204., 1212., 1992.));
    }

    #[test]
    fn edges_stick_to_stage_and_neighbours() {
        let all = Edges { left: true, right: true, top: true, bottom: true };
        // Near the left inset: pulled onto it, size kept.
        let f = magnet(Frame { x: 15., y: 200., w: 300., h: 200. }, all, &[], 1000., 700.);
        assert_eq!((f.x, f.y, f.w, f.h), (8., 200., 300., 200.));
        // Next to a neighbour: sits one inset away from its right edge.
        let other = Frame { x: 100., y: 100., w: 300., h: 300. };
        let f = magnet(Frame { x: 412., y: 150., w: 200., h: 200. }, all, &[other], 1000., 700.);
        assert_eq!(f.x, 408.);
        // A neighbour far below does not attract sideways.
        let low = Frame { x: 100., y: 600., w: 300., h: 50. };
        assert_eq!(magnet(Frame { x: 412., y: 100., w: 200., h: 200. }, all, &[low], 1000., 700.).x, 412.);
        // Resizing the right edge only moves that edge.
        let f = magnet(Frame { x: 300., y: 200., w: 685., h: 200. }, Edges::right(), &[], 1000., 700.);
        assert_eq!((f.x, f.w), (300., 692.));
    }

    #[test]
    fn resize_cursors_match_the_edge() {
        assert_eq!(resize_cursor(Edges::left()), CursorStyle::ResizeLeftRight);
        assert_eq!(resize_cursor(Edges::right()), CursorStyle::ResizeLeftRight);
        assert_eq!(resize_cursor(Edges::top()), CursorStyle::ResizeUpDown);
        assert_eq!(resize_cursor(Edges::bottom()), CursorStyle::ResizeUpDown);
        assert_eq!(resize_cursor(Edges::top_left()), CursorStyle::ResizeUpLeftDownRight);
        assert_eq!(resize_cursor(Edges::bottom_right()), CursorStyle::ResizeUpLeftDownRight);
        assert_eq!(resize_cursor(Edges::top_right()), CursorStyle::ResizeUpRightDownLeft);
        assert_eq!(resize_cursor(Edges::bottom_left()), CursorStyle::ResizeUpRightDownLeft);
    }
}
