mod tiling;

use crate::passwords::PasswordState;
use crate::settings::SettingsSection;
use crate::tabs::tab_label;
use crate::terminal::{self, is_terminal, TERMINAL_URL};
use crate::SharedModel;
use browser_core::{
    app_support_dir, downloads_dir, fetch_suggestions, normalize_url, snapshot, spawn_favicon, unique_path, Effect,
    Model, TabState,
};
use browser_engine::{Dir, EngineEvent, EngineSettings, Hole, LogicalRect, Notch, Shortcut, WebEngine};
use browser_engine_macos::{choose_folder_path, choose_open_path, choose_save_path, Extensions, LiveTab, MacEngine, EXTENSION_SCHEME};
use browser_extensions::{discover, install_from_store, local_id, patch_missing_apis, read_manifest, store_id, Manifest};
use gpui::{
    canvas, div, img, prelude::*, px, relative, rems, ClickEvent, Context, CursorStyle, DispatchPhase, Entity, FocusHandle,
    Focusable, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString,
    Subscription, Task, Window,
};
use crate::extensions::extension_icon;
use crate::settings::measure_overlay;
use crate::ui::{action_button, Kind};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::combobox::{combobox, ComboboxState};
use gpuikit::elements::input::input;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use tiling::{cell, move_border, neighbour, set_span, span, uncell, Side, Tile};
use gpuikit::input::{bindings, InputState, InputStateEvent, SubmitOn};
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{parse_hex, ActiveTheme, GlobalTheme, Theme, ThemeVariant, Themeable};
use futures::StreamExt;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct BrowserWindow {
    pub(crate) model: WindowModel,
    /// Space chip being dragged, so a release outside the window can open it in its own.
    space_drag: Option<String>,
    /// Held during a window drag when nothing else has GPUI focus: modifier changes only travel the focus path.
    gesture_focus: FocusHandle,
    /// The shortcut whose new keys settings is waiting for; app shortcuts are paused meanwhile.
    pub(crate) recording: Option<&'static str>,
    /// Takes the keys being recorded.
    pub(crate) shortcut_focus: FocusHandle,
    /// Each space's tiled windows, keyed by window group, as a split tree laid out over the stage.
    tiles: HashMap<String, Tile>,
    /// Holds one torn-off window instead of a space: its tabs take the spaces' place in the title bar.
    tab_window: bool,
    /// A torn-off window's last tab closed; the window goes on the next render.
    close_window: bool,
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
    pub(crate) home_input: Entity<InputState>,
    pub(crate) ua_input: Entity<InputState>,
    /// Searchable font list in the Terminal settings; built on first show since it needs a `Window`.
    pub(crate) term_font_picker: Option<Entity<ComboboxState<String>>>,
    pub(crate) term_shell_input: Entity<InputState>,
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
    /// The open title-bar or rail popover; at most one at a time.
    pub(crate) popover: Option<Popover>,
    /// The popover a press outside it just closed, and where; that press's click on its own button keeps it closed.
    popover_dismissed: Option<(Popover, gpui::Point<Pixels>)>,
    pub(crate) history_modal: bool,
    pub(crate) history_query: Entity<InputState>,
    /// The bookmark editor: (bookmark id, pinned); no id adds a new pinned site.
    pub(crate) bookmark_edit: Option<(Option<String>, bool)>,
    pub(crate) bookmark_title: Entity<InputState>,
    pub(crate) bookmark_url: Entity<InputState>,
    pub(crate) focus_history: bool,
    /// Icon and color picked in the new-profile modal while it is open.
    pub(crate) new_profile: Option<(String, String)>,
    pub(crate) new_profile_input: Entity<InputState>,
    /// Live (received, total) bytes per download id, fed by the download threads.
    pub(crate) download_progress: HashMap<String, (u64, Option<u64>)>,
    /// Right-click menu on a rail profile: (profile id, index, click position).
    pub(crate) rail_menu: Option<(String, usize, gpui::Point<Pixels>)>,
    address_focused: bool,
    focus_address: bool,
    /// The active window shows the editable field (focus requested or held); otherwise a plain pill.
    address_editing: bool,
    /// The edit was started from the new-tab page's big field, so the input is drawn there instead of the title bar.
    address_in_page: bool,
    /// A page was clicked while the address was being edited; end the edit on the next render.
    blur_address: bool,
    find_open: bool,
    focus_find: bool,
    /// Give keys to the active terminal on the next render.
    focus_terminal: bool,
    pub(crate) palette_open: bool,
    pub(crate) palette_input: Entity<InputState>,
    pub(crate) palette_index: usize,
    /// The bang shown as a pill in the palette; the input then holds only the query.
    pub(crate) palette_bang: Option<&'static (&'static str, &'static str, &'static str)>,
    pub(crate) focus_palette: bool,
    chrome: HashMap<String, TabChrome>,
    gesture: Option<Gesture>,
    suppress_window_drag: bool,
    /// Where the last tab pill was pressed: GPUI turns a 2px wobble into a drag, which must still count as a click.
    tab_press: Option<gpui::Point<Pixels>>,
    pub(crate) stage_bounds: Option<gpui::Bounds<Pixels>>,
    layout_stage: Option<(i32, i32)>,
    order: Vec<String>,
    subscriptions: Vec<Subscription>,
    pub(crate) status: SharedString,
    /// Space whose chip is showing the rename field.
    space_rename: Option<String>,
    space_input: Entity<InputState>,
    /// Camera/microphone request the page is waiting on: (request, origin, kind).
    permission: Option<(u64, String, String)>,
    pub(crate) passwords: PasswordState,
    /// Downloads WebKit is writing, by engine id: (download id in the model, file path).
    web_downloads: HashMap<u64, (String, String)>,
    attached: bool,
    /// Tabs whose page is playing sound, for the speaker beside their title.
    playing: HashSet<String>,
    /// Tabs whose page is recording: (camera, microphone).
    capturing: HashMap<String, (bool, bool)>,
    /// Tabs a page opened (popup → opener). An opener stays live while its popup is in front:
    /// suspending it would drop the page an OAuth popup reports back to.
    openers: HashMap<String, String>,
    started: Instant,
    self_test: Option<SelfTest>,
    cookies_saved: Instant,
    ticks: u32,
    /// Window dock under the title bar.
    dock: Tween,
    /// Pointer over the dock; it stays shut while a window drag is in progress.
    dock_hovered: bool,
    /// A window carried up to the spaces bar shrinks under the pointer (open = small).
    lift: Tween,
    /// Space chips laid out this frame, so a carried window can be dropped on one.
    space_chips: Vec<(String, gpui::Bounds<Pixels>)>,
    /// Rail profiles laid out this frame, so a carried window can be dropped on one.
    rail_badges: Vec<(String, gpui::Bounds<Pixels>)>,
    /// A dropped carried window growing back into place: (tab id, rect it was dropped at, started).
    landing: Option<(String, Frame, Instant)>,
    /// A window closed; re-tile the stage on the next layout pass.
    retile: bool,
    /// Tab pill being dragged and the pointer; drawn here so pages can be cut out beneath it.
    tab_drag: Option<(browser_core::Tab, gpui::Point<Pixels>)>,
    /// Where tab pills and rail badges were laid out last frame, before any slide offset.
    slots: HashMap<String, gpui::Bounds<Pixels>>,
    /// Items sliding into place after a reorder: the offset they start from, and when.
    slides: HashMap<String, (gpui::Point<Pixels>, Instant)>,
    /// Where the dragged rail badge was grabbed, so its drop starts from where its preview was drawn.
    rail_grab: Rc<Cell<gpui::Point<Pixels>>>,
    /// Where the dragged tab pill was grabbed, so it stays under the pointer at that spot.
    tab_grab: Rc<Cell<gpui::Point<Pixels>>>,
    /// A tab or profile being dragged, and the row it would drop into.
    reorder: Option<Reorder>,
    find: Tween,
    /// Status message shown as a toast, and when it changed.
    toast: (SharedString, Instant),
    /// Load bar width for the active tab, and when its load finished (the bar then fades).
    load_bar: f32,
    load_done: Option<Instant>,
    /// The active tab's load progress at the last tick; a change repaints the bar.
    load_seen: Option<f64>,
    /// Each tab's last finished URL: later reports of the same URL are title changes.
    finished_url: HashMap<String, String>,
    /// New windows growing in.
    opened: HashMap<String, Instant>,
    /// Address bar matches (url or search query, title, is a search) and the one picked with the arrow keys.
    suggestions: Vec<(String, String, bool)>,
    suggestion: Option<usize>,
    /// In-flight search-engine completion fetch; replacing it cancels the stale one.
    suggest_task: Option<Task<()>>,
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

/// A drag over reorderable rows: tab rows keyed by window, or the profile rail.
struct Reorder {
    id: String,
    size: gpui::Size<Pixels>,
    /// Where the dragged item's top-left is drawn.
    at: gpui::Point<Pixels>,
    /// The row under the dragged item and its order with a gap for it there; none off every row.
    row: Option<String>,
    order: Vec<String>,
}

/// `ids` of row `key` as drawn during the drag in `state`: with the gap where it hovers,
/// and without the dragged item in the row it left for another.
fn shown_row(state: Option<&Reorder>, key: &str, ids: &[String]) -> Vec<String> {
    match state {
        Some(state) if state.row.as_deref() == Some(key) => state.order.clone(),
        Some(state) if state.row.is_some() => ids.iter().filter(|id| **id != state.id).cloned().collect(),
        _ => ids.to_vec(),
    }
}

const RAIL_ROW: &str = "rail";

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
    pub fn new(model: SharedModel, handoff: Option<Handoff>, self_test: bool, started: Instant, cx: &mut Context<Self>) -> Self {
        let (installs_tx, installs_rx) = mpsc::channel();
        let ext_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let address = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let find_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let palette_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let space_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let home_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let ua_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let term_shell_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let name_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let new_profile_input = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let history_query = cx.new(InputState::new_singleline);
        let bookmark_title = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let bookmark_url = cx.new(|cx| InputState::new_singleline(cx).submit_on(SubmitOn::Enter));
        let mut subscriptions = vec![
            cx.subscribe(&address, |this, input, event, cx| match event {
                InputStateEvent::Submit => {
                    let raw = input.read(cx).content().to_string();
                    this.commit_navigation(&raw, cx);
                }
                // Focus and Blur are left to `sync_address_focus`: the input sends them only while drawn.
                InputStateEvent::TextChanged => {
                    let typed = input.read(cx).content().to_string();
                    this.update_suggestions(typed, cx);
                }
                _ => {}
            }),
            cx.subscribe(&palette_input, |this, _, event, cx| {
                if let InputStateEvent::TextChanged = event {
                    this.palette_index = 0;
                    this.absorb_bang(cx);
                    cx.notify();
                }
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
            cx.subscribe(&space_input, |this, input, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit | InputStateEvent::Blur => {
                    if let Some(id) = this.space_rename.take() {
                        let name = input.read(cx).content().to_string();
                        this.with_model(|model| { let _ = model.rename_session(&id, &name); });
                        cx.notify();
                    }
                }
                _ => {}
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
            cx.subscribe(&term_shell_input, |this, input, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit | InputStateEvent::Blur => {
                    let value = input.read(cx).content().trim().to_string();
                    this.set_pref(cx, |s| s.terminal_shell = value);
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
            cx.subscribe(&history_query, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::TextChanged => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&bookmark_title, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_bookmark_edit(cx),
                _ => {}
            }),
            cx.subscribe(&bookmark_url, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_bookmark_edit(cx),
                _ => {}
            }),
            cx.subscribe(&name_input, |this, _, event, cx| match event {
                InputStateEvent::Focus => this.engine.focus_chrome(),
                InputStateEvent::Submit => this.save_profile_name(cx),
                _ => {}
            }),
        ];
        let (passwords, password_subscriptions) = PasswordState::new(cx);
        subscriptions.extend(password_subscriptions);
        let (detached, tab_window, extensions, pages, chrome) = match handoff {
            Some(handoff) => (Some(handoff.session), handoff.tab_window, Some(handoff.extensions), handoff.pages, handoff.chrome),
            None => (None, false, None, Vec::new(), HashMap::new()),
        };
        let mut view = Self {
            model: WindowModel { shared: model, detached },
            space_drag: None,
            gesture_focus: cx.focus_handle(),
            recording: None,
            shortcut_focus: cx.focus_handle(),
            tiles: HashMap::new(),
            tab_window,
            close_window: false,
            engine: MacEngine::new(extensions),
            ext_input,
            ext_meta: HashMap::new(),
            ext_prompts: Vec::new(),
            installing: Vec::new(),
            installs_tx,
            installs_rx,
            ext_active: None,
            address,
            find_input,
            home_input,
            ua_input,
            term_font_picker: None,
            term_shell_input,
            name_input,
            settings_open: false,
            settings_section: SettingsSection::Profiles,
            settings_note: None,
            settings_profile: None,
            delete_prompt: None,
            overlay_rects: Vec::new(),
            rail_menu: None,
            popover: None,
            popover_dismissed: None,
            history_modal: false,
            history_query,
            bookmark_edit: None,
            bookmark_title,
            bookmark_url,
            focus_history: false,
            new_profile: None,
            new_profile_input,
            download_progress: HashMap::new(),
            address_focused: false,
            focus_address: false,
            address_in_page: false,
            address_editing: false,
            blur_address: false,
            find_open: false,
            focus_find: false,
            focus_terminal: false,
            palette_open: false,
            palette_input,
            palette_index: 0,
            palette_bang: None,
            focus_palette: false,
            chrome,
            gesture: None,
            suppress_window_drag: false,
            tab_press: None,
            stage_bounds: None,
            layout_stage: None,
            order: Vec::new(),
            subscriptions,
            passwords,
            status: "Ready".into(),
            space_rename: None,
            space_input,
            permission: None,
            web_downloads: HashMap::new(),
            attached: false,
            playing: HashSet::new(),
            capturing: HashMap::new(),
            openers: HashMap::new(),
            started,
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
            ticks: 0,
            dock: Tween::new(false),
            dock_hovered: false,
            lift: Tween::new(false),
            space_chips: Vec::new(),
            rail_badges: Vec::new(),
            landing: None,
            retile: false,
            tab_drag: None,
            slots: HashMap::new(),
            slides: HashMap::new(),
            rail_grab: Rc::default(),
            tab_grab: Rc::default(),
            reorder: None,
            find: Tween::new(false),
            toast: ("Ready".into(), Instant::now() - TOAST_LIFE),
            load_bar: 0.0,
            load_done: None,
            load_seen: None,
            finished_url: HashMap::new(),
            opened: HashMap::new(),
            suggestions: Vec::new(),
            suggestion: None,
            suggest_task: None,
            address_bounds: None,
            compact: false,
            reduce_motion: false,
            applied_theme: String::new(),
        };
        view.apply_settings();
        if let Some(own) = view.model.detached.clone().filter(|_| pages.is_empty()) {
            // Reopened at launch: like the main window's restored pages, these wait for a click to play media.
            let ids = view.model.lock().tabs.iter().filter(|tab| tab.session_id == own).map(|tab| tab.id.clone()).collect::<Vec<_>>();
            view.engine.hold_media(ids);
        }
        for (id, page) in pages {
            view.engine.adopt_tab(&id, page);
            view.order.push(id);
        }
        // Cookies and extensions are app-wide; the main window's engine looks after them.
        if view.model.detached.is_none() && view.self_test.is_none() {
            let (restore, tab_ids) = {
                let model = view.model.lock();
                (model.settings.restore_session, model.tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>())
            };
            view.engine.set_session_cookie_dir(app_support_dir().join("session-cookies"), restore);
            view.engine.hold_media(tab_ids);
        }
        if view.model.detached.is_none() {
            view.reload_extensions();
        } else {
            view.read_ext_meta();
        }
        view.sync_permissions();
        let url = view.model.lock().active_tab().map(|tab| tab.url.clone()).unwrap_or_else(|| "nxbrowser://home".into());
        view.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |view, cx| view.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        view
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.drain(cx);
        self.warm_tabs();
        changed |= self.sync_terminals(cx);
        while let Ok((id, result)) = self.installs_rx.try_recv() {
            changed = true;
            self.installing.retain(|pending| pending != &id);
            match result {
                Ok(path) => self.add_extension(&id, &path, "store"),
                Err(error) => self.status = format!("Install failed: {error}").into(),
            }
        }
        if self.model.detached.is_some() {
            // Installs and removals happen in the main window.
            let stale = self.model.lock().extensions.iter().any(|extension| !self.ext_meta.contains_key(&extension.id));
            if stale {
                self.read_ext_meta();
            }
        }
        let active = self.active_tab_id();
        if active.is_some() && active != self.ext_active {
            self.engine.extensions().activated(active.as_deref().unwrap_or_default());
            self.ext_active = active;
        }
        if let Some(id) = self.engine.take_page_click() {
            changed = true;
            // Page clicks never reach GPUI, so whichever input had GPUI focus would keep it. A later click on
            // that input then fires no Focus event, the chrome never takes first responder, and keys go to the page.
            self.blur_address = true;
            cx.notify();
            self.close_palette(cx);
            let already = self.order.last().map(String::as_str) == Some(id.as_str())
                && self.active_tab_id().as_deref() == Some(id.as_str());
            if !already {
                self.focus_window(&id, cx);
            }
        }
        // Catches logins done without a page load (XHR); finished loads save right away.
        if self.cookies_saved.elapsed() > Duration::from_secs(10) {
            self.save_cookies();
        }
        let load = self.active_tab_id().and_then(|id| self.engine.progress(&id));
        changed |= std::mem::replace(&mut self.load_seen, load) != load;
        // The frame loop is off while the toast sits still, so wake it in time for the fade-out.
        changed |= self.toast_fading();
        self.run_self_test();
        self.ticks = self.ticks.wrapping_add(1);
        // Extension errors have no change event; they only show in the popover, so poll them while it is open.
        if changed || self.self_test.is_some() || (self.popover == Some(Popover::Extensions) && self.ticks.is_multiple_of(4)) {
            cx.notify();
        }
    }

    /// Copies each terminal's title and folder into its tab, closes tabs whose shell exited,
    /// and ends shells whose tab is gone; true if anything changed.
    fn sync_terminals(&mut self, cx: &mut Context<Self>) -> bool {
        let tabs = self.model.lock().session_tabs().into_iter().filter(|tab| is_terminal(&tab.url)).cloned().collect::<Vec<_>>();
        let mut changed = false;
        let mut exited = Vec::new();
        for tab in tabs {
            let Some(view) = terminal::get(&tab.id, cx) else {
                continue;
            };
            let view = view.read(cx);
            if view.exited {
                exited.push(tab.id.clone());
                continue;
            }
            let title = view.title();
            let url = view.cwd().map(|dir| terminal::terminal_url(&dir));
            let mut model = self.model.lock();
            if tab.title != title {
                changed = true;
                let _ = model.set_loading_title(&tab.id, &title);
            }
            if let Some(url) = url.filter(|url| *url != tab.url) {
                let _ = model.set_tab_url(&tab.id, &url);
            }
        }
        if !exited.is_empty() {
            changed = true;
            self.close_tabs(exited, cx);
        }
        let live = self.model.lock().tabs.iter().filter(|tab| is_terminal(&tab.url)).map(|tab| tab.id.clone()).collect::<Vec<_>>();
        terminal::retain(|id| live.iter().any(|live| live == id), cx);
        changed
    }

    fn save_cookies(&mut self) {
        self.cookies_saved = Instant::now();
        self.engine.save_session_cookies();
    }

    /// Applies queued model effects, download updates and engine events; true if any arrived.
    fn drain(&mut self, cx: &mut Context<Self>) -> bool {
        let effects = self.model.lock().take_effects();
        let mut changed = !effects.is_empty();
        self.apply(effects);
        for (download, (id, _)) in &self.web_downloads {
            if let Some(bytes) = self.engine.download_bytes(*download) {
                changed |= self.download_progress.insert(id.clone(), bytes) != Some(bytes);
            }
        }
        let mut events = Vec::new();
        while let Ok(event) = self.engine.events().try_recv() {
            events.push(event);
        }
        changed |= !events.is_empty();
        for event in events {
            self.on_event(event, cx);
        }
        let suspend = self.model.lock().tabs_due_for_suspension();
        if suspend.is_empty() {
            return changed;
        }
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        for tab_id in suspend {
            // Every window on the stage is showing its page; suspending one makes it reload in place.
            let entry = self.chrome.get(&tab_id);
            let hidden = entry.is_some_and(|chrome| chrome.minimized) || (maximized && !entry.is_some_and(|chrome| chrome.maximized)) || !self.is_front(&tab_id);
            let popup_in_front = self.openers.iter().any(|(child, opener)| opener == &tab_id && self.is_front(child));
            let mut model = self.model.lock();
            if hidden && !popup_in_front {
                changed |= model.mark_suspended(&tab_id).is_ok();
            } else {
                // Over the cap with every page on screen: candidates come back each tick, which is no change.
                model.keep_alive(&tab_id);
            }
        }
        let effects = self.model.lock().take_effects();
        self.apply(effects);
        changed
    }

    fn on_event(&mut self, event: EngineEvent, cx: &mut Context<Self>) {
        match event {
            EngineEvent::Navigated { tab_id, url, title, loading } => {
                // Title-only reports (unread counters, timers) re-send the finished URL; they need no cookie save or icon probe.
                let moved = !loading && self.finished_url.get(&tab_id) != Some(&url);
                if moved {
                    self.finished_url.insert(tab_id.clone(), url.clone());
                }
                if !loading {
                    let _ = self.model.lock().note_navigation(&tab_id, &url, &title);
                } else if !title.is_empty() {
                    let _ = self.model.lock().set_loading_title(&tab_id, &title);
                }
                let active = self.model.lock().active_tab().map(|tab| tab.id.clone());
                if active.as_deref() == Some(tab_id.as_str()) && !self.address_focused && !url.is_empty() {
                    let shown = if is_blank(&url) { String::new() } else { url.clone() };
                    self.address.update(cx, |input, cx| input.set_content_silent(shown, cx));
                }
                if is_web(&url) {
                    let zoom = self.model.lock().settings.zoom_for(&site_glyph(&url).1);
                    self.engine.set_zoom(&tab_id, zoom);
                }
                if moved {
                    self.save_cookies();
                    let (_, host) = site_glyph(&url);
                    if self.self_test.is_none() && is_web(&url) && !favicon_path(&host).exists() {
                        self.engine.eval(&tab_id, "favicon", FAVICON_JS);
                    }
                }
            }
            EngineEvent::Failed { message, .. } => self.status = message.into(),
            EngineEvent::Download { url, filename, download, .. } => self.start_download(download, &url, &filename),
            EngineEvent::DownloadDone { download, bytes, error } => {
                // Unknown ids are downloads cancelled before they were recorded.
                if let Some((id, path)) = self.web_downloads.remove(&download) {
                    let status = error.map_or_else(|| "done".to_string(), |error| format!("error: {error}"));
                    self.download_progress.insert(id.clone(), (bytes, Some(bytes)));
                    let _ = self.model.lock().update_download(&id, &status, &path);
                    self.status = format!("Download {status}").into();
                }
            }
            EngineEvent::Permission { origin, kind, request, .. } => {
                // One prompt at a time: a newer request turns down the one still showing.
                if let Some((older, _, _)) = self.permission.replace((request, origin, kind)) {
                    self.engine.answer_permission(older, false);
                }
                cx.notify();
            }
            EngineEvent::Passwords { tab_id, origin, request, message } => self.on_password_message(&tab_id, &origin, request, &message, cx),
            EngineEvent::OpenInNewTab { url, from } => {
                // `chrome.sidePanel.open()` arrives here as a plain `tabs.create` of the panel page.
                let panel = self.ext_meta.keys().find(|id| self.panel_url(id).as_deref() == Some(url.as_str())).cloned();
                // A side panel's links get a regular window, not a tab squeezed into the panel.
                let from = from.filter(|id| self.chrome.get(id).is_some_and(|chrome| chrome.panel.is_none()));
                match (panel, from) {
                    (Some(id), _) => self.open_extension_panel(&id, &url, cx),
                    (None, Some(from)) => {
                        self.new_tab_in(&from, Some(&url), cx);
                        if let Some(child) = self.model.lock().active_tab().map(|tab| tab.id.clone()) {
                            self.openers.insert(child, from);
                        }
                    }
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
            // Torn-off windows share the main window's extensions but not its event channel; repaint their badges too.
            EngineEvent::ExtensionChanged { .. } => cx.refresh_windows(),
            EngineEvent::ExtensionPrompt { prompt, id, request } => {
                self.ext_prompts.push((prompt, id, request));
                self.popover = Some(Popover::Extensions);
            }
            EngineEvent::ActivateTab { tab_id } => self.focus_window(&tab_id, cx),
            EngineEvent::CloseTab { tab_id } => self.close_tabs(vec![tab_id], cx),
            EngineEvent::Peek { url } => self.open_peek(&url, cx),
            EngineEvent::Audio { tab_id, playing } => {
                if playing { self.playing.insert(tab_id) } else { self.playing.remove(&tab_id) };
                cx.notify();
            }
            EngineEvent::Capture { tab_id, camera, microphone } => {
                if camera || microphone { self.capturing.insert(tab_id, (camera, microphone)) } else { self.capturing.remove(&tab_id) };
                cx.notify();
            }
        }
    }

    pub(crate) fn on_shortcut(&mut self, shortcut: Shortcut, cx: &mut Context<Self>) {
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
            // A terminal's address pill is not editable.
            Shortcut::FocusUrl => self.focus_address = !self.model.lock().active_tab().is_some_and(|tab| is_terminal(&tab.url)),
            Shortcut::NewTerminal => {
                // Opens beside the active tab, in the active terminal's folder if there is one.
                let url = self.model.lock().active_tab().map(|tab| tab.url.clone()).filter(|url| is_terminal(url)).unwrap_or_else(|| TERMINAL_URL.into());
                match self.active_tab_id() {
                    Some(id) => self.new_tab_in(&id, Some(&url), cx),
                    None => {
                        if let Ok(id) = self.with_model(|model| model.create_tab(&url)) {
                            self.focus_window(&id, cx);
                        }
                    }
                }
            }
            Shortcut::Reload => self.reload(),
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::Find => {
                self.find_open = true;
                self.focus_find = true;
            }
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
                let ids = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
                let pick = if n == 9 { ids.last() } else { ids.get(n - 1) };
                if let Some(id) = pick.cloned() {
                    self.focus_window(&id, cx);
                }
            }
            Shortcut::NextTab | Shortcut::PrevTab => {
                let ids = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
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
            Shortcut::CommandPalette => self.open_palette(cx),
            Shortcut::Dismiss => {
                if let Some(id) = self.active_tab_id().filter(|id| self.chrome.get(id).is_some_and(|chrome| chrome.peek)) {
                    self.close_tabs(vec![id], cx);
                }
            }
            Shortcut::TileWindows => {
                let session = self.model.lock().active_session_id.clone();
                if self.free_space() {
                    self.with_model(|model| { let _ = model.set_session_free(&session, false); });
                }
                self.retile = true;
            }
            Shortcut::Inspect => match self.active_tab_id() {
                Some(_) if !self.model.lock().settings.inspector => self.status = "Turn on Web Inspector in Settings".into(),
                Some(id) => self.engine.inspect(&id),
                None => {}
            },
            Shortcut::HardReload => {
                if let Some(id) = self.active_tab_id() {
                    self.engine.hard_reload(&id);
                }
            }
            Shortcut::FocusWindow(dir) => {
                if let Some(id) = self.window_toward(dir) {
                    self.focus_window(&id, cx);
                }
            }
            Shortcut::MoveWindow(dir) => self.move_window(dir, cx),
            Shortcut::CloseWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.close_window(&id, cx);
                }
            }
            Shortcut::MaximizeWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.toggle_maximize(&id, cx);
                }
            }
            Shortcut::MinimizeWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.minimize_window(&id, cx);
                }
            }
            Shortcut::NextSpace => self.step_space(1),
            Shortcut::PrevSpace => self.step_space(-1),
            Shortcut::NewSpace => {
                if self.model.detached.is_none() {
                    self.with_model(|model| { let _ = model.create_session(); });
                }
            }
            Shortcut::History => self.open_history_modal(cx),
            Shortcut::Downloads | Shortcut::Bookmarks => {
                let popover = if shortcut == Shortcut::Downloads { Popover::Downloads } else { Popover::Bookmarks };
                self.popover = (self.popover != Some(popover)).then_some(popover);
            }
            Shortcut::CopyUrl => {
                if let Some(url) = self.model.lock().active_tab().map(|tab| tab.url.clone()) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
                    self.status = "Copied page address".into();
                }
            }
        }
        self.refresh_url(cx);
    }

    pub(crate) fn with_model<T>(&mut self, f: impl FnOnce(&mut Model) -> T) -> T {
        let (value, effects) = {
            let mut model = self.model.lock();
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
                Effect::EnsureTab { url, .. } if is_terminal(&url) => {}
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
                Effect::Navigate { url, .. } if is_terminal(&url) => {}
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
        let tabs = {
            let model = self.model.lock();
            // A window moved in from another profile keeps that profile's site data.
            let store = |tab: &browser_core::Tab| model.tab_profile(&tab.id).map(|profile| profile.data_store_id.clone());
            model.session_tabs().into_iter().filter_map(|tab| Some((tab.clone(), store(tab)?))).collect::<Vec<_>>()
        };
        let maximized = tabs.iter().any(|(tab, _)| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        for (tab, store) in tabs {
            let entry = self.chrome.get(&tab.id);
            let minimized = entry.is_some_and(|chrome| chrome.minimized);
            let covered = maximized && !entry.is_some_and(|chrome| chrome.maximized);
            // A suspended tab stacked behind another stays unloaded until raised, or `drain` suspends it again each tick.
            let suspended = tab.state == TabState::Suspended;
            if minimized || covered || is_terminal(&tab.url) || (suspended && !self.is_front(&tab.id)) {
                continue;
            }
            if self.engine.ensure_tab(&tab.id, &store, &tab.url).is_ok() && suspended {
                let _ = self.model.lock().mark_live(&tab.id);
            }
        }
    }

    /// Torn-off windows: the manifests (icons, names, panels) of extensions the main window already loaded into WebKit.
    fn read_ext_meta(&mut self) {
        let installed = self.model.lock().installed_extensions().unwrap_or_default();
        self.ext_meta = installed.into_iter().map(|(id, path, _)| (id, read_manifest(&path).unwrap_or_default())).collect();
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
            let _ = self.model.lock().upsert_extension(&local_id(&path), &name, &path, "bundled");
        }
        // Every profile's extensions, so background profiles keep running theirs.
        let ids = self.model.lock().installed_extensions().unwrap_or_default();
        for (id, ..) in ids {
            self.sync_extension(&id);
        }
    }

    /// Re-reads one extension's manifest and starts or stops it in each profile that has it enabled.
    pub(crate) fn sync_extension(&mut self, id: &str) {
        let (installed, stores) = {
            let model = self.model.lock();
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
        let _ = self.model.lock().upsert_extension(id, &manifest.name, path, source);
        self.sync_extension(id);
        self.status = format!("Installed {}", manifest.name).into();
        self.popover = Some(Popover::Extensions);
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
        let record = self.model.lock().extensions.iter().find(|extension| extension.id == id).cloned();
        let last = self.model.lock().remove_extension(id).unwrap_or(false);
        self.sync_extension(id);
        self.engine.extensions().remove_data(id, &self.active_store());
        // Only folders we unpacked ourselves are deleted, once no profile uses them; picked folders and examples stay.
        let root = app_support_dir().join("extensions");
        if let Some(record) = record.filter(|record| last && record.source == "store" && record.path.starts_with(&root)) {
            let _ = std::fs::remove_dir_all(&record.path);
        }
    }

    pub(crate) fn active_store(&self) -> String {
        self.model.lock().active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default()
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

    /// A floating window centred over the stage, out of the tiling, for a quick look at a link.
    fn open_peek(&mut self, url: &str, cx: &mut Context<Self>) {
        let Ok(tab_id) = self.with_model(|model| model.create_tab(url)) else {
            return;
        };
        let entry = self.chrome.entry(tab_id.clone()).or_default();
        entry.peek = true;
        if let Some(bounds) = self.stage_bounds {
            let (sw, sh) = (f64::from(bounds.size.width) as f32, f64::from(bounds.size.height) as f32);
            let (w, h) = (sw * 0.8, sh * 0.85);
            entry.frame = Some(Frame { x: (sw - w) / 2.0, y: (sh - h) / 2.0, w, h });
        }
        self.focus_window(&tab_id, cx);
    }

    /// A side panel: a narrow window docked right, titled with just the extension. One per space.
    fn open_extension_panel(&mut self, id: &str, url: &str, cx: &mut Context<Self>) {
        let open = self
            .model
            .lock()
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
        let settings = self.model.lock().settings.clone();
        self.engine.set_settings(EngineSettings {
            javascript: settings.javascript,
            block_popups: settings.block_popups,
            fraud_warnings: settings.fraud_warnings,
            https_only: settings.https_only,
            block_ads: settings.block_ads,
            inspectable: settings.inspector,
            user_agent: settings.user_agent.clone(),
            page_zoom: settings.page_zoom,
        });
        self.engine.set_shortcuts(if self.recording.is_some() { Vec::new() } else { crate::shortcuts::keymap(&settings) });
        self.compact = settings.compact();
        self.reduce_motion = settings.reduce_motion;
        // set_settings reset every page to the default zoom.
        let tabs = self.model.lock().tabs.iter().map(|tab| (tab.id.clone(), tab.url.clone())).collect::<Vec<_>>();
        for (id, url) in tabs.iter().filter(|(_, url)| is_web(url)) {
            self.engine.set_zoom(id, settings.zoom_for(&site_glyph(url).1));
        }
    }

    /// Steps the active site's zoom through Chrome's levels; 0 resets it to the default.
    fn zoom(&mut self, direction: i32) {
        let Some(tab) = self.model.lock().active_tab().cloned() else {
            return;
        };
        if !is_web(&tab.url) {
            return;
        }
        let host = site_glyph(&tab.url).1;
        let (current, default) = {
            let model = self.model.lock();
            (model.settings.zoom_for(&host), model.settings.page_zoom)
        };
        let next = match direction {
            0 => default,
            d if d > 0 => ZOOM_LEVELS.iter().copied().find(|level| *level > current + 0.001).unwrap_or(ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]),
            _ => ZOOM_LEVELS.iter().rev().copied().find(|level| *level < current - 0.001).unwrap_or(ZOOM_LEVELS[0]),
        };
        let _ = self.model.lock().update_settings(|s| {
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
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let open = |id: &&String| current.contains(id) && !ids.contains(id);
        let survivor = self.order.iter().rev().filter(open).find(|id| self.group_id(id) == group).cloned().or_else(|| {
            self.order
                .iter()
                .rev()
                .filter(open)
                .find(|id| self.is_front(id) && !self.chrome.get(*id).is_some_and(|chrome| chrome.minimized))
                .cloned()
        });
        if self.model.detached.is_some() && current.iter().all(|id| ids.contains(id)) {
            self.close_window = true;
            cx.notify();
            return;
        }
        self.openers.retain(|child, _| !ids.contains(child));
        self.playing.retain(|id| !ids.contains(id));
        self.finished_url.retain(|id, _| !ids.contains(id));
        self.capturing.retain(|id, _| !ids.contains(id));
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
        if self.compact { 24.0 } else { TITLE_H }
    }

    /// Pills and buttons leave a 3px margin above and below in the window title bar.
    fn pill_h(&self) -> f32 {
        self.title_h() as f32 - 6.0
    }

    fn apply_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (theme, accent) = {
            let model = self.model.lock();
            (model.settings.theme.clone(), model.settings.accent.clone())
        };
        let dark = match theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark),
        };
        let key = format!("{theme}{dark}{accent}");
        if key != self.applied_theme || (cx.theme().variant == ThemeVariant::Dark) != dark {
            // Pages read prefers-color-scheme from the app appearance; None follows the system.
            self.engine.set_appearance(match theme.as_str() {
                "light" => Some(false),
                "dark" => Some(true),
                _ => None,
            });
            let mut next = if dark { Theme::default() } else { light_theme() };
            if !accent.is_empty() {
                next.accent_color = parse_hex(&accent);
            }
            cx.set_global(GlobalTheme(Arc::new(next)));
            self.applied_theme = key;
        }
    }

    pub(crate) fn sync_permissions(&mut self) {
        if let Ok(entries) = self.model.lock().permissions_map() {
            self.engine.set_permissions(entries);
        }
    }

    pub(crate) fn refresh_url(&mut self, cx: &mut Context<Self>) {
        if self.address_focused {
            return;
        }
        if let Some(url) = Some(self.model.lock()).and_then(|model| model.active_tab().map(|tab| tab.url.clone())) {
            let url = if is_blank(&url) { String::new() } else { url };
            self.address.update(cx, |input, cx| input.set_content_silent(url, cx));
        }
    }

    /// Chrome-style: history and bookmark matches first, then the search engine's completions once they arrive.
    fn update_suggestions(&mut self, typed: String, cx: &mut Context<Self>) {
        let (history, template) = {
            let model = self.model.lock();
            (model.suggest(&typed, 4).unwrap_or_default(), model.settings.suggest_template())
        };
        self.suggestions = history.into_iter().map(|(url, title)| (url, title, false)).collect();
        self.suggestion = None;
        self.suggest_task = template.filter(|_| !typed.trim().is_empty()).map(|template| {
            let fetch = fetch_suggestions(template, typed.trim());
            cx.spawn(async move |this, cx| {
                let found = fetch.await.unwrap_or_default();
                let _ = this.update(cx, |view, cx| {
                    if !view.address_focused {
                        return;
                    }
                    let room = 8usize.saturating_sub(view.suggestions.len());
                    view.suggestions.extend(found.into_iter().take(room).map(|query| (query, String::new(), true)));
                    cx.notify();
                });
            })
        });
        cx.notify();
    }

    /// Arrow, Enter and Escape in the address bar; keys it doesn't use go on to the input.
    fn address_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.suggestions.len();
        match key {
            "down" if count > 0 => self.suggestion = Some(self.suggestion.map_or(0, |at| (at + 1) % count)),
            "up" if count > 0 => self.suggestion = Some(self.suggestion.map_or(count - 1, |at| (at + count - 1) % count)),
            "enter" => match self.suggestion.and_then(|at| self.suggestions.get(at)).map(|(url, _, _)| url.clone()) {
                Some(url) => self.commit_navigation(&url, cx),
                None => return,
            },
            "escape" if count > 0 => self.suggestions.clear(),
            "escape" => self.end_address_edit(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn commit_navigation(&mut self, raw: &str, cx: &mut Context<Self>) {
        let url = {
            let model = self.model.lock();
            normalize_url(raw, model.settings.search_template(), model.settings.https_only)
        };
        self.address_focused = false;
        self.address_editing = false;
        self.address_in_page = false;
        self.suggestions.clear();
        self.suggestion = None;
        self.suggest_task = None;
        // Let go of the hidden input's GPUI focus, or a later click on the chrome would send keys to it.
        self.blur_address = true;
        self.address.update(cx, |input, cx| input.set_content_silent(url.clone(), cx));
        self.with_model(|model| {
            let _ = model.navigate_active(&url);
        });
        self.focus_active_content();
    }

    /// Gives keys to the active tab's content: its web page, or its terminal on the next render.
    pub(crate) fn focus_active_content(&mut self) {
        let Some(tab) = self.model.lock().active_tab().cloned() else {
            return;
        };
        if is_terminal(&tab.url) {
            self.focus_terminal = true;
        } else {
            self.engine.focus_page(&tab.id);
        }
    }

    /// A popover button: opens its popover, or closes it if open.
    fn toggle_popover(&mut self, popover: Popover, event: &ClickEvent, cx: &mut Context<Self>) {
        let press = match event {
            ClickEvent::Mouse(click) => Some(click.down.position),
            _ => None,
        };
        self.popover = toggled(self.popover, self.popover_dismissed.take(), popover, press);
        cx.notify();
    }

    /// Closes the open popover on a press outside it.
    pub(crate) fn dismiss_popover(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        self.popover_dismissed = self.popover.take().map(|kind| (kind, event.position));
        cx.notify();
    }

    pub(crate) fn open_history_url(&mut self, url: &str, cx: &mut Context<Self>) {
        let url = url.to_string();
        self.popover = None;
        self.history_modal = false;
        self.commit_navigation(&url, cx);
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
        let added = self.model.lock().toggle_bookmark().unwrap_or(false);
        self.status = if added { "Bookmark added".into() } else { "Bookmark removed".into() };
    }

    pub(crate) fn active_tab_id(&self) -> Option<String> {
        Some(self.model.lock()).and_then(|model| model.active_tab().map(|tab| tab.id.clone()))
    }

    /// Picks the file for a download WebKit is holding, then lets it write there.
    fn start_download(&mut self, download: u64, url: &str, filename: &str) {
        let (dir, ask) = {
            let settings = &self.model.lock().settings;
            (settings.download_dir.clone(), settings.ask_download_path)
        };
        let dir = if dir.is_empty() { downloads_dir() } else { std::path::PathBuf::from(dir) };
        let path = unique_path(&dir, filename);
        let path = if ask {
            let Some(path) = path.file_name().and_then(|name| choose_save_path(&name.to_string_lossy())) else {
                self.engine.save_download(download, None);
                return;
            };
            // The save panel already asked about replacing it; WebKit won't write over a file.
            let _ = std::fs::remove_file(&path);
            path
        } else {
            let _ = std::fs::create_dir_all(&dir);
            path
        };
        let id = Uuid::new_v4().to_string();
        let shown = path.display().to_string();
        let _ = self.model.lock().add_download(&id, url, &shown);
        self.download_progress.insert(id.clone(), (0, None));
        self.web_downloads.insert(download, (id, shown));
        self.engine.save_download(download, Some(&path));
        self.status = "Download started".into();
        self.popover = Some(Popover::Downloads);
    }

    pub(crate) fn export_profile(&mut self) {
        let bundle = self.model.lock().export_active();
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

    fn attach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.attached {
            return;
        }
        if let Some(id) = self.model.detached.clone() {
            // Closed by the user (not at quit): the space goes back to the main window.
            let model = self.model.shared.clone();
            let tab_window = self.tab_window;
            // A closed space comes back to the main window; a closed window closes its tabs, like any window.
            window.on_window_should_close(cx, move |_, _| {
                forget_detached(&mut model.lock().unwrap_or_else(PoisonError::into_inner), &id, tab_window);
                true
            });
        } else if self.self_test.is_none() {
            let saved = {
                let model = self.model.lock();
                let spaces = model.settings.detached_spaces.iter().map(|id| (id.clone(), false));
                spaces.chain(model.settings.detached_windows.iter().map(|id| (id.clone(), true))).collect::<Vec<_>>()
            };
            let extensions = self.engine.extensions().clone();
            let model = self.model.shared.clone();
            let origin = window.bounds().origin;
            cx.defer(move |cx| {
                for (n, (id, tab_window)) in saved.into_iter().enumerate() {
                    let at = origin + gpui::point(px(40. * (n + 1) as f32), px(40. * (n + 1) as f32));
                    open_space(cx, model.clone(), extensions.clone(), id, tab_window, Vec::new(), HashMap::new(), at);
                }
            });
        }
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        if self.engine.attach(handle.ns_view.as_ptr()).is_ok() {
            self.attached = true;
            // Page clicks never reach GPUI, so popovers and the palette can't see them as outside presses.
            // They close at once; refocusing waits for the tick, as raising a window mid-press could drop the click.
            let (wake, mut clicks) = futures::channel::mpsc::unbounded();
            self.engine.on_page_click(move || {
                let _ = wake.unbounded_send(());
            });
            cx.spawn(async move |this, cx| {
                while clicks.next().await.is_some() {
                    let closed = this.update(cx, |view, cx| {
                        view.popover = None;
                        view.close_palette(cx);
                        cx.notify();
                    });
                    if closed.is_err() {
                        break;
                    }
                }
            })
            .detach();
            let effects = self.model.lock().take_effects();
            self.apply(effects);
        }
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
            let waiting = self.model.lock().extensions.iter().any(|extension| {
                extension.enabled && !self.engine.extensions().is_loaded(&extension.id) && self.engine.extensions().errors(&extension.id).is_empty()
            });
            if waiting {
                return;
            }
            let (tab_a, store_a) = {
                let model = self.model.lock();
                (
                    model.active_tab().map(|tab| tab.id.clone()).unwrap_or_default(),
                    model.active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default(),
                )
            };
            self.with_model(|model| {
                let _ = model.create_profile("Profile B");
            });
            let (tab_b, store_b) = {
                let model = self.model.lock();
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
            let hello = self.model.lock().extensions.iter().any(|extension| extension.enabled && extension.name == "Hello World");
            let extension = !hello || ext.contains("banner");
            for record in self.model.lock().extensions.iter().filter(|extension| extension.enabled) {
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
        self.address_in_page = false;
        self.refresh_url(cx);
        cx.notify();
    }

    fn request_palette_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.focus_palette) {
            return;
        }
        let palette = self.palette_input.clone();
        self.engine.focus_chrome();
        window.defer(cx, move |window, cx| window.focus(&palette.focus_handle(cx), cx));
    }

    fn request_terminal_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.focus_terminal) {
            return;
        }
        let Some(tab) = self.model.lock().active_tab().filter(|tab| is_terminal(&tab.url)).cloned() else {
            return;
        };
        // A brand-new terminal is created here rather than later in this render, so it can take focus now.
        let view = terminal::terminal_for(&tab.id, &tab.url, cx);
        self.engine.focus_chrome();
        window.defer(cx, move |window, cx| window.focus(&view.focus_handle(cx), cx));
    }

    fn build_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open || self.settings_section != SettingsSection::Terminal || self.term_font_picker.is_some() {
            return;
        }
        let current = self.model.lock().settings.terminal_font.clone();
        let mut fonts = terminal::monospace_fonts().to_vec();
        if !fonts.contains(&current) {
            // A font typed in earlier (or one CoreText doesn't flag as monospace) stays selectable.
            fonts.insert(0, current.clone());
        }
        let options = fonts.into_iter().map(|name| (name.clone(), name)).collect::<Vec<_>>();
        let (pick, create) = (cx.entity().downgrade(), cx.entity().downgrade());
        let picker = combobox("terminal-font", "Terminal font", options)
            .selected(current)
            .placeholder("Search fonts…")
            .full_width(true)
            .on_change(move |font: Option<String>, _, cx| {
                if let Some(font) = font {
                    let _ = pick.update(cx, |this, cx| this.set_pref(cx, |s| s.terminal_font = font));
                }
            })
            // A name not in the list is kept as typed, so fonts CoreText doesn't flag as monospace still work.
            .on_create(move |font, _, cx| {
                let font = font.trim().to_string();
                if !font.is_empty() {
                    let _ = create.update(cx, |this, cx| this.set_pref(cx, |s| s.terminal_font = font));
                }
            });
        self.term_font_picker = Some(cx.new(|cx| ComboboxState::new(picker, window, cx)));
    }

    fn request_find_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.focus_find) {
            return;
        }
        let find = self.find_input.clone();
        self.engine.focus_chrome();
        window.defer(cx, move |window, cx| window.focus(&find.focus_handle(cx), cx));
    }

    fn request_history_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.focus_history) {
            return;
        }
        let query = self.history_query.clone();
        self.engine.focus_chrome();
        window.defer(cx, move |window, cx| window.focus(&query.focus_handle(cx), cx));
    }

    /// Follows GPUI's focus on the address input. The input's own Focus/Blur events fire only while it is drawn,
    /// so they went stale once the field collapsed to a pill, and a stale Blur closed the next edit as it opened.
    fn sync_address_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.address_editing && self.address.focus_handle(cx).is_focused(window);
        if focused == self.address_focused {
            return;
        }
        self.address_focused = focused;
        if focused {
            self.engine.focus_chrome();
        } else {
            // Focus moved to another field mid-edit.
            self.address_editing = false;
            self.address_in_page = false;
            self.refresh_url(cx);
        }
    }

    fn request_address_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.blur_address) && !self.focus_address {
            window.blur();
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

/// Neutral macOS-style light palette (gpuikit only ships tinted light themes).
fn light_theme() -> Theme {
    let mut t = Theme::new(
        "Light",
        ThemeVariant::Light,
        parse_hex("#1d1d1f"), // fg
        parse_hex("#ffffff"), // bg
        parse_hex("#f5f5f7"), // surface
        parse_hex("#d2d2d7"), // border
        parse_hex("#007aff"), // accent
    );
    t.fg_muted_color = Some(parse_hex("#6e6e73"));
    t.fg_disabled_color = Some(parse_hex("#aeaeb2"));
    t.surface_secondary_color = Some(parse_hex("#e8e8ed"));
    t.surface_tertiary_color = Some(parse_hex("#dcdce1"));
    t.border_secondary_color = Some(parse_hex("#c7c7cc"));
    t.border_subtle_color = Some(parse_hex("#e5e5ea"));
    t.info_color = Some(parse_hex("#007aff"));
    t.success_color = Some(parse_hex("#248a3d"));
    t.warning_color = Some(parse_hex("#c93400"));
    t.danger_color = Some(parse_hex("#d70015"));
    t.button_bg_color = Some(parse_hex("#f5f5f7"));
    t.button_bg_hover_color = Some(parse_hex("#e8e8ed"));
    t.button_bg_active_color = Some(parse_hex("#dcdce1"));
    t.button_border_color = Some(parse_hex("#d2d2d7"));
    t.input_border_hover_color = Some(parse_hex("#c7c7cc"));
    t
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
        self.attach(window, cx);
        self.overlay_rects.clear();
        self.space_chips.clear();
        self.rail_badges.clear();
        if !cx.has_active_drag() {
            // A drag dropped nowhere puts its item back in its gap.
            if let Some(state) = self.reorder.take().filter(|state| state.row.is_none()) {
                self.land(&state.id, state.at);
            }
        }
        // Only rows drawn this frame count as drop targets.
        self.slots.clear();
        self.apply_theme(window, cx);
        self.sync_address_focus(window, cx);
        self.request_address_focus(window, cx);
        self.request_palette_focus(window, cx);
        self.request_history_focus(window, cx);
        self.request_find_focus(window, cx);
        self.request_terminal_focus(window, cx);
        self.build_font_picker(window, cx);
        let _subscriptions = &self.subscriptions;
        let entity = cx.entity();
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let profiles = self.model.lock().profiles.clone();
        let active_profile = self.model.lock().active_profile_id.clone();
        let sessions = {
            let model = self.model.lock();
            let (spaces, windows) = (&model.settings.detached_spaces, &model.settings.detached_windows);
            let shown = |session: &&browser_core::Session| match &self.model.detached {
                Some(own) => session.id == *own,
                None => !spaces.contains(&session.id) && !windows.contains(&session.id),
            };
            model.profile_sessions().into_iter().filter(shown).cloned().collect::<Vec<_>>()
        };
        if let Some(own) = self.model.detached.clone().filter(|_| self.close_window) {
            // Its last tab closed: the window and its space go, tabs and all.
            forget_detached(&mut self.model.shared.lock().unwrap_or_else(PoisonError::into_inner), &own, true);
            window.remove_window();
        }
        if let Some(own) = &self.model.detached {
            // Its space was deleted (with its profile): nothing left to show.
            if !self.model.lock().sessions.iter().any(|session| session.id == *own) {
                window.remove_window();
            }
        }
        if !cx.has_active_drag() {
            self.space_drag = None;
        }
        let active_session = self.model.lock().active_session_id.clone();
        let all_tabs = self.model.lock().tabs.clone();
        let colors = paint(cx);
        if !cx.has_active_drag() {
            self.tab_drag = None;
            self.reorder = None;
        }
        // Keep layouts of every open tab, not just this session's, so switching profile and back keeps them.
        let live_ids = self.model.lock().tabs.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        self.chrome.retain(|id, _| live_ids.iter().any(|live| live == id));
        self.order.retain(|id| live_ids.iter().any(|live| live == id));
        if self.tab_window {
            // One window filling the OS window; tabs opened any way join it.
            let group = tabs.first().map(|tab| self.group_id(&tab.id));
            for tab in &tabs {
                let entry = self.chrome.entry(tab.id.clone()).or_default();
                (entry.maximized, entry.minimized, entry.window) = (true, false, group.clone());
            }
        }
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
            // Shift pressed or released without moving still shows or drops the swap at once.
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, window, cx| {
                if let Some((x, y)) = this.gesture.as_ref().map(|gesture| gesture.pointer) {
                    this.track_gesture(gpui::point(px(x), px(y)), event.modifiers.shift, window, cx);
                }
            }))
            .child(div().absolute().track_focus(&self.gesture_focus))
            .text_color(colors.fg)
            .text_size(px(13.))
            // Inputs only ask for keys on a GPUI focus change; clicking one that already had GPUI focus while a page
            // held first responder left typing going to the page. Once the click settles, give keys to whoever GPUI focused.
            .capture_any_mouse_down(cx.listener(|_, _: &MouseDownEvent, window, cx| {
                let view = cx.entity();
                window.defer(cx, move |window, cx| {
                    if window.focused(cx).is_some() {
                        view.update(cx, |view, _| view.engine.focus_chrome());
                    }
                });
            }))
            .child(self.titlebar(&sessions, &all_tabs, &active_session, cx))
            .child(
                h_stack()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .when(self.model.detached.is_none(), |row| row.child(self.rail(&profiles, &active_profile, cx)))
                    .child(
                v_stack()
                    .relative()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .child(self.stage(&tabs, entity, cx)),
            ),
            )
            .when(!self.tab_window, |root| root.child(self.dock(&tabs, cx)))
            .children(self.carried_view(&tabs, cx))
            .children(self.tab_drag_view(cx))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<TabDrag>, _, cx| {
                let id = event.drag(cx).0.id.clone();
                this.tab_drag = Some((event.drag(cx).0.clone(), event.event.position));
                let rows = this.tab_rows();
                this.preview_drag(&id, event.event.position - this.tab_grab.get(), rows, false, 2.0);
                cx.notify();
            }))
            .on_drag_move(cx.listener(|this, event: &gpui::DragMoveEvent<RailDrag>, _, cx| {
                let id = event.drag(cx).0.id.clone();
                let row = this.model.lock().profiles.iter().map(|profile| profile.id.clone()).collect();
                this.preview_drag(&id, event.event.position - this.rail_grab.get(), vec![(RAIL_ROW.to_string(), row)], true, 6.0);
                cx.notify();
            }))
            .when(self.address_focused && !self.suggestions.is_empty(), |root| root.child(self.suggestions_view(cx)))
            .child(self.toast_view(cx))
            .child(self.permission_prompt(cx))
            .child(self.save_password_prompt(cx))
            .when(self.palette_open, |root| root.child(self.palette_view(cx)))
            .when(self.settings_open, |root| root.child(self.settings_modal(cx)))
            .when(self.new_profile.is_some(), |root| root.child(self.new_profile_modal(cx)))
            .when_some(self.delete_prompt.clone(), |root, id| root.child(self.delete_modal(&id, cx)))
            .map(|root| match self.popover {
                Some(Popover::Downloads) => root.child(self.downloads_popover(cx)),
                Some(Popover::Extensions) => root.child(self.extensions_popover(cx)),
                Some(Popover::History) => root.child(self.history_popover(cx)),
                Some(Popover::Bookmarks) => root.child(self.bookmarks_popover(cx)),
                None => root,
            })
            .when(self.history_modal, |root| root.child(self.history_modal(cx)))
            .when(self.bookmark_edit.is_some(), |root| root.child(self.bookmark_editor(cx)))
            .when(self.passwords.edit.is_some(), |root| root.child(self.login_editor(cx)))
            .when_some(self.rail_menu.clone(), |root, menu| root.child(self.rail_menu_view(menu, cx)))
    }
}

impl BrowserWindow {
    /// Moves every animation one frame along: tweens follow their targets, finished closes run.
    fn step_motion(&mut self) {
        self.find.set(self.find_open, self.anim(PANEL_ANIM));
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
        self.slides.retain(|_, (_, at)| at.elapsed() < SLIDE_ANIM);
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
        for (index, (url, title, search)) in self.suggestions.iter().enumerate() {
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
                    .when(*search, |row| row.child(Icons::magnifying_glass().size(px(12.)).text_color(paint.muted)))
                    .child(div().flex_none().max_w(px(220.)).truncate().child(if *search || title.is_empty() { display_url(url) } else { title.clone() }))
                    .when(!*search, |row| row.child(div().flex_1().min_w_0().truncate().text_color(paint.muted).child(display_url(url))))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.commit_navigation(&target, cx);
                    })),
            );
        }
        // Absolute so the list's offsets count from the window origin, not from where this sits in the root column.
        div().absolute().top_0().left_0().child(list)
    }

    /// What each profile's pages are doing across this window's tabs, for the rail.
    fn profile_activity(&self) -> HashMap<String, Activity> {
        let model = self.model.lock();
        let mut out = HashMap::<String, Activity>::new();
        for tab in &model.tabs {
            let Some(session) = model.sessions.iter().find(|session| session.id == tab.session_id) else {
                continue;
            };
            let (camera, microphone) = self.capturing.get(&tab.id).copied().unwrap_or_default();
            let sound = self.playing.contains(&tab.id) && !self.engine.is_muted(&tab.id);
            let entry = out.entry(session.profile_id.clone()).or_default();
            entry.camera |= camera;
            entry.microphone |= microphone;
            entry.sound |= sound;
        }
        out
    }

    fn rail(
        &self,
        profiles: &[browser_core::Profile],
        active_profile: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let paint = paint(cx);
        // Sized to match the 36px profile badges.
        let rail_button = |id: &'static str, icon| icon_button(id, icon).box_size(rems(2.25)).icon_size(rems(1.125));
        let mut profile_list = v_stack().w_full().items_center().gap(px(8.)).py(px(4.));
        let ids = profiles.iter().map(|profile| profile.id.clone()).collect::<Vec<_>>();
        let activity = self.profile_activity();
        for profile in shown_row(self.reorder.as_ref(), RAIL_ROW, &ids).iter().filter_map(|id| profiles.iter().find(|profile| profile.id == *id)) {
            let index = profiles.iter().position(|other| other.id == profile.id).unwrap_or(0);
            let id = profile.id.clone();
            let active = profile.id == active_profile;
            let dragged = RailDrag(profile.clone());
            let (grab, offset) = (self.rail_grab.clone(), self.slide_offset(&id));
            let tint = profile_tint(profile, paint);
            let badge = profile_badge(profile, active, 36., paint)
                .left(offset.x)
                .top(offset.y)
                .group(SharedString::from(format!("rail-{id}")))
                // The pill at the rail's edge marks the open profile, and peeks out on hover for the rest.
                .child(
                    div()
                        .absolute()
                        .left(px(-10.))
                        .top(px(if active { 8. } else { 14. }))
                        .w(px(3.))
                        .h(px(if active { 20. } else { 8. }))
                        .rounded_r(px(3.))
                        .bg(tint)
                        .when(!active, |pill| pill.opacity(0.).group_hover(format!("rail-{id}"), |style| style.opacity(0.6))),
                )
                .when(!active, |badge| badge.hover(|style| style.bg(tint.opacity(0.24))))
                .when_some(activity.get(&profile.id).filter(|state| state.any()), |badge, state| badge.child(activity_chip(*state, paint)))
                .when(self.picked(&id, cx), |badge| badge.opacity(0.))
                .child(self.measure_slot(&id, cx))
                .child({
                    let (watch, id) = (cx.entity(), id.clone());
                    canvas(move |bounds, _, cx| watch.update(cx, |view, _| view.rail_badges.push((id, bounds))), |_, _, _, _| {}).absolute().size_full()
                })
                .id(SharedString::from(format!("rail-{id}")))
                .cursor_pointer()
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        this.with_model(|model| { let _ = model.switch_profile(&id); });
                        this.refresh_url(cx);
                    }
                }))
                .on_drag(dragged, move |drag, at, _, cx| {
                    grab.set(at);
                    cx.new(|_| RailDragPreview(drag.0.clone(), paint))
                })
                .on_drop(cx.listener(|this, drag: &RailDrag, window, cx| this.drop_profile(&drag.0.id, window.mouse_position(), cx)));
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
            .on_drop(cx.listener(|this, drag: &RailDrag, window, cx| this.drop_profile(&drag.0.id, window.mouse_position(), cx)))
            .child(
                div().flex_1().w_full().overflow_hidden().child(
                    scroll_area("rail-profiles").vertical().full_height(true).child(profile_list),
                ),
            )
            .child(div().w(px(28.)).h(px(1.)).flex_none().bg(paint.border))
            .child(
                v_stack()
                    .items_center()
                    .gap(px(4.))
                    .child(rail_button("new-profile-rail", Icons::plus()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.open_new_profile(cx);
                    })))
                    .child(rail_button("downloads-list", Icons::download()).selected(self.popover == Some(Popover::Downloads)).on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.toggle_popover(Popover::Downloads, event, cx);
                    })))
                    .child(rail_button("open-settings", Icons::gear()).selected(self.settings_open).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.toggle_settings(cx);
                    }))),
            )
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
                            // First layout of this run: seeded frames were already fitted to this stage.
                            (None, Some(frame)) => frame,
                            _ => snap.frame(sw, sh),
                        });
                    }
                    if entry.frame.is_some_and(|frame| mostly_off(frame, sw, sh)) {
                        entry.frame = None;
                    }
                }
            }
        }
        let session = self.model.lock().active_session_id.clone();
        let free = self.free_space();
        // New windows open beside the window used last: tiled, they split it; free, they open a step down from it.
        let mut anchor = self
            .order
            .iter()
            .rev()
            .filter(|id| tabs.iter().any(|tab| tab.id == **id))
            .find_map(|id| self.chrome.get(id).filter(|entry| !entry.minimized && entry.panel.is_none() && !entry.peek)?.frame);
        for tab in tabs {
            if !self.order.iter().any(|id| id == &tab.id) {
                self.order.push(tab.id.clone());
            }
            let entry = self.chrome.entry(tab.id.clone()).or_default();
            if free && entry.snap == Some(Snap::Tiled) {
                // Free space: tiled windows (switched over, or carried in from a tiled space) float where they are.
                entry.snap = None;
            }
            if entry.frame.is_some() || entry.panel.is_some() || entry.snap.is_some() {
                continue;
            }
            if free {
                let frame = match anchor {
                    Some(last) => {
                        let (w, h) = (last.w.min(sw * 0.8), last.h.min(sh * 0.8));
                        let next = Frame { x: last.x + CASCADE, y: last.y + CASCADE, w, h };
                        // Past the stage's edge the cascade starts again at its top left.
                        if next.x + w > sw - SNAP_INSET || next.y + h > sh - SNAP_INSET { Frame { x: SNAP_INSET, y: SNAP_INSET, w, h } } else { next }
                    }
                    None => Frame { x: sw * 0.15, y: sh * 0.1, w: sw * 0.7, h: sh * 0.8 },
                };
                entry.frame = Some(frame);
                anchor = Some(frame);
            } else {
                entry.snap = Some(Snap::Tiled);
            }
            if self.layout_stage.is_some() && self.self_test.is_none() && !self.reduce_motion {
                self.opened.insert(tab.id.clone(), Instant::now());
            }
        }
        if std::mem::take(&mut self.retile) {
            // Tile windows: every window on the stage joins, rebuilt from where each one is and how big.
            let fronts = tabs.iter().filter(|tab| self.is_front(&tab.id)).map(|tab| tab.id.clone()).collect::<Vec<_>>();
            for id in fronts {
                if let Some(entry) = self.chrome.get_mut(&id).filter(|entry| !entry.minimized && entry.panel.is_none() && !entry.peek) {
                    if entry.snap.is_none() && !entry.maximized {
                        entry.restore = entry.frame;
                    }
                    (entry.snap, entry.maximized) = (Some(Snap::Tiled), false);
                }
            }
            self.tiles.remove(&session);
        }
        if free {
            self.tiles.remove(&session);
        } else if !self.tab_window {
            self.sync_tiles(tabs, &session, sw, sh);
        }
    }

    /// Whether the open space lets windows float instead of tiling them.
    fn free_space(&self) -> bool {
        let model = self.model.lock();
        model.sessions.iter().any(|session| session.id == model.active_session_id && session.free)
    }

    /// Switches space `id` between tiling and free windows.
    fn toggle_free(&mut self, id: &str, cx: &mut Context<Self>) {
        let free = !self.model.lock().sessions.iter().any(|session| session.id == id && session.free);
        self.with_model(|model| { let _ = model.set_session_free(id, free); });
        // Into tiling, every window joins from where it is, as with Tile windows.
        self.retile = !free;
        cx.notify();
    }

    /// Brings this space's tile tree in line with its tiled windows, then lays their frames out from it:
    /// windows gone from the tiling hand their space to their siblings, and new ones split the window used last.
    fn sync_tiles(&mut self, tabs: &[browser_core::Tab], session: &str, sw: f32, sh: f32) {
        // One leaf per window; a maximized one keeps its slot for when it comes back.
        let members = tabs
            .iter()
            .filter(|tab| self.is_front(&tab.id))
            .filter_map(|tab| {
                let entry = self.chrome.get(&tab.id)?;
                (entry.snap == Some(Snap::Tiled) && !entry.minimized && entry.panel.is_none()).then(|| (self.group_id(&tab.id), tab.id.clone(), entry.frame))
            })
            .collect::<Vec<_>>();
        let rect = cell(Snap::Tiled.frame(sw, sh));
        // After a restart the tree comes back from the frames it laid out before.
        let mut tree = self.tiles.remove(session).or_else(|| {
            Tile::infer(&members.iter().filter_map(|(group, _, frame)| Some((group.clone(), cell((*frame)?)))).collect::<Vec<_>>())
        });
        for leaf in tree.as_ref().map(Tile::leaves).unwrap_or_default() {
            if !members.iter().any(|(group, ..)| *group == leaf) {
                tree = tree.and_then(|tree| tree.without(&leaf));
            }
        }
        for (group, ..) in &members {
            tree = Some(match tree {
                Some(tree) if tree.contains(group) => tree,
                Some(tree) => {
                    let laid = tree.layout(rect);
                    // The window raised last splits, along its longer side: right of it, or below it.
                    let last = self.order.iter().rev().map(|id| self.group_id(id)).find(|other| tree.contains(other));
                    match last.and_then(|last| laid.into_iter().find(|(leaf, _)| *leaf == last)) {
                        Some((target, at)) => tree.insert(&target, group, if at.w >= at.h { Side::Right } else { Side::Bottom }),
                        None => tree.beside(group, Side::Right),
                    }
                }
                None => Tile::Leaf(group.clone()),
            });
        }
        let Some(tree) = tree else {
            return;
        };
        // The window being dragged follows the pointer; its slot waits for the drop.
        let moving = self.gesture.as_ref().filter(|gesture| gesture.kind == GestureKind::Move).map(|gesture| self.group_id(&gesture.tab_id));
        for (group, laid) in tree.layout(rect) {
            if moving.as_ref() == Some(&group) {
                continue;
            }
            if let Some((_, id, _)) = members.iter().find(|(member, ..)| *member == group) {
                if let Some(entry) = self.chrome.get_mut(id) {
                    entry.frame = Some(uncell(laid));
                }
            }
        }
        self.tiles.insert(session.to_string(), tree);
    }

    /// Where dropping a tiled window would leave the tiling: on another tile, beside it on the nearest side;
    /// on a stage-edge snap zone, along that whole side. None for other drops, and for windows not tiled.
    fn dropped_tiles(&self, gesture: &Gesture) -> Option<Tile> {
        if gesture.kind != GestureKind::Move {
            return None;
        }
        let session = self.model.lock().active_session_id.clone();
        let tree = self.tiles.get(&session)?.clone();
        let group = self.group_id(&gesture.tab_id);
        if !tree.contains(&group) {
            return None;
        }
        let rest = tree.without(&group);
        match (gesture.snap, &gesture.insert) {
            (Some(snap), _) if snap != Snap::Maximize => {
                let side = if snap.place().0 < 0 { Side::Left } else { Side::Right };
                Some(rest.map_or_else(|| Tile::Leaf(group.clone()), |rest| rest.beside(&group, side)))
            }
            (None, Some((target, side))) => Some(rest?.insert(target, &group, *side)),
            _ => None,
        }
    }

    /// The tiled window under stage point (x, y), other than `except`'s, and the side of it nearest the point.
    fn tile_under(&self, x: f32, y: f32, except: &str) -> Option<(String, Side)> {
        let session = self.model.lock().active_session_id.clone();
        let tree = self.tiles.get(&session)?;
        let (sw, sh) = self.layout_stage.map(|(w, h)| (w as f32, h as f32))?;
        tree.layout(cell(Snap::Tiled.frame(sw, sh)))
            .into_iter()
            .find(|(group, laid)| group != except && x >= laid.x && x < laid.x + laid.w && y >= laid.y && y < laid.y + laid.h)
            .map(|(group, laid)| (group, Side::nearest(laid, x, y)))
    }

    /// Windows snapped to the stage's halves and quarters, and their frames: the ones that share borders outside the tile tree.
    fn snapped_windows(&self) -> Vec<(String, Frame)> {
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        current
            .into_iter()
            .filter(|id| self.is_front(id))
            .filter_map(|id| {
                let entry = self.chrome.get(&id)?;
                (entry.snap.is_some_and(|snap| snap != Snap::Tiled) && !entry.minimized && !entry.maximized && entry.panel.is_none()).then_some((id.clone(), entry.frame?))
            })
            .collect()
    }

    /// Writes changed window layouts to the tab rows so they survive restarts.
    fn save_layouts(&mut self, tabs: &[browser_core::Tab]) {
        // Mid-motion layouts change every frame; save once things settle instead of writing SQLite per frame.
        if self.gesture.is_some() || self.self_test.is_some() || self.animating() {
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

    /// A space chip released outside the window opens that space in a window of its own.
    fn listen_space_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.space_drag.clone() else {
            return;
        };
        let entity = cx.entity();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                return;
            }
            let inside = gpui::Bounds::new(gpui::Point::default(), window.viewport_size()).contains(&event.position);
            let at = window.bounds().origin + event.position;
            entity.update(cx, |view, cx| {
                view.space_drag = None;
                match (inside, view.model.detached.is_some()) {
                    (true, _) => {}
                    // A torn-off space goes back only when dropped on the main window's bar.
                    (false, true) => {
                        if over_main_bar(at, cx) {
                            view.merge_into_main(window, cx);
                        }
                    }
                    (false, false) => view.open_space_window(&space, false, at, cx),
                }
            });
        });
    }

    fn listen_gesture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gesture.is_none() {
            return;
        }
        let entity = cx.entity();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            entity.update(cx, |view, cx| view.track_gesture(event.position, event.modifiers.shift, window, cx));
        });
        let entity = cx.entity();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                return;
            }
            // Released past the window's edge: the window leaves the app for an OS window of its own.
            let out = torn_off(window, event.position).then(|| window.bounds().origin + event.position);
            entity.update(cx, |view, cx| view.finish_gesture(out, window, cx));
        });
    }

    fn track_gesture(&mut self, position: gpui::Point<Pixels>, shift: bool, window: &Window, cx: &mut Context<Self>) {
        let out = torn_off(window, position);
        self.apply_gesture(position, shift, cx);
        let tears = self.model.detached.is_none();
        // Out past the margin the window leaves on release, so it previews no snap or swap.
        if let Some(gesture) = self.gesture.as_mut().filter(|gesture| out && tears && gesture.kind == GestureKind::Move) {
            gesture.snap = None;
            gesture.swap = None;
            gesture.insert = None;
        }
    }

    fn apply_gesture(&mut self, position: gpui::Point<Pixels>, shift: bool, cx: &mut Context<Self>) {
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
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
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
        if let GestureKind::Resize(edges) = gesture.kind {
            if self.chrome.get(&gesture.tab_id).is_some_and(|entry| entry.snap == Some(Snap::Tiled)) {
                // A tiled window's border is a split in its space's tree; moving it re-lays the tree on the next layout.
                let session = self.model.lock().active_session_id.clone();
                let rect = cell(Snap::Tiled.frame(sw, sh));
                let to = cell(frame);
                if let Some(tree) = self.tiles.get_mut(&session) {
                    for (on, across, after) in [(edges.left, true, false), (edges.right, true, true), (edges.top, false, false), (edges.bottom, false, true)] {
                        if on {
                            let (lo, hi) = span(to, across);
                            tree.set_divider(rect, &group, across, after, if after { hi } else { lo });
                        }
                    }
                }
                self.gesture = Some(gesture);
                cx.notify();
                return;
            }
        }
        match gesture.kind {
            GestureKind::Move => {
                frame = magnet(frame, Edges { left: true, right: true, top: true, bottom: true }, &others, sw, sh);
                let x = f64::from(position.x - bounds.origin.x) as f32;
                let y = f64::from(position.y - bounds.origin.y) as f32;
                gesture.snap = snap_zone(x, y, sw, sh);
                // Up on the spaces bar the window is carried to another space instead of snapped;
                // left over the profile rail, to another profile.
                let over_rail = x < 0.0 && self.rail_badges.len() > 1;
                let over_bar = (y < 0.0 && self.space_chips.len() > 1) || over_rail;
                self.lift.set(over_bar, self.anim(LIFT_ANIM));
                if over_rail {
                    gesture.snap = None;
                    let profile = self.rail_badges.iter().find(|(_, badge)| badge.contains(&position)).map(|(id, _)| id.clone());
                    let active = self.model.lock().active_profile_id.clone();
                    // Hovering a profile opens it, taking the window along into its open space.
                    if let Some(profile) = profile.filter(|profile| *profile != active) {
                        let group = self.group_id(&gesture.tab_id);
                        let ids = self.chrome.keys().filter(|id| self.group_id(id) == group).cloned().collect::<Vec<_>>();
                        let space = {
                            let model = self.model.lock();
                            let mut spaces = model.sessions.iter().filter(|session| session.profile_id == profile);
                            spaces.clone().find(|session| session.active).or_else(|| spaces.next()).map(|session| session.id.clone())
                        };
                        if let Some(space) = space {
                            self.with_model(|model| {
                                let _ = model.move_tabs_to_session(&ids, &space);
                                let _ = model.switch_profile(&profile);
                            });
                            self.focus_window(&gesture.tab_id, cx);
                            self.refresh_url(cx);
                        }
                    }
                } else if over_bar {
                    gesture.snap = None;
                    let space = self.space_chips.iter().find(|(_, chip)| chip.contains(&position)).map(|(id, _)| id.clone());
                    let active = self.model.lock().active_session_id.clone();
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
                // With Shift held, a snapped window dropped on another snapped one (away from the snap edges) trades places with it.
                let inside = |f: Frame| x >= f.x && x < f.x + f.w && y >= f.y && y < f.y + f.h;
                gesture.swap = None;
                if shift && gesture.from.is_some() && gesture.snap.is_none() && !over_bar {
                    gesture.swap = self
                        .order
                        .iter()
                        .rev()
                        .filter(|other| current.contains(other) && self.group_id(other) != group && self.is_front(other))
                        .find(|other| {
                            self.chrome.get(*other).is_some_and(|entry| {
                                entry.snap.is_some() && !entry.minimized && !entry.maximized && entry.frame.is_some_and(inside)
                            })
                        })
                        .cloned();
                }
                // Without Shift, a tiled window dropped on another tile goes beside it.
                gesture.insert = None;
                if !shift && gesture.snap.is_none() && !over_bar {
                    gesture.insert = self.tile_under(x, y, &group);
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
            let mut cells = std::iter::once(gesture.start).chain(gesture.partners.iter().map(|(_, start)| *start)).map(cell).collect::<Vec<_>>();
            let (from, to) = (cell(gesture.start), cell(frame));
            for (on, x, lead) in [(edges.left, true, true), (edges.right, true, false), (edges.top, false, true), (edges.bottom, false, false)] {
                let (line, at) = if lead { (span(from, x).0, span(to, x).0) } else { (span(from, x).1, span(to, x).1) };
                // An edge bordering no tiled window, like one on the stage edge, just moves on its own.
                if on && !move_border(&mut cells, 0, x, line, at) {
                    let (lo, hi) = span(cells[0], x);
                    set_span(&mut cells[0], x, if lead { (at, hi) } else { (lo, at) });
                }
            }
            frame = uncell(cells[0]);
            for ((id, _), laid) in gesture.partners.iter().zip(&cells[1..]) {
                if let Some(entry) = self.chrome.get_mut(id) {
                    entry.frame = Some(uncell(*laid));
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

    fn finish_gesture(&mut self, out: Option<gpui::Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let carried = self.carried();
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        self.lift = Tween::new(false);
        // shortcut: only the main window tears windows off; a detached one would need its own space hand-off.
        if let (Some(at), GestureKind::Move, None) = (out, gesture.kind, &self.model.detached) {
            let snap = gesture.from.map(|(snap, ..)| snap);
            let group = self.group_id(&gesture.tab_id);
            let ids = self.chrome.keys().filter(|id| self.group_id(id) == group).cloned().collect::<Vec<_>>();
            for id in ids {
                if let Some(entry) = self.chrome.get_mut(&id) {
                    entry.home = Some((gesture.start, snap, gesture.restore));
                }
            }
            if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                (entry.frame, entry.restore, entry.snap) = (Some(gesture.start), gesture.restore, None);
            }
            self.detach_window(&gesture.tab_id, at, cx);
            return;
        }
        // Released still small: grow into its frame in whichever space is open now.
        if let Some((id, from)) = carried {
            self.landing = (!self.reduce_motion).then(|| (id, from, Instant::now()));
        }
        self.update_dock(cx);
        let session = self.model.lock().active_session_id.clone();
        let group = self.group_id(&gesture.tab_id);
        let was_tiled = self.tiles.get(&session).is_some_and(|tree| tree.contains(&group));
        if let Some(tree) = self.dropped_tiles(&gesture) {
            self.tiles.insert(session.clone(), tree);
        }
        if let (Some(target), Some((snap, frame, restore))) = (&gesture.swap, gesture.from) {
            if let Some(theirs) = self.chrome.get(target).cloned() {
                if let Some(entry) = self.chrome.get_mut(target) {
                    (entry.snap, entry.frame, entry.restore) = (Some(snap), Some(frame), restore);
                }
                if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                    (entry.snap, entry.frame, entry.restore) = (theirs.snap, theirs.frame, theirs.restore);
                }
                let theirs = self.group_id(target);
                if let Some(tree) = self.tiles.get_mut(&session) {
                    tree.swap(&group, &theirs);
                }
            }
        }
        // A tiled window dropped on an edge zone was rearranged above; it only maximizes from here.
        if let (Some(snap), Some(stage)) = (gesture.snap.filter(|snap| !was_tiled || *snap == Snap::Maximize), self.stage_bounds) {
            let (sw, sh) = (f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32);
            // The size the window had when the drag began is what dragging it out of the snap brings back.
            if snap == Snap::Maximize {
                for chrome in self.chrome.values_mut() {
                    chrome.maximized = false;
                }
            }
            let fitted = self.snap_frame(snap, &gesture.tab_id, sw, sh);
            if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                if snap == Snap::Maximize {
                    entry.maximized = true;
                    // A maximized tile keeps its slot to come back to.
                    entry.snap = was_tiled.then_some(Snap::Tiled);
                    entry.restore = None;
                    entry.frame = Some(gesture.start);
                } else {
                    entry.snap = Some(snap);
                    entry.restore = Some(gesture.start);
                    entry.frame = Some(fitted);
                }
            }
        }
        // The press gave keys to the chrome for the drag; hand them back to the window's page or terminal.
        if self.gesture_focus.is_focused(window) {
            window.blur();
        }
        self.focus_active_content();
        cx.notify();
    }

    fn place_windows(&mut self) {
        if self.self_test.is_some() {
            return;
        }
        let modal = self.settings_open || self.delete_prompt.is_some() || self.new_profile.is_some() || self.history_modal || self.bookmark_edit.is_some() || self.passwords.edit.is_some();
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
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let drawn = self.model.lock().session_tabs().into_iter().filter(|tab| is_terminal(&tab.url) || is_blank(&tab.url)).map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let mut stack = Vec::new();
        let preview = self.drop_preview();
        let carried = self.carried().map(|(id, _)| id);
        for id in ids {
            let Some(entry) = self.chrome.get(&id).cloned() else {
                continue;
            };
            if let Some((_, frame)) = preview.as_ref().filter(|(dragged, _)| *dragged == id) {
                // The preview hides pages behind it but has no page of its own.
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
                self.maximized_frame(stage_w as f32, stage_h as f32)
            } else if let Some(frame) = entry.frame {
                frame
            } else {
                self.engine.place_tab(&id, None);
                continue;
            };
            let find = if active.as_deref() == Some(id.as_str()) { FIND_H * f64::from(self.find.progress()) } else { 0.0 };
            let (scale, alpha) = self.window_motion(&id);
            let frame = scaled(frame, scale);
            if drawn.contains(&id) {
                // Drawn by GPUI inside the shell: it hides pages behind it but needs no hole of its own.
                self.engine.place_tab(&id, None);
                let shell = LogicalRect { x: origin_x + f64::from(frame.x), y: origin_y + f64::from(frame.y), width: f64::from(frame.w), height: f64::from(frame.h) };
                stack.push((String::new(), shell, LogicalRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }));
                continue;
            }
            // A torn-off window's page fills it edge to edge: no title bar, no border.
            let (title_h, inset) = if self.tab_window { (0.0, 0.0) } else { (self.title_h(), PAGE_INSET) };
            let rect = LogicalRect {
                x: origin_x + f64::from(frame.x) + inset,
                y: origin_y + f64::from(frame.y) + title_h + find,
                width: (f64::from(frame.w) - inset * 2.0).max(1.0),
                height: (f64::from(frame.h) - title_h - find - inset).max(1.0),
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
        let resizable = self.chrome.iter().filter(|(_, entry)| !entry.maximized).map(|(id, _)| id.clone()).collect::<Vec<_>>();
        self.engine.set_grab_zones(&grab_zones(&stack, &resizable));
    }

    fn begin_move(&mut self, id: &str, position: gpui::Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
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
        // A tiled window keeps its size while dragged: it is rearranged, not floated.
        let restore = self.chrome.get(id).and_then(|entry| if entry.maximized { entry.frame } else if entry.snap == Some(Snap::Tiled) { None } else { entry.restore });
        let from = self.chrome.get(id).filter(|entry| !entry.maximized).and_then(|entry| Some((entry.snap?, start, entry.restore)));
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
            from,
            swap: None,
            insert: None,
        });
        self.engine.focus_chrome();
        if window.focused(cx).is_none() {
            window.focus(&self.gesture_focus, cx);
        }
        cx.notify();
    }

    fn begin_resize(&mut self, id: &str, edges: Edges, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.focus_window(id, cx);
        let Some(start) = self.visual_frame(id) else {
            return;
        };
        let snapped = self.chrome.get(id).is_some_and(|entry| entry.snap.is_some_and(|snap| snap != Snap::Tiled));
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.maximized = false;
            entry.frame = Some(start);
        }
        // Windows snapped to halves and quarters share borders: the ones across a dragged border make room.
        // Tiled windows need none of this; their tree moves the border.
        let partners = if snapped {
            let group = self.group_id(id);
            self.snapped_windows().into_iter().filter(|(other, _)| self.group_id(other) != group).collect()
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
            from: None,
            swap: None,
            insert: None,
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
                            view.listen_space_drag(window, cx);
                        });
                    },
                )
                .absolute()
                .size_full(),
            );
        let mut ordered = shown;
        ordered.sort_by_key(|tab| self.order.iter().position(|id| id == &tab.id).unwrap_or(0));
        let preview = self.drop_preview();
        let carried = self.carried().map(|(id, _)| id);
        // Windows occlude, so only a tab dropped on empty stage lands here.
        stage = stage.on_drop(cx.listener(|this, drag: &TabDrag, window, cx| {
            this.reorder = None;
            this.detach_tab(&drag.0.id, window.mouse_position(), cx);
        }));
        for tab in ordered {
            if let Some((_, frame)) = preview.as_ref().filter(|(dragged, _)| *dragged == tab.id) {
                stage = stage.child(snap_preview(*frame, cx));
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

    /// Where `snap` puts window `id`, fitted beside this space's other snapped windows.
    fn snap_frame(&self, snap: Snap, id: &str, sw: f32, sh: f32) -> Frame {
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let group = self.group_id(id);
        let others = self
            .chrome
            .iter()
            .filter(|(other, entry)| current.contains(other) && self.group_id(other) != group && self.is_front(other) && !entry.minimized && !entry.maximized)
            .filter_map(|(_, entry)| Some((entry.snap?, entry.frame?)))
            .collect::<Vec<_>>();
        fit_snap(snap, sw, sh, &others)
    }

    /// Where releasing the drag would put the dragged window: its snap target, or the snapped window it would swap with.
    fn drop_preview(&self) -> Option<(String, Frame)> {
        let gesture = self.gesture.as_ref()?;
        if let Some(tree) = self.dropped_tiles(gesture) {
            let (sw, sh) = self.layout_stage.map(|(w, h)| (w as f32, h as f32))?;
            let group = self.group_id(&gesture.tab_id);
            let (_, laid) = tree.layout(cell(Snap::Tiled.frame(sw, sh))).into_iter().find(|(leaf, _)| *leaf == group)?;
            return Some((gesture.tab_id.clone(), uncell(laid)));
        }
        let frame = match (gesture.snap, &gesture.swap) {
            (Some(snap), _) => {
                let stage = self.stage_bounds?;
                self.snap_frame(snap, &gesture.tab_id, f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32)
            }
            (None, Some(target)) => self.chrome.get(target)?.frame?,
            (None, None) => return None,
        };
        Some((gesture.tab_id.clone(), frame))
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
            return Some(self.maximized_frame(f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32));
        }
        entry.frame
    }

    /// A maximized window's frame on a `w` × `h` stage; a torn-off window's one fills the stage.
    fn maximized_frame(&self, w: f32, h: f32) -> Frame {
        let margin = if self.tab_window { 0.0 } else { 8.0 };
        Frame { x: margin, y: margin, w: (w - margin * 2.0).max(1.0), h: (h - margin * 2.0).max(1.0) }
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
        // After placing: a page un-minimized just now is hidden until placed, and AppKit won't focus a hidden view.
        self.place_windows();
        self.focus_active_content();
        cx.notify();
    }

    /// The color of the profile window `id` belongs to, for its decorations; the theme accent when it has none.
    fn profile_accent(&self, id: &str, paint: Paint) -> gpui::Hsla {
        self.model.lock().tab_profile(id).map_or(paint.accent, |profile| profile_tint(profile, paint))
    }

    /// The profile of window `id` when it was moved in from another profile than the open one.
    fn guest_profile(&self, id: &str) -> Option<browser_core::Profile> {
        let model = self.model.lock();
        model.tab_profile(id).filter(|profile| profile.id != model.active_profile_id).cloned()
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
        let accent = self.profile_accent(&tab.id, paint);
        let active = tab.active;
        let maximized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized);
        let frame = self.chrome.get(&tab.id).and_then(|chrome| chrome.frame)?;
        // Occlude so a press on this window never also reaches the windows stacked beneath it.
        let (scale, alpha) = self.window_motion(&tab.id);
        let mut shell = div().absolute().overflow_hidden().occlude().opacity(alpha);
        let frame = scaled(frame, scale);
        // Not maximized: the shell sits inside a box GRIP_OUT larger, whose rim holds the resize grips.
        shell = if self.tab_window {
            shell.size_full()
        } else if maximized {
            shell.left(px(8.)).top(px(8.)).right(px(8.)).bottom(px(8.))
        } else {
            shell.left(px(GRIP_OUT)).top(px(GRIP_OUT)).right(px(GRIP_OUT)).bottom(px(GRIP_OUT))
        };
        let target = tab.id.clone();
        let hover_id = tab.id.clone();
        let hover_focus = !active && self.model.lock().settings.focus_on_hover;
        let body = div().relative().size_full().child(
            v_stack()
                .id(SharedString::from(format!("window-{}", tab.id)))
                .size_full()
                .when(hover_focus, |column| {
                    column.on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                        // Not mid-drag, nor while typing an address, which passing over another window would cut off.
                        if event.pressed_button.is_none() && this.gesture.is_none() && !cx.has_active_drag() && !this.palette_open && !this.address_editing {
                            this.focus_window(&hover_id, cx);
                        }
                    }))
                })
                .when(!self.tab_window, |column| {
                    column.rounded(px(WINDOW_RADIUS as f32)).border_1().border_color(if active { accent } else { paint.border })
                })
                .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| this.drop_tab_on(&drag.0.id, &target, window.mouse_position(), cx)))
                .shadow_md()
                .bg(paint.secondary)
                .overflow_hidden()
                .when(!self.tab_window, |column| column.child(self.window_titlebar(tab, members, cx)))
                .when(active && self.find.progress() > 0.0, |column| {
                    column.child(div().h(px(FIND_H as f32 * self.find.progress())).flex_none().overflow_hidden().child(self.find_bar(cx)))
                })
                .child(match is_terminal(&tab.url) {
                    true => div()
                        // The shell occludes the root's capture handler, so a click here must itself take keys back
                        // from a page that had them, and make this the active tab (else ⌘W closes the other one).
                        .capture_any_mouse_down({
                            let id = tab.id.clone();
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.engine.focus_chrome();
                                this.focus_window(&id, cx);
                            })
                        })
                        .flex_1()
                        .min_h(px(0.))
                        .mx(px(PAGE_INSET as f32))
                        .mb(px(PAGE_INSET as f32))
                        .rounded_b(px(PAGE_RADIUS as f32))
                        .overflow_hidden()
                        .bg(paint.bg)
                        // Cached: the screen rebuilds only when the terminal itself notifies, not on every chrome repaint.
                        .child(terminal::terminal_for(&tab.id, &tab.url, cx).cached(gpui::StyleRefinement::default().size_full())),
                    false if is_blank(&tab.url) => div()
                        .flex_1()
                        .min_h(px(0.))
                        .mx(px(PAGE_INSET as f32))
                        .mb(px(PAGE_INSET as f32))
                        .rounded_b(px(PAGE_RADIUS as f32))
                        .overflow_hidden()
                        .bg(paint.bg)
                        .child(self.new_tab_page(tab, cx)),
                    false => div().flex_1().min_h(px(0.)),
                }),
        );
        if maximized {
            return Some(shell.child(body));
        }
        Some(
            div()
                .absolute()
                .left(px(frame.x - GRIP_OUT))
                .top(px(frame.y - GRIP_OUT))
                .w(px(frame.w + GRIP_OUT * 2.0))
                .h(px(frame.h + GRIP_OUT * 2.0))
                .child(shell.child(body))
                .child(self.resize_grip(&tab.id, Edges::left(), cx))
                .child(self.resize_grip(&tab.id, Edges::right(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom(), cx))
                .child(self.resize_grip(&tab.id, Edges::top(), cx))
                .child(self.resize_grip(&tab.id, Edges::top_left(), cx))
                .child(self.resize_grip(&tab.id, Edges::top_right(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom_left(), cx))
                .child(self.resize_grip(&tab.id, Edges::bottom_right(), cx)),
        )
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
        let band = px(GRIP_OUT + GRIP_IN);
        let corner_size = px(GRIP_OUT + GRIP_CORNER);
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
        let new_tab_id = tab.id.clone();
        let new_tab = self.tool_button(format!("new-tab-{new_tab_id}"), Icons::plus(), false, cx, move |this, event, cx| {
            this.click_new_tab(&new_tab_id, event.modifiers().shift, cx);
        });
        let bar = (tab.active && self.load_bar > 0.0).then(|| {
            let fade = self.load_done.map_or(1.0, |at| 1.0 - (at.elapsed().as_secs_f32() / LOAD_FADE.as_secs_f32()).min(1.0));
            div().absolute().left_0().bottom_0().h(px(2.)).w(relative(self.load_bar)).bg(paint.accent).opacity(fade)
        });
        // The bar goes after the title bar, whose opaque background would otherwise paint over it.
        div().relative().child(
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
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.begin_move(&id, event.position, window, cx);
                }))
                // Buttons keep their size; the tabs shrink, and the empty space either side drags the window.
                .child(self.hit_guard(format!("nav-guard-{}", tab.id), nav, cx).flex_none())
                .children(self.guest_profile(&tab.id).map(|profile| profile_badge(&profile, true, 20., paint)))
                .child(div().flex_1().min_w(px(8.)).h_full())
                .child(self.compact_tabs(&tab.id, members, cx))
                .child(self.hit_guard(format!("new-tab-guard-{}", tab.id), new_tab, cx).flex_none())
                .child(div().flex_1().min_w(px(8.)).h_full())
                .child(self.hit_guard(format!("tools-guard-{}", tab.id), tools, cx).flex_none()),
        )
        .children(bar)
    }

    /// Small centred tab pills; clicking the shown tab widens it into the address field.
    fn compact_tabs(&self, front: &str, members: &[&browser_core::Tab], cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let mut row = h_stack().min_w(px(0.)).h_full().items_center().gap(px(2.)).text_size(px(12.));
        let ids = members.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        for id in shown_row(self.reorder.as_ref(), &self.group_id(front), &ids) {
            let Some(tab) = members.iter().find(|tab| tab.id == id) else {
                // The gap for a tab dragged in from another window.
                let width = self.reorder.as_ref().map_or(px(0.), |state| state.size.width);
                row = row.child(
                    div().id(SharedString::from(format!("ctab-{id}"))).relative().flex_none().w(width).h(px(self.pill_h())).child(self.measure_slot(&id, cx)),
                );
                continue;
            };
            let shown = tab.id == front;
            if shown && tab.active && (self.address_editing || self.address_focused) {
                row = row.child(
                    div()
                        .id(SharedString::from(format!("ctab-{}", tab.id)))
                        .relative()
                        .left(self.slide_offset(&tab.id).x)
                        .child(self.measure_slot(&tab.id, cx))
                        .w(px(520.))
                        .min_w(px(120.))
                        .on_mouse_down(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, _| this.suppress_window_drag = true))
                        .child(self.address_slot(tab, cx)),
                );
                continue;
            }
            let id = tab.id.clone();
            let close_id = tab.id.clone();
            let offset = self.slide_offset(&tab.id);
            let hover_group = SharedString::from(format!("ctab-{id}"));
            row = row.child(
                h_stack()
                    .id(SharedString::from(format!("ctab-{id}")))
                    .group(hover_group.clone())
                    .relative()
                    .left(offset.x)
                    .top(offset.y)
                    // Shrinks to just the icon when the window is narrow.
                    .min_w(px(28.))
                    .max_w(px(160.))
                    .h(px(self.pill_h()))
                    .px(px(8.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(8.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(shown, |pill| pill.bg(paint.secondary).text_color(paint.fg))
                    .when(!shown, |pill| pill.text_color(paint.muted).hover(|style| style.bg(paint.secondary.opacity(0.6)).text_color(paint.fg)))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _, _| {
                        this.suppress_window_drag = true;
                        this.tab_press = Some(event.position);
                    }))
                    .on_drag(TabDrag((*tab).clone()), {
                        let grab = self.tab_grab.clone();
                        move |_, at, _, cx| {
                            grab.set(at);
                            cx.new(|_| TabDragPreview)
                        }
                    })
                    .when(self.picked(&tab.id, cx), |pill| pill.opacity(0.))
                    .child(self.measure_slot(&tab.id, cx))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.click_tab(&id, cx)))
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
                    .child(div().min_w(px(0.)).truncate().child(tab_label(&tab.title, &tab.url)))
                    .children(self.audio_button(tab, cx)),
            );
        }
        row
    }

    /// `[icon] Name ──── [×]`: a side panel needs no address bar or navigation.
    fn panel_titlebar(&self, tab_id: &str, ext: &str, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let meta = self.ext_meta.get(ext).cloned().unwrap_or_default();
        let (move_id, close_id) = (tab_id.to_string(), tab_id.to_string());
        let close = self.tool_button(format!("close-{tab_id}"), Icons::cross_2(), false, cx, move |this, _, cx| {
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
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.begin_move(&move_id, event.position, window, cx);
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
        // One input can only be drawn once: a blank tab's edit started from the page's big field is drawn there.
        let editing = tab.active && !(is_blank(&tab.url) && self.address_in_page) && (self.address_editing || self.address_focused);
        let pill = div()
            .h(px(self.pill_h()))
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
            return self.address_field(pill.border_color(paint.accent.opacity(0.6)), 12., cx);
        }
        let id = tab.id.clone();
        if is_terminal(&tab.url) {
            return div().w_full().child(
                pill.border_color(gpui::transparent_black())
                    .text_color(if tab.active { paint.fg } else { paint.muted })
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(terminal::location(&tab.url))),
            );
        }
        let (label, color) = if is_blank(&tab.url) {
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

    /// The live address input inside `pill`, with suggestion keys and the bounds the suggestion list hangs from.
    fn address_field(&self, pill: gpui::Div, size: f32, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let watch = cx.entity();
        pill.relative()
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| this.end_address_edit(window, cx)))
            // The input turns these keys into actions; catching them first lets arrows move through suggestions instead of the caret.
            .capture_action(cx.listener(|this, _: &bindings::Down, window, cx| this.address_key("down", window, cx)))
            .capture_action(cx.listener(|this, _: &bindings::Up, window, cx| this.address_key("up", window, cx)))
            .capture_action(cx.listener(|this, _: &bindings::Enter, window, cx| this.address_key("enter", window, cx)))
            .capture_action(cx.listener(|this, _: &bindings::Escape, window, cx| this.address_key("escape", window, cx)))
            .child(canvas(move |bounds, _, cx| watch.update(cx, |view, _| view.address_bounds = Some(bounds)), |_, _, _, _| {}).absolute().size_full())
            .child(
                input(&self.address, cx)
                    .placeholder("Search or enter an address")
                    .cursor_color(paint.accent)
                    .flex_1()
                    .h_full()
                    .text_size(px(size))
                    .text_color(paint.fg),
            )
    }

    /// Drawn in place of a blank tab's page: bookmarks along the top, then a big address field over pinned and recently visited sites.
    fn new_tab_page(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        let id = tab.id.clone();
        let pill = div()
            .h(px(46.))
            .w_full()
            .px(px(16.))
            .gap(px(10.))
            .rounded(px(12.))
            .flex()
            .items_center()
            .bg(paint.secondary)
            .border_1()
            .shadow_sm()
            .child(Icons::magnifying_glass().size(px(16.)).text_color(paint.muted));
        let field = if tab.active && self.address_in_page && (self.address_editing || self.address_focused) {
            self.address_field(pill.border_color(paint.accent.opacity(0.6)), 16., cx)
        } else {
            let open = id.clone();
            div().w_full().child(
                pill.id(SharedString::from(format!("home-field-{id}")))
                    .border_color(paint.border)
                    .cursor_text()
                    .text_size(px(16.))
                    .text_color(paint.muted)
                    .child("Search or enter an address")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.focus_window(&open, cx);
                        this.focus_address = true;
                        this.address_in_page = true;
                        cx.notify();
                    })),
            )
        };
        let (bookmarks, recent) = {
            let model = self.model.lock();
            let bookmarks = model.bookmarks().unwrap_or_default();
            // Pinned sites already have a tile; recent skips them too.
            let mut hosts = bookmarks.iter().filter(|b| b.pinned).map(|b| site_host(&b.url)).collect::<HashSet<_>>();
            let recent = model
                .history()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| hosts.insert(site_host(&entry.url)))
                .take(8)
                .collect::<Vec<_>>();
            (bookmarks, recent)
        };
        let (pinned, saved): (Vec<_>, Vec<_>) = bookmarks.into_iter().partition(|b| b.pinned);
        let action = |key: String, icon: gpui::Svg, run: WindowAction, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(key))
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .bg(paint.surface)
                .border_1()
                .border_color(paint.border)
                .hover(|style| style.bg(paint.secondary))
                .child(icon.size(px(10.)).text_color(paint.fg))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    run(this, cx);
                    cx.notify();
                }))
        };
        let tile = |key: String, url: &str, title: &str, actions: gpui::Div, cx: &mut Context<Self>| {
            let (window_id, open) = (id.clone(), url.to_string());
            let name = if title.is_empty() { site_host(url) } else { title.to_string() };
            v_stack()
                .id(SharedString::from(key.clone()))
                .group(SharedString::from(key.clone()))
                .relative()
                .w(px(72.))
                .p(px(8.))
                .gap(px(6.))
                .items_center()
                .rounded(px(10.))
                .cursor_pointer()
                .hover(|tile| tile.bg(paint.secondary))
                .child(site_icon(url, 32., false))
                .child(div().w_full().text_size(px(11.)).text_center().truncate().child(name))
                .child(h_stack().absolute().top(px(2.)).right(px(2.)).gap(px(2.)).opacity(0.).group_hover(SharedString::from(key), |style| style.opacity(1.)).child(actions))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.focus_window(&window_id, cx);
                    this.commit_navigation(&open, cx);
                }))
        };
        let mut column = v_stack().w_full().max_w(px(640.)).gap(px(20.)).child(field);
        let mut grid = div().flex().flex_wrap().gap(px(8.));
        for bookmark in pinned.iter().rev() {
            let (edit, unpin) = (bookmark.clone(), bookmark.clone());
            let actions = h_stack()
                .gap(px(2.))
                .child(action(format!("home-edit-{}-{id}", bookmark.id), Icons::pencil_1(), Box::new(move |this, cx| this.open_bookmark_editor(Some(&edit), cx)), cx))
                .child(action(format!("home-unpin-{}-{id}", bookmark.id), Icons::cross_2(), Box::new(move |this, _| {
                    let _ = this.model.lock().save_bookmark(Some(&unpin.id), &unpin.url, &unpin.title, false);
                }), cx));
            grid = grid.child(tile(format!("home-pin-{}-{id}", bookmark.id), &bookmark.url, &bookmark.title, actions, cx));
        }
        grid = grid.child(
            v_stack()
                .id(SharedString::from(format!("home-add-{id}")))
                .w(px(72.))
                .p(px(8.))
                .gap(px(6.))
                .items_center()
                .rounded(px(10.))
                .cursor_pointer()
                .text_color(paint.muted)
                .hover(|tile| tile.bg(paint.secondary))
                .child(div().size(px(32.)).flex().items_center().justify_center().rounded_full().bg(paint.secondary).child(Icons::plus().size(px(14.))))
                .child(div().text_size(px(11.)).child("Add"))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_bookmark_editor(None, cx))),
        );
        column = column.child(v_stack().gap(px(6.)).child(self.section_label("Pinned", paint)).child(grid));
        if !recent.is_empty() {
            let mut grid = div().flex().flex_wrap().gap(px(8.));
            for (index, entry) in recent.into_iter().enumerate() {
                let (url, title) = (entry.url.clone(), entry.title.clone());
                let actions = h_stack().child(action(format!("home-pin-recent-{index}-{id}"), Icons::drawing_pin(), Box::new(move |this, _| {
                    let _ = this.model.lock().save_bookmark(None, &url, &title, true);
                }), cx));
                grid = grid.child(tile(format!("home-recent-{index}-{id}"), &entry.url, &entry.title, actions, cx));
            }
            column = column.child(v_stack().gap(px(6.)).child(self.section_label("Recent", paint)).child(grid));
        }
        // Bookmarks as a bar of chips along the top, like a bookmarks bar.
        let mut bar = div().w_full().flex().flex_wrap().justify_center().gap(px(6.)).px(px(16.)).pt(px(12.));
        for bookmark in saved.iter().take(12) {
            let (window_id, open) = (id.clone(), bookmark.url.clone());
            let name = if bookmark.title.is_empty() { site_host(&bookmark.url) } else { bookmark.title.clone() };
            bar = bar.child(
                h_stack()
                    .id(SharedString::from(format!("home-mark-{}-{id}", bookmark.id)))
                    .h(px(28.))
                    .px(px(10.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(paint.border)
                    .bg(paint.secondary)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(|chip| chip.border_color(paint.accent.opacity(0.5)))
                    .child(site_icon(&bookmark.url, 14., false))
                    .child(div().max_w(px(160.)).truncate().child(name))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.focus_window(&window_id, cx);
                        this.commit_navigation(&open, cx);
                    })),
            );
        }
        if saved.len() > 12 {
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("home-marks-more-{id}")))
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .text_size(px(12.))
                    .text_color(paint.muted)
                    .cursor_pointer()
                    .hover(|chip| chip.bg(paint.secondary))
                    .child(format!("{} more", saved.len() - 12))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.popover = Some(Popover::Bookmarks);
                        cx.notify();
                    })),
            );
        }
        v_stack()
            .size_full()
            .when(!saved.is_empty(), |page| page.child(bar))
            .child(div().flex_1().flex().justify_center().pt(relative(0.14)).px(px(24.)).child(column))
    }

    fn nav_controls(&self, tab_id: &str, cx: &mut Context<Self>) -> gpui::Div {
        if self.model.lock().tabs.iter().any(|tab| tab.id == tab_id && is_terminal(&tab.url)) {
            return h_stack();
        }
        let back_id = tab_id.to_string();
        let forward_id = tab_id.to_string();
        h_stack()
            .items_center()
            .gap(px(2.))
            .child(self.tool_button(format!("back-{back_id}"), Icons::arrow_left(), true, cx, move |this, _, cx| {
                this.focus_window(&back_id, cx);
                this.back();
            }))
            .child(self.tool_button(format!("forward-{forward_id}"), Icons::arrow_right(), true, cx, move |this, _, cx| {
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
        let keep_id = tab_id.to_string();
        let peek = self.chrome.get(tab_id).is_some_and(|chrome| chrome.peek);
        let bookmarked = {
            let model = self.model.lock();
            let url = model.tabs.iter().find(|tab| tab.id == tab_id).map(|tab| tab.url.clone());
            url.is_some_and(|url| model.bookmarks().unwrap_or_default().iter().any(|bookmark| bookmark.url == url))
        };
        h_stack()
            .items_center()
            .gap(px(2.))
            .when(peek, |row| row.child(self.tool_button(format!("keep-{keep_id}"), Icons::open_in_new_window(), false, cx, move |this, _, cx| {
                // Placed afresh like any new window: tiled, or cascaded in a free space.
                if let Some(entry) = this.chrome.get_mut(&keep_id) {
                    (entry.peek, entry.frame) = (false, None);
                }
                this.focus_window(&keep_id, cx);
            })))
            .child(self.tool_button(format!("reload-{reload_id}"), Icons::reload(), false, cx, move |this, _, cx| {
                this.focus_window(&reload_id, cx);
                this.reload();
            }))
            .child(self.tool_button(format!("bookmark-{bookmark_id}"), if bookmarked { Icons::star_filled() } else { Icons::star() }, false, cx, move |this, _, cx| {
                this.focus_window(&bookmark_id, cx);
                this.bookmark();
            }))
            .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(paint(cx).border))
            .child(self.tool_button(format!("min-{min_id}"), Icons::minus(), false, cx, move |this, _, cx| this.minimize_window(&min_id, cx)))
            .child(self.tool_button(format!("zoom-{zoom_id}"), zoom_icon, false, cx, move |this, _, cx| this.toggle_maximize(&zoom_id, cx)))
            .child(self.tool_button(format!("close-{close_id}"), Icons::cross_2(), false, cx, move |this, _, cx| this.close_window(&close_id, cx)))
    }

    fn minimize_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let entry = self.chrome.entry(id.to_string()).or_default();
        entry.minimized = true;
        entry.maximized = false;
        cx.notify();
    }

    fn toggle_maximize(&mut self, id: &str, cx: &mut Context<Self>) {
        let next = !self.chrome.get(id).is_some_and(|chrome| chrome.maximized);
        for chrome in self.chrome.values_mut() {
            chrome.maximized = false;
        }
        let entry = self.chrome.entry(id.to_string()).or_default();
        entry.maximized = next;
        entry.minimized = false;
        self.focus_window(id, cx);
    }

    /// Closes every tab of the window holding `id`.
    fn close_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let group = self.group_id(id);
        let members = self.chrome.keys().filter(|other| self.group_id(other) == group).cloned().collect::<Vec<_>>();
        self.close_tabs(members, cx);
    }

    /// The shown window nearest the active one in direction `dir`.
    fn window_toward(&self, dir: Dir) -> Option<String> {
        let active = self.active_tab_id()?;
        let from = self.visual_frame(&active)?;
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let others = self
            .shown_tabs(&tabs)
            .into_iter()
            .filter(|tab| tab.id != active)
            .filter_map(|tab| Some((tab.id.clone(), self.visual_frame(&tab.id)?)))
            .collect::<Vec<_>>();
        neighbour(from, &others, dir)
    }

    /// Tiled: trades places with the neighbour that way. Free: snaps to that half of the stage.
    fn move_window(&mut self, dir: Dir, cx: &mut Context<Self>) {
        let Some(active) = self.active_tab_id() else {
            return;
        };
        let session = self.model.lock().active_session_id.clone();
        let group = self.group_id(&active);
        if self.tiles.get(&session).is_some_and(|tree| tree.contains(&group)) {
            if let Some(theirs) = self.window_toward(dir).map(|id| self.group_id(&id)) {
                if let Some(tree) = self.tiles.get_mut(&session) {
                    tree.swap(&group, &theirs);
                }
            }
        } else if let (Some(stage), Some(snap)) = (self.stage_bounds, match dir {
            Dir::Left => Some(Snap::Left),
            Dir::Right => Some(Snap::Right),
            Dir::Up | Dir::Down => None,
        }) {
            let fitted = self.snap_frame(snap, &active, f64::from(stage.size.width) as f32, f64::from(stage.size.height) as f32);
            if let Some(entry) = self.chrome.get_mut(&active) {
                if entry.snap.is_none() && !entry.maximized {
                    entry.restore = entry.frame;
                }
                (entry.snap, entry.frame, entry.maximized) = (Some(snap), Some(fitted), false);
            }
        }
        self.place_windows();
        cx.notify();
    }

    /// Switches to the next space (`step` 1) or the previous one (-1), in the order the spaces bar shows.
    fn step_space(&mut self, step: isize) {
        if self.model.detached.is_some() {
            return;
        }
        let next = {
            let model = self.model.lock();
            let detached = [model.settings.detached_spaces.as_slice(), model.settings.detached_windows.as_slice()].concat();
            let ids = model.profile_sessions().into_iter().filter(|s| !detached.contains(&s.id)).map(|s| s.id.clone()).collect::<Vec<_>>();
            let at = ids.iter().position(|id| *id == model.active_session_id).unwrap_or(0) as isize;
            ids.get((at + step).rem_euclid(ids.len().max(1) as isize) as usize).cloned()
        };
        if let Some(id) = next {
            self.with_model(|model| { let _ = model.switch_session(&id); });
        }
    }

    fn tool_button(
        &self,
        id: String,
        icon: gpui::Svg,
        prominent: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &ClickEvent, &mut Context<Self>) + 'static,
    ) -> gpuikit::elements::icon_button::IconButton {
        // Sized to the title bar so the bar sets the height, not its buttons.
        let size = self.pill_h();
        let icon_size = if prominent { size * 0.75 } else { size * 0.65 };
        let button = icon_button(id, icon).box_size(rems(size / 16.)).icon_size(rems(icon_size / 16.));
        button.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| action(this, event, cx)))
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
                    .h(px(22.))
                    .px(px(8.))
                    .gap(px(6.))
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .text_color(if active { paint.fg } else { paint.muted })
                    .when(active, |chip| chip.bg(paint.secondary))
                    .when(!active, |chip| chip.hover(|style| style.bg(paint.secondary.opacity(0.5)).text_color(paint.fg)))
                    .when(closable || (self.model.detached.is_some() && !self.tab_window), |chip| {
                        let watch = cx.entity();
                        let (space, name) = (session.id.clone(), session.name.clone());
                        chip.on_drag(SpaceDrag, move |_, _, _, cx| {
                            watch.update(cx, |view, _| view.space_drag = Some(space.clone()));
                            let colors = crate::window::paint(cx);
                            cx.new(|_| SpaceDragPreview(name.clone().into(), colors))
                        })
                    })
                    .child(canvas(move |bounds, _, cx| watch.update(cx, |view, _| view.space_chips.push((chip_id, bounds))), |_, _, _, _| {}).absolute().size_full())
                    .child(favicon_stack(&members, if active { paint.secondary } else { paint.surface }))
                    .map(|chip| match self.space_rename.as_deref() == Some(session.id.as_str()) {
                        true => chip.child(input(&self.space_input, cx).w(px(96.)).text_size(px(12.)).text_color(paint.fg)),
                        false => chip.child(session.name.clone()),
                    })
                    .when(closable, |chip| {
                        chip.child(
                            div()
                                .id(SharedString::from(format!("space-close-{close_id}")))
                                .absolute()
                                .top(px(-4.))
                                .left(px(-4.))
                                .size(px(14.))
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
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            let name = this.model.lock().sessions.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
                            this.space_input.update(cx, |input, cx| input.set_content_silent(name, cx));
                            this.space_rename = Some(id.clone());
                            this.engine.focus_chrome();
                            let field = this.space_input.clone();
                            window.defer(cx, move |window, cx| window.focus(&field.focus_handle(cx), cx));
                            cx.notify();
                            return;
                        }
                        this.with_model(|model| { let _ = model.switch_session(&id); });
                        this.refresh_url(cx);
                    })),
            );
        }
        if self.model.detached.is_none() {
            spaces = spaces.child(self.tool_button("new-space".to_string(), Icons::plus(), false, cx, |this, _, cx| {
                this.with_model(|model| { let _ = model.create_session(); });
                this.refresh_url(cx);
            }));
        }
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
            .when(self.model.detached.is_none(), |bar| {
                // Where torn-off windows and spaces are dropped to come back.
                let main = cx.entity().downgrade();
                bar.child(canvas(move |bounds, window, _| MAIN_BAR.with(|cell| *cell.borrow_mut() = Some((main, gpui::Window::window_handle(window), bounds))), |_, _, _, _| {}).absolute().size_full())
            })
            .when_some(self.tab_window.then(|| self.window_front(all_tabs, active_session)).flatten(), |bar, (front, _)| {
                bar.child(self.nav_controls(&front, cx))
            })
            // Equal flex on both sides keeps the spaces centred in the bar.
            .child(self.bar_area(cx).flex_1())
            .child(if self.tab_window { self.window_tabs(all_tabs, active_session, cx) } else { spaces })
            .child(
                h_stack()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .gap(px(2.))
                    .child(self.bar_area(cx).flex_1())
                    // Torn-off windows: the visible way back, besides dropping the title bar on the main one.
                    .when(self.model.detached.is_some(), |row| {
                        row.child(
                            icon_button("merge-into-main", Icons::exit())
                                .box_size(rems(1.375))
                                .icon_size(rems(1.0))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.merge_into_main(window, cx))),
                        )
                    })
                    .child(self.pinned_extensions(cx))
                    .child(icon_button("find-toggle", Icons::magnifying_glass()).box_size(rems(1.375)).icon_size(rems(0.875)).selected(self.find_open).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.find_open = !this.find_open;
                        this.focus_find = this.find_open;
                        cx.notify();
                    })))
                    .child(
                        icon_button("history-toggle", Icons::file_text())
                            .box_size(rems(1.375))
                            .icon_size(rems(1.0))
                            .selected(self.popover == Some(Popover::History) || self.history_modal)
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| this.toggle_popover(Popover::History, event, cx))),
                    )
                    .child(
                        icon_button("bookmarks-toggle", Icons::bookmark())
                            .box_size(rems(1.375))
                            .icon_size(rems(0.875))
                            .selected(self.popover == Some(Popover::Bookmarks))
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| this.toggle_popover(Popover::Bookmarks, event, cx))),
                    )
                    .child(
                        icon_button("extensions-toggle", gpui::svg().path(crate::PUZZLE_ICON))
                            .box_size(rems(1.375))
                            .icon_size(rems(1.0))
                            .selected(self.popover == Some(Popover::Extensions))
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| this.toggle_popover(Popover::Extensions, event, cx))),
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
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        let shift = event.modifiers().shift;
                        this.with_model(|model| { let _ = model.create_tab(&if shift { TERMINAL_URL.into() } else { model.settings.new_tab_url() }); });
                        this.focus_address = !shift;
                        this.refresh_url(cx);
                    })),
            )
            .child(
                tile("dock-tile".into(), DOCK_NEW_W)
                    .hover(|style| style.bg(ink.opacity(0.08)))
                    // Tiles while the space auto-tiles; overlapping squares while its windows float freely.
                    .child(if self.free_space() { Icons::stack() } else { Icons::layout() }.size(px(14.)).text_color(ink.opacity(0.7)))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        let session = this.model.lock().active_session_id.clone();
                        this.toggle_free(&session, cx);
                    })),
            );
        // Every child has a fixed width, so the island opens to exactly fit its content.
        let shelf_w = DOCK_PAD * 2.0 + count as f32 * (DOCK_ITEM_W + DOCK_GAP) + 1.0 + (DOCK_GAP + DOCK_NEW_W) * 2.0;
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

    /// Empty title bar space. In a torn-off window, dragging it onto the main window's bar merges the two.
    fn bar_area(&self, cx: &mut Context<Self>) -> gpui::Div {
        if self.model.detached.is_none() {
            return drag_area();
        }
        div().h_full().on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| {
            if event.click_count == 2 {
                window.titlebar_double_click();
                return;
            }
            let before = window.bounds().origin;
            // AppKit runs the move in its own loop and returns on release; the grab point stays put in the window.
            window.start_window_move();
            let after = window.bounds().origin;
            if after != before && over_main_bar(after + event.position, cx) {
                this.merge_into_main(window, cx);
            }
        }))
    }

    /// The torn-off window's tabs, with back/forward and new tab, in the title bar's centre.
    fn window_tabs(&self, all_tabs: &[browser_core::Tab], session: &str, cx: &mut Context<Self>) -> gpui::Div {
        let Some((front, members)) = self.window_front(all_tabs, session) else {
            return h_stack();
        };
        let new_tab_id = front.clone();
        let new_tab = self.tool_button(format!("new-tab-{new_tab_id}"), Icons::plus(), false, cx, move |this, event, cx| {
            this.click_new_tab(&new_tab_id, event.modifiers().shift, cx);
        });
        h_stack()
            .flex_none()
            .h_full()
            .items_center()
            .gap(px(8.))
            .child(self.compact_tabs(&front, &members, cx))
            .child(new_tab)
    }

    /// The shown tab of a torn-off window and all its tabs in order.
    fn window_front<'a>(&self, all_tabs: &'a [browser_core::Tab], session: &str) -> Option<(String, Vec<&'a browser_core::Tab>)> {
        let mut members = all_tabs.iter().filter(|tab| tab.session_id == session).collect::<Vec<_>>();
        members.sort_by_key(|tab| tab.position);
        let active = self.active_tab_id();
        let front = members.iter().find(|tab| Some(&tab.id) == active.as_ref()).or(members.first())?.id.clone();
        Some((front, members))
    }

    /// Gives this torn-off window's pages and layout back to the main window and closes this one.
    fn merge_into_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(own) = self.model.detached.clone() else {
            return;
        };
        let Some(main) = MAIN_BAR.with(|cell| cell.borrow().as_ref().and_then(|(main, ..)| main.upgrade())) else {
            return;
        };
        let ids = self.model.lock().tabs.iter().filter(|tab| tab.session_id == own).map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let pages = ids.iter().filter_map(|id| Some((id.clone(), self.engine.take_tab(id)?))).collect::<Vec<_>>();
        let mut chrome = ids.iter().filter_map(|id| Some((id.clone(), self.chrome.remove(id)?))).collect::<HashMap<_, _>>();
        if self.tab_window {
            for entry in chrome.values_mut() {
                entry.maximized = false;
                if let Some((frame, snap, restore)) = entry.home.take() {
                    (entry.frame, entry.snap, entry.restore) = (Some(frame), snap, restore);
                }
            }
        }
        let tab_window = self.tab_window;
        window.remove_window();
        main.update(cx, |main, cx| main.take_back(&own, tab_window, pages, chrome, cx));
    }

    /// Main window: takes back a torn-off space, or a torn-off window into the space it shows now.
    fn take_back(&mut self, space: &str, tab_window: bool, pages: Vec<(String, LiveTab)>, chrome: HashMap<String, TabChrome>, cx: &mut Context<Self>) {
        let first = pages.first().map(|(id, _)| id.clone());
        for (id, page) in pages {
            self.engine.adopt_tab(&id, page);
            self.order.push(id);
        }
        self.chrome.extend(chrome);
        let _ = self.model.lock().update_settings(|s| {
            s.detached_spaces.retain(|id| id != space);
            s.detached_windows.retain(|id| id != space);
        });
        if !tab_window {
            self.with_model(|model| {
                let _ = model.switch_session(space);
            });
            self.refresh_url(cx);
            return;
        }
        let profile = self.model.lock().sessions.iter().find(|s| s.id == space).map(|s| s.profile_id.clone());
        if let Some(profile) = profile.filter(|profile| *profile != self.model.lock().active_profile_id) {
            self.with_model(|model| {
                let _ = model.switch_profile(&profile);
            });
        }
        self.with_model(|model| {
            let here = model.active_session_id.clone();
            let ids = model.tabs.iter().filter(|tab| tab.session_id == space).map(|tab| tab.id.clone()).collect::<Vec<_>>();
            // Tabs move out of the active space only, so point at the hidden one for the move.
            let _ = model.set_active(space);
            let _ = model.move_tabs_to_session(&ids, &here);
            let _ = model.set_active(&here);
            let _ = model.delete_session(space);
        });
        if let Some(id) = first {
            self.focus_window(&id, cx);
        }
        self.refresh_url(cx);
    }

    /// Moves the window holding `id` into a new space and opens that space in an OS window at `at` (screen point).
    fn detach_window(&mut self, id: &str, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let group = self.group_id(id);
        let ids = self.chrome.keys().filter(|other| self.group_id(other) == group).cloned().collect::<Vec<_>>();
        let Ok(space) = self.with_model(|model| model.create_empty_session()) else {
            return;
        };
        self.with_model(|model| {
            let _ = model.move_tabs_to_session(&ids, &space);
        });
        self.open_space_window(&space, true, at, cx);
    }

    /// Hands `space` with its live pages and window layout to a new OS window at `at` (screen point).
    /// `tab_window`: `space` is a hidden one holding a torn-off window.
    fn open_space_window(&mut self, space: &str, tab_window: bool, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if self.model.detached.is_some() {
            return;
        }
        let (others, ids) = {
            let model = self.model.lock();
            let detached = &model.settings.detached_spaces;
            let others = model.profile_sessions().iter().filter(|s| s.id != space && !detached.contains(&s.id)).map(|s| s.id.clone()).collect::<Vec<_>>();
            let ids = model.tabs.iter().filter(|tab| tab.session_id == space).map(|tab| tab.id.clone()).collect::<Vec<_>>();
            (others, ids)
        };
        // The main window always keeps a space of its own.
        let Some(fallback) = others.first().cloned() else {
            return;
        };
        if self.model.lock().active_session_id == space {
            self.with_model(|model| {
                let _ = model.switch_session(&fallback);
            });
        }
        let _ = self.model.lock().update_settings(|s| match tab_window {
            true => s.detached_windows.push(space.to_string()),
            false => s.detached_spaces.push(space.to_string()),
        });
        let pages = ids.iter().filter_map(|id| Some((id.clone(), self.engine.take_tab(id)?))).collect::<Vec<_>>();
        let chrome = ids.iter().filter_map(|id| Some((id.clone(), self.chrome.remove(id)?))).collect::<HashMap<_, _>>();
        self.order.retain(|id| !ids.contains(id));
        let (model, extensions, space) = (self.model.shared.clone(), self.engine.extensions().clone(), space.to_string());
        // A grab near the title bar's left end, so the new window lands under the pointer.
        let at = at - gpui::point(px(80.), px(16.));
        cx.defer(move |cx| open_space(cx, model, extensions, space, tab_window, pages, chrome, at));
        self.refresh_url(cx);
    }

    /// Opens a new tab inside the window holding `id`, sharing its frame. `None` opens the new-tab page
    /// with the address field focused.
    pub(crate) fn new_tab_in(&mut self, id: &str, url: Option<&str>, cx: &mut Context<Self>) {
        let group = self.group_id(id);
        let mut chrome = self.chrome.get(id).cloned().unwrap_or_default();
        chrome.window = Some(group.clone());
        let created = self.with_model(|model| model.create_tab_beside(&url.map_or_else(|| model.settings.new_tab_url(), str::to_string), id));
        let Ok(new) = created else {
            return;
        };
        self.chrome.insert(new.clone(), chrome);
        self.focus_window(&new, cx);
        self.focus_address = url.is_none();
    }

    /// The + button: a new tab in the window holding `id`, or a terminal with Shift held.
    fn click_new_tab(&mut self, id: &str, shift: bool, cx: &mut Context<Self>) {
        if shift {
            self.focus_window(id, cx);
            self.on_shortcut(Shortcut::NewTerminal, cx);
        } else {
            self.new_tab_in(id, None, cx);
        }
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
        let group = Uuid::new_v4().to_string();
        chrome.window = Some(group.clone());
        // It tiles beside the tile it was dropped on; dropped elsewhere, it splits the largest one.
        chrome.snap = (!self.free_space()).then_some(Snap::Tiled);
        let session = self.model.lock().active_session_id.clone();
        if let Some((target, side)) = self.tile_under(f64::from(at.x - stage.origin.x) as f32, f64::from(at.y - stage.origin.y) as f32, "") {
            if let Some(tree) = self.tiles.remove(&session) {
                self.tiles.insert(session, tree.insert(&target, &group, side));
            }
        }
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
                .border_color(self.profile_accent(&id, paint))
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

    /// How far `id` is still drawn from its slot while it slides into place.
    fn slide_offset(&self, id: &str) -> gpui::Point<Pixels> {
        self.slides.get(id).map_or_else(gpui::Point::default, |(from, at)| {
            let left = 1.0 - ease(at.elapsed().as_secs_f32() / SLIDE_ANIM.as_secs_f32());
            gpui::point(from.x * left, from.y * left)
        })
    }

    /// Records where `id` is laid out this frame, without its slide offset.
    fn measure_slot(&self, id: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let (watch, id, offset) = (cx.entity(), id.to_string(), self.slide_offset(id));
        canvas(
            move |bounds, _, cx| watch.update(cx, |view, _| { view.slots.insert(id, gpui::Bounds { origin: bounds.origin - offset, size: bounds.size }); }),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// Slides a row's items from where they are drawn in `before` to their places in `after`, laid out `gap` apart.
    /// Items new to the row just appear: the only one is the invisible gap for a dragged item.
    fn slide_row(&mut self, before: &[String], after: &[String], gap: f32, vertical: bool) {
        if self.reduce_motion {
            return;
        }
        let along = |p: gpui::Point<Pixels>| if vertical { p.y } else { p.x };
        let start = before.iter().filter_map(|id| self.slots.get(id)).map(|slot| along(slot.origin)).reduce(|a, b| if b < a { b } else { a });
        let (Some(mut next), Some(size)) = (start, self.reorder.as_ref().map(|state| state.size)) else {
            return;
        };
        let now = Instant::now();
        for id in after {
            // A dragged item new to the row has no slot here yet, but its size is known.
            let slot = self.slots.get(id).copied().filter(|_| before.contains(id));
            let target = match slot {
                Some(slot) if vertical => gpui::point(slot.origin.x, next),
                Some(slot) => gpui::point(next, slot.origin.y),
                None => gpui::Point::default(),
            };
            let span = slot.map_or(size, |slot| slot.size);
            next += if vertical { span.height } else { span.width } + px(gap);
            match slot {
                Some(slot) => {
                    let from = slot.origin + self.slide_offset(id);
                    self.slides.insert(id.clone(), (from - target, now));
                }
                None => {
                    self.slides.remove(id);
                }
            }
        }
    }

    /// Whether `id` is lifted out of its row by a drag, leaving its gap behind.
    fn picked(&self, id: &str, cx: &Context<Self>) -> bool {
        cx.has_active_drag() && self.reorder.as_ref().is_some_and(|state| state.id == id)
    }

    /// Tab rows of the windows on show, back to front, each in its tab order.
    fn tab_rows(&self) -> Vec<(String, Vec<String>)> {
        let tabs = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let mut groups = Vec::<String>::new();
        for id in tabs.iter().chain(&self.order) {
            let group = self.group_id(id);
            groups.retain(|other| *other != group);
            groups.push(group);
        }
        groups
            .into_iter()
            .map(|group| {
                let ids = tabs.iter().filter(|id| self.group_id(id) == group).cloned().collect();
                (group, ids)
            })
            .collect()
    }

    /// Opens a gap under the dragged `id` in whichever of `rows` it is over (the front one wins),
    /// sliding that row's items aside and closing the gap in the row it left.
    /// `origin` is where the dragged item's top-left is drawn; its centre decides the gap.
    fn preview_drag(&mut self, id: &str, origin: gpui::Point<Pixels>, rows: Vec<(String, Vec<String>)>, vertical: bool, gap: f32) {
        let old = self.reorder.take().filter(|state| state.id == id);
        let Some(size) = old.as_ref().map(|state| state.size).or_else(|| self.slots.get(id).map(|slot| slot.size)) else {
            return;
        };
        let along = |p: gpui::Point<Pixels>| if vertical { p.y } else { p.x };
        let across = |p: gpui::Point<Pixels>| if vertical { p.x } else { p.y };
        let probe = origin + gpui::point(size.width / 2., size.height / 2.);
        let (length, depth) = if vertical { (size.height, size.width) } else { (size.width, size.height) };
        let mut target = None;
        for (key, ids) in &rows {
            let shown = shown_row(old.as_ref(), key, ids);
            let Some(slots) = shown.iter().map(|other| self.slots.get(other).copied()).collect::<Option<Vec<_>>>() else {
                continue;
            };
            let Some(first) = slots.first() else {
                continue;
            };
            let (mut lo, mut hi, mut top, mut bottom) = (along(first.origin), along(first.origin), across(first.origin), across(first.origin));
            for slot in &slots {
                let (start, side) = (slot.origin, if vertical { (slot.size.height, slot.size.width) } else { (slot.size.width, slot.size.height) });
                lo = if along(start) < lo { along(start) } else { lo };
                hi = if along(start) + side.0 > hi { along(start) + side.0 } else { hi };
                top = if across(start) < top { across(start) } else { top };
                bottom = if across(start) + side.1 > bottom { across(start) + side.1 } else { bottom };
            }
            let over = along(probe) > lo - length / 2. && along(probe) < hi + length / 2. && across(probe) > top - depth / 2. && across(probe) < bottom + depth / 2.;
            if over {
                let mut order = shown.into_iter().filter(|other| other != id).collect::<Vec<_>>();
                let index = order.iter().filter(|other| self.slots.get(*other).is_some_and(|slot| along(slot.center()) < along(probe))).count();
                order.insert(index, id.to_string());
                target = Some((key.clone(), order));
            }
        }
        let (row, order) = target.map_or((None, Vec::new()), |(key, order)| (Some(key), order));
        self.reorder = Some(Reorder { id: id.to_string(), size, at: origin, row, order });
        for (key, ids) in &rows {
            let before = shown_row(old.as_ref(), key, ids);
            let after = shown_row(self.reorder.as_ref(), key, ids);
            if before != after {
                self.slide_row(&before, &after, gap, vertical);
            }
        }
    }

    /// The order the drag of `id` showed for row `key`, once it is dropped there.
    fn dropped_order(&mut self, id: &str, key: &str) -> Option<Vec<String>> {
        self.reorder.take().filter(|state| state.id == id && state.row.as_deref() == Some(key)).map(|state| state.order)
    }

    /// Safari compact: a click selects a tab, a click on the selected one edits its address.
    fn click_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        let edit = self.is_front(id) && self.active_tab_id().as_deref() == Some(id);
        self.focus_window(id, cx);
        if edit {
            self.focus_address = true;
        }
        cx.notify();
    }

    /// A tab dropped on a window lands where the drag showed it in that window's tabs.
    fn drop_tab_on(&mut self, id: &str, target: &str, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let group = self.group_id(target);
        let own = self.group_id(id) == group;
        if own && self.tab_press.take().is_some_and(|press| (at - press).magnitude() < TAP_SLOP) {
            self.reorder = None;
            self.click_tab(id, cx);
            return;
        }
        let order = self.dropped_order(id, &group);
        self.reorder = None;
        // Only the tab row takes a tab in; dropped on the window's body it becomes a window of its own.
        if order.is_none() && self.group_len(id) > 1 {
            self.detach_tab(id, at, cx);
            return;
        }
        if !own && order.is_some() {
            self.move_tab_to_window(id, target, cx);
        }
        if let Some(order) = &order {
            // The window's row is a slice of the space's tab order: land before the next tab in it, or after the one before.
            let mut rest = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
            rest.retain(|other| other != id);
            let at_row = order.iter().position(|other| other == id).unwrap_or(0);
            let index = match (order.get(at_row + 1), at_row.checked_sub(1).and_then(|before| order.get(before))) {
                (Some(next), _) => rest.iter().position(|other| other == next),
                (None, Some(before)) => rest.iter().position(|other| other == before).map(|index| index + 1),
                (None, None) => None,
            };
            if let Some(index) = index {
                self.with_model(|model| { let _ = model.move_tab(id, index); });
            }
        }
        // Its slot is the gap it was dropped into, or its own when dropped off the row of its own window.
        if order.is_some() || own {
            self.land(id, at - self.tab_grab.get());
        }
        cx.notify();
    }

    /// Glides a dropped item from where its drag preview was into its slot; the others already sit in theirs.
    fn land(&mut self, id: &str, from: gpui::Point<Pixels>) {
        if let Some(slot) = self.slots.get(id).filter(|_| !self.reduce_motion) {
            self.slides.insert(id.to_string(), (from - slot.origin, Instant::now()));
        }
    }

    /// A profile dropped on the rail lands where the drag showed it.
    fn drop_profile(&mut self, id: &str, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(order) = self.dropped_order(id, RAIL_ROW) {
            let index = order.iter().position(|other| other == id).unwrap_or(0);
            self.with_model(|model| { let _ = model.move_profile(id, index); });
        }
        self.reorder = None;
        self.land(id, at - self.rail_grab.get());
        cx.notify();
    }

    /// The tab pill following the pointer while it is dragged.
    fn tab_drag_view(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (tab, at) = self.tab_drag.as_ref()?;
        let paint = paint(cx);
        let at = *at - self.tab_grab.get();
        let width = self.reorder.as_ref().map_or(px(160.), |state| state.size.width);
        Some(
            h_stack()
                .absolute()
                .left(at.x)
                .top(at.y)
                .h(px(self.pill_h()))
                .w(width)
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

    /// A speaker after the title while the tab plays sound or is muted; clicking it toggles mute.
    fn audio_button(&self, tab: &browser_core::Tab, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let muted = self.engine.is_muted(&tab.id);
        if !muted && !self.playing.contains(&tab.id) {
            return None;
        }
        let paint = paint(cx);
        let id = tab.id.clone();
        let icon = if muted { Icons::speaker_off() } else { Icons::speaker_loud() };
        Some(
            div()
                .id(SharedString::from(format!("ctab-audio-{id}")))
                .flex_none()
                .size(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .hover(|style| style.bg(paint.border))
                .child(icon.size(px(12.)).text_color(paint.fg))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let mute = !this.engine.is_muted(&id);
                    this.engine.set_muted(&id, mute);
                    cx.notify();
                })),
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
            || self.toast.1.elapsed() < TOAST_IN
            || self.toast_fading()
            || self.load_moving()
            || !self.opened.is_empty()
            || !self.slides.is_empty()
            || self.lift.running()
            || self.landing.is_some()
    }

    /// From one tick before the toast's fade-out until it is gone (the tick is longer than the fade).
    fn toast_fading(&self) -> bool {
        let age = self.toast.1.elapsed();
        age + TICK > TOAST_LIFE - TOAST_IN && age < TOAST_LIFE
    }

    /// The load bar is still easing toward the page's progress, or fading out after it finished.
    fn load_moving(&self) -> bool {
        let target = self.load_seen.map_or(1.0, |progress| (progress as f32).max(0.08));
        self.load_bar > 0.0 && (self.load_done.is_some() || (target - self.load_bar).abs() > 0.002)
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

    /// "site wants to use your camera" with Allow / Don't Allow, floating at the top of the window while WebKit waits.
    fn permission_prompt(&self, cx: &mut Context<Self>) -> gpui::Div {
        let Some((request, origin, kind)) = self.permission.clone() else {
            return div();
        };
        let paint = paint(cx);
        let what = match kind.as_str() {
            "microphone" => "your microphone",
            "camera-microphone" => "your camera and microphone",
            _ => "your camera",
        };
        let host = origin.split("://").nth(1).unwrap_or(&origin).to_string();
        let answer = move |allow: bool| {
            let (origin, kind) = (origin.clone(), kind.clone());
            move |this: &mut Self, cx: &mut Context<Self>| {
                let _ = this.model.lock().set_permission(&origin, &kind, if allow { "allow" } else { "deny" });
                this.sync_permissions();
                this.engine.answer_permission(request, allow);
                this.permission = None;
                cx.notify();
            }
        };
        div().absolute().top(px(self.titlebar_h() + 10.)).left_0().right_0().flex().justify_center().child(
            v_stack()
                .relative()
                .w(px(340.))
                .p(px(14.))
                .gap(px(10.))
                .rounded(px(10.))
                .bg(paint.surface)
                .border_1()
                .border_color(paint.border)
                .shadow_md()
                .text_size(px(13.))
                .occlude()
                .child(measure_overlay(10., cx))
                .child(format!("{host} wants to use {what}."))
                .child(
                    h_stack()
                        .gap(px(8.))
                        .justify_end()
                        .child(action_button("deny-permission", "Don't Allow", Kind::Secondary, true, cx, answer(false)))
                        .child(action_button("allow-permission", "Allow", Kind::Primary, true, cx, answer(true))),
                ),
        )
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
    /// A link preview floating over the stage; Escape closes it, "Open as window" keeps it.
    #[serde(default)]
    peek: bool,
    /// Frame, snap and restore frame it had in the main window before being torn off, given back when it returns.
    #[serde(default)]
    home: Option<(Frame, Option<Snap>, Option<Frame>)>,
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
    /// Other tiled windows and their frames when a resize began; the ones across a dragged border make room.
    partners: Vec<(String, Frame)>,
    /// Pointer in window coordinates.
    pointer: (f32, f32),
    /// Snap, frame and restore frame of a snapped window when its move began, handed to the window it swaps with.
    from: Option<(Snap, Frame, Option<Frame>)>,
    /// Snapped window under the pointer that the dragged one would trade places with on release.
    swap: Option<String>,
    /// Tile (window group) a dragged tiled window would be put beside on release, and on which side.
    insert: Option<(String, Side)>,
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
    /// Placed by the tiler; follows the stage like a snap, and its neighbours share borders.
    Tiled,
}

impl Snap {
    /// (column, row): -1 left/top, 1 right/bottom, 0 spanning.
    fn place(self) -> (i8, i8) {
        match self {
            Snap::Maximize | Snap::Tiled => (0, 0),
            Snap::Left => (-1, 0),
            Snap::Right => (1, 0),
            Snap::TopLeft => (-1, -1),
            Snap::TopRight => (1, -1),
            Snap::BottomLeft => (-1, 1),
            Snap::BottomRight => (1, 1),
        }
    }

    /// Stage-local frame for this snap, laid out with the same inset as a maximized window.
    fn frame(self, sw: f32, sh: f32) -> Frame {
        let full_w = (sw - SNAP_INSET * 2.0).max(1.0);
        let full_h = (sh - SNAP_INSET * 2.0).max(1.0);
        let half_w = ((full_w - SNAP_INSET) / 2.0).max(1.0);
        let half_h = ((full_h - SNAP_INSET) / 2.0).max(1.0);
        let right = SNAP_INSET * 2.0 + half_w;
        let bottom = SNAP_INSET * 2.0 + half_h;
        let (x, y, w, h) = match self {
            Snap::Maximize | Snap::Tiled => (SNAP_INSET, SNAP_INSET, full_w, full_h),
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
/// How far a pressed tab pill may wobble and still count as a click rather than a drag.
const TAP_SLOP: f64 = 6.0;
/// How far past the app window's edge a dragged window must be released to tear off, so snaps near the edge stay snaps.
const TEAR_OFF_MARGIN: f32 = 48.0;
/// On a side edge, this close to the top or bottom snaps to a quarter.
const SNAP_CORNER: f32 = 72.0;
const SNAP_INSET: f32 = 8.0;
/// Window edges within this distance stick to the stage or a neighbour.
const MAGNET: f32 = 12.0;
const RESTORE_DRAG: f32 = 6.0;
/// Smallest size a resize shrinks a window to.
const MIN_W: f32 = 220.0;
/// How far down and right of the last window a new free window opens.
const CASCADE: f32 = 28.0;
const MIN_H: f32 = 120.0;

/// The frame for `snap`, fitted to the space windows already snapped opposite leave free,
/// so snapping beside a resized half fills the rest instead of overlapping it or leaving a gap.
/// Falls back to the plain snap frame when that space would be smaller than a window may be.
fn fit_snap(snap: Snap, sw: f32, sh: f32, others: &[(Snap, Frame)]) -> Frame {
    let base = snap.frame(sw, sh);
    let (column, row) = snap.place();
    if column == 0 {
        return base;
    }
    let (mut lead_x, mut trail_x, mut lead_y, mut trail_y) = (None::<f32>, None::<f32>, None::<f32>, None::<f32>);
    for (other, o) in others {
        let (o_column, o_row) = other.place();
        let beside_y = o.y < base.y + base.h && base.y < o.y + o.h;
        let beside_x = o.x < base.x + base.w && base.x < o.x + o.w;
        if o_column == -column && beside_y {
            if column > 0 {
                lead_x = Some(lead_x.map_or(o.x + o.w + SNAP_INSET, |x| x.max(o.x + o.w + SNAP_INSET)));
            } else {
                trail_x = Some(trail_x.map_or(o.x - SNAP_INSET, |x| x.min(o.x - SNAP_INSET)));
            }
        }
        if row != 0 && o_column == column && o_row == -row && beside_x {
            if row > 0 {
                lead_y = Some(lead_y.map_or(o.y + o.h + SNAP_INSET, |y| y.max(o.y + o.h + SNAP_INSET)));
            } else {
                trail_y = Some(trail_y.map_or(o.y - SNAP_INSET, |y| y.min(o.y - SNAP_INSET)));
            }
        }
    }
    let (x, y) = (lead_x.unwrap_or(base.x), lead_y.unwrap_or(base.y));
    let (r, b) = (trail_x.unwrap_or(base.x + base.w), trail_y.unwrap_or(base.y + base.h));
    let fitted = Frame { x, y, w: r - x, h: b - y };
    if fitted.w < MIN_W || fitted.h < MIN_H { base } else { fitted }
}

/// Fits a snapped frame to a resized stage. Borders on the stage inset stay there; inner borders keep
/// their proportional place, measured at the middle of the gap so neighbours stay one inset apart.
fn rescale_snapped(frame: Frame, ow: f32, oh: f32, sw: f32, sh: f32) -> Frame {
    // A saved stage of size 0 would divide into NaN frames.
    if ow <= 0.0 || oh <= 0.0 {
        return frame;
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Popover {
    Downloads,
    Extensions,
    History,
    Bookmarks,
}

/// What a click on `popover`'s button leaves open. Its press landed outside the popover and so
/// already closed it (`dismissed` there); that click must not reopen it.
fn toggled(open: Option<Popover>, dismissed: Option<(Popover, gpui::Point<Pixels>)>, popover: Popover, press: Option<gpui::Point<Pixels>>) -> Option<Popover> {
    let just_closed = dismissed.is_some_and(|(kind, at)| kind == popover && Some(at) == press);
    (open != Some(popover) && !just_closed).then_some(popover)
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
/// Resize grips reach this far outside the window edge; the page now runs almost to the edge,
/// so the grab area lives mostly in the gap around the window.
const GRIP_OUT: f32 = 6.0;
/// …and this far inside it, over the page's thin border.
const GRIP_IN: f32 = 3.0;
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

const TITLEBAR_H: f32 = 30.0;
const DOCK_ANIM: Duration = Duration::from_millis(260);
const LIFT_ANIM: Duration = Duration::from_millis(220);
const LAND_ANIM: Duration = Duration::from_millis(280);
/// Width a window shrinks to while carried over the spaces bar.
const LIFT_W: f32 = 140.0;
const PANEL_ANIM: Duration = Duration::from_millis(180);
const WINDOW_ANIM: Duration = Duration::from_millis(170);
const SLIDE_ANIM: Duration = Duration::from_millis(220);
const WINDOW_FROM_SCALE: f32 = 0.94;
/// How often each window polls the engine, model and terminals.
const TICK: Duration = Duration::from_millis(250);
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
const TITLE_H: f64 = 26.0;
/// Window border beside and below the page; PAGE_RADIUS in the macOS engine is WINDOW_RADIUS minus this.
const PAGE_INSET: f64 = 1.0;
/// Must match PAGE_RADIUS in the macOS engine, which rounds the page's bottom corners.
const PAGE_RADIUS: f64 = WINDOW_RADIUS - PAGE_INSET;
const FIND_H: f64 = 36.0;

/// Visible parts of each page, as disjoint rects: a page is hidden wherever a window in front of it sits.
/// `stack` is back to front: (tab id, window shell, page rect). Pages never show outside `stage`.
/// Also returns the slivers outside each front window's rounded corners where a back page shows,
/// as (outer corner, arc centre) points.
fn visible_pages(
    stack: &[(String, LogicalRect, LogicalRect)],
    stage: &LogicalRect,
) -> (Vec<Hole>, Vec<Notch>) {
    let mut holes = Vec::new();
    let mut notches = Vec::new();
    for (index, (id, _, full)) in stack.iter().enumerate() {
        let page = full;
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
        // The page's own bottom corners are rounded (PAGE_RADIUS in the engine), so the square hole
        // would show nothing outside the curve; these wedges put the window border back there.
        let (r, bottom) = (PAGE_RADIUS, full.y + full.height);
        for (corner, center) in [((full.x, bottom), (full.x + r, bottom - r)), ((full.x + full.width, bottom), (full.x + full.width - r, bottom - r))] {
            let square = LogicalRect { x: corner.0.min(center.0), y: center.1, width: r, height: r };
            if contains(&page, &square) && !above.iter().any(|(_, shell, _)| overlaps(&square, shell)) {
                notches.push((corner, center));
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
/// The resize rims around resizable windows, minus what windows in front cover. Clicks there go to the
/// grips even over a page. `stack` is back to front, as for `visible_pages`.
fn grab_zones(stack: &[(String, LogicalRect, LogicalRect)], resizable: &[String]) -> Vec<LogicalRect> {
    let (out, inn) = (f64::from(GRIP_OUT), f64::from(GRIP_IN));
    let mut zones = Vec::new();
    for (index, (id, shell, _)) in stack.iter().enumerate() {
        if !resizable.contains(id) {
            continue;
        }
        let (x, y, w, h) = (shell.x - out, shell.y - out, shell.width + out * 2.0, shell.height + out * 2.0);
        let band = out + inn;
        let mut parts = vec![
            LogicalRect { x, y, width: band, height: h },
            LogicalRect { x: x + w - band, y, width: band, height: h },
            LogicalRect { x: x + band, y, width: w - band * 2.0, height: band },
            LogicalRect { x: x + band, y: y + h - band, width: w - band * 2.0, height: band },
        ];
        for (_, front, _) in &stack[index + 1..] {
            parts = parts.iter().flat_map(|part| subtract(part, front)).collect();
        }
        zones.extend(parts);
    }
    zones
}

fn cut_overlay(holes: &mut Vec<Hole>, notches: &mut Vec<Notch>, rect: &LogicalRect, radius: f64) {
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
    // A wedge toggles the mask, so one even partly under the card would cut a sliver out of the card itself.
    notches.retain(|&(corner, center)| {
        let square = LogicalRect {
            x: corner.0.min(center.0),
            y: corner.1.min(center.1),
            width: (corner.0 - center.0).abs(),
            height: (corner.1 - center.1).abs(),
        };
        !overlaps(&square, rect)
    });
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

fn snap_preview(frame: Frame, cx: &mut Context<BrowserWindow>) -> gpui::Div {
    let paint = paint(cx);
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

fn keep_on_stage(frame: Frame, sw: f32, sh: f32) -> Frame {
    let visible = 64.0;
    Frame {
        x: frame.x.clamp(visible - frame.w, (sw - visible).max(visible - frame.w)),
        y: frame.y.clamp(visible - frame.h, (sh - visible).max(visible - frame.h)),
        w: frame.w,
        h: frame.h,
    }
}

fn site_glyph(url: &str) -> (String, String) {
    if is_terminal(url) {
        return (">_".into(), "terminal".into());
    }
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest).trim_start_matches("www.");
    let host = if host.is_empty() { "new tab" } else { host };
    let glyph = host.chars().next().unwrap_or('N').to_uppercase().to_string();
    (glyph, host.to_string())
}

pub(crate) fn site_host(url: &str) -> String {
    site_glyph(url).1
}

/// The site's icon once cached, otherwise a coloured initial.
fn favicon(tab: &browser_core::Tab, round: bool) -> gpui::AnyElement {
    site_icon(&tab.url, 16., round)
}

pub(crate) fn site_icon(url: &str, size: f32, round: bool) -> gpui::AnyElement {
    let (glyph, host) = site_glyph(url);
    let radius = if round { size / 2. } else { (size / 4.).max(4.) };
    let path = favicon_path(&host);
    let tile = move || {
        div()
            .size(px(size))
            .flex_none()
            .rounded(px(radius))
            .flex()
            .items_center()
            .justify_center()
            .bg(site_color(&host))
            .text_color(gpui::rgb(0xFFFFFF))
            .text_size(px((size * 0.55).max(10.)))
            .child(glyph.clone())
            .into_any_element()
    };
    if is_web(url) && path.exists() {
        img(path).size(px(size)).flex_none().rounded(px(radius)).with_fallback(tile).into_any_element()
    } else {
        tile()
    }
}

pub(crate) fn favicon_path(host: &str) -> std::path::PathBuf {
    let name: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    app_support_dir().join("favicons").join(format!("icon-{name}"))
}

/// A new tab: drawn by GPUI as the new-tab page, its web view kept hidden until it navigates.
fn is_blank(url: &str) -> bool {
    url.is_empty() || url == "about:blank" || url == "nxbrowser://home"
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

/// Drag payload for a space chip; the space itself is in `BrowserWindow::space_drag`.
struct SpaceDrag;

struct SpaceDragPreview(SharedString, Paint);

impl Render for SpaceDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(8.)).py(px(3.)).rounded(px(6.)).text_size(px(12.)).bg(self.1.secondary).text_color(self.1.fg).child(self.0.clone())
    }
}

/// What a window torn off the main one brings: its space, the space's live pages and their window layout.
pub(crate) struct Handoff {
    pub(crate) session: String,
    tab_window: bool,
    pub(crate) extensions: Extensions,
    pages: Vec<(String, LiveTab)>,
    chrome: HashMap<String, TabChrome>,
}

thread_local! {
    /// The main window, its handle and its title bar (window coordinates), where torn-off windows are dropped to merge back.
    static MAIN_BAR: std::cell::RefCell<Option<MainBar>> = const { std::cell::RefCell::new(None) };
}

type MainBar = (gpui::WeakEntity<BrowserWindow>, gpui::AnyWindowHandle, gpui::Bounds<Pixels>);
type WindowAction = Box<dyn Fn(&mut BrowserWindow, &mut Context<BrowserWindow>)>;

/// A torn-off window closing: it no longer reopens at launch, and with `delete` its space and tabs go too.
fn forget_detached(model: &mut Model, id: &str, delete: bool) {
    let _ = model.update_settings(|s| {
        s.detached_spaces.retain(|space| space != id);
        s.detached_windows.retain(|space| space != id);
    });
    if delete {
        let _ = model.delete_session(id);
    }
}

/// Whether `position` (window point) is past `TEAR_OFF_MARGIN` outside the window.
fn torn_off(window: &Window, position: gpui::Point<Pixels>) -> bool {
    let margin = px(TEAR_OFF_MARGIN);
    let size = window.viewport_size();
    let area = gpui::Bounds::new(gpui::point(-margin, -margin), gpui::size(size.width + margin * 2., size.height + margin * 2.));
    !area.contains(&position)
}

/// Whether `at` (screen point) is over the main window's title bar.
fn over_main_bar(at: gpui::Point<Pixels>, cx: &mut gpui::App) -> bool {
    let Some((handle, bar)) = MAIN_BAR.with(|cell| cell.borrow().as_ref().map(|(_, handle, bar)| (*handle, *bar))) else {
        return false;
    };
    handle.update(cx, |_, window, _| gpui::Bounds::new(window.bounds().origin + bar.origin, bar.size).contains(&at)).unwrap_or(false)
}

/// Opens `session` in an OS window of its own whose top-left is at `at` (screen point).
#[allow(clippy::too_many_arguments)]
fn open_space(cx: &mut gpui::App, model: SharedModel, extensions: Extensions, session: String, tab_window: bool, pages: Vec<(String, LiveTab)>, chrome: HashMap<String, TabChrome>, at: gpui::Point<Pixels>) {
    let handoff = Handoff { session, tab_window, extensions, pages, chrome };
    crate::open_browser_window(cx, gpui::Bounds::new(at, gpui::size(px(1000.), px(700.))), move |cx| {
        BrowserWindow::new(model, Some(handoff), false, Instant::now(), cx)
    });
}

/// The shared model, seen from one window.
pub(crate) struct WindowModel {
    shared: SharedModel,
    /// Space this window is torn off to show; `None` for the main window, which shows the rail and every space.
    pub(crate) detached: Option<String>,
}

impl WindowModel {
    /// The model pointed at this window's space for as long as the guard lives.
    pub(crate) fn lock(&self) -> Pinned<'_> {
        let mut model = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        let back = self.detached.as_ref().filter(|id| **id != model.active_session_id).map(|id| {
            let back = model.active_session_id.clone();
            let _ = model.set_active(id);
            back
        });
        Pinned { model, back }
    }
}

/// `WindowModel::lock`'s guard; a detached window points the model back at the main window's space on drop.
pub(crate) struct Pinned<'a> {
    model: MutexGuard<'a, Model>,
    back: Option<String>,
}

impl std::ops::Deref for Pinned<'_> {
    type Target = Model;
    fn deref(&self) -> &Model {
        &self.model
    }
}

impl std::ops::DerefMut for Pinned<'_> {
    fn deref_mut(&mut self) -> &mut Model {
        &mut self.model
    }
}

impl Drop for Pinned<'_> {
    fn drop(&mut self) {
        if let Some(back) = self.back.take() {
            let _ = self.model.set_active(&back);
        }
    }
}

/// Drag payload for reordering profiles in the rail.
struct RailDrag(browser_core::Profile);

struct RailDragPreview(browser_core::Profile, Paint);

impl Render for RailDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        profile_badge(&self.0, false, 36., self.1).opacity(0.85)
    }
}

/// The profile's color, or the theme accent when it has none.
pub(crate) fn profile_tint(profile: &browser_core::Profile, paint: Paint) -> gpui::Hsla {
    if profile.color.is_empty() { paint.accent } else { parse_hex(&profile.color) }
}

/// Profile icon (emoji, Radix icon, or initial) on a wash of its color; the active one gets a stronger wash and a ring.
pub(crate) fn profile_badge(profile: &browser_core::Profile, active: bool, size: f32, paint: Paint) -> gpui::Div {
    let tint = profile_tint(profile, paint);
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .rounded(px(size * 0.3))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.45))
        .bg(tint.opacity(if active { 0.3 } else { 0.12 }))
        .border_1()
        .border_color(if active { tint.opacity(0.9) } else { tint.opacity(0.18) })
        .text_color(paint.fg)
        .child(profile_glyph(&profile.icon, &profile.name, size * 0.5, if active { tint } else { paint.fg }))
}

/// Media in use by a profile's pages.
#[derive(Clone, Copy, Default)]
struct Activity {
    camera: bool,
    microphone: bool,
    sound: bool,
}

impl Activity {
    fn any(self) -> bool {
        self.camera || self.microphone || self.sound
    }
}

/// Tiny dots along the badge's bottom edge, like macOS's privacy dots: green camera, orange microphone, and a
/// faint dot for sound.
fn activity_chip(state: Activity, paint: Paint) -> gpui::Div {
    let dot = |color: gpui::Hsla| div().size(px(4.)).rounded_full().bg(color);
    h_stack()
        .absolute()
        .bottom(px(3.))
        .left_0()
        .right_0()
        .justify_center()
        .gap(px(3.))
        .when(state.camera, |row| row.child(dot(parse_hex("#30d158"))))
        .when(state.microphone, |row| row.child(dot(parse_hex("#ff9f0a"))))
        .when(state.sound, |row| row.child(dot(paint.fg.opacity(0.55))))
}

/// Draws an icon value: a `.svg` asset path, an emoji, or (empty) the name's initial.
pub(crate) fn profile_glyph(icon: &str, name: &str, size: f32, color: gpui::Hsla) -> gpui::AnyElement {
    // SVGs don't inherit the parent's text color, so it is passed in.
    if icon.ends_with(".svg") {
        gpui::svg().path(icon.to_string()).size(px(size)).text_color(color).into_any_element()
    } else if icon.is_empty() {
        rail_mark(name).into_any_element()
    } else {
        icon.to_string().into_any_element()
    }
}

pub(crate) fn rail_mark(name: &str) -> String {
    let trimmed = name.trim();
    let word = trimmed.strip_prefix("Profile ").unwrap_or(trimmed);
    word.chars().next().unwrap_or('P').to_uppercase().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_tab_leaves_its_row_for_the_one_it_hovers() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let state = Reorder { id: "b".into(), size: Default::default(), at: Default::default(), row: Some("w2".into()), order: ids(&["x", "b", "y"]) };
        assert_eq!(shown_row(Some(&state), "w1", &ids(&["a", "b", "c"])), ids(&["a", "c"]));
        assert_eq!(shown_row(Some(&state), "w2", &ids(&["x", "y"])), ids(&["x", "b", "y"]));
        let off = Reorder { row: None, ..state };
        assert_eq!(shown_row(Some(&off), "w1", &ids(&["a", "b", "c"])), ids(&["a", "b", "c"]));
    }

    fn r(x: f64, y: f64, width: f64, height: f64) -> LogicalRect {
        LogicalRect { x, y, width, height }
    }

    #[test]
    fn grab_zones_ring_the_window_but_not_what_covers_it() {
        let stack = vec![
            ("back".to_string(), r(100., 100., 200., 100.), r(101., 126., 198., 73.)),
            ("front".to_string(), r(250., 50., 200., 200.), r(251., 76., 198., 173.)),
        ];
        let zones = grab_zones(&stack, &["back".to_string(), "front".to_string()]);
        let hit = |x: f64, y: f64| zones.iter().any(|z| x >= z.x && y >= z.y && x < z.x + z.width && y < z.y + z.height);
        // Just outside and just inside the back window's left edge.
        assert!(hit(96., 150.) && hit(102., 150.));
        // Deep inside a page is never a grip.
        assert!(!hit(150., 150.));
        // The back window's right rim lies under the front window, so the front page keeps those clicks.
        assert!(!hit(299., 150.));
        // The front window's own left rim is a grip.
        assert!(hit(248., 150.));
        // A maximized (non-resizable) window has no rim.
        assert!(grab_zones(&stack, &[]).is_empty());
    }

    #[test]
    fn popover_button_toggles() {
        use Popover::{Bookmarks, History};
        let (here, there) = (Some(gpui::point(px(5.), px(5.))), Some(gpui::point(px(9.), px(5.))));
        // Closed: opens.
        assert_eq!(toggled(None, None, History, here), Some(History));
        // Open: its press closed it, and the click keeps it closed.
        assert_eq!(toggled(None, Some((History, here.unwrap())), History, here), None);
        // Another popover's press closed it: this one opens.
        assert_eq!(toggled(None, Some((History, here.unwrap())), Bookmarks, here), Some(Bookmarks));
        // An older outside press doesn't block opening later.
        assert_eq!(toggled(None, Some((History, there.unwrap())), History, here), Some(History));
        // Keyboard: plain toggle.
        assert_eq!(toggled(Some(History), None, History, None), None);
    }

    #[test]
    fn overlay_drops_page_corner_wedges_it_touches() {
        let stack = vec![("page".to_string(), r(0., 0., 100., 120.), r(1., 26., 98., 93.))];
        let (mut holes, mut notches) = visible_pages(&stack, &r(0., 0., 1000., 1000.));
        assert_eq!(notches.len(), 2);
        // A popover whose edge crosses the bottom-left corner square, but not the corner point.
        cut_overlay(&mut holes, &mut notches, &r(5., 60., 40., 55.), 10.);
        assert!(!notches.contains(&((1., 119.), (10., 110.))));
        assert!(notches.contains(&((99., 119.), (90., 110.))));
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
        // Each page's own rounded bottom corners get a wedge too, unless a front window covers them.
        assert_eq!(notches, vec![
            ((60., 100.), (70., 90.)),
            ((0., 120.), (9., 111.)),
            ((100., 120.), (91., 111.)),
            ((60., 100.), (69., 91.)),
            ((160., 100.), (151., 91.)),
        ]);
        let covered = vec![
            ("back".to_string(), r(0., 0., 100., 120.), r(0., 10., 100., 110.)),
            ("front".to_string(), r(-5., 100., 50., 40.), r(-5., 110., 50., 30.)),
        ];
        assert!(!visible_pages(&covered, &r(0., 0., 1000., 1000.)).1.contains(&((0., 120.), (9., 111.))));
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
    fn snap_fills_space_beside_a_resized_neighbour() {
        let mut left = Snap::Left.frame(1000., 700.);
        left.w = 300.;
        let right = fit_snap(Snap::Right, 1000., 700., &[(Snap::Left, left)]);
        assert_eq!((right.x, right.x + right.w), (left.x + left.w + SNAP_INSET, 992.));
        // A quarter under a shortened quarter in the same column takes the rest of the height.
        let mut top = Snap::TopRight.frame(1000., 700.);
        top.h = 200.;
        let bottom = fit_snap(Snap::BottomRight, 1000., 700., &[(Snap::TopRight, top), (Snap::Left, left)]);
        assert_eq!((bottom.x, bottom.y, bottom.y + bottom.h), (right.x, top.y + top.h + SNAP_INSET, 692.));
        // No room left for a window: fall back to the plain half.
        left.w = 900.;
        assert_eq!(fit_snap(Snap::Right, 1000., 700., &[(Snap::Left, left)]).x, Snap::Right.frame(1000., 700.).x);
        // Neighbours on the same side don't change it.
        assert_eq!(fit_snap(Snap::Left, 1000., 700., &[(Snap::TopLeft, left)]).w, Snap::Left.frame(1000., 700.).w);
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
