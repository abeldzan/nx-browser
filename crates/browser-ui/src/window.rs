use crate::profiles::profile_label;
use crate::settings::SettingsSection;
use crate::sidebar::SidebarPage;
use crate::tabs::tab_label;
use crate::SharedModel;
use browser_core::{
    app_support_dir, downloads_dir, normalize_url, snapshot, spawn_download, spawn_favicon, DownloadUpdate, Effect,
    ExtensionRecord, Model,
};
use browser_engine::{EngineEvent, EngineSettings, LogicalRect, ScriptInjection, Shortcut, WebEngine};
use browser_engine_macos::{choose_open_path, choose_save_path, MacEngine};
use browser_extensions::{load_extension_dirs, BackgroundRuntime, HostFn};
use browser_storage::Storage;
use gpui::{
    canvas, div, img, prelude::*, px, rems, ClickEvent, Context, CursorStyle, DispatchPhase, Entity, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString,
    Subscription, Window,
};
use crate::ui::{action_button, Kind};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::input::{InputState, InputStateEvent, SubmitOn};
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{parse_hex, ActiveTheme, GlobalTheme, Theme, ThemeVariant, Themeable};
use gpuikit::traits::control_sized::ControlSized;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct BrowserWindow {
    pub(crate) model: SharedModel,
    pub(crate) engine: MacEngine,
    runtime: BackgroundRuntime,
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
    /// Icon and color picked in the new-profile modal while it is open.
    pub(crate) new_profile: Option<(String, String)>,
    pub(crate) new_profile_input: Entity<InputState>,
    /// Live (received, total) bytes per download id, fed by the download threads.
    pub(crate) download_progress: HashMap<String, (u64, Option<u64>)>,
    pub(crate) rail_menu: Option<(String, usize, gpui::Point<Pixels>)>,
    address_focused: bool,
    focus_address: bool,
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
}

struct SelfTest {
    stage: u8,
    tab_a: String,
    tab_b: String,
    value_a: Option<String>,
    value_b: Option<String>,
    sum: Option<String>,
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
        let runtime = BackgroundRuntime::new(host(Arc::clone(&model)));
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
                InputStateEvent::Blur => this.address_focused = false,
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
            cx.subscribe(&name_input, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_profile_name(cx),
                _ => {}
            }),
        ];
        let mut view = Self {
            model,
            engine: MacEngine::new(),
            runtime,
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
            new_profile: None,
            new_profile_input,
            download_progress: HashMap::new(),
            address_focused: false,
            focus_address: false,
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
                started: Instant::now(),
            }),
        };
        view.apply_settings();
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
        if let Some(id) = self.engine.take_page_click() {
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
        self.run_self_test();
        cx.notify();
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
        for tab_id in suspend {
            let _ = self.model.lock().expect("model").mark_suspended(&tab_id);
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
                if !loading {
                    self.status = if title.is_empty() { "Loaded".into() } else { title.into() };
                    let (_, host) = site_glyph(&url);
                    if self.self_test.is_none() && is_web(&url) && !favicon_path(&host).exists() {
                        self.engine.eval(&tab_id, "favicon", FAVICON_JS);
                    }
                }
            }
            EngineEvent::Failed { message, .. } => self.status = message.into(),
            EngineEvent::ScriptMessage { tab_id, body } => self.on_script_message(&tab_id, &body),
            EngineEvent::Download { url, .. } => self.start_download(&url),
            EngineEvent::Permission { origin, kind, .. } => {
                self.permission = Some((origin.clone(), kind.clone()));
                self.status = format!("Permission request: {kind} for {origin}").into();
            }
            EngineEvent::OpenInNewTab { url } => {
                self.with_model(|model| {
                    let _ = model.create_tab(&url);
                });
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
                        _ => {}
                    }
                }
            }
            EngineEvent::Shortcut(shortcut) => self.on_shortcut(shortcut, cx),
        }
    }

    fn on_shortcut(&mut self, shortcut: Shortcut, cx: &mut Context<Self>) {
        match shortcut {
            Shortcut::NewTab => self.with_model(|model| { let _ = model.create_tab(&model.settings.new_tab_url()); }),
            Shortcut::CloseTab => {
                let id = self.model.lock().expect("model").active_tab().map(|tab| tab.id.clone());
                if let Some(id) = id {
                    self.with_model(|model| { let _ = model.close_tab(&id); });
                }
            }
            Shortcut::FocusUrl => {
                self.focus_address = true;
                self.status = "Address bar".into();
            }
            Shortcut::Reload => self.reload(),
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::Find => self.find_open = true,
            Shortcut::Bookmark => self.bookmark(),
            Shortcut::Settings => self.toggle_settings(cx),
        }
        self.refresh_url(cx);
    }

    fn on_script_message(&mut self, tab_id: &str, body: &str) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(body) else {
            return;
        };
        let extension_id = payload.get("extensionId").and_then(|value| value.as_str()).unwrap_or("").to_string();
        let method = payload.get("method").and_then(|value| value.as_str()).unwrap_or("").to_string();
        let id = payload.get("id").and_then(|value| value.as_u64()).unwrap_or(0);
        let args = payload.get("args").cloned().unwrap_or(serde_json::json!({}));
        let reply = self.with_model(|model| model.extension_call(&extension_id, &method, &args));
        let javascript = format!(
            "globalThis.__nxResolve({id}, {}, {});",
            reply["ok"], reply["value"]
        );
        self.engine.resolve_extension(tab_id, &extension_id, &javascript);
        if method == "runtime.sendMessage" {
            self.runtime.dispatch(&extension_id, &reply["value"].to_string());
        }
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
        let scripts = self.script_list();
        for effect in effects {
            match effect {
                Effect::EnsureTab { tab_id, data_store_id, url } => {
                    let _ = self.engine.ensure_tab(&tab_id, &data_store_id, &url, &scripts);
                }
                Effect::CloseTab { tab_id } | Effect::SuspendTab { tab_id } => self.engine.close_tab(&tab_id),
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
        let scripts = self.script_list();
        for tab in tabs {
            let entry = self.chrome.get(&tab.id);
            let minimized = entry.is_some_and(|chrome| chrome.minimized);
            let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
            if minimized || covered {
                continue;
            }
            let _ = self.engine.ensure_tab(&tab.id, &store, &tab.url, &scripts);
        }
    }

    fn script_list(&self) -> Vec<ScriptInjection> {
        self.model
            .lock()
            .expect("model")
            .extensions
            .iter()
            .filter(|extension| extension.enabled && !extension.content_script.is_empty())
            .map(|extension| ScriptInjection {
                world: extension.id.clone(),
                source: extension.content_script.clone(),
            })
            .collect()
    }

    fn reload_extensions(&mut self) {
        let mut dirs = vec![
            std::env::current_dir().unwrap_or_default().join("extensions/examples"),
            app_support_dir().join("extensions"),
        ];
        if let Some(parent) = std::env::current_exe().ok().and_then(|path| path.parent().map(|path| path.to_path_buf())) {
            dirs.push(parent.join("../../../extensions/examples"));
        }
        let loaded = load_extension_dirs(&dirs);
        let records = loaded
            .iter()
            .map(|extension| ExtensionRecord {
                id: extension.id.clone(),
                name: extension.name.clone(),
                path: extension.path.clone(),
                enabled: true,
                permissions: extension.permissions.clone(),
                host_permissions: extension.host_permissions.clone(),
                matches: extension.matches.clone(),
                content_script: extension.content_script.clone(),
                background: extension.background.clone(),
            })
            .collect();
        if let Ok(mut model) = self.model.lock() {
            let _ = model.replace_extensions(records);
        }
        let extensions = self.model.lock().expect("model").extensions.clone();
        for extension in &extensions {
            self.runtime.stop(&extension.id);
        }
        for extension in extensions {
            if extension.enabled {
                if let Some(script) = extension.background {
                    self.runtime.start(&extension.id, &script);
                }
            }
        }
        let tab_ids: Vec<String> = self.model.lock().expect("model").session_tabs().into_iter().map(|tab| tab.id.clone()).collect();
        let scripts = self.script_list();
        for tab_id in tab_ids {
            self.engine.set_scripts(&tab_id, &scripts);
        }
    }

    pub(crate) fn apply_settings(&mut self) {
        let settings = self.model.lock().expect("model").settings.clone();
        self.engine.set_settings(EngineSettings {
            javascript: settings.javascript,
            block_popups: settings.block_popups,
            fraud_warnings: settings.fraud_warnings,
            https_only: settings.https_only,
            inspectable: settings.inspector,
            user_agent: settings.user_agent,
            page_zoom: settings.page_zoom,
        });
    }

    fn apply_theme(&self, window: &Window, cx: &mut Context<Self>) {
        let theme = self.model.lock().expect("model").settings.theme.clone();
        let dark = match theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark),
        };
        if (cx.theme().variant == ThemeVariant::Dark) != dark {
            let next = if dark { Theme::default() } else { Theme::gruvbox_light() };
            cx.set_global(GlobalTheme(Arc::new(next)));
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

    fn active_tab_id(&self) -> Option<String> {
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
            let _ = self.engine.ensure_tab(&tab_a, &store_a, "nxbrowser://home", &[]);
            let _ = self.engine.ensure_tab(&tab_b, &store_b, "nxbrowser://home", &[]);
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
            ))
        });
        if let Some((a, b, sum, tab_a)) = outcome {
            let frame = self.engine.frame(&tab_a);
            let sample = snapshot(self.started);
            let isolated = a.contains('A') && b.contains('B') && !a.contains('B');
            let math = sum.contains('2');
            let sized = frame.map(|(width, _)| width > 100.0).unwrap_or(false);
            println!(
                "self-test localStorage A={a:?} B={b:?} js={sum:?} frame={frame:?} startup_ms={} rss={}",
                sample.startup.as_millis(),
                sample.rss_bytes
            );
            if isolated && math && sized {
                println!("SELFTEST OK");
                std::process::exit(0);
            }
            eprintln!("SELFTEST FAIL");
            std::process::exit(1);
        }
    }


    fn request_address_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_address {
            return;
        }
        self.focus_address = false;
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
        self.apply_theme(window, cx);
        self.request_address_focus(window, cx);
        let _subscriptions = &self.subscriptions;
        let entity = cx.entity();
        let tabs = self.model.lock().expect("model").session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let profiles = self.model.lock().expect("model").profiles.clone();
        let active_profile = self.model.lock().expect("model").active_profile_id.clone();
        let sessions = self.model.lock().expect("model").profile_sessions().into_iter().cloned().collect::<Vec<_>>();
        let history = self.model.lock().expect("model").history().unwrap_or_default();
        let bookmarks = self.model.lock().expect("model").bookmarks().unwrap_or_default();
        let extensions = self.model.lock().expect("model").extensions.clone();
        let colors = paint(cx);
        let live_ids = tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        self.chrome.retain(|id, _| live_ids.iter().any(|live| live == id));
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        self.prepare_frames(&tabs);
        if self.self_test.is_none() {
            for tab in &tabs {
                let entry = self.chrome.get(&tab.id);
                let minimized = entry.is_some_and(|chrome| chrome.minimized);
                let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
                if minimized || covered {
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
            .child(self.titlebar(&tabs, cx))
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
                    .when_some(self.panel, |column, page| {
                        column.child(self.panel_view(
                            page,
                            &profiles,
                            &active_profile,
                            &sessions,
                            &history,
                            &bookmarks,
                            &extensions,
                            cx,
                        ))
                    }),
            ),
            )
            .when(self.settings_open, |root| root.child(self.settings_modal(cx)))
            .when(self.new_profile.is_some(), |root| root.child(self.new_profile_modal(cx)))
            .when_some(self.delete_prompt.clone(), |root, id| root.child(self.delete_modal(&id, cx)))
            .when(self.downloads_open, |root| root.child(self.downloads_popover(cx)))
            .when_some(self.rail_menu.clone(), |root, menu| root.child(self.rail_menu_view(menu, cx)))
    }
}

impl BrowserWindow {
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
        let key = (sw.round() as i32, sh.round() as i32);
        if self.layout_stage != Some(key) {
            self.layout_stage = Some(key);
            if self.gesture.is_none() {
                for entry in self.chrome.values_mut() {
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
            let col = (index % cols) as f32;
            let row = (index / cols) as f32;
            entry.frame = Some(Frame {
                x: pad + col * (cell_w + gap),
                y: pad + row * (cell_h + gap),
                w: cell_w.max(160.0),
                h: cell_h.max(120.0),
            });
        }
        let live = tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        self.order.retain(|id| live.iter().any(|tab| tab == id));
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
            entity.update(cx, |view, cx| {
                view.gesture = None;
                cx.notify();
            });
        });
    }

    fn apply_gesture(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(gesture) = self.gesture.clone() else {
            return;
        };
        let Some(bounds) = self.stage_bounds else {
            return;
        };
        let dx = f64::from(position.x) as f32 - gesture.origin_x;
        let dy = f64::from(position.y) as f32 - gesture.origin_y;
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
        let sw = f64::from(bounds.size.width) as f32;
        let sh = f64::from(bounds.size.height) as f32;
        if matches!(gesture.kind, GestureKind::Resize(_)) {
            frame.w = frame.w.max(220.0_f32.min(gesture.start.w));
            frame.h = frame.h.max(120.0_f32.min(gesture.start.h));
        }
        frame = keep_on_stage(frame, sw, sh);
        if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
            entry.maximized = false;
            entry.frame = Some(frame);
        }
        // Native views follow in the canvas paint, in the same frame as the GPUI chrome.
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
        let mut stack = Vec::new();
        for id in ids {
            let Some(entry) = self.chrome.get(&id).cloned() else {
                continue;
            };
            if entry.minimized {
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
            let find = if active.as_deref() == Some(id.as_str()) && self.find_open { FIND_H } else { 0.0 };
            let rect = LogicalRect {
                x: origin_x + f64::from(frame.x) + PAGE_INSET,
                y: origin_y + f64::from(frame.y) + TITLE_H + find,
                width: (f64::from(frame.w) - PAGE_INSET * 2.0).max(1.0),
                height: (f64::from(frame.h) - TITLE_H - find - PAGE_INSET).max(1.0),
            };
            self.engine.place_tab(&id, Some(rect.clone()));
            let shell = LogicalRect {
                x: origin_x + f64::from(frame.x),
                y: origin_y + f64::from(frame.y),
                width: f64::from(frame.w),
                height: f64::from(frame.h),
            };
            stack.push((id, shell, rect));
        }
        let ids = stack.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>();
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
        let Some(start) = self.visual_frame(id) else {
            return;
        };
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.maximized = false;
            entry.frame = Some(start);
        }
        self.gesture = Some(Gesture {
            tab_id: id.to_string(),
            kind: GestureKind::Move,
            origin_x: f64::from(position.x) as f32,
            origin_y: f64::from(position.y) as f32,
            start,
        });
        self.engine.focus_chrome();
        cx.notify();
    }

    fn begin_resize(&mut self, id: &str, edges: Edges, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.focus_window(id, cx);
        let Some(start) = self.visual_frame(id) else {
            return;
        };
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.maximized = false;
            entry.frame = Some(start);
        }
        self.gesture = Some(Gesture {
            tab_id: id.to_string(),
            kind: GestureKind::Resize(edges),
            origin_x: f64::from(position.x) as f32,
            origin_y: f64::from(position.y) as f32,
            start,
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
        for tab in ordered {
            if let Some(window) = self.floating_window(tab, cx) {
                stage = stage.child(window);
            }
        }
        stage
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

    fn focus_window(&mut self, id: &str, cx: &mut Context<Self>) {
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
                !minimized && !covered
            })
            .collect()
    }

    fn floating_window(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let paint = paint(cx);
        let active = tab.active;
        let maximized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized);
        let frame = self.chrome.get(&tab.id).and_then(|chrome| chrome.frame)?;
        let mut shell = div().absolute().overflow_hidden();
        shell = if maximized {
            shell.left(px(8.)).top(px(8.)).right(px(8.)).bottom(px(8.))
        } else {
            shell.left(px(frame.x)).top(px(frame.y)).w(px(frame.w)).h(px(frame.h))
        };
        let mut body = div().relative().size_full().child(
            v_stack()
                .size_full()
                .rounded(px(WINDOW_RADIUS as f32))
                .border_1()
                .border_color(if active { paint.accent } else { paint.border })
                .shadow_md()
                .bg(paint.secondary)
                .overflow_hidden()
                .child(self.window_titlebar(tab, cx))
                .when(active && self.find_open, |column| column.child(self.find_bar(cx)))
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

    fn window_titlebar(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let id = tab.id.clone();
        let nav = self.nav_controls(&tab.id, cx);
        let tools = self.window_tools(&tab.id, cx);
        let address = self.hit_guard(format!("addr-guard-{}", tab.id), self.address_slot(tab, cx), cx);
        div().child(
            h_stack()
                .id(SharedString::from(format!("title-{id}")))
                .h(px(TITLE_H as f32))
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
                .child(self.hit_guard(format!("nav-guard-{}", tab.id), nav, cx))
                .child(
                    div().flex_1().h_full().px(px(28.)).flex().justify_center().items_center().child(
                        div().w(px(240.)).max_w_full().child(address),
                    ),
                )
                .child(self.hit_guard(format!("tools-guard-{}", tab.id), tools, cx)),
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
        if tab.active {
            return div().w_full().child(
                text_field(&self.address, cx)
                    .placeholder("Search or enter an address")
                    .full_width(true),
            );
        }
        let id = tab.id.clone();
        let label = if tab.url.is_empty() || tab.url == "nxbrowser://home" {
            "New tab".to_string()
        } else {
            tab.url.clone()
        };
        div().w_full().child(
            div()
                .id(SharedString::from(format!("address-{id}")))
                .h(px(28.))
                .w_full()
                .px(px(10.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .overflow_hidden()
                .cursor_pointer()
                .bg(paint.secondary)
                .text_color(paint.muted)
                .text_size(px(12.))
                .child(label)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.focus_window(&id, cx);
                    this.focus_address = true;
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
                this.with_model(|model| { let _ = model.close_tab(&close_id); });
                this.refresh_url(cx);
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

    fn titlebar(&self, tabs: &[browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let mut strip = h_stack().min_w(px(0.)).flex_shrink(1.).overflow_hidden().items_center().gap(px(4.));
        for tab in tabs {
            strip = strip.child(self.title_tab(tab, cx));
        }
        h_stack()
            .h(px(TITLEBAR_H))
            .w_full()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .pl(px(80.))
            .pr(px(8.))
            .bg(paint.surface)
            .border_b_1()
            .border_color(paint.border)
            .child(strip)
            .child(self.tool_button("new-tab".to_string(), Icons::plus(), false, cx, |this, cx| {
                this.with_model(|model| { let _ = model.create_tab(&model.settings.new_tab_url()); });
                this.refresh_url(cx);
            }))
            .child(
                div().flex_1().h_full().on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, _| {
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                }),
            )
            .child(
                h_stack()
                    .flex_none()
                    .items_center()
                    .gap(px(2.))
                    .child(self.panel_icon("find-toggle", Icons::magnifying_glass(), None, cx))
                    .child(self.panel_icon("history-toggle", Icons::file_text(), Some(SidebarPage::History), cx))
                    .child(self.panel_icon("bookmarks-toggle", Icons::bookmark(), Some(SidebarPage::Bookmarks), cx))
                    .child(self.panel_icon("extensions-toggle", Icons::mixer_horizontal(), Some(SidebarPage::Extensions), cx)),
            )
    }

    fn title_tab(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let id = tab.id.clone();
        let close_id = tab.id.clone();
        let minimized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.minimized);
        h_stack()
            .id(SharedString::from(format!("tab-{id}")))
            .h(px(28.))
            .min_w(px(36.))
            .max_w(px(200.))
            .flex_shrink(1.)
            .pl(px(8.))
            .pr(px(2.))
            .gap(px(6.))
            .items_center()
            .rounded(px(7.))
            .cursor_pointer()
            .when(tab.active, |tab| tab.bg(paint.secondary))
            .when(minimized, |tab| tab.opacity(0.55))
            .child(favicon(tab))
            .child(div().flex_1().min_w(px(0.)).truncate().child(tab_label(&tab.title, &tab.url)))
            .child(self.tool_button(format!("tab-close-{close_id}"), Icons::cross_2(), false, cx, move |this, cx| {
                this.with_model(|model| { let _ = model.close_tab(&close_id); });
                this.refresh_url(cx);
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.chrome.entry(id.clone()).or_default().minimized = false;
                this.focus_window(&id, cx);
            }))
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
        extensions: &[browser_core::ExtensionRecord],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        div()
            .h(px(260.))
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
                        extensions,
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
        extensions: &[browser_core::ExtensionRecord],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match page {
            SidebarPage::Profiles => self.profile_page(profiles, active_profile, sessions, cx),
            SidebarPage::History => self.link_list("No history yet", history.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
            SidebarPage::Bookmarks => self.link_list("No bookmarks yet", bookmarks.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
            SidebarPage::Extensions => self.extension_page(extensions, cx),
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

    fn extension_page(&self, extensions: &[browser_core::ExtensionRecord], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let mut column = v_stack().gap(px(6.)).w_full();
        if extensions.is_empty() {
            column = column.child(div().text_color(paint.muted).child("No extensions in extensions/examples"));
        }
        for extension in extensions {
            let id = extension.id.clone();
            let enabled = extension.enabled;
            column = column.child(
                action_button(
                    SharedString::from(format!("extension-{id}")),
                    format!("{}  {}", if enabled { "On" } else { "Off" }, extension.name),
                    Kind::Secondary,
                    true,
                    cx,
                    move |this, cx| {
                        let _ = this.model.lock().expect("model").set_extension_enabled(&id, !enabled);
                        this.reload_extensions();
                        cx.notify();
                    },
                ),
            );
        }
        column.child(action_button("reload-extensions", "Reload extensions", Kind::Secondary, true, cx, |this, cx| {
            this.reload_extensions();
            cx.notify();
        }))
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

#[derive(Clone, Copy, Debug)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Default)]
struct TabChrome {
    minimized: bool,
    maximized: bool,
    frame: Option<Frame>,
}

#[derive(Clone)]
struct Gesture {
    tab_id: String,
    kind: GestureKind,
    origin_x: f32,
    origin_y: f32,
    start: Frame,
}

#[derive(Clone, Copy, PartialEq)]
enum GestureKind {
    Move,
    Resize(Edges),
}

#[derive(Clone, Copy, PartialEq)]
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
fn favicon(tab: &browser_core::Tab) -> gpui::AnyElement {
    let (glyph, host) = site_glyph(&tab.url);
    let tile = move || {
        div()
            .size(px(16.))
            .flex_none()
            .rounded(px(4.))
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
        img(path).size(px(16.)).flex_none().with_fallback(tile).into_any_element()
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

fn site_color(host: &str) -> gpui::Hsla {
    let mut hash = 0u32;
    for byte in host.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    gpui::hsla((hash % 360) as f32 / 360.0, 0.62, 0.38, 1.)
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

fn host(model: SharedModel) -> HostFn {
    Arc::new(move |payload| {
        let call: serde_json::Value = serde_json::from_str(payload).unwrap_or_else(|_| serde_json::json!({}));
        let extension_id = call.get("extensionId").and_then(|value| value.as_str()).unwrap_or("");
        let method = call.get("method").and_then(|value| value.as_str()).unwrap_or("");
        let args = call.get("args").cloned().unwrap_or(serde_json::json!({}));
        match model.lock() {
            Ok(mut model) => model.extension_call(extension_id, method, &args).to_string(),
            Err(_) => r#"{"ok":false,"error":"locked","value":null}"#.into(),
        }
    })
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
    fn top_edge_is_a_resize_grip() {
        assert_eq!(grip_edges(80., 2., 400., 300.), Some(Edges::top()));
        assert_eq!(grip_edges(2., 2., 400., 300.), Some(Edges::top_left()));
        assert_eq!(grip_edges(398., 2., 400., 300.), Some(Edges::top_right()));
        assert_eq!(grip_edges(80., 20., 400., 300.), None);
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
