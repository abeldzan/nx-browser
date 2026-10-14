use crate::profiles::profile_label;
use crate::sidebar::SidebarPage;
use crate::SharedModel;
use browser_core::{
    app_support_dir, downloads_dir, normalize_url, snapshot, spawn_download, DownloadUpdate, Effect,
    ExtensionRecord, Model,
};
use browser_engine::{EngineEvent, LogicalRect, ScriptInjection, Shortcut, WebEngine};
use browser_engine_macos::{choose_open_path, choose_save_path, MacEngine};
use browser_extensions::{load_extension_dirs, BackgroundRuntime, HostFn};
use browser_storage::Storage;
use gpui::{
    canvas, div, prelude::*, px, rems, ClickEvent, Context, CursorStyle, DispatchPhase, Entity, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString,
    Subscription, Window,
};
use gpuikit::elements::button::button;
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::input::{InputState, InputStateEvent, SubmitOn};
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{ActiveTheme, Themeable};
use gpuikit::traits::control_sized::ControlSized;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct BrowserWindow {
    pub(crate) model: SharedModel,
    engine: MacEngine,
    runtime: BackgroundRuntime,
    address: Entity<InputState>,
    find_input: Entity<InputState>,
    rename_input: Entity<InputState>,
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
    status: SharedString,
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
        model.request_show_active();
        let model = Arc::new(Mutex::new(model));
        let runtime = BackgroundRuntime::new(host(Arc::clone(&model)));
        let (downloads_tx, downloads_rx) = mpsc::channel();
        let address = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let find_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let rename_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
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
        ];
        let mut view = Self {
            model,
            engine: MacEngine::new(),
            runtime,
            address,
            find_input,
            rename_input,
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
        view.reload_extensions();
        view.sync_permissions();
        let url = view
            .model
            .lock()
            .ok()
            .and_then(|model| model.active_tab().map(|tab| tab.url.clone()))
            .unwrap_or_else(|| "nxbrowser://home".into());
        view.address.update(cx, |input, cx| input.set_content_silent(url, cx));
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
            let _ = self.model.lock().expect("model").update_download(&update.id, &update.status, &update.path);
            self.status = format!("Download {}", update.status).into();
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
                    self.address.update(cx, |input, cx| input.set_content_silent(url, cx));
                }
                if !loading {
                    self.status = if title.is_empty() { "Loaded".into() } else { title.into() };
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
            Shortcut::NewTab => self.with_model(|model| { let _ = model.create_tab("nxbrowser://home"); }),
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

    fn with_model<T>(&mut self, f: impl FnOnce(&mut Model) -> T) -> T {
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

    fn sync_permissions(&mut self) {
        if let Ok(entries) = self.model.lock().expect("model").permissions_map() {
            self.engine.set_permissions(entries);
        }
    }

    fn refresh_url(&mut self, cx: &mut Context<Self>) {
        if self.address_focused {
            return;
        }
        if let Some(url) = self.model.lock().ok().and_then(|model| model.active_tab().map(|tab| tab.url.clone())) {
            self.address.update(cx, |input, cx| input.set_content_silent(url, cx));
        }
    }

    fn commit_navigation(&mut self, raw: &str, cx: &mut Context<Self>) {
        let url = normalize_url(raw);
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
        let path = downloads_dir().join(name);
        let _ = self.model.lock().expect("model").add_download(&id, url, &path.display().to_string());
        spawn_download(id, url.to_string(), path, self.downloads_tx.clone());
        self.status = "Download started".into();
    }

    fn export_profile(&mut self) {
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

    fn import_profile(&mut self) {
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
struct Paint {
    bg: gpui::Hsla,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    surface: gpui::Hsla,
    secondary: gpui::Hsla,
    border: gpui::Hsla,
    accent: gpui::Hsla,
    accent_bg: gpui::Hsla,
}

fn paint(cx: &gpui::App) -> Paint {
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
        self.request_address_focus(window, cx);
        let _subscriptions = &self.subscriptions;
        let entity = cx.entity();
        let tabs = self.model.lock().expect("model").session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let profiles = self.model.lock().expect("model").profiles.clone();
        let active_profile = self.model.lock().expect("model").active_profile_id.clone();
        let sessions = self.model.lock().expect("model").profile_sessions().into_iter().cloned().collect::<Vec<_>>();
        let history = self.model.lock().expect("model").history().unwrap_or_default();
        let bookmarks = self.model.lock().expect("model").bookmarks().unwrap_or_default();
        let downloads = self.model.lock().expect("model").downloads().unwrap_or_default();
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
        h_stack()
            .size_full()
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(px(13.))
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
                            &downloads,
                            &extensions,
                            cx,
                        ))
                    })
                    .child(self.dock(&tabs, cx).absolute().bottom(px(0.)).left(px(0.)).right(px(0.))),
            )
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
        for profile in profiles {
            let id = profile.id.clone();
            let active = profile.id == active_profile;
            let initial = rail_mark(&profile.name);
            profile_list = profile_list.child(
                div()
                    .id(SharedString::from(format!("rail-{id}")))
                    .size(px(32.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(if active { paint.accent } else { paint.secondary })
                    .text_color(if active { paint.bg } else { paint.fg })
                    .child(initial)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_profile(&id); });
                        this.refresh_url(cx);
                        this.confirm_delete = false;
                    })),
            );
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
                let count = this.model.lock().map(|model| model.profiles.len()).unwrap_or(1) + 1;
                this.with_model(|model| { let _ = model.create_profile(&format!("Profile {count}")); });
                this.refresh_url(cx);
            })))
            .child(icon_button("download-page", Icons::download()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let url = this.address.read(cx).content().to_string();
                if url.starts_with("http://") || url.starts_with("https://") {
                    this.start_download(&url);
                    this.panel = Some(SidebarPage::Downloads);
                } else {
                    this.status = "Only http and https links can be downloaded".into();
                }
                cx.notify();
            })))
            .child(self.rail_icon("downloads-list", Icons::archive(), SidebarPage::Downloads, cx))
            .child(icon_button("more-actions", Icons::dots_vertical()).small().selected(self.panel == Some(SidebarPage::Profiles)).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.toggle_panel(SidebarPage::Profiles, cx);
            })))
    }

    fn rail_icon(
        &self,
        id: &'static str,
        icon: gpui::Svg,
        page: SidebarPage,
        cx: &mut Context<Self>,
    ) -> gpuikit::elements::icon_button::IconButton {
        icon_button(id, icon).small().selected(self.panel == Some(page)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.toggle_panel(page, cx);
        }))
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
        let mut bounds = bounds;
        bounds.size.height = px((f64::from(bounds.size.height) - DOCK_H).max(1.0) as f32);
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
        self.engine.set_page_holes(&visible_pages(&stack, &stage));
    }

    fn begin_move(&mut self, id: &str, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.suppress_window_drag {
            self.suppress_window_drag = false;
            return;
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
                .rounded(px(10.))
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
            .cursor(resize_cursor(edges))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.begin_resize(&id, edges, event.position, cx);
            }));
        let corner = (edges.left || edges.right) && (edges.top || edges.bottom);
        let band = px(6.);
        let corner_size = px(12.);
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

    fn dock(&self, tabs: &[browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let mut icons = h_stack().items_center().gap(px(6.));
        for tab in tabs {
            icons = icons.child(self.dock_icon(tab, cx));
        }
        icons = icons
            .child(div().w(px(1.)).h(px(16.)).bg(paint.border))
            .child(self.tool_button("new-tab".to_string(), Icons::plus(), false, cx, |this, cx| {
                this.with_model(|model| { let _ = model.create_tab("nxbrowser://home"); });
                this.refresh_url(cx);
            }));
        h_stack()
            .h(px(46.))
            .w_full()
            .px(px(10.))
            .items_center()
            .child(div().w(px(112.)))
            .child(div().flex_1().flex().justify_center().child(
                h_stack()
                    .items_center()
                    .px(px(6.))
                    .py(px(4.))
                    .gap(px(6.))
                    .rounded(px(12.))
                    .bg(paint.surface)
                    .border_1()
                    .border_color(paint.border)
                    .shadow_sm()
                    .child(icons),
            ))
            .child(
                h_stack()
                    .w(px(112.))
                    .justify_end()
                    .items_center()
                    .gap(px(2.))
                    .child(self.panel_icon("find-toggle", Icons::magnifying_glass(), None, cx))
                    .child(self.panel_icon("history-toggle", Icons::file_text(), Some(SidebarPage::History), cx))
                    .child(self.panel_icon("bookmarks-toggle", Icons::bookmark(), Some(SidebarPage::Bookmarks), cx))
                    .child(self.panel_icon("extensions-toggle", Icons::mixer_horizontal(), Some(SidebarPage::Extensions), cx)),
            )
    }

    fn dock_icon(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let id = tab.id.clone();
        let (glyph, host) = site_glyph(&tab.url);
        let color = site_color(&host);
        let side = if tab.active { px(28.) } else { px(24.) };
        v_stack()
            .items_center()
            .gap(px(2.))
            .child(
                div()
                    .id(SharedString::from(format!("dock-{id}")))
                    .size(side)
                    .rounded(px(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(color)
                    .text_color(gpui::rgb(0xFFFFFF))
                    .text_size(px(12.))
                    .child(glyph)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.chrome.entry(id.clone()).or_default().minimized = false;
                        this.focus_window(&id, cx);
                    })),
            )
            .child(
                div()
                    .size(px(3.))
                    .rounded(px(2.))
                    .bg(if tab.active { paint.fg } else { gpui::hsla(0., 0., 0., 0.) }),
            )
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
        downloads: &[browser_core::DownloadRecord],
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
                        downloads,
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
            .child(button("find-next", "Next").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let query = this.find_input.read(cx).content().to_string();
                if let Some(id) = this.active_tab_id() {
                    this.engine.find(&id, &query);
                }
            })))
            .child(button("find-close", "Close").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.find_open = false;
                cx.notify();
            })))
    }

    fn library_page(
        &self,
        page: SidebarPage,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        sessions: &[browser_core::Session],
        history: &[browser_core::HistoryEntry],
        bookmarks: &[browser_core::Bookmark],
        downloads: &[browser_core::DownloadRecord],
        extensions: &[browser_core::ExtensionRecord],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        match page {
            SidebarPage::Profiles => self.profile_page(profiles, active_profile, sessions, cx),
            SidebarPage::History => self.link_list("No history yet", history.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
            SidebarPage::Bookmarks => self.link_list("No bookmarks yet", bookmarks.iter().map(|entry| (entry.title.clone(), entry.url.clone())).collect(), cx),
            SidebarPage::Extensions => self.extension_page(extensions, cx),
            SidebarPage::Downloads => self.download_page(downloads, cx),
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
            .child(button("new-profile", "New profile").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let count = this.model.lock().map(|model| model.profiles.len()).unwrap_or(1) + 1;
                this.with_model(|model| { let _ = model.create_profile(&format!("Profile {count}")); });
                this.refresh_url(cx);
            })))
            .child(button("rename-profile", if self.renaming { "Save name" } else { "Rename" }).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
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
            })));
        if self.renaming {
            column = column.child(
                text_field(&self.rename_input, cx)
                    .placeholder("Profile name")
                    .full_width(true),
            );
        }
        let delete_label = if self.confirm_delete { "Confirm delete" } else { "Delete profile" };
        let mut delete = button("delete-profile", delete_label).small();
        if self.confirm_delete {
            delete = delete.destructive();
        }
        column = column.child(delete.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            if !this.confirm_delete {
                this.confirm_delete = true;
                cx.notify();
                return;
            }
            let id = this.model.lock().expect("model").active_profile_id.clone();
            let store = this.with_model(|model| model.delete_profile(&id).ok().flatten());
            if let Some(store) = store {
                this.engine.remove_data_store(&store);
            }
            this.confirm_delete = false;
            this.refresh_url(cx);
        })));
        column = column.child(self.section_label("Sessions", paint));
        for session in sessions {
            let id = session.id.clone();
            column = column.child(
                button(format!("session-{id}"), session.name.clone())
                    .small()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_session(&id); });
                        this.refresh_url(cx);
                    })),
            );
        }
        column
            .child(button("new-session", "New session").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.with_model(|model| { let _ = model.create_session(); });
                this.refresh_url(cx);
            })))
            .child(button("export-profile", "Export profile").small().on_click(cx.listener(|this, _: &ClickEvent, _, _| this.export_profile())))
            .child(button("import-profile", "Import profile").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.import_profile();
                this.refresh_url(cx);
            })))
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
                button(
                    format!("extension-{id}"),
                    format!("{}  {}", if enabled { "On" } else { "Off" }, extension.name),
                )
                .small()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let _ = this.model.lock().expect("model").set_extension_enabled(&id, !enabled);
                    this.reload_extensions();
                    cx.notify();
                })),
            );
        }
        column.child(button("reload-extensions", "Reload extensions").small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.reload_extensions();
            cx.notify();
        })))
    }

    fn download_page(&self, downloads: &[browser_core::DownloadRecord], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        if downloads.is_empty() {
            return div().py(px(8.)).text_color(paint.muted).child("No downloads yet");
        }
        let mut column = v_stack().gap(px(6.)).w_full();
        for entry in downloads {
            column = column.child(
                v_stack()
                    .gap(px(2.))
                    .child(entry.url.clone())
                    .child(div().text_size(px(11.)).text_color(paint.muted).child(entry.status.clone())),
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
            .child(button("allow-permission", "Allow").small().on_click({
                let origin = origin.clone();
                let kind = kind.clone();
                cx.listener(move |this, _: &ClickEvent, _, _| {
                    let _ = this.model.lock().expect("model").set_permission(&origin, &kind, "allow");
                    this.sync_permissions();
                    this.status = "Allowed for the next request".into();
                })
            }))
            .child(button("deny-permission", "Deny").small().on_click({
                let origin = origin.clone();
                let kind = kind.clone();
                cx.listener(move |this, _: &ClickEvent, _, _| {
                    let _ = this.model.lock().expect("model").set_permission(&origin, &kind, "deny");
                    this.sync_permissions();
                    this.permission = None;
                })
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

fn resize_cursor(edges: Edges) -> CursorStyle {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        (true, _, true, _) | (_, true, _, true) => CursorStyle::ResizeUpRightDownLeft,
        (_, true, true, _) | (true, _, _, true) => CursorStyle::ResizeUpLeftDownRight,
        (true, _, _, _) | (_, true, _, _) => CursorStyle::ResizeLeftRight,
        _ => CursorStyle::ResizeUpDown,
    }
}

const DOCK_H: f64 = 58.0;
const TITLE_H: f64 = 36.0;
const PAGE_INSET: f64 = 6.0;
const FIND_H: f64 = 36.0;

/// Visible parts of each page, as disjoint rects: a page is hidden wherever a window in front of it sits.
/// `stack` is back to front: (tab id, window shell, page rect). Pages never show outside `stage`.
fn visible_pages(stack: &[(String, LogicalRect, LogicalRect)], stage: &LogicalRect) -> Vec<(String, LogicalRect)> {
    let mut holes = Vec::new();
    for (index, (id, _, page)) in stack.iter().enumerate() {
        let x = page.x.max(stage.x);
        let y = page.y.max(stage.y);
        let width = (page.x + page.width).min(stage.x + stage.width) - x;
        let height = (page.y + page.height).min(stage.y + stage.height) - y;
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let mut parts = vec![LogicalRect { x, y, width, height }];
        for (_, shell, _) in &stack[index + 1..] {
            parts = parts.iter().flat_map(|part| subtract(part, shell)).collect();
        }
        holes.extend(parts.into_iter().map(|part| (id.clone(), part)));
    }
    holes
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

fn site_color(host: &str) -> gpui::Hsla {
    let mut hash = 0u32;
    for byte in host.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    gpui::hsla((hash % 360) as f32 / 360.0, 0.62, 0.38, 1.)
}

fn rail_mark(name: &str) -> String {
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
            ("back".to_string(), r(0., 0., 100., 100.), r(0., 10., 100., 90.)),
            ("front".to_string(), r(60., 0., 100., 100.), r(60., 10., 100., 90.)),
        ];
        let holes = visible_pages(&stack, &r(0., 0., 1000., 1000.));
        assert_eq!(visible_pages(&stack, &r(20., 0., 1000., 50.))[0].1, r(20., 10., 40., 40.));
        let back: Vec<_> = holes.iter().filter(|(id, _)| id == "back").map(|(_, rect)| rect.clone()).collect();
        assert_eq!(back, vec![r(0., 10., 60., 90.)]);
        assert!(holes.contains(&("front".to_string(), r(60., 10., 100., 90.))));
        let area: f64 = subtract(&r(0., 0., 10., 10.), &r(3., 3., 4., 4.)).iter().map(|p| p.width * p.height).sum();
        assert_eq!(area, 84.);
    }
}
