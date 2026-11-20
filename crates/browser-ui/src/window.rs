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
use browser_engine::{DialogKind, Dir, EngineEvent, EngineSettings, Hole, LogicalRect, Notch, ScreenshotArea, Shortcut, WebEngine};
use browser_engine_macos::{Extensions, LiveTab, MacEngine, EXTENSION_SCHEME};
use browser_extensions::{discover, install_from_store, local_id, patch_missing_apis, patch_unpacked_copy, read_manifest, store_id, Manifest};
use crate::geom::{point, size, Bounds, Point, Size};
use browser_engine_macos::ShellWindow;
use crate::settings::logical;
use tiling::{cell, move_border, neighbour, set_span, span, uncell, Side, Tile};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[path = "frames.rs"]
mod frames;
use frames::{DragItem, NativeInput};

pub struct BrowserWindow {
    pub(crate) model: WindowModel,
    /// Space chip being dragged, so a release outside the window can open it in its own.
    space_drag: Option<String>,
    /// The OS window this is.
    pub(crate) shell: ShellWindow,
    /// A frame is already on its way.
    scheduled: Cell<bool>,
    /// The shortcut whose new keys settings is waiting for; app shortcuts are paused meanwhile.
    pub(crate) recording: Option<&'static str>,
    /// Takes the keys being recorded.
    /// Each space's tiled windows, keyed by window group, as a split tree laid out over the stage.
    tiles: HashMap<String, Tile>,
    /// Holds one torn-off window instead of a space: its tabs take the spaces' place in the title bar.
    pub(crate) tab_window: bool,
    /// A torn-off window's last tab closed; the window goes on the next render.
    close_window: bool,
    pub(crate) engine: MacEngine,
    /// Store URL or extension id typed in the extensions panel.
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
    /// Searchable font list in the Terminal settings; built on first show since it needs a `Window`.
    pub(crate) settings_open: bool,
    pub(crate) settings_section: SettingsSection,
    /// Filters the settings nav and the shortcut list.
    /// Confirmation shown in the settings footer after an action.
    pub(crate) settings_note: Option<String>,
    pub(crate) settings_profile: Option<String>,
    pub(crate) delete_prompt: Option<String>,
    /// Bounds of open modal cards this frame; web pages are cut out beneath them.
    /// Open overlays this frame (bounds, corner radius); web pages are cut out beneath them.
    pub(crate) overlay_rects: Vec<(LogicalRect, f64)>,
    /// The open title-bar or rail popover; at most one at a time.
    pub(crate) popover: Option<Popover>,
    /// What each control in the open native popover runs, by its action id.
    popover_actions: Vec<crate::settings::MenuAction>,
    /// The same for the open native sheet, whose ids start at `native::SHEET_BASE`.
    sheet_actions: Vec<crate::settings::MenuAction>,
    /// The command palette's handlers, from `native::PANEL_BASE` up.
    panel_actions: Vec<crate::settings::MenuAction>,
    /// The address suggestions' handlers, from `native::LIST_BASE` up.
    list_actions: Vec<crate::settings::MenuAction>,
    pub(crate) home_actions: Vec<crate::settings::MenuAction>,
    /// The native chrome's handlers, from `native::BAR_BASE` up.
    bar_actions: Vec<crate::settings::MenuAction>,
    /// The native title bar tools asked to merge this torn-off window back into the main one.
    pub(crate) merge_requested: bool,
    /// Presses, drags and hovers on native chrome since the last frame.
    native_input: Vec<NativeInput>,
    /// What each native drag id built this frame stands for.
    drag_items: Vec<(u32, DragItem)>,
    /// The native reorder lists shown, by list id.
    reorder_lists: Vec<(u32, frames::ReorderList)>,
    /// How each tab row built this frame should stand for the drag over it: its list, item widths, the item
    /// hidden and the gap open; the engine slides it there once the row is in place.
    pub(crate) list_arrangements: Vec<Arrangement>,
    /// A dropped tab gliding from where its pill was (and that wide) into its slot.
    pub(crate) tab_landing: Option<(String, Point, f64, Instant)>,
    /// The address field was just opened and wants the caret.
    address_caret: bool,
    /// The address field was asked for by clicking the shown tab.
    edit_in_strip: bool,
    /// A new tab page to put the caret in once it is placed.
    home_caret: Option<String>,
    /// A space's rename field was just opened and wants the caret.
    space_caret: bool,
    /// How wide the space chips' capsule was last laid out, to centre it.
    spaces_w: f64,
    /// Where a dragged space chip is.
    space_drag_at: Option<Point>,
    /// How wide the native title bar tools are, kept clear in the title bar.
    pub(crate) toolbar_w: f64,
    /// The main window's dock at the stage's foot: a line at rest, sliding up into the spaces and tools on hover.
    dock: Tween,
    /// Pointer over the dock or its resting line.
    dock_hovered: bool,
    /// Each blank tab's page area (width, height) when last placed, for laying out its new tab page.
    pub(crate) page_sizes: HashMap<String, (f64, f64)>,
    /// The find bar's handlers, from `native::FIND_BASE` up.
    find_actions: Vec<crate::settings::MenuAction>,
    /// The settings window's handlers, from `native::WINDOW_BASE` up.
    window_actions: Vec<crate::settings::MenuAction>,
    /// What is typed in the open popover's text fields, by field id.
    pub(crate) popover_texts: HashMap<u32, String>,
    /// Where each popover's button was last drawn, for the popover to point at.
    pub(crate) popover_anchors: HashMap<Popover, Bounds>,
    pub(crate) history_modal: bool,
    /// The extension whose logs are open in a modal.
    pub(crate) ext_logs: Option<String>,
    /// Links from other apps waiting for the user to pick a profile.
    pub(crate) pending_links: Vec<String>,
    /// The bookmark editor: (bookmark id, pinned); no id adds a new pinned site.
    pub(crate) bookmark_edit: Option<(Option<String>, bool)>,
    /// Icon and color picked in the new-profile modal while it is open.
    pub(crate) new_profile: Option<(String, String)>,
    /// Live (received, total) bytes per download id, fed by the download threads.
    pub(crate) download_progress: HashMap<String, (u64, Option<u64>)>,
    /// What each row of the native menu last shown runs, by index; `None` for separators.
    pub(crate) menu_actions: Vec<Option<crate::settings::MenuAction>>,
    address_focused: bool,
    pub(crate) focus_address: bool,
    /// The active window shows the editable field (focus requested or held); otherwise a plain pill.
    address_editing: bool,
    /// A page was clicked while the address was being edited; end the edit on the next render.
    blur_address: bool,
    find_open: bool,
    /// The tab the find bar belongs to; kept after close so the bar animates shut on that window.
    find_tab: String,
    /// Matches of the find query on the find tab; `None` until the page has counted them.
    find_count: Option<usize>,
    /// Which match is selected, from 1.
    find_index: usize,
    focus_find: bool,
    /// Give keys to the active terminal on the next render.
    focus_terminal: bool,
    pub(crate) palette_open: bool,
    pub(crate) palette_index: usize,
    /// The bang shown as a pill in the palette; the input then holds only the query.
    pub(crate) palette_bang: Option<&'static (&'static str, &'static str, &'static str)>,
    pub(crate) chrome: HashMap<String, TabChrome>,
    gesture: Option<Gesture>,
    suppress_window_drag: bool,
    /// Where the last tab pill was pressed: a 2px wobble starts a drag, which must still count as a click.
    tab_press: Option<Point>,
    pub(crate) stage_bounds: Option<Bounds>,
    layout_stage: Option<(i32, i32)>,
    /// When the stage last changed size; layouts are saved once it has settled, and `tick` saves the last one.
    stage_moved: Instant,
    order: Vec<String>,
    pub(crate) status: String,
    /// Space whose chip is showing the rename field.
    space_rename: Option<String>,
    /// Camera/microphone request the page is waiting on: (request, origin, kind).
    permission: Option<(u64, String, String)>,
    /// Page dialogs waiting on the user, oldest first; only the first one shows.
    dialogs: Vec<PageDialog>,
    pub(crate) passwords: PasswordState,
    /// Downloads WebKit is writing, by engine id: (download id in the model, file path).
    pub(crate) web_downloads: HashMap<u64, (String, String)>,
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
    /// A window carried up to the spaces bar shrinks under the pointer (open = small).
    lift: Tween,
    /// Space chips laid out this frame, so a carried window can be dropped on one.
    space_chips: Vec<(String, Bounds)>,
    /// Rail profiles laid out this frame, so a carried window can be dropped on one.
    pub(crate) rail_badges: Vec<(String, Bounds)>,
    /// A dropped carried window growing back into place: (tab id, rect it was dropped at, started).
    landing: Option<(String, Frame, Instant)>,
    /// A window closed; re-tile the stage on the next layout pass.
    retile: bool,
    /// Tab pill being dragged and the pointer; drawn here so pages can be cut out beneath it.
    tab_drag: Option<(browser_core::Tab, Point)>,
    /// Where tab pills and rail badges were laid out last frame, before any slide offset.
    slots: HashMap<String, Bounds>,
    /// Items sliding into place after a reorder: the offset they start from, and when.
    slides: HashMap<String, (Point, Instant)>,
    /// Where the dragged tab pill was grabbed, so it stays under the pointer at that spot.
    tab_grab: Rc<Cell<Point>>,
    /// A tab or profile being dragged, and the row it would drop into.
    reorder: Option<Reorder>,
    /// Where a link dragged from a page is over the chrome; applied to the tab rows on the next render.
    link_hover: Option<(f64, f64)>,
    /// Where the find bar's page sits (window coordinates), for the bar to hang at its top right.
    find_anchor: Option<LogicalRect>,
    /// Status message shown as a toast, and when it changed.
    toast: (String, Instant),
    /// Load bar width for the active tab, and when its load finished (the bar then fades).
    load_bar: f32,
    load_done: Option<Instant>,
    /// The active tab's load progress at the last tick; a change repaints the bar.
    load_seen: Option<f64>,
    /// Typed bare hosts open over https first; if that https URL fails, the tab retries the http one, as other browsers do.
    http_fallback: HashMap<String, (String, String)>,
    /// Each tab's last finished URL: later reports of the same URL are title changes.
    finished_url: HashMap<String, String>,
    /// New windows growing in.
    opened: HashMap<String, Instant>,
    /// Address bar matches (url or search query, title, is a search) and the one picked with the arrow keys.
    pub(crate) suggestions: Vec<(String, String, bool)>,
    pub(crate) suggestion: Option<usize>,
    /// Counts search-engine completion fetches; an answer to an older one than the last is dropped.
    suggest_seq: u64,
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
    size: Size,
    /// Where the dragged item's top-left is drawn.
    at: Point,
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

/// A tab row's list, item widths, the item hidden, the item cleared and the gap open; see `arrange_list`.
pub(crate) type Arrangement = (u32, Vec<f64>, Option<usize>, Option<usize>, Option<(usize, f64)>);

/// Stands in for the tab a link dragged from a page would become, so a tab row opens a gap for it.
const LINK_DROP: &str = "link-drop";

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
    pub fn new(model: SharedModel, handoff: Option<Handoff>, self_test: bool, started: Instant, shell: ShellWindow) -> Self {
        let (installs_tx, installs_rx) = mpsc::channel();
        let passwords = PasswordState::default();
        let (detached, tab_window, extensions, pages, chrome) = match handoff {
            Some(handoff) => (Some(handoff.session), handoff.tab_window, Some(handoff.extensions), handoff.pages, handoff.chrome),
            None => (None, false, None, Vec::new(), HashMap::new()),
        };
        let mut view = Self {
            model: WindowModel { shared: model, detached },
            space_drag: None,
            shell,
            scheduled: Cell::new(false),
            recording: None,
            tiles: HashMap::new(),
            tab_window,
            close_window: false,
            engine: MacEngine::new(extensions),
            ext_meta: HashMap::new(),
            ext_prompts: Vec::new(),
            installing: Vec::new(),
            installs_tx,
            installs_rx,
            ext_active: None,
            settings_open: false,
            settings_section: SettingsSection::Profiles,
            settings_note: None,
            settings_profile: None,
            delete_prompt: None,
            overlay_rects: Vec::new(),
            menu_actions: Vec::new(),
            popover: None,
            popover_actions: Vec::new(),
            sheet_actions: Vec::new(),
            window_actions: Vec::new(),
            panel_actions: Vec::new(),
            list_actions: Vec::new(),
            home_actions: Vec::new(),
            bar_actions: Vec::new(),
            merge_requested: false,
            native_input: Vec::new(),
            drag_items: Vec::new(),
            reorder_lists: Vec::new(),
            list_arrangements: Vec::new(),
            tab_landing: None,
            address_caret: false,
            home_caret: None,
            edit_in_strip: false,
            space_caret: false,
            spaces_w: 0.,
            space_drag_at: None,
            toolbar_w: 0.,
            dock: Tween::new(false),
            dock_hovered: false,
            page_sizes: HashMap::new(),
            find_actions: Vec::new(),
            popover_texts: HashMap::new(),
            popover_anchors: HashMap::new(),
            history_modal: false,
            ext_logs: None,
            pending_links: Vec::new(),
            bookmark_edit: None,
            new_profile: None,
            download_progress: HashMap::new(),
            address_focused: false,
            focus_address: false,
            address_editing: false,
            blur_address: false,
            find_open: false,
            find_tab: String::new(),
            find_count: None,
            find_index: 1,
            focus_find: false,
            focus_terminal: false,
            palette_open: false,
            palette_index: 0,
            palette_bang: None,
            chrome,
            gesture: None,
            suppress_window_drag: false,
            tab_press: None,
            stage_bounds: None,
            layout_stage: None,
            stage_moved: Instant::now(),
            order: Vec::new(),
            passwords,
            status: "Ready".into(),
            space_rename: None,
            permission: None,
            dialogs: Vec::new(),
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
            lift: Tween::new(false),
            space_chips: Vec::new(),
            rail_badges: Vec::new(),
            landing: None,
            retile: false,
            tab_drag: None,
            slots: HashMap::new(),
            slides: HashMap::new(),
            tab_grab: Rc::default(),
            reorder: None,
            link_hover: None,
            find_anchor: None,
            toast: ("Ready".into(), Instant::now() - TOAST_LIFE),
            load_bar: 0.0,
            load_done: None,
            load_seen: None,
            http_fallback: HashMap::new(),
            finished_url: HashMap::new(),
            opened: HashMap::new(),
            suggestions: Vec::new(),
            suggestion: None,
            suggest_seq: 0,
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
        view.set_typed(crate::native::ADDRESS_QUERY, url.clone());
        crate::every(view.shell.number(), TICK, BrowserWindow::tick);
        view
    }

    fn tick(&mut self) {
        let mut changed = self.drain();
        self.warm_tabs();
        changed |= self.sync_terminals();
        while let Ok((id, result)) = self.installs_rx.try_recv() {
            changed = true;
            self.installing.retain(|pending| pending != &id);
            match result {
                Ok(path) => self.add_extension(&id, &path, "store"),
                Err(error) => self.status = format!("Install failed: {error}"),
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
            // A click on a page ends an address edit.
            self.blur_address = true;
            self.notify();
            self.close_palette();
            let already = self.order.last().map(String::as_str) == Some(id.as_str())
                && self.active_tab_id().as_deref() == Some(id.as_str());
            if !already {
                self.focus_window(&id);
            }
        }
        // A resize ends without a frame of its own: the layout it left is saved here once the stage settles.
        if self.stage_moved.elapsed() < STAGE_SETTLE + TICK {
            let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
            self.save_layouts(&tabs);
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
            self.notify();
        }
    }

    /// Copies each terminal's title and folder into its tab, closes tabs whose shell exited,
    /// and ends shells whose tab is gone; true if anything changed.
    fn sync_terminals(&mut self) -> bool {
        let tabs = self.model.lock().session_tabs().into_iter().filter(|tab| is_terminal(&tab.url)).cloned().collect::<Vec<_>>();
        let mut changed = false;
        let mut exited = Vec::new();
        let mut links = Vec::new();
        let active = self.active_tab_id();
        for tab in tabs {
            let Some(view) = terminal::get(&tab.id) else {
                continue;
            };
            let seen = active.as_deref() == Some(tab.id.as_str());
            let mut view = view.borrow_mut();
            changed |= view.refresh_agent(seen);
            if let Some(url) = view.open_url.take() {
                links.push((tab.id.clone(), url));
            }
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
        for (id, url) in links {
            changed = true;
            self.open_from(&id, &url);
        }
        if !exited.is_empty() {
            changed = true;
            self.close_tabs(exited);
        }
        let live = self.model.lock().tabs.iter().filter(|tab| is_terminal(&tab.url)).map(|tab| tab.id.clone()).collect::<Vec<_>>();
        terminal::retain(|id| live.iter().any(|live| live == id));
        changed
    }

    fn save_cookies(&mut self) {
        self.cookies_saved = Instant::now();
        self.engine.save_session_cookies();
    }

    /// Applies queued model effects, download updates and engine events; true if any arrived.
    fn drain(&mut self) -> bool {
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
            self.on_event(event);
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
            // Audible or capturing tabs stay live so background audio and calls don't cut out.
            let busy = self.playing.contains(&tab_id) || self.capturing.contains_key(&tab_id);
            if hidden && !popup_in_front && !busy {
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

    fn on_event(&mut self, event: EngineEvent) {
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
                    self.set_typed(crate::native::ADDRESS_QUERY, shown);
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
            EngineEvent::Failed { tab_id, url, message } => {
                // Matched by URL: WebKit reports the page it falls back to as finished before the failure arrives.
                let retry = self.http_fallback.get(&tab_id).is_some_and(|(https, _)| https.trim_end_matches('/') == url.trim_end_matches('/'));
                if let (true, Some((_, http))) = (retry, self.http_fallback.remove(&tab_id)) {
                    let _ = self.engine.navigate(&tab_id, &http);
                } else if self.active_tab_id().as_deref() == Some(tab_id.as_str()) {
                    // Other tabs show their failure in the page; a toast over this one would point at the wrong page.
                    self.status = message;
                }
            }
            EngineEvent::Download { url, filename, download, .. } => self.start_download(download, &url, &filename),
            EngineEvent::DownloadDone { download, bytes, error } => {
                // Unknown ids are downloads cancelled before they were recorded.
                if let Some((id, path)) = self.web_downloads.remove(&download) {
                    let status = error.map_or_else(|| "done".to_string(), |error| format!("error: {error}"));
                    self.download_progress.insert(id.clone(), (bytes, Some(bytes)));
                    let _ = self.model.lock().update_download(&id, &status, &path);
                    self.status = format!("Download {status}");
                }
            }
            EngineEvent::Screenshot { path, error } => {
                self.status = match error {
                    Some(error) => format!("Screenshot failed: {error}"),
                    None => {
                        let id = Uuid::new_v4().to_string();
                        let model = self.model.lock();
                        let _ = model.add_download(&id, &format!("file://{path}"), &path);
                        let _ = model.update_download(&id, "done", &path);
                        "Screenshot saved to downloads".into()
                    }
                };
            }
            EngineEvent::Permission { origin, kind, request, .. } => {
                // One prompt at a time: a newer request turns down the one still showing.
                if let Some((older, _, _)) = self.permission.replace((request, origin, kind)) {
                    self.engine.answer_permission(older, false);
                }
                self.notify();
            }
            EngineEvent::Passwords { tab_id, origin, request, message } => self.on_password_message(&tab_id, &origin, request, &message),
            EngineEvent::OpenInNewTab { url, from } => {
                // `chrome.sidePanel.open()` arrives here as a plain `tabs.create` of the panel page.
                let panel = self.ext_meta.keys().find(|id| self.panel_url(id).as_deref() == Some(url.as_str())).cloned();
                // A side panel's links get a regular window, not a tab squeezed into the panel.
                let from = from.filter(|id| !self.chrome.get(id).is_some_and(|chrome| chrome.panel.is_some()));
                match (panel, from) {
                    (Some(id), _) => self.open_extension_panel(&id, &url),
                    (None, Some(from)) => self.open_from(&from, &url),
                    (None, None) => {
                        if let Ok(id) = self.with_model(|model| model.create_tab(&url)) {
                            self.focus_window(&id);
                        }
                    }
                }
            }
            EngineEvent::Dialog { tab_id, origin, kind, message, default, request } => {
                self.dialogs.push(PageDialog { tab_id, origin, kind, message, default, request });
                if self.dialogs.len() == 1 {
                    self.raise_dialog();
                }
                self.notify();
            }
            EngineEvent::Eval { tab_id, token, value } if token == "find-count" => {
                if tab_id == self.find_tab {
                    self.find_count = value.parse().ok();
                }
            }
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
            EngineEvent::Shortcut(shortcut) => self.on_shortcut(shortcut),
            EngineEvent::PopoverAction(action) => {
                use crate::native::{BAR_BASE, FIND_BASE, HOME_BASE, LIST_BASE, PANEL_BASE, SHEET_BASE, WINDOW_BASE};
                let (slot, base) = match action {
                    BAR_BASE.. => (&mut self.bar_actions, BAR_BASE),
                    HOME_BASE.. => (&mut self.home_actions, HOME_BASE),
                    FIND_BASE.. => (&mut self.find_actions, FIND_BASE),
                    LIST_BASE.. => (&mut self.list_actions, LIST_BASE),
                    PANEL_BASE.. => (&mut self.panel_actions, PANEL_BASE),
                    WINDOW_BASE.. => (&mut self.window_actions, WINDOW_BASE),
                    SHEET_BASE.. => (&mut self.sheet_actions, SHEET_BASE),
                    _ => (&mut self.popover_actions, 0),
                };
                // Taken while it runs, as it gets `self` mutably.
                let actions = std::mem::take(slot);
                if let Some(run) = actions.get((action - base) as usize) {
                    run(self);
                    self.notify();
                }
                let slot = match action {
                    BAR_BASE.. => &mut self.bar_actions,
                    HOME_BASE.. => &mut self.home_actions,
                    FIND_BASE.. => &mut self.find_actions,
                    LIST_BASE.. => &mut self.list_actions,
                    PANEL_BASE.. => &mut self.panel_actions,
                    WINDOW_BASE.. => &mut self.window_actions,
                    SHEET_BASE.. => &mut self.sheet_actions,
                    _ => &mut self.popover_actions,
                };
                // Unless running it rebuilt the window, which registered fresh handlers.
                if slot.is_empty() {
                    *slot = actions;
                }
            }
            EngineEvent::TerminalMenu { tab_id, x, y } => self.open_term_menu(tab_id, point(x as f32, y as f32)),
            EngineEvent::NativeWindowClosed => {
                if self.settings_open {
                    self.toggle_settings();
                }
            }
            EngineEvent::RecordedKey(keys) => self.record_key(&keys),
            EngineEvent::PopoverKey { field, key } if field == crate::native::FIND_QUERY => self.find_key(&key),
            EngineEvent::PopoverKey { field, key } if field == crate::native::HOME_QUERY => self.home_key(&key),
            EngineEvent::PopoverKey { field, key } if field == crate::native::ADDRESS_QUERY => self.address_key(&key),
            // Only the address field cares that a field lost the caret.
            EngineEvent::PopoverKey { key, .. } if key == "blur" => {}
            EngineEvent::PopoverKey { key, .. } => self.palette_key(&key),
            EngineEvent::ListReorder { list, from, to } if list == crate::rail::RAIL_LIST => self.reorder_rail(from, to),
            EngineEvent::ListReorder { list, from, to } => {
                self.reorder_list(list, from, to);
            }
            EngineEvent::WindowGesture { key, phase, x, y, edges, shift } => {
                self.native_input.push(NativeInput::Gesture { key, phase, at: point(x as f32, y as f32), edges, shift });
                self.notify();
            }
            EngineEvent::ItemDrag { id, phase, x, y } => {
                self.native_input.push(NativeInput::Drag { id, phase, at: point(x as f32, y as f32) });
                self.notify();
            }
            EngineEvent::PanelClosed => {
                if self.palette_open {
                    self.palette_open = false;
                    self.focus_active_content();
                    self.notify();
                }
            }
            EngineEvent::PopoverText { field, text } => {
                self.popover_texts.insert(field, text);
                if field == crate::native::HOME_QUERY || field == crate::native::ADDRESS_QUERY {
                    self.suggestion = None;
                    self.update_suggestions(self.typed(field));
                }
                if field == crate::native::PALETTE_QUERY {
                    self.palette_index = 0;
                    self.absorb_bang();
                }
                if field == crate::native::FIND_QUERY {
                    let query = self.typed(field);
                    self.find_fresh(&query);
                }
                // Sheets that show what is typed (the new profile's badge, history's matches) follow it.
                self.notify();
            }
            EngineEvent::PopoverClosed => {
                if !self.engine.popover_shown() {
                    // With no title-bar popover open, the one that closed was the save-password offer: not now.
                    if self.popover.take().is_none() {
                        self.passwords.prompt = None;
                    }
                    self.popover_texts.remove(&crate::extensions::EXT_FIELD);
                    self.notify();
                }
            }
            EngineEvent::MenuPick(index) => {
                if let Some(Some(run)) = std::mem::take(&mut self.menu_actions).get(index) {
                    run(self);
                    self.notify();
                }
            }
            // Torn-off windows share the main window's extensions but not its event channel; repaint their badges too.
            EngineEvent::ExtensionChanged { .. } => crate::notify_all(),
            EngineEvent::ExtensionPrompt { prompt, id, request } => {
                self.ext_prompts.push((prompt, id, request));
                self.popover = Some(Popover::Extensions);
            }
            EngineEvent::ActivateTab { tab_id } => self.focus_window(&tab_id),
            EngineEvent::CloseTab { tab_id } => self.close_tabs(vec![tab_id]),
            EngineEvent::LinkDrag { at } => {
                self.link_hover = at;
                if at.is_none() {
                    self.reorder = self.reorder.take().filter(|state| state.id != LINK_DROP);
                }
                self.notify();
            }
            EngineEvent::LinkDrop { url, at } => {
                self.link_hover = None;
                self.drop_link(&url, at);
            }
            EngineEvent::Peek { url } => self.open_peek(&url),
            EngineEvent::Audio { tab_id, playing } => {
                if playing { self.playing.insert(tab_id) } else { self.playing.remove(&tab_id) };
                self.notify();
            }
            EngineEvent::Capture { tab_id, camera, microphone } => {
                if camera || microphone { self.capturing.insert(tab_id, (camera, microphone)) } else { self.capturing.remove(&tab_id) };
                self.notify();
            }
        }
    }

    pub(crate) fn on_shortcut(&mut self, shortcut: Shortcut) {
        match shortcut {
            Shortcut::NewTab => match self.active_tab_id() {
                Some(id) => self.new_tab_in(&id, None),
                None => self.with_model(|model| { let _ = model.create_tab(&model.settings.new_tab_url()); }),
            },
            Shortcut::CloseTab => {
                if let Some(id) = self.active_tab_id() {
                    self.close_tabs(vec![id]);
                }
            }
            // A terminal's address pill is not editable.
            Shortcut::FocusUrl => self.focus_address = !self.model.lock().active_tab().is_some_and(|tab| is_terminal(&tab.url)),
            Shortcut::NewTerminal => {
                // Opens beside the active tab, in the active terminal's folder if there is one.
                let url = self.model.lock().active_tab().map(|tab| tab.url.clone()).filter(|url| is_terminal(url)).unwrap_or_else(|| TERMINAL_URL.into());
                match self.active_tab_id() {
                    Some(id) => self.new_tab_in(&id, Some(&url)),
                    None => {
                        if let Ok(id) = self.with_model(|model| model.create_tab(&url)) {
                            self.focus_window(&id);
                        }
                    }
                }
            }
            Shortcut::Reload => self.reload(),
            Shortcut::Back => self.back(),
            Shortcut::Forward => self.forward(),
            Shortcut::Find => self.open_find(),
            Shortcut::Bookmark => self.bookmark(),
            Shortcut::Settings => self.toggle_settings(),
            Shortcut::ReopenTab => match self.with_model(|model| model.reopen_closed()) {
                Ok(Some(id)) => self.focus_window(&id),
                _ => self.status = "No recently closed tabs".into(),
            },
            Shortcut::ZoomIn => self.zoom(1),
            Shortcut::ZoomOut => self.zoom(-1),
            Shortcut::ZoomReset => self.zoom(0),
            Shortcut::SelectTab(n) => {
                let ids = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
                let pick = if n == 9 { ids.last() } else { ids.get(n - 1) };
                if let Some(id) = pick.cloned() {
                    self.focus_window(&id);
                }
            }
            Shortcut::NextTab | Shortcut::PrevTab => {
                let ids = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
                let at = self.active_tab_id().and_then(|id| ids.iter().position(|other| *other == id)).unwrap_or(0);
                let step = if matches!(shortcut, Shortcut::NextTab) { 1 } else { ids.len().saturating_sub(1) };
                if let Some(id) = ids.get((at + step) % ids.len().max(1)).cloned() {
                    self.focus_window(&id);
                }
            }
            Shortcut::Print => {
                if let Some(id) = self.active_tab_id() {
                    self.engine.print(&id);
                }
            }
            Shortcut::Screenshot | Shortcut::ScreenshotElement => {
                if let Some(id) = self.active_tab_id() {
                    let dir = {
                        let model = self.model.lock();
                        let dir = [&model.settings.screenshot_dir, &model.settings.download_dir].into_iter().find(|dir| !dir.is_empty()).cloned();
                        dir
                    };
                    let dir = dir.map_or_else(downloads_dir, std::path::PathBuf::from);
                    let _ = std::fs::create_dir_all(&dir);
                    let path = unique_path(&dir, &format!("Screenshot {}.png", browser_core::now_secs()));
                    let area = if shortcut == Shortcut::ScreenshotElement { ScreenshotArea::Element } else { ScreenshotArea::Page };
                    self.engine.screenshot(&id, area, path);
                    if area == ScreenshotArea::Element {
                        self.status = "Click an element, then Capture (Esc to cancel)".into();
                    }
                }
            }
            Shortcut::CommandPalette => self.open_palette(),
            Shortcut::Dismiss => {
                if let Some(id) = self.active_tab_id().filter(|id| self.chrome.get(id).is_some_and(|chrome| chrome.peek)) {
                    self.close_tabs(vec![id]);
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
                    self.focus_window(&id);
                }
            }
            Shortcut::MoveWindow(dir) => self.move_window(dir),
            Shortcut::CloseWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.close_window(&id);
                }
            }
            Shortcut::MaximizeWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.toggle_maximize(&id);
                }
            }
            Shortcut::MinimizeWindow => {
                if let Some(id) = self.active_tab_id() {
                    self.minimize_window(&id);
                }
            }
            Shortcut::NextSpace => self.step_space(1),
            Shortcut::PrevSpace => self.step_space(-1),
            Shortcut::NewSpace => {
                if self.model.detached.is_none() {
                    self.with_model(|model| { let _ = model.create_session(); });
                }
            }
            Shortcut::History => self.open_history_modal(),
            Shortcut::Downloads | Shortcut::Bookmarks => {
                let popover = if shortcut == Shortcut::Downloads { Popover::Downloads } else { Popover::Bookmarks };
                self.popover = (self.popover != Some(popover)).then_some(popover);
            }
            Shortcut::CopyUrl => {
                if let Some(url) = self.model.lock().active_tab().map(|tab| tab.url.clone()) {
                    browser_engine_macos::set_clipboard_text(&url);
                    self.status = "Copied page address".into();
                }
            }
            Shortcut::ClearTerminal => {
                if let Some(view) = self.active_tab_id().and_then(|id| terminal::get(&id)) {
                    view.borrow_mut().clear();
                }
            }
        }
        self.refresh_url();
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
        let Some((_, mut path, source)) = installed else {
            self.ext_meta.remove(id);
            self.engine.extensions().unload(id);
            return;
        };
        if source == "store" {
            if let Err(error) = patch_missing_apis(&path) {
                eprintln!("could not add API polyfill to {id}: {error}");
            }
        } else if !stores.is_empty() {
            // Stop it before its running copy is replaced.
            self.engine.extensions().unload(id);
            let copy = app_support_dir().join("unpacked").join(id);
            match patch_unpacked_copy(&path, &copy) {
                Ok(()) => path = copy,
                Err(error) => eprintln!("could not add API polyfill to {id}: {error}"),
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
                self.status = format!("Not an extension: {error}");
                return;
            }
        };
        let _ = self.model.lock().upsert_extension(id, &manifest.name, path, source);
        self.sync_extension(id);
        self.status = format!("Installed {}", manifest.name);
        self.popover = Some(Popover::Extensions);
    }

    /// Installs from a Chrome Web Store URL or extension id, downloading on a background thread.
    pub(crate) fn install_from_input(&mut self, raw: &str) {
        let Some(id) = store_id(raw) else {
            self.status = "Paste a Chrome Web Store link or a 32-letter extension id".into();
            self.notify();
            return;
        };
        self.popover_texts.remove(&crate::extensions::EXT_FIELD);
        self.engine.set_popover_field(crate::extensions::EXT_FIELD, "");
        self.install_from_store(id);
        self.notify();
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
        let no = self.shell.number();
        browser_engine_macos::choose_path(no, true, move |path| {
            let Some(path) = path else { return };
            crate::update(no, |this| {
                if path.join("manifest.json").is_file() {
                    this.add_extension(&local_id(&path), &path, "unpacked");
                } else {
                    this.status = "That folder has no manifest.json".into();
                }
                this.notify();
            });
        });
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
        if last {
            let _ = std::fs::remove_dir_all(app_support_dir().join("unpacked").join(id));
        }
    }

    pub(crate) fn active_store(&self) -> String {
        self.model.lock().active_profile().map(|profile| profile.data_store_id.clone()).unwrap_or_default()
    }

    /// What clicking an extension's button at window x `at` does: its popup (a popover under the
    /// title bar there) or `onClicked` via WebKit, or its side panel in a slim window.
    pub(crate) fn open_extension(&mut self, id: &str, at: f32) {
        let meta = self.ext_meta.get(id).cloned().unwrap_or_default();
        match self.panel_url(id).filter(|_| !meta.popup) {
            Some(url) => self.open_extension_panel(id, &url),
            None => {
                let (store, tab) = (self.active_store(), self.active_tab_id());
                // Over the dock's extensions button in the main window, under the title bar in a torn-off one.
                let (y, height) = self.popover_anchors.get(&Popover::Extensions).filter(|_| self.model.detached.is_none()).map_or((0., f64::from(self.titlebar_h())), |bounds| (f64::from(bounds.origin.y), f64::from(bounds.size.height)));
                let anchor = [f64::from(at) - 13., y, 26., height];
                if !self.engine.extensions().perform_action(id, &store, tab.as_deref(), anchor) {
                    self.status = format!("Open a website to use {}", meta.name);
                    self.notify();
                }
            }
        }
    }

    pub(crate) fn open_extension_options(&mut self, id: &str) {
        match self.engine.extensions().options_url(id, &self.active_store()) {
            Some(url) => self.open_extension_window(&url),
            None => self.status = "This extension has no options page".into(),
        }
    }

    /// Options pages open as a regular floating window.
    fn open_extension_window(&mut self, url: &str) {
        if let Ok(id) = self.with_model(|model| model.create_tab(url)) {
            self.focus_window(&id);
        }
    }

    fn panel_url(&self, id: &str) -> Option<String> {
        let path = self.ext_meta.get(id)?.side_panel.clone()?;
        Some(format!("{EXTENSION_SCHEME}{id}/{}", path.trim_start_matches('/')))
    }

    /// A floating window centred over the stage, out of the tiling, for a quick look at a link.
    fn open_peek(&mut self, url: &str) {
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
        self.focus_window(&tab_id);
    }

    /// A side panel: a narrow window docked right, titled with just the extension. One per space.
    fn open_extension_panel(&mut self, id: &str, url: &str) {
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
                entry.frame = Some(panel_frame(f64::from(bounds.size.width) as f32, f64::from(bounds.size.height) as f32));
            }
        }
        self.focus_window(&tab_id);
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
            page_font: if settings.ui_font_pages { settings.ui_font.clone() } else { String::new() },
        });
        self.engine.set_shortcuts(if self.recording.is_some() { Vec::new() } else { crate::shortcuts::keymap(&settings) });
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
        self.status = format!("Zoom {:.0}%", next * 100.0);
    }

    fn anim(&self, duration: Duration) -> Duration {
        if self.reduce_motion { Duration::ZERO } else { duration }
    }

    /// Closes tabs; a whole window shrinks away first, then its tabs close.
    pub(crate) fn close_tabs(&mut self, ids: Vec<String>) {
        let Some(first) = ids.first() else {
            return;
        };
        let group = self.group_id(first);
        // The model would activate the space's first tab; focus the rest of this window, else the window on top.
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let open = |id: &&String| current.contains(id) && !ids.contains(id);
        let survivor = self.order.iter().rev().filter(open).find(|id| self.group_of(id) == group).cloned().or_else(|| {
            self.order
                .iter()
                .rev()
                .filter(open)
                .find(|id| self.is_front(id) && !self.chrome.get(*id).is_some_and(|chrome| chrome.minimized))
                .cloned()
        });
        if self.model.detached.is_some() && current.iter().all(|id| ids.contains(id)) {
            self.close_window = true;
            self.notify();
            return;
        }
        // A closed page can't wait on its dialogs; answer them so WebKit lets go of their handlers.
        let front = self.dialogs.first().map(|dialog| dialog.request);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.dialogs).into_iter().partition(|dialog| ids.contains(&dialog.tab_id));
        self.dialogs = kept;
        for dialog in gone {
            self.engine.answer_dialog(dialog.request, false, "");
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
        self.refresh_url();
        if let Some(survivor) = survivor {
            self.focus_window(&survivor);
        }
        if !self.dialogs.is_empty() && self.dialogs.first().map(|dialog| dialog.request) != front {
            self.raise_dialog();
        }
        self.notify();
    }

    /// Scale and opacity for a window opening; (1, 1) when at rest.
    fn window_motion(&self, id: &str) -> (f32, f32) {
        let Some(at) = self.opened.get(id) else {
            return (1.0, 1.0);
        };
        let p = ease(at.elapsed().as_secs_f32() / WINDOW_ANIM.as_secs_f32());
        (WINDOW_FROM_SCALE + (1.0 - WINDOW_FROM_SCALE) * p, p)
    }

    /// The title bar of a torn-off window; the main window has none, its rail and dock taking its place.
    pub(crate) fn titlebar_h(&self) -> f32 {
        if self.model.detached.is_some() { TITLEBAR_H } else { 0.0 }
    }

    /// The main window's dock: its right edge and top now, and its top when open. Shut, it sits just below the
    /// window, out of sight; it rises into the stage's foot, over the windows, centred on the stage.
    pub(crate) fn dock_at(&self) -> Option<(f64, f64, f64)> {
        if self.model.detached.is_some() {
            return None;
        }
        let size = self.viewport();
        let (width, height, rail) = (f64::from(size.width), f64::from(size.height), f64::from(crate::rail::RAIL_W));
        let right = rail + (width - rail) / 2. + (self.spaces_w + DOCK_GAP + self.toolbar_w) / 2.;
        let (open, shut) = (height - f64::from(SNAP_INSET) - DOCK_H, height + 2.);
        Some((right, shut + (open - shut) * f64::from(self.dock.progress()), open))
    }

    /// Opens the dock while the pointer is on it, while what it opened is up, or while a window or tab is carried
    /// down to it to drop on a space.
    fn update_dock(&mut self) {
        let low = f64::from(self.viewport().height) - 48.;
        let carried = (self.gesture.is_some() || self.tab_drag.is_some()) && self.engine.pointer().is_some_and(|(_, y)| y > low);
        let busy = matches!(self.popover, Some(Popover::History | Popover::Bookmarks | Popover::Extensions)) || self.history_modal || self.space_rename.is_some();
        self.dock.set(self.dock_hovered || carried || busy, self.anim(DOCK_ANIM));
    }

    /// The dock's hover target, wider than its resting line, and the line itself, fading as the dock rises.
    fn sync_dock(&mut self, pop: &mut crate::native::Pop, bars: &mut Vec<&'static str>) {
        use crate::native::{Ink, Node};
        if self.model.detached.is_some() {
            return;
        }
        let p = f64::from(self.dock.progress());
        let lerp = |from: f64, to: f64| from + (to - from) * p;
        let size = self.viewport();
        let (width, height, rail) = (f64::from(size.width), f64::from(size.height), f64::from(crate::rail::RAIL_W));
        let center = rail + (width - rail) / 2.;
        let open_w = self.spaces_w + DOCK_GAP + self.toolbar_w;
        let (zone_w, zone_h) = (lerp(f64::from(DOCK_REST_W) + 56., open_w + 24.), lerp(14., DOCK_H + f64::from(SNAP_INSET) + 12.));
        let enter = pop.act(|this| {
            this.dock_hovered = true;
            this.notify();
        });
        let leave = pop.act(|this| {
            this.dock_hovered = false;
            this.notify();
        });
        let zone = LogicalRect { x: center - zone_w / 2., y: height - zone_h, width: zone_w, height: zone_h };
        self.engine.set_bar("dock-zone", zone, None, &Node::Watch { enter, leave, child: Box::new(Node::Spacer) });
        let (line_w, line_h) = (f64::from(DOCK_REST_W), f64::from(DOCK_REST_H));
        let line = LogicalRect { x: center - line_w / 2., y: height - 2. - line_h, width: line_w, height: line_h };
        let pill = Node::Round { radius: line_h / 2., tint: Some(Ink::Tertiary), child: Box::new(Node::Spacer) };
        self.engine.set_bar("dock-line", line, None, &pill);
        self.engine.set_bar_alpha("dock-line", 1. - p);
        bars.extend(["dock-zone", "dock-line"]);
    }

    fn title_h(&self) -> f64 {
        TITLE_H
    }

    /// Pills and buttons leave a 3px margin above and below in the window title bar.
    fn pill_h(&self) -> f32 {
        self.title_h() as f32 - 6.0
    }

    fn apply_theme(&mut self) {
        let (theme, accent, wallpaper) = {
            let model = self.model.lock();
            (model.settings.theme.clone(), model.settings.accent.clone(), model.settings.wallpaper.clone())
        };
        self.engine.set_backdrop(&wallpaper);
        let dark = match theme.as_str() {
            "light" => false,
            "dark" => true,
            _ => self.shell.is_dark(),
        };
        let key = format!("{theme}{dark}{accent}");
        if key != self.applied_theme {
            // Pages read prefers-color-scheme from the app appearance; None follows the system.
            self.engine.set_appearance(match theme.as_str() {
                "light" => Some(false),
                "dark" => Some(true),
                _ => None,
            });
            let mut next = if dark { DARK } else { LIGHT };
            if !accent.is_empty() {
                next.accent = parse_hex(&accent);
            }
            THEME.set(next);
            self.applied_theme = key;
        }
        let paint = paint();
        terminal::set_colors([paint.fg, paint.bg, paint.accent]);
    }

    pub(crate) fn sync_permissions(&mut self) {
        if let Ok(entries) = self.model.lock().permissions_map() {
            self.engine.set_permissions(entries);
        }
    }

    pub(crate) fn refresh_url(&mut self) {
        if self.address_focused {
            return;
        }
        if let Some(url) = Some(self.model.lock()).and_then(|model| model.active_tab().map(|tab| tab.url.clone())) {
            let url = if is_blank(&url) { String::new() } else { url };
            self.set_typed(crate::native::ADDRESS_QUERY, url);
        }
    }

    /// Chrome-style: history and bookmark matches first, then the search engine's completions once they arrive.
    pub(crate) fn update_suggestions(&mut self, typed: String) {
        let (history, template) = {
            let model = self.model.lock();
            (model.suggest(&typed, 4).unwrap_or_default(), model.settings.suggest_template())
        };
        // The last fetch's searches that still fit stay until this one answers: cleared on every key, the list
        // blinked away and back while typing.
        let query = typed.trim().to_lowercase();
        let kept = std::mem::take(&mut self.suggestions)
            .into_iter()
            .filter(|(text, _, search)| *search && !query.is_empty() && text.to_lowercase().starts_with(&query));
        self.suggestions = history.into_iter().map(|(url, title)| (url, title, false)).chain(kept).collect();
        self.suggestion = None;
        self.suggest_seq += 1;
        if let Some(template) = template.filter(|_| !typed.trim().is_empty()) {
            let (fetch, seq, no) = (fetch_suggestions(template, typed.trim()), self.suggest_seq, self.shell.number());
            std::thread::spawn(move || {
                let found = futures::executor::block_on(fetch).unwrap_or_default();
                crate::later(no, move |view| {
                    if !view.address_focused || view.suggest_seq != seq {
                        return;
                    }
                    view.suggestions.retain(|(_, _, search)| !search);
                    let room = 8usize.saturating_sub(view.suggestions.len());
                    view.suggestions.extend(found.into_iter().take(room).map(|query| (query, String::new(), true)));
                });
            });
        }
        self.notify();
    }

    /// Arrow, Tab, Enter, Escape and losing the caret in the native address field.
    fn address_key(&mut self, key: &str) {
        let count = self.suggestions.len();
        match key {
            "down" if count > 0 => self.suggestion = Some(self.suggestion.map_or(0, |at| (at + 1) % count)),
            "up" if count > 0 => self.suggestion = Some(self.suggestion.map_or(count - 1, |at| (at + count - 1) % count)),
            "enter" => {
                let picked = self.suggestion.and_then(|at| self.suggestions.get(at)).map(|(url, _, _)| url.clone());
                let typed = self.typed(crate::native::ADDRESS_QUERY);
                match picked.or_else(|| (!typed.trim().is_empty()).then_some(typed)) {
                    Some(target) => self.commit_navigation(&target),
                    None => return,
                }
            }
            // Tab fills the picked suggestion, or the first, into the field to edit before going.
            "tab" if count > 0 => {
                let text = self.suggestions[self.suggestion.unwrap_or(0).min(count - 1)].0.clone();
                self.set_typed(crate::native::ADDRESS_QUERY, text.clone());
                self.update_suggestions(text);
            }
            "escape" if count > 0 => self.suggestions.clear(),
            "escape" | "blur" => self.end_address_edit(),
            _ => return,
        }
        self.notify();
    }

    pub(crate) fn commit_navigation(&mut self, raw: &str) {
        let (url, tab_id) = {
            let model = self.model.lock();
            (normalize_url(raw, model.settings.search_template(), model.settings.https_only), model.active_tab().map(|tab| tab.id.clone()))
        };
        if let Some(tab_id) = tab_id {
            // Only a host we prefixed ourselves: a typed scheme, a search or a bang is meant as given.
            if url == format!("https://{}", raw.trim()) && !self.model.lock().settings.https_only {
                self.http_fallback.insert(tab_id, (url.clone(), format!("http://{}", raw.trim())));
            } else {
                self.http_fallback.remove(&tab_id);
            }
        }
        self.address_focused = false;
        self.address_editing = false;
        self.suggestions.clear();
        self.suggestion = None;
        self.suggest_seq += 1;
        self.blur_address = true;
        self.set_typed(crate::native::ADDRESS_QUERY, url.clone());
        self.with_model(|model| {
            let _ = model.navigate_active(&url);
        });
        self.focus_active_content();
    }

    /// Gives keys to the active tab's content: its web page, or its terminal on the next render.
    /// A chrome modal is open: pages are dimmed, take no clicks and must not take keys.
    fn modal_open(&self) -> bool {
        self.settings_open || self.popover == Some(Popover::Extensions) || self.delete_prompt.is_some() || self.new_profile.is_some() || self.history_modal || self.ext_logs.is_some() || self.bookmark_edit.is_some() || self.passwords.edit.is_some() || !self.pending_links.is_empty()
    }

    pub(crate) fn focus_active_content(&mut self) {
        // A modal's inputs keep the keys; a tab opened or activated behind it waits until it closes.
        if self.modal_open() {
            return;
        }
        // An open address edit ends with it.
        self.blur_address = true;
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
    pub(crate) fn toggle_popover(&mut self, popover: Popover) {
        // A press on its own button closed it a moment ago (popovers close on any press outside them): keep it closed.
        let closing = self.popover == Some(popover) || self.engine.popover_just_closed(popover as u64);
        self.popover = (!closing).then_some(popover);
        self.notify();
    }

    /// Shows the open popover natively, or updates it from the model; closes it once `popover` is `None`.
    fn sync_popover(&mut self) {
        let mut pop = crate::native::Pop::default();
        let Some(kind) = self.popover else {
            // The save-password offer sits under the title bar's right end until answered.
            match self.save_password_popover(&mut pop) {
                Some(content) => {
                    let anchor = LogicalRect { x: f64::from(self.viewport().width) - 40., y: 0., width: 28., height: f64::from(self.titlebar_h()) };
                    self.engine.present_popover(SAVE_PASSWORD_KEY, &anchor, browser_engine_macos::Edge::Below, 320., false, &content);
                    self.popover_actions = pop.actions;
                }
                None if self.engine.popover_shown() => self.engine.close_popover(),
                None => {}
            }
            return;
        };
        // It closed itself and the event saying so hasn't been handled yet: don't open it again.
        if !self.engine.popover_shown() && self.engine.popover_just_closed(kind as u64) {
            self.popover = None;
            return;
        }
        let (content, width) = match kind {
            Popover::History => (self.history_popover(&mut pop), crate::history::POPOVER_W),
            Popover::Bookmarks => (self.bookmarks_popover(&mut pop), crate::history::POPOVER_W),
            Popover::Downloads => (self.downloads_popover(&mut pop), crate::history::POPOVER_W),
            Popover::Extensions => {
                // A sheet, not a popover: `sync_sheet` shows it.
                if self.engine.popover_shown() {
                    self.engine.close_popover();
                }
                return;
            }
        };
        let edge = match kind {
            Popover::Downloads => browser_engine_macos::Edge::Right,
            _ if self.model.detached.is_none() => browser_engine_macos::Edge::Above,
            _ => browser_engine_macos::Edge::Below,
        };
        // Opened from the keyboard before its button was drawn: under the title bar's right end, or by the rail.
        let anchor = self.popover_anchors.get(&kind).map(|bounds| logical(*bounds)).unwrap_or_else(|| match kind {
            Popover::Downloads => LogicalRect { x: 8., y: f64::from(self.viewport().height) - 40., width: 28., height: 28. },
            _ => LogicalRect { x: f64::from(self.viewport().width) - 40., y: 0., width: 28., height: f64::from(self.titlebar_h()) },
        });
        self.engine.present_popover(kind as u64, &anchor, edge, width, true, &content);
        self.popover_actions = pop.actions;
    }

    /// Shows the topmost open dialog as a native sheet, or updates it; closes the sheet once none is open.
    fn sync_sheet(&mut self) {
        let mut pop = crate::native::Pop::sheet();
        // Checked top down, in the order the dialogs used to stack.
        let page = self.dialog_sheet(&mut pop).or_else(|| self.permission_sheet(&mut pop));
        let open = if let Some((key, content)) = page {
            Some((key, 400., content))
        } else if let Some(edit) = self.passwords.edit.clone() {
            Some((format!("login:{edit:?}"), 420., self.login_sheet(&mut pop)))
        } else if let Some((id, _)) = self.bookmark_edit.clone() {
            Some((format!("bookmark:{id:?}"), 400., self.bookmark_sheet(&mut pop)))
        } else if let Some(id) = self.ext_logs.clone() {
            Some((format!("logs:{id}"), 640., self.logs_sheet(&id, &mut pop)))
        } else if self.popover == Some(Popover::Extensions) {
            Some(("extensions".into(), 460., self.extensions_sheet(&mut pop)))
        } else if self.history_modal {
            Some(("history".into(), 560., self.history_sheet(&mut pop)))
        } else if !self.pending_links.is_empty() {
            Some(("links".into(), 380., self.link_sheet(&mut pop)))
        } else if let Some(id) = self.delete_prompt.clone() {
            Some((format!("delete:{id}"), 380., self.delete_sheet(&id, &mut pop)))
        } else if self.new_profile.is_some() {
            Some(("profile".into(), 440., self.profile_sheet(&mut pop)))
        } else {
            None
        };
        match open {
            Some((key, width, content)) => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut hasher);
                self.engine.present_sheet(hasher.finish(), width, &content);
                self.sheet_actions = pop.actions;
            }
            None => self.engine.close_sheet(),
        }
    }

    /// Shows the settings window while settings are open, or updates it; closes it once they're not.
    fn sync_settings(&mut self) {
        if !self.settings_open {
            return self.engine.close_window();
        }
        let mut pop = crate::native::Pop::window();
        let content = self.settings_window(&mut pop);
        // One key: it stays the same window, updated in place, from section to section.
        self.engine.present_window(1, "Settings", 780., &content);
        self.window_actions = pop.actions;
    }

    /// Shows the command palette's panel while it is open, or updates it; closes it once it's not.
    fn sync_palette(&mut self) {
        if !self.palette_open {
            return self.engine.close_panel();
        }
        let mut pop = crate::native::Pop::panel();
        let content = self.palette_node(&mut pop);
        self.engine.present_panel(1, 560., 72., &content);
        self.panel_actions = pop.actions;
    }

    /// Puts `text` in native field `field`, as if typed there.
    pub(crate) fn set_typed(&mut self, field: u32, text: String) {
        self.engine.set_popover_field(field, &text);
        self.popover_texts.insert(field, text);
    }

    /// What is typed in native field `field`.
    pub(crate) fn typed(&self, field: u32) -> String {
        self.popover_texts.get(&field).cloned().unwrap_or_default()
    }

    /// Records where `popover`'s button is drawn; goes inside the button.
    pub(crate) fn open_history_url(&mut self, url: &str) {
        let url = url.to_string();
        self.popover = None;
        self.history_modal = false;
        self.commit_navigation(&url);
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
        if !ask {
            let _ = std::fs::create_dir_all(&dir);
            return self.begin_download(download, url, path);
        }
        // A sheet, not runModal: this runs from the tick, and a modal loop here would let the tick re-enter.
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned());
        let (no, url) = (self.shell.number(), url.to_string());
        browser_engine_macos::save_path(no, &dir, name.as_deref(), move |path| {
            crate::update(no, |this| {
                match path {
                    Some(path) => {
                        // The save sheet already asked about replacing it; WebKit won't write over a file.
                        let _ = std::fs::remove_file(&path);
                        this.begin_download(download, &url, path);
                    }
                    None => this.engine.save_download(download, None),
                }
                this.notify();
            });
        });
    }

    /// Lets WebKit write a held download to `path` and starts tracking it.
    fn begin_download(&mut self, download: u64, url: &str, path: std::path::PathBuf) {
        let id = Uuid::new_v4().to_string();
        let shown = path.display().to_string();
        let _ = self.model.lock().add_download(&id, url, &shown);
        self.download_progress.insert(id.clone(), (0, None));
        self.web_downloads.insert(download, (id, shown));
        self.engine.save_download(download, Some(&path));
        self.status = "Download started".into();
        self.popover = Some(Popover::Downloads);
    }

    /// Stops the running download recorded as `id` in the model.
    pub(crate) fn cancel_download(&mut self, id: &str) {
        if let Some(download) = self.web_downloads.iter().find(|(_, (model_id, _))| model_id == id).map(|(download, _)| *download) {
            self.engine.cancel_download(download);
        }
    }

    pub(crate) fn export_profile(&mut self) {
        let bundle = self.model.lock().export_active();
        let Ok(bundle) = bundle else {
            self.status = "Export failed".into();
            return;
        };
        let no = self.shell.number();
        browser_engine_macos::save_path(no, &downloads_dir(), Some("profile.json"), move |path| {
            let Some(path) = path else { return };
            crate::update(no, |this| {
                match std::fs::write(&path, serde_json::to_string_pretty(&bundle).unwrap_or_default()) {
                    Ok(()) => this.status = format!("Exported {}", path.display()),
                    Err(error) => this.status = error.to_string(),
                }
                this.notify();
            });
        });
    }

    pub(crate) fn import_profile(&mut self) {
        let no = self.shell.number();
        browser_engine_macos::choose_path(no, false, move |path| {
            let Some(path) = path else { return };
            crate::update(no, |this| {
                this.import_profile_from(&path);
                this.refresh_url();
                this.notify();
            });
        });
    }

    fn import_profile_from(&mut self, path: &std::path::Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.status = error.to_string();
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
            Err(error) => self.status = error.to_string(),
        }
    }

    fn attach(&mut self) {
        if self.attached {
            return;
        }
        if self.model.detached.is_none() && self.self_test.is_none() {
            let saved = {
                let model = self.model.lock();
                let spaces = model.settings.detached_spaces.iter().map(|id| (id.clone(), false));
                spaces.chain(model.settings.detached_windows.iter().map(|id| (id.clone(), true))).collect::<Vec<_>>()
            };
            let extensions = self.engine.extensions().clone();
            let model = self.model.shared.clone();
            let (x, y) = self.shell.origin();
            crate::defer(move || {
                for (n, (id, tab_window)) in saved.into_iter().enumerate() {
                    let at = point(x as f32 + 40. * (n + 1) as f32, y as f32 + 40. * (n + 1) as f32);
                    open_space(model.clone(), extensions.clone(), id, tab_window, Vec::new(), HashMap::new(), at);
                }
            });
        }
        if self.engine.attach(self.shell.chrome()).is_ok() {
            self.attached = true;
            let no = self.shell.number();
            // Page clicks close popovers and the palette at once; refocusing waits for the mouse-up, as raising a
            // window mid-press could drop the click.
            self.engine.on_page_click(move || {
                crate::later(no, |view| {
                    view.popover = None;
                    view.close_palette();
                });
            });
            // Shortcuts and finished page clicks run at once rather than on the next tick.
            self.engine.on_wake(move || crate::later(no, BrowserWindow::tick));
            let effects = self.model.lock().take_effects();
            self.apply(effects);
        }
    }

    /// The close button or ⌘W on the OS window: a closed space comes back to the main window; a closed torn-off
    /// window closes its tabs, like any window.
    pub(crate) fn should_close(&mut self) -> bool {
        if let Some(id) = &self.model.detached {
            forget_detached(&mut self.model.shared.lock().unwrap_or_else(PoisonError::into_inner), id, self.tab_window);
        }
        true
    }

    /// Draws this window again on the next turn of the main thread; several asks before then make one frame.
    pub(crate) fn notify(&self) {
        if !self.scheduled.replace(true) {
            crate::later(self.shell.number(), |_| {});
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
    fn end_address_edit(&mut self) {
        if !self.address_editing && !self.address_focused {
            return;
        }
        self.address_focused = false;
        self.address_editing = false;
        self.suggestions.clear();
        self.suggestion = None;
        self.refresh_url();
        self.notify();
    }

    /// Gives keys to the active terminal; after the windows are placed, so a brand-new one's screen is in the window.
    fn request_terminal_focus(&mut self) {
        if !std::mem::take(&mut self.focus_terminal) {
            return;
        }
        let active = self.model.lock().active_tab().filter(|tab| is_terminal(&tab.url)).map(|tab| tab.id.clone());
        if let Some(id) = active {
            self.engine.focus_page(&id);
        }
    }

    /// Opens the find bar on the active window, unless it's a terminal, which has no page to search.
    pub(crate) fn open_find(&mut self) {
        let active = self.model.lock().active_tab().filter(|tab| !is_terminal(&tab.url)).map(|tab| tab.id.clone());
        if let Some(id) = active {
            if self.find_tab != id {
                self.close_find();
                self.find_tab = id;
                let query = self.typed(crate::native::FIND_QUERY);
                self.find_fresh(&query);
            }
            self.find_open = true;
            self.focus_find = true;
        }
    }

    /// Closes the find bar and drops the match it left selected on the page.
    pub(crate) fn close_find(&mut self) {
        self.find_open = false;
        self.engine.eval(&self.find_tab, "find-clear", "getSelection().removeAllRanges()");
    }

    /// Searches for `query` from the top of the page and recounts its matches.
    fn find_fresh(&mut self, query: &str) {
        // Drop the old match first, else the search goes on from after it.
        self.engine.eval(&self.find_tab, "find-clear", "getSelection().removeAllRanges()");
        // The old count stays until the page reports the new one: cleared, the tally blinked out on every key.
        if query.is_empty() {
            self.find_count = None;
        }
        self.find_index = 1;
        if !query.is_empty() {
            self.engine.eval(&self.find_tab, "find-count", &find_count_js(query));
            self.engine.find(&self.find_tab, query, false);
        }
    }

    /// Moves to the next (or previous, `backwards`) match.
    // shortcut: the index is counted here, so it drifts if the user selects text on the page; read WebKit's match index if that matters.
    fn find_step(&mut self, backwards: bool) {
        if let Some(count) = self.find_count.filter(|&count| count > 0) {
            self.find_index = if backwards { (self.find_index + count - 2) % count + 1 } else { self.find_index % count + 1 };
        }
        let query = self.typed(crate::native::FIND_QUERY);
        self.engine.find(&self.find_tab, &query, backwards);
        self.notify();
    }

    /// The find bar is open on the active window.
    pub(crate) fn find_here(&self) -> bool {
        self.find_open && self.active_tab_id().is_some_and(|id| id == self.find_tab)
    }

    /// Opens the active window's address field for typing, or closes it, as asked since the last frame.
    fn request_address_focus(&mut self) {
        if std::mem::take(&mut self.blur_address) && !self.focus_address {
            self.end_address_edit();
        }
        if !std::mem::take(&mut self.focus_address) {
            return;
        }
        // A terminal's address is where its shell is, not something to type over.
        if self.model.lock().active_tab().is_some_and(|tab| is_terminal(&tab.url)) {
            return;
        }
        let url = self.model.lock().active_tab().map(|tab| tab.url.clone()).unwrap_or_default();
        // A new tab page has its own search box: the caret goes there, not into a second one in the tab row; but a
        // click on the tab itself asks for the address field.
        let strip = std::mem::take(&mut self.edit_in_strip);
        if let Some(id) = self.active_tab_id().filter(|id| !strip && is_blank(&url) && self.engine.has_home(id)) {
            self.chrome.entry(id.clone()).or_default().minimized = false;
            // Once placed (a hidden page takes no caret), in `place_windows`.
            self.home_caret = Some(id);
            return;
        }
        self.address_editing = true;
        self.address_focused = true;
        self.suggestions.clear();
        self.suggestion = None;
        self.set_typed(crate::native::ADDRESS_QUERY, if is_blank(&url) { String::new() } else { url });
        if let Some(id) = self.active_tab_id() {
            self.chrome.entry(id).or_default().minimized = false;
        }
        self.engine.focus_chrome();
        // The strips are built next in this render, with the field; the caret goes in once it is there.
        self.address_caret = true;
    }
}

/// sRGB with alpha.
pub(crate) type Color = [f64; 4];

#[derive(Clone, Copy)]
pub(crate) struct Paint {
    pub(crate) bg: Color,
    pub(crate) fg: Color,
    pub(crate) muted: Color,
    pub(crate) secondary: Color,
    pub(crate) border: Color,
    pub(crate) accent: Color,
}

/// Gruvbox dark, the dark palette the app has always had.
const DARK: Paint = Paint {
    fg: hex(0xebdbb2),
    bg: hex(0x282828),
    secondary: hex(0x504945),
    border: hex(0x504945),
    muted: hex(0xa89984),
    accent: hex(0x8ec07c),
};

/// Neutral macOS-style light palette.
const LIGHT: Paint = Paint {
    fg: hex(0x1d1d1f),
    bg: hex(0xffffff),
    secondary: hex(0xe8e8ed),
    border: hex(0xd2d2d7),
    muted: hex(0x6e6e73),
    accent: hex(0x007aff),
};

thread_local! {
    /// The palette `apply_theme` picked for the theme setting, the system appearance and the accent.
    static THEME: Cell<Paint> = const { Cell::new(DARK) };
}

/// `0xRRGGBB`, opaque.
pub(crate) const fn hex(value: u32) -> Color {
    [((value >> 16) & 0xff) as f64 / 255., ((value >> 8) & 0xff) as f64 / 255., (value & 0xff) as f64 / 255., 1.]
}

/// `#rrggbb` as a colour; malformed digits read as 0.
pub(crate) fn parse_hex(text: &str) -> Color {
    let digits = text.trim_start_matches('#');
    let channel = |at: usize| digits.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).map_or(0., |v| f64::from(v) / 255.);
    [channel(0), channel(2), channel(4), 1.]
}

/// A colour from hue, saturation and lightness, each 0..=1.
fn hsl(hue: f64, saturation: f64, lightness: f64) -> Color {
    let chroma = (1. - (2. * lightness - 1.).abs()) * saturation;
    let channel = |n: f64| {
        let k = (n + hue * 12.) % 12.;
        lightness - chroma / 2. * (k - 3.).min(9. - k).clamp(-1., 1.)
    };
    [channel(0.), channel(8.), channel(4.), 1.]
}

pub(crate) fn paint() -> Paint {
    THEME.get()
}

impl BrowserWindow {
    /// Lays the window out and syncs every native view with the model: on a change, an animation frame, a resize.
    pub(crate) fn frame(&mut self) {
        self.scheduled.set(false);
        self.attach();
        self.run_native_input();
        self.overlay_rects.clear();
        self.space_chips.clear();
        self.rail_badges.clear();
        // A link drag's gap stays until the engine says it left or dropped.
        // Once a frame, against the slots last laid out, as AppKit reports moves far more often.
        if let Some(at) = self.link_hover {
            self.preview_link(at);
        } else if self.tab_drag.is_none() {
            // A drag dropped nowhere puts its item back in its gap.
            if let Some(state) = self.reorder.take().filter(|state| state.row.is_none()) {
                self.land(&state.id, state.at);
            }
        }
        // Only rows drawn this frame count as drop targets.
        self.slots.clear();
        self.apply_theme();
        self.sync_popover();
        self.sync_sheet();
        self.sync_settings();
        self.sync_palette();
        self.sync_suggestions();
        self.sync_homes();
        self.sync_find();
        self.sync_toast();
        self.update_dock();
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let active_session = self.model.lock().active_session_id.clone();
        let all_tabs = self.model.lock().tabs.clone();

        // Keep layouts of every open tab, not just this session's, so switching profile and back keeps them.
        let live_ids = all_tabs.iter().map(|tab| tab.id.as_str()).collect::<HashSet<_>>();
        self.chrome.retain(|id, _| live_ids.contains(id.as_str()));
        self.order.retain(|id| live_ids.contains(id.as_str()));
        if self.tab_window {
            // One window filling the OS window; tabs opened any way join it.
            let group = tabs.first().map(|tab| self.group_id(&tab.id));
            for tab in &tabs {
                let entry = self.chrome.entry(tab.id.clone()).or_default();
                (entry.maximized, entry.minimized, entry.window) = (true, false, group.clone());
            }
        }
        let maximized = tabs.iter().any(|tab| self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized));
        // Laid out before the title strips are built, so they show this frame's frames.
        self.sync_stage();
        self.prepare_frames(&tabs);
        self.sync_groups();
        // Before the window strips are built, so a click on a tab shows its address field in this very frame: a
        // notify from inside render asks for no further frame, and the field waited for the next event.
        self.request_address_focus();
        self.sync_bars();
        if let Some(own) = self.model.detached.clone().filter(|_| self.close_window) {
            // Its last tab closed: the window and its space go, tabs and all.
            forget_detached(&mut self.model.shared.lock().unwrap_or_else(PoisonError::into_inner), &own, true);
            return self.shell.close();
        }
        if let Some(own) = &self.model.detached {
            // Its space was deleted (with its profile): nothing left to show.
            if !self.model.lock().sessions.iter().any(|session| session.id == *own) {
                return self.shell.close();
            }
        }
        if std::mem::take(&mut self.merge_requested) {
            return self.merge_into_main();
        }
        self.save_layouts(&tabs);
        self.step_motion();
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
        // Last, once this frame has laid everything out. A new tab page whose area changed is laid out again next frame.
        if self.place_windows() {
            self.notify();
        }
        self.request_terminal_focus();
        self.shell.set_drag_zones(self.drag_zones());
        self.shell.animate(self.animating());
    }

    /// Where a press moves the OS window, as a title bar's would: a torn-off window's title bar; in the main window,
    /// the rail's free space and the gap windows leave along the stage's top.
    fn drag_zones(&self) -> Vec<LogicalRect> {
        let (width, height) = self.shell.size();
        if self.model.detached.is_some() {
            return vec![LogicalRect { x: 0., y: 0., width, height: f64::from(self.titlebar_h()) }];
        }
        let rail = f64::from(crate::rail::RAIL_W);
        vec![LogicalRect { x: 0., y: 0., width: rail, height }, LogicalRect { x: rail, y: 0., width: width - rail, height: f64::from(SNAP_INSET) }]
    }

    /// The content's size in points.
    pub(crate) fn viewport(&self) -> Size {
        let (width, height) = self.shell.size();
        size(width as f32, height as f32)
    }

    /// The OS window's top-left on screen.
    pub(crate) fn screen_origin(&self) -> Point {
        let (x, y) = self.shell.origin();
        point(x as f32, y as f32)
    }

    /// Whether `at` (window point) is past the OS window's edge and margin: a window let go there tears off.
    pub(crate) fn torn_off(&self, at: Point) -> bool {
        let (margin, size) = (TEAR_OFF_MARGIN, self.viewport());
        !Bounds::new(point(-margin, -margin), crate::geom::size(size.width + margin * 2., size.height + margin * 2.)).contains(&at)
    }

    /// The OS window was dragged by a drag area; a torn-off window let go over the main window's rail merges back.
    pub(crate) fn dragged(&mut self, moved: bool, (x, y): (f64, f64)) {
        let (left, top) = self.shell.origin();
        if moved && self.model.detached.is_some() && over_main_bar(point((left + x) as f32, (top + y) as f32)) {
            self.merge_into_main();
        }
    }

    /// Shift pressed or released without moving still shows or drops the swap at once.
    pub(crate) fn shift(&mut self, down: bool) {
        if let Some((x, y)) = self.gesture.as_ref().map(|gesture| gesture.pointer) {
            self.track_gesture(point(x, y), down);
        }
    }
}

impl BrowserWindow {
    /// Moves every animation one frame along: tweens follow their targets, finished closes run.
    fn step_motion(&mut self) {
        if self.status != self.toast.0 {
            self.toast = (self.status.clone(), Instant::now());
        }
        let progress = self.active_tab_id().and_then(|id| self.engine.progress(&id));
        match progress {
            Some(target) => {
                let target = (target as f32).max(0.15);
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
        if self.landing().is_none() {
            self.tab_landing = None;
        }
        if self.landing.as_ref().is_some_and(|(_, _, at)| at.elapsed() >= LAND_ANIM) {
            self.landing = None;
        }
    }

    /// Shows the status message in a glass toast at the bottom, until it has been up for `TOAST_LIFE`.
    fn sync_toast(&mut self) {
        let animate = !self.reduce_motion;
        if self.toast.0.is_empty() || self.self_test.is_some() || self.toast.1.elapsed() >= TOAST_LIFE - TOAST_IN {
            self.engine.hide_toast(animate);
        } else {
            self.engine.show_toast(&self.toast.0, animate);
        }
    }

    /// Hangs the address suggestions below the address field while it is focused, or updates them.
    fn sync_suggestions(&mut self) {
        use crate::native::{Ink, Node, Weight};
        let address = self.engine.field_frame(crate::native::ADDRESS_QUERY).filter(|_| self.address_focused);
        let anchor = match address {
            Some(frame) => Some((frame.x - 8., frame.y + frame.height + 3., frame.width + 16.)),
            // Or a new tab page's search field, while it is being typed in.
            None => self.engine.field_frame(crate::native::HOME_QUERY).map(|frame| (frame.x - 32., frame.y + frame.height + 8., frame.width + 40.)),
        };
        let Some((x, bottom, width)) = anchor.filter(|_| !self.suggestions.is_empty()) else {
            return self.engine.close_list();
        };
        let mut pop = crate::native::Pop::list();
        let mut rows = Vec::new();
        for (index, (url, title, search)) in self.suggestions.iter().enumerate() {
            let target = url.clone();
            let mut children = Vec::new();
            if *search {
                children.push(Node::Glyph { name: "magnifyingglass", size: 11., ink: Ink::Secondary });
            }
            children.push(crate::native::title(if *search || title.is_empty() { display_url(url) } else { title.clone() }, 12., Weight::Regular, Ink::Primary));
            if !*search {
                children.push(crate::native::title(display_url(url), 12., Weight::Regular, Ink::Secondary));
            }
            children.push(Node::Spacer);
            let go = pop.act(move |this| this.commit_navigation(&target));
            let row = Node::Item { height: 26., click: Some(go), drag: None, children };
            rows.push(if self.suggestion == Some(index) { Node::Card { tint: Ink::Accent, child: Box::new(row) } } else { row });
        }
        let content = Node::Column { gap: 0., pad: (4., 4.), children: rows };
        self.engine.show_list(1, (x, bottom + 4.), width.max(320.), &content);
        self.list_actions = pop.actions;
    }

    /// Builds every piece of native chrome for this frame, and drops what is no longer shown.
    fn sync_bars(&mut self) {
        let mut pop = crate::native::Pop::bar();
        let mut bars = Vec::new();
        self.drag_items.clear();
        self.reorder_lists.clear();
        self.list_arrangements.clear();
        self.sync_rail(&mut pop, &mut bars);
        self.sync_dock(&mut pop, &mut bars);
        if self.model.detached.is_some() {
            // A torn-off window keeps its title bar and the lights in a row along it.
            self.engine.place_traffic_lights((12., 8.), false);
        }
        self.sync_toolbar(&mut pop, &mut bars);
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        self.sync_spaces(&mut pop, &mut bars, &tabs);
        self.sync_frames(&mut pop, &tabs);
        self.sync_drag_views(&mut bars, &tabs);
        self.engine.keep_bars(&bars);
        self.bar_actions = pop.actions;
    }

    /// What each profile's pages are doing across this window's tabs, for the rail.
    pub(crate) fn profile_activity(&self) -> HashMap<String, Activity> {
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
            // A panel is always its own window; a tab saved inside one (an older bug) comes back as a window of its own.
            if chrome.panel.is_some() && chrome.window.is_some() {
                chrome = TabChrome::default();
            }
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
                    if entry.snap.is_none() {
                        entry.frame = entry.frame.map(|frame| fit_on_stage(frame, sw, sh));
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
            if entry.panel.is_some() && entry.frame.is_none() {
                // A panel pushed off a shrunk stage would otherwise stay in the dock but never show.
                entry.frame = Some(panel_frame(sw, sh));
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
    pub(crate) fn free_space(&self) -> bool {
        let model = self.model.lock();
        model.sessions.iter().any(|session| session.id == model.active_session_id && session.free)
    }

    /// Switches space `id` between tiling and free windows.
    pub(crate) fn toggle_free(&mut self, id: &str) {
        let free = !self.model.lock().sessions.iter().any(|session| session.id == id && session.free);
        self.with_model(|model| { let _ = model.set_session_free(id, free); });
        // Into tiling, every window joins from where it is, as with Tile windows.
        self.retile = !free;
        self.notify();
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
        // Mid-motion layouts change every frame (a window resize retiles every frame too); save once things settle
        // instead of writing SQLite per frame.
        if self.gesture.is_some() || self.self_test.is_some() || self.animating() || self.stage_moved.elapsed() < STAGE_SETTLE {
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

    /// Takes `bounds` as the stage's; true if its size changed.
    fn note_stage(&mut self, bounds: Bounds) -> bool {
        let changed = self.stage_bounds.map(|old| (f64::from(old.size.width) - f64::from(bounds.size.width)).abs() > 1.0 || (f64::from(old.size.height) - f64::from(bounds.size.height)).abs() > 1.0).unwrap_or(true);
        self.stage_bounds = Some(bounds);
        if changed {
            self.stage_moved = Instant::now();
        }
        changed
    }

    /// The stage for the window's size now: right of the rail in the main window, under the title bar in a torn-off
    /// one. Set before anything is placed, so during a live resize everything moves in the very frame the window does.
    fn sync_stage(&mut self) {
        let rail = if self.model.detached.is_none() { crate::rail::RAIL_W } else { 0. };
        let origin = point(rail, self.titlebar_h());
        let viewport = self.viewport();
        self.note_stage(Bounds::new(origin, size(viewport.width - origin.x, viewport.height - origin.y)));
    }

    fn track_gesture(&mut self, position: Point, shift: bool) {
        let out = self.torn_off(position);
        self.apply_gesture(position, shift);
        let tears = self.model.detached.is_none();
        // Out past the margin the window leaves on release, so it previews no snap or swap.
        if let Some(gesture) = self.gesture.as_mut().filter(|gesture| out && tears && gesture.kind == GestureKind::Move) {
            gesture.snap = None;
            gesture.swap = None;
            gesture.insert = None;
        }
    }

    fn apply_gesture(&mut self, position: Point, shift: bool) {
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
            .filter(|(id, _)| self.group_of(id) != group && current.contains(id))
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
                self.notify();
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
                        let ids = self.chrome.keys().filter(|id| self.group_of(id) == group).cloned().collect::<Vec<_>>();
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
                            self.focus_window(&gesture.tab_id);
                            self.refresh_url();
                        }
                    }
                } else if over_bar {
                    gesture.snap = None;
                    let space = self.space_chips.iter().find(|(_, chip)| chip.contains(&position)).map(|(id, _)| id.clone());
                    let active = self.model.lock().active_session_id.clone();
                    // Hovering a space opens it, taking the window along to be dragged down into it.
                    if let Some(space) = space.filter(|space| *space != active) {
                        let group = self.group_id(&gesture.tab_id);
                        let ids = self.chrome.keys().filter(|id| self.group_of(id) == group).cloned().collect::<Vec<_>>();
                        self.with_model(|model| {
                            let _ = model.move_tabs_to_session(&ids, &space);
                            let _ = model.switch_session(&space);
                        });
                        self.focus_window(&gesture.tab_id);
                        self.refresh_url();
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
                        .filter(|other| current.contains(other) && self.group_of(other) != group && self.is_front(other))
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
        self.notify();
    }

    fn finish_gesture(&mut self, out: Option<Point>) {
        let carried = self.carried();
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        self.lift = Tween::new(false);
        // shortcut: only the main window tears windows off; a detached one would need its own space hand-off.
        if let (Some(at), GestureKind::Move, None) = (out, gesture.kind, &self.model.detached) {
            let snap = gesture.from.map(|(snap, ..)| snap);
            let group = self.group_id(&gesture.tab_id);
            let ids = self.chrome.keys().filter(|id| self.group_of(id) == group).cloned().collect::<Vec<_>>();
            for id in ids {
                if let Some(entry) = self.chrome.get_mut(&id) {
                    entry.home = Some((gesture.start, snap, gesture.restore));
                }
            }
            if let Some(entry) = self.chrome.get_mut(&gesture.tab_id) {
                (entry.frame, entry.restore, entry.snap) = (Some(gesture.start), gesture.restore, None);
            }
            self.detach_window(&gesture.tab_id, at);
            return;
        }
        // Released still small: grow into its frame in whichever space is open now.
        if let Some((id, from)) = carried {
            self.landing = (!self.reduce_motion).then(|| (id, from, Instant::now()));
        }
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
        self.focus_active_content();
        self.notify();
    }

    /// Places pages for this frame; true when a new tab page's area changed size and needs laying out again.
    fn place_windows(&mut self) -> bool {
        if self.self_test.is_some() {
            return false;
        }
        self.engine.set_modal(self.modal_open());
        let Some(stage) = self.stage_bounds else {
            return false;
        };
        let origin_x = f64::from(stage.origin.x);
        let origin_y = f64::from(stage.origin.y);
        let stage_w = f64::from(stage.size.width);
        let stage_h = f64::from(stage.size.height);
        let ids = self.order.clone();
        // `order` spans every space; only this space's windows are on the stage.
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let mut stack = Vec::new();
        let mut resized = false;
        let carried = self.carried().map(|(id, _)| id);
        for id in ids {
            let Some(entry) = self.chrome.get(&id).cloned() else {
                continue;
            };
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
            let (scale, alpha) = self.window_motion(&id);
            let frame = scaled(frame, scale);
            // A torn-off window's page fills it edge to edge: no title bar, no border.
            let (title_h, inset) = if self.tab_window { (0.0, 0.0) } else { (self.title_h(), PAGE_INSET) };
            let rect = LogicalRect {
                x: origin_x + f64::from(frame.x) + inset,
                y: origin_y + f64::from(frame.y) + title_h,
                width: (f64::from(frame.w) - inset * 2.0).max(1.0),
                height: (f64::from(frame.h) - title_h - inset).max(1.0),
            };
            if id == self.find_tab {
                self.find_anchor = Some(rect.clone());
            }
            let home = self.engine.has_home(&id);
            if home {
                let size = (rect.width, rect.height);
                resized |= self.page_sizes.insert(id.clone(), size) != Some(size);
            }
            self.engine.place_tab(&id, Some(rect.clone()));
            self.engine.set_alpha(&id, f64::from(alpha));
            let shell = LogicalRect {
                x: origin_x + f64::from(frame.x),
                y: origin_y + f64::from(frame.y),
                width: f64::from(frame.w),
                height: f64::from(frame.h),
            };
            // The whole window shows through: its native frame sits over its page, beneath this view.
            let hole = if self.tab_window { rect } else { shell.clone() };
            stack.push((id, shell, hole));
        }
        let windows = stack.iter().map(|(id, _, _)| (id.clone(), self.group_id(id))).collect::<Vec<_>>();
        if !self.tab_window {
            // Frames move with their pages here, in the same pass; `sync_frames` (in render) runs a step behind.
            for ((_, shell, _), (_, group)) in stack.iter().zip(&windows) {
                self.engine.place_window_frame(group, shell.clone());
            }
        }
        self.engine.stack_tabs(&windows);
        if let Some(id) = self.home_caret.take() {
            self.engine.focus_home(&id);
        }
        let stage = LogicalRect { x: origin_x, y: origin_y, width: stage_w, height: stage_h };
        let (mut holes, mut notches) = visible_pages(&stack, &stage);
        // Pages show through holes in the chrome, so cut modal cards and menus out of them.
        for (rect, radius) in &self.overlay_rects {
            cut_overlay(&mut holes, &mut notches, rect, *radius);
        }
        self.engine.set_page_holes(&holes, &notches);
        let resizable = self.chrome.iter().filter(|(_, entry)| !entry.maximized).map(|(id, _)| id.clone()).collect::<Vec<_>>();
        self.engine.set_grab_zones(&grab_zones(&stack, &resizable));
        resized
    }

    fn begin_move(&mut self, id: &str, position: Point) {
        if self.suppress_window_drag {
            self.suppress_window_drag = false;
            return;
        }
        // The title bar covers the top edge, so a press there has to resize instead of drag.
        if let Some(edges) = self.edges_under_pointer(id, position) {
            if edges.top {
                self.begin_resize(id, edges, position);
                return;
            }
        }
        self.focus_window(id);
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
        self.notify();
    }

    fn begin_resize(&mut self, id: &str, edges: Edges, position: Point) {
        self.focus_window(id);
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
            self.snapped_windows().into_iter().filter(|(other, _)| self.group_of(other) != group).collect()
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
        self.notify();
    }

    /// The stage under the windows: measured and laid out here, with each terminal's screen drawn where its
    /// page would be. The windows' frames are native, beneath this view (see `frames.rs`).
    /// Where `snap` puts window `id`, fitted beside this space's other snapped windows.
    fn snap_frame(&self, snap: Snap, id: &str, sw: f32, sh: f32) -> Frame {
        let current = self.model.lock().session_tabs().into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let group = self.group_id(id);
        let others = self
            .chrome
            .iter()
            .filter(|(other, entry)| current.contains(other) && self.group_of(other) != group && self.is_front(other) && !entry.minimized && !entry.maximized)
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

    fn edges_under_pointer(&self, id: &str, position: Point) -> Option<Edges> {
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

    pub(crate) fn focus_window(&mut self, id: &str) {
        self.order.retain(|existing| existing != id);
        self.order.push(id.to_string());
        if let Some(entry) = self.chrome.get_mut(id) {
            entry.minimized = false;
        }
        if self.active_tab_id().as_deref() != Some(id) {
            self.with_model(|model| { let _ = model.activate_tab(id); });
            self.refresh_url();
        }
        // After placing: a page un-minimized just now is hidden until placed, and AppKit won't focus a hidden view.
        let _ = self.place_windows();
        self.focus_active_content();
        self.notify();
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

    fn minimize_window(&mut self, id: &str) {
        let entry = self.chrome.entry(id.to_string()).or_default();
        entry.minimized = true;
        entry.maximized = false;
        self.notify();
    }

    fn toggle_maximize(&mut self, id: &str) {
        let next = !self.chrome.get(id).is_some_and(|chrome| chrome.maximized);
        for chrome in self.chrome.values_mut() {
            chrome.maximized = false;
        }
        let entry = self.chrome.entry(id.to_string()).or_default();
        entry.maximized = next;
        entry.minimized = false;
        self.focus_window(id);
    }

    /// Closes every tab of the window holding `id`.
    fn close_window(&mut self, id: &str) {
        let group = self.group_id(id);
        let members = self.chrome.keys().filter(|other| self.group_of(other) == group).cloned().collect::<Vec<_>>();
        self.close_tabs(members);
    }

    /// The shown window nearest the active one in direction `dir`.
    fn window_toward(&self, dir: Dir) -> Option<String> {
        let active = self.active_tab_id()?;
        let from = self.visual_frame(&active)?;
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let mut others = self
            .shown_tabs(&tabs)
            .into_iter()
            .filter(|tab| tab.id != active)
            .filter_map(|tab| Some((tab.id.clone(), self.visual_frame(&tab.id)?)))
            .collect::<Vec<_>>();
        // Least to most recently focused, so going back returns to where you came from.
        others.sort_by_key(|(id, _)| self.order.iter().position(|o| o == id));
        neighbour(from, &others, dir)
    }

    /// Tiled: trades places with the neighbour that way. Free: snaps to that half of the stage.
    fn move_window(&mut self, dir: Dir) {
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
        let _ = self.place_windows();
        self.notify();
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

    /// A torn-off window's title bar: what its native spaces and tools don't cover drags the window.
    /// The shown tab of a torn-off window and all its tabs in order.
    fn window_front<'a>(&self, all_tabs: &'a [browser_core::Tab], session: &str) -> Option<(String, Vec<&'a browser_core::Tab>)> {
        let mut members = all_tabs.iter().filter(|tab| tab.session_id == session).collect::<Vec<_>>();
        members.sort_by_key(|tab| tab.position);
        let active = self.active_tab_id();
        let front = members.iter().find(|tab| Some(&tab.id) == active.as_ref()).or(members.first())?.id.clone();
        Some((front, members))
    }

    /// Gives this torn-off window's pages and layout back to the main window and closes this one.
    fn merge_into_main(&mut self) {
        let Some(own) = self.model.detached.clone() else {
            return;
        };
        let Some(main) = crate::main_window() else {
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
        self.shell.close();
        crate::update(main, |main| {
            main.take_back(&own, tab_window, pages, chrome);
            main.notify();
        });
    }

    /// Main window: takes back a torn-off space, or a torn-off window into the space it shows now.
    fn take_back(&mut self, space: &str, tab_window: bool, pages: Vec<(String, LiveTab)>, chrome: HashMap<String, TabChrome>) {
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
            self.refresh_url();
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
            self.focus_window(&id);
        }
        self.refresh_url();
    }

    /// Moves the window holding `id` into a new space and opens that space in an OS window at `at` (screen point).
    fn detach_window(&mut self, id: &str, at: Point) {
        let group = self.group_id(id);
        let ids = self.chrome.keys().filter(|other| self.group_of(other) == group).cloned().collect::<Vec<_>>();
        self.detach_tabs(&ids, at);
    }

    /// Moves `ids` into a new space and opens that space in an OS window at `at` (screen point).
    fn detach_tabs(&mut self, ids: &[String], at: Point) {
        let Ok(space) = self.with_model(|model| model.create_empty_session()) else {
            return;
        };
        self.with_model(|model| {
            let _ = model.move_tabs_to_session(ids, &space);
        });
        self.open_space_window(&space, true, at);
    }

    /// Hands `space` with its live pages and window layout to a new OS window at `at` (screen point).
    /// `tab_window`: `space` is a hidden one holding a torn-off window.
    fn open_space_window(&mut self, space: &str, tab_window: bool, at: Point) {
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
        let at = at - point(80., 16.);
        crate::defer(move || open_space(model, extensions, space, tab_window, pages, chrome, at));
        self.refresh_url();
    }

    /// Opens a new tab inside the window holding `id`, sharing its frame. `None` opens the new-tab page
    /// with the address field focused.
    pub(crate) fn new_tab_in(&mut self, id: &str, url: Option<&str>) {
        if self.chrome.get(id).is_some_and(|chrome| chrome.panel.is_some()) {
            // A side panel holds only its extension page: the new tab gets a regular window.
            let Ok(new) = self.with_model(|model| model.create_tab(&url.map_or_else(|| model.settings.new_tab_url(), str::to_string))) else {
                return;
            };
            self.focus_window(&new);
            self.focus_address = url.is_none();
            return;
        }
        let group = self.group_id(id);
        let mut chrome = self.chrome.get(id).cloned().unwrap_or_default();
        chrome.window = Some(group);
        let created = self.with_model(|model| model.create_tab_beside(&url.map_or_else(|| model.settings.new_tab_url(), str::to_string), id));
        let Ok(new) = created else {
            return;
        };
        self.chrome.insert(new.clone(), chrome);
        self.focus_window(&new);
        self.focus_address = url.is_none();
    }

    /// A link opened from tab `from`'s page: the new tab lands right after `from` and the tabs it
    /// opened before, so several links keep their order.
    fn open_from(&mut self, from: &str, url: &str) {
        self.new_tab_in(from, Some(url));
        let Some(child) = self.active_tab_id().filter(|child| child != from) else {
            return;
        };
        let rest = self.model.lock().session_tabs().iter().map(|tab| tab.id.clone()).filter(|id| *id != child).collect::<Vec<_>>();
        if let Some(at) = rest.iter().position(|id| id == from) {
            let siblings = rest[at + 1..].iter().take_while(|id| self.openers.get(*id).is_some_and(|opener| opener == from)).count();
            self.with_model(|model| { let _ = model.move_tab(&child, at + 1 + siblings); });
        }
        self.openers.insert(child, from.to_string());
    }

    /// Links opened from other apps: ask which profile takes them, or use the active one.
    pub(crate) fn open_external(&mut self, urls: Vec<String>) {
        let (ask, active) = {
            let model = self.model.lock();
            (model.settings.ask_link_profile && model.profiles.len() > 1, model.active_profile_id.clone())
        };
        if ask {
            self.pending_links.extend(urls);
            self.notify();
        } else {
            self.open_links_in(&active, urls);
        }
    }

    /// Switches to `profile` and opens each link in a new tab, as Cmd+T would.
    pub(crate) fn open_links_in(&mut self, profile: &str, urls: Vec<String>) {
        if self.model.lock().active_profile_id != profile {
            self.with_model(|model| { let _ = model.switch_profile(profile); });
            self.refresh_url();
        }
        for url in urls {
            match self.active_tab_id() {
                Some(id) => self.new_tab_in(&id, Some(&url)),
                None => {
                    if let Ok(id) = self.with_model(|model| model.create_tab(&url)) {
                        self.focus_window(&id);
                    }
                }
            }
        }
        self.notify();
    }

    /// The + button: a new tab in the window holding `id`, or a terminal with Shift held.
    fn click_new_tab(&mut self, id: &str, shift: bool) {
        if shift {
            self.focus_window(id);
            self.on_shortcut(Shortcut::NewTerminal);
        } else {
            self.new_tab_in(id, None);
        }
    }

    /// Moves a tab into the window holding `target`, in front, sharing its frame.
    fn move_tab_to_window(&mut self, id: &str, target: &str) {
        let group = self.group_id(target);
        if self.group_of(id) == group {
            return;
        }
        let mut chrome = self.chrome.get(target).cloned().unwrap_or_default();
        chrome.window = Some(group);
        self.chrome.insert(id.to_string(), chrome);
        self.focus_window(id);
    }

    /// Pulls a tab out of its window into one of its own, under the pointer.
    pub(crate) fn detach_tab(&mut self, id: &str, at: Point) {
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
        self.focus_window(id);
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

    /// How far `id` is still drawn from its slot while it slides into place.
    fn slide_offset(&self, id: &str) -> Point {
        self.slides.get(id).map_or_else(Point::default, |(from, at)| {
            let left = 1.0 - ease(at.elapsed().as_secs_f32() / SLIDE_ANIM.as_secs_f32());
            point(from.x * left, from.y * left)
        })
    }

    /// Slides a row's items from where they are drawn in `before` to their places in `after`, laid out `gap` apart.
    /// Items new to the row just appear: the only one is the invisible gap for a dragged item.
    fn slide_row(&mut self, before: &[String], after: &[String], gap: f32, vertical: bool) {
        if self.reduce_motion {
            return;
        }
        let along = |p: Point| if vertical { p.y } else { p.x };
        let start = before.iter().filter_map(|id| self.slots.get(id)).map(|slot| along(slot.origin)).reduce(|a, b| if b < a { b } else { a });
        let (Some(start), Some(size)) = (start, self.reorder.as_ref().map(|state| state.size)) else {
            return;
        };
        // A dragged item new to the row has no slot here yet, but its size is known.
        let slot_of = |id: &String| self.slots.get(id).copied().filter(|_| before.contains(id));
        let span = |id: &String| slot_of(id).map_or(size, |slot| slot.size);
        let length = |ids: &[String]| ids.iter().map(|id| if vertical { span(id).height } else { span(id).width } + (gap)).fold(0., |a, b| a + b);
        // Tab rows sit centred in their title bar, so one that grows or shrinks moves its start by half the change.
        let mut next = if vertical { start } else { start - (length(after) - length(before)) / 2. };
        let now = Instant::now();
        for id in after {
            let slot = slot_of(id);
            let target = match slot {
                Some(slot) if vertical => point(slot.origin.x, next),
                Some(slot) => point(next, slot.origin.y),
                None => Point::default(),
            };
            let span = span(id);
            next += if vertical { span.height } else { span.width } + (gap);
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
    fn picked(&self, id: &str) -> bool {
        self.tab_drag.is_some() && self.reorder.as_ref().is_some_and(|state| state.id == id)
    }

    /// Tab rows of the windows on show, back to front, each in its tab order.
    pub(crate) fn tab_rows(&self) -> Vec<(String, Vec<String>)> {
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
                let ids = tabs.iter().filter(|id| self.group_of(id) == group).cloned().collect();
                (group, ids)
            })
            .collect()
    }

    /// Opens a gap under the dragged `id` in whichever of `rows` it is over (the front one wins),
    /// sliding that row's items aside and closing the gap in the row it left.
    /// `origin` is where the dragged item's top-left is drawn; its centre decides the gap.
    fn preview_drag(&mut self, id: &str, origin: Point, rows: Vec<(String, Vec<String>)>, vertical: bool, gap: f32) {
        let old = self.reorder.take().filter(|state| state.id == id);
        let Some(size) = old.as_ref().map(|state| state.size).or_else(|| self.slots.get(id).map(|slot| slot.size)) else {
            return;
        };
        let along = |p: Point| if vertical { p.y } else { p.x };
        let across = |p: Point| if vertical { p.x } else { p.y };
        let probe = origin + point(size.width / 2., size.height / 2.);
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
    fn click_tab(&mut self, id: &str) {
        // The shown tab is the address field, focused window or not, as clicking Safari's field always edits it.
        let edit = self.is_front(id);
        self.focus_window(id);
        if edit {
            self.focus_address = true;
            self.edit_in_strip = true;
        }
        self.notify();
    }

    /// A tab dropped on a window lands where the drag showed it in that window's tabs.
    fn drop_tab_on(&mut self, id: &str, target: &str, at: Point) {
        let group = self.group_id(target);
        let own = self.group_of(id) == group;
        if own && self.tab_press.take().is_some_and(|press| (at - press).magnitude() < TAP_SLOP) {
            self.reorder = None;
            self.click_tab(id);
            return;
        }
        let order = self.dropped_order(id, &group);
        self.reorder = None;
        // Only the tab row takes a tab in; dropped on the window's body it becomes a window of its own.
        if order.is_none() && self.group_len(id) > 1 {
            self.detach_tab(id, at);
            return;
        }
        if !own && order.is_some() {
            self.move_tab_to_window(id, target);
        }
        if let Some(order) = &order {
            self.move_into_gap(id, order);
        }
        // Its slot is the gap it was dropped into, or its own when dropped off the row of its own window.
        if order.is_some() || own {
            self.land(id, at - self.tab_grab.get());
        }
        self.notify();
    }

    /// Moves `id` to its place in `order`, a window's row as dropped.
    fn move_into_gap(&mut self, id: &str, order: &[String]) {
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

    /// Opens a gap in the tab row under a link dragged from a page, as for a dragged tab.
    fn preview_link(&mut self, at: (f64, f64)) {
        let size = size(120., self.pill_h());
        if self.reorder.as_ref().is_none_or(|state| state.id != LINK_DROP) {
            self.reorder = Some(Reorder { id: LINK_DROP.into(), size, at: Point::default(), row: None, order: Vec::new() });
        }
        let origin = point(at.0 as f32, at.1 as f32) - point(size.width / 2., size.height / 2.);
        let rows = self.tab_rows();
        self.preview_drag(LINK_DROP, origin, rows, false, 2.0);
    }

    /// A link dropped into a tab row's gap opens there as a new tab of that window.
    fn drop_link(&mut self, url: &str, at: (f64, f64)) {
        self.preview_link(at);
        let dropped = self.reorder.take().and_then(|state| state.row.map(|_| state.order));
        self.notify();
        let Some(mut order) = dropped else {
            return;
        };
        let Some(member) = order.iter().find(|id| *id != LINK_DROP).cloned() else {
            return;
        };
        self.new_tab_in(&member, Some(url));
        let Some(new) = self.active_tab_id() else {
            return;
        };
        for id in &mut order {
            if id == LINK_DROP {
                id.clone_from(&new);
            }
        }
        self.move_into_gap(&new, &order);
    }

    /// Glides a dropped item from where its drag preview was into its slot; the others already sit in theirs.
    fn land(&mut self, id: &str, from: Point) {
        if !self.reduce_motion {
            let width = frames::drag_pill_w(self.slots.get(id).map_or(160., |slot| f64::from(slot.size.width)));
            self.tab_landing = Some((id.to_string(), from, width, Instant::now()));
        }
    }

    /// The landing tab and how far along its glide is, while it glides.
    pub(crate) fn landing(&self) -> Option<(&str, Point, f64, f32)> {
        let (id, from, width, at) = self.tab_landing.as_ref()?;
        let t = at.elapsed().as_secs_f32() / SLIDE_ANIM.as_secs_f32();
        (t < 1.).then(|| (id.as_str(), *from, *width, ease(t)))
    }

    /// The window a tab belongs to; a tab never added to another window is its own.
    pub(crate) fn group_id(&self, id: &str) -> String {
        self.group_of(id).to_string()
    }

    /// `group_id` without the allocation, for the per-render comparisons over every tab.
    fn group_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.chrome.get(id).and_then(|chrome| chrome.window.as_deref()).unwrap_or(id)
    }

    pub(crate) fn group_len(&self, id: &str) -> usize {
        let group = self.group_of(id);
        self.chrome.keys().filter(|other| self.group_of(other) == group).count()
    }

    /// Whether this tab is the one its window shows: the group member raised most recently.
    pub(crate) fn is_front(&self, id: &str) -> bool {
        let group = self.group_of(id);
        raised_last(&self.order, id, |other| self.group_of(other) == group)
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

    fn animating(&self) -> bool {
        self.dock.running()
            || self.toast.1.elapsed() < TOAST_IN
            || self.toast_fading()
            || self.load_moving()
            || !self.opened.is_empty()
            || !self.slides.is_empty()
            || self.tab_landing.is_some()
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


    /// Shows the find bar at the top right of its page while it is open there, or updates it; closes it otherwise.
    fn sync_find(&mut self) {
        use crate::native::{text, Ink, Node, Weight, FIND_QUERY};
        let Some(page) = self.find_anchor.clone().filter(|_| self.find_here()) else {
            return self.engine.close_find();
        };
        let tally = match self.find_count {
            _ if self.typed(FIND_QUERY).is_empty() => String::new(),
            Some(0) => "No matches".to_string(),
            Some(count) => format!("{} of {count}", self.find_index.min(count)),
            None => String::new(),
        };
        let mut pop = crate::native::Pop::find();
        let previous = pop.act(|this| this.find_step(true));
        let next = pop.act(|this| this.find_step(false));
        let close = pop.act(|this| this.close_find());
        let content = Node::Column {
            gap: 0.,
            pad: (10., 4.),
            children: vec![Node::Row {
                gap: 4.,
                children: vec![
                    Node::Search { field: FIND_QUERY, value: crate::native::Initial(self.typed(FIND_QUERY)), placeholder: "Find in page".into(), size: 13. },
                    text(tally, 11., Weight::Regular, Ink::Secondary),
                    Node::Tool { symbol: "chevron.up", tip: "Previous match", action: previous, on: false },
                    Node::Tool { symbol: "chevron.down", tip: "Next match", action: next, on: false },
                    Node::Tool { symbol: "xmark", tip: "Close", action: close, on: false },
                ],
            }],
        };
        const WIDTH: f64 = 340.;
        self.engine.show_find((page.x + page.width - WIDTH - 10., page.y + 10.), WIDTH, &content);
        if std::mem::take(&mut self.focus_find) {
            self.engine.focus_find();
        }
        self.find_actions = pop.actions;
    }

    /// A key from the find bar's field: Return finds the next match, ⇧Return the previous, Esc closes it.
    fn find_key(&mut self, key: &str) {
        match key {
            "enter" => self.find_step(false),
            "shift-enter" => self.find_step(true),
            "escape" => {
                self.close_find();
                self.focus_active_content();
            }
            _ => {}
        }
        self.notify();
    }

    /// "site wants to use your camera" with Allow / Don't Allow, floating at the top of the window while WebKit waits.
    /// A page asking for the camera or microphone.
    fn permission_sheet(&self, pop: &mut crate::native::Pop) -> Option<(String, crate::native::Node)> {
        use crate::native::{button, sheet, Key, Style};
        let (request, origin, kind) = self.permission.clone()?;
        let what = match kind.as_str() {
            "microphone" => "your microphone",
            "camera-microphone" => "your camera and microphone",
            _ => "your camera",
        };
        let host = origin.split("://").nth(1).unwrap_or(&origin).to_string();
        let mut answer = |allow: bool| {
            let (origin, kind) = (origin.clone(), kind.clone());
            pop.act(move |this| {
                let _ = this.model.lock().set_permission(&origin, &kind, if allow { "allow" } else { "deny" });
                this.sync_permissions();
                this.engine.answer_permission(request, allow);
                this.permission = None;
            })
        };
        let (deny, allow) = (answer(false), answer(true));
        let content = sheet(
            format!("{host} wants to use {what}."),
            Vec::new(),
            vec![button("Don't Allow", deny, Style::Plain, Key::Escape), button("Allow", allow, Style::Primary, Key::Return)],
        );
        Some((format!("permission:{request}"), content))
    }

    /// Brings the first waiting dialog's page to the front, with the prompt's preset text and the keys.
    fn raise_dialog(&mut self) {
        let Some(dialog) = self.dialogs.first() else {
            return;
        };
        let (tab_id, default) = (dialog.tab_id.clone(), dialog.default.clone());
        self.popover_texts.insert(crate::native::DIALOG_INPUT, default);
        self.focus_window(&tab_id);
    }

    /// Answers dialog `request` if it is the one showing: a key and a click can both land on the same one.
    fn answer_dialog(&mut self, request: u64, ok: bool) {
        if self.dialogs.first().map(|dialog| dialog.request) != Some(request) {
            return;
        }
        self.dialogs.remove(0);
        let text = self.typed(crate::native::DIALOG_INPUT);
        self.engine.answer_dialog(request, ok, &text);
        if self.dialogs.is_empty() {
            self.focus_active_content();
        } else {
            self.raise_dialog();
        }
        self.notify();
    }

    /// The page dialog in front, floating at the top of the window while the page waits.
    /// The page dialog in front, while the page waits.
    fn dialog_sheet(&self, pop: &mut crate::native::Pop) -> Option<(String, crate::native::Node)> {
        use crate::native::{button, field, para, sheet, Key, Node, Style, DIALOG_INPUT};
        let dialog = self.dialogs.first()?;
        let request = dialog.request;
        let host = dialog.origin.split("://").nth(1).filter(|host| !host.is_empty()).unwrap_or("This page");
        // Browsers stopped showing a page's own beforeunload text, since pages used it to scare people into staying.
        let (title, message, cancel, ok) = match dialog.kind {
            DialogKind::Leave => ("Leave this page?".to_string(), "Changes you made may not be saved.".to_string(), Some("Stay"), "Leave"),
            DialogKind::Alert => (format!("{host} says"), dialog.message.clone(), None, "OK"),
            DialogKind::Confirm | DialogKind::Prompt => (format!("{host} says"), dialog.message.clone(), Some("Cancel"), "OK"),
            DialogKind::OpenApp => (format!("Open \u{201c}{}\u{201d}?", dialog.message), format!("{host} wants to open this app."), Some("Cancel"), "Open"),
        };
        let confirm = pop.act(move |this| this.answer_dialog(request, true));
        let mut body = Vec::new();
        if !message.is_empty() {
            body.push(Node::Scroll { max: 320., child: Box::new(para(message, 13., crate::native::Ink::Primary)) });
        }
        if dialog.kind == DialogKind::Prompt {
            body.push(field(DIALOG_INPUT, dialog.default.clone(), "", confirm));
        }
        let mut buttons = Vec::new();
        if let Some(label) = cancel {
            buttons.push(button(label, pop.act(move |this| this.answer_dialog(request, false)), Style::Plain, Key::Escape));
        }
        // An alert has no Cancel, so Esc dismisses it like OK; otherwise Return is OK.
        buttons.push(button(ok, confirm, Style::Primary, if cancel.is_some() { Key::Return } else { Key::Escape }));
        Some((format!("dialog:{request}"), sheet(title, body, buttons)))
    }


}

/// A page's alert, confirm, prompt or leave-page question, waiting on `answer_dialog`.
struct PageDialog {
    tab_id: String,
    origin: String,
    kind: DialogKind,
    message: String,
    default: String,
    request: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct TabChrome {
    pub(crate) minimized: bool,
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
const TAP_SLOP: f32 = 6.0;
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

/// The save-password offer's popover key, apart from `Popover`'s.
const SAVE_PASSWORD_KEY: u64 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Popover {
    Downloads,
    Extensions,
    History,
    Bookmarks,
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

const TITLEBAR_H: f32 = 30.0;
const LIFT_ANIM: Duration = Duration::from_millis(220);
const LAND_ANIM: Duration = Duration::from_millis(280);
/// Width a window shrinks to while carried over the spaces bar.
const LIFT_W: f32 = 140.0;
const WINDOW_ANIM: Duration = Duration::from_millis(170);
const SLIDE_ANIM: Duration = Duration::from_millis(220);
const WINDOW_FROM_SCALE: f32 = 0.94;
/// How often each window polls the engine, model and terminals.
const TICK: Duration = Duration::from_millis(250);
/// How long the stage keeps its size before window layouts are saved.
const STAGE_SETTLE: Duration = Duration::from_millis(300);
const TOAST_IN: Duration = Duration::from_millis(180);
const DOCK_ANIM: Duration = Duration::from_millis(260);
/// The dock's resting line, in the stage's foot inset (2pt + 5pt < SNAP_INSET).
const DOCK_REST_W: f32 = 56.0;
const DOCK_REST_H: f32 = 5.0;
/// The open dock's capsules, and the gap between the spaces and the tools.
pub(crate) const DOCK_H: f64 = 26.0;
pub(crate) const DOCK_GAP: f64 = 8.0;
const TOAST_LIFE: Duration = Duration::from_millis(2800);
const LOAD_FADE: Duration = Duration::from_millis(300);
const ZOOM_LEVELS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

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
/// Whether `id` sits above every other tab in `order` (back to front) that `same` puts in its group.
fn raised_last(order: &[String], id: &str, same: impl Fn(&str) -> bool) -> bool {
    let later = match order.iter().position(|other| other == id) {
        Some(index) => &order[index + 1..],
        None => order,
    };
    !later.iter().any(|other| other != id && same(other))
}

const WINDOW_RADIUS: f64 = 16.0;
/// Page URL and its icon URL, newline separated.
const FAVICON_JS: &str = r#"(() => { const l = document.querySelector('link[rel~="icon" i], link[rel="apple-touch-icon"]'); return location.href + "\n" + (l ? l.href : location.origin + "/favicon.ico"); })()"#;

/// Counts `query` in the page's visible text, ignoring case as WebKit's find does.
// shortcut: innerText skips iframes and can disagree with WebKit on odd whitespace; count via WebKit's own matcher if that shows.
fn find_count_js(query: &str) -> String {
    let query = serde_json::to_string(&query.to_lowercase()).unwrap_or_default();
    format!("(() => {{ const q = {query}, t = (document.body?.innerText ?? '').toLowerCase(); let n = 0; for (let i = t.indexOf(q); i !== -1; i = t.indexOf(q, i + q.length)) n++; return n; }})()")
}
const TITLE_H: f64 = 30.0;
/// Window border beside and below the page; PAGE_RADIUS in the macOS engine is WINDOW_RADIUS minus this.
const PAGE_INSET: f64 = 1.0;

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
    for (index, (id, own, full)) in stack.iter().enumerate() {
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
        // The window's own corners are rounded (its native frame draws the curve), so where the hole reaches one
        // these wedges keep the stage outside the curve.
        let (r, x2, y2) = (WINDOW_RADIUS, own.x + own.width, own.y + own.height);
        for (corner, center) in [((own.x, own.y), (own.x + r, own.y + r)), ((x2, own.y), (x2 - r, own.y + r)), ((own.x, y2), (own.x + r, y2 - r)), ((x2, y2), (x2 - r, y2 - r))] {
            let square = LogicalRect { x: corner.0.min(center.0), y: corner.1.min(center.1), width: r, height: r };
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

/// A side panel's place: a narrow column down the stage's right edge.
fn panel_frame(sw: f32, sh: f32) -> Frame {
    let w = 380f32.min(sw - 20.0);
    Frame { x: sw - w - 10.0, y: 10.0, w, h: sh - 20.0 }
}

fn mostly_off(frame: Frame, sw: f32, sh: f32) -> bool {
    let visible_w = (frame.x + frame.w).min(sw) - frame.x.max(0.0);
    let visible_h = (frame.y + frame.h).min(sh) - frame.y.max(0.0);
    visible_w < frame.w * 0.6 || visible_h < frame.h * 0.6 || frame.w > sw + 1.0 || frame.h > sh + 1.0
}

/// `frame` moved, and shrunk if it must be, to lie wholly on a `sw` × `sh` stage: a floating window the stage
/// shrank under (the app window resized, or reopened smaller) comes back into view, as macOS does on a smaller screen.
fn fit_on_stage(frame: Frame, sw: f32, sh: f32) -> Frame {
    let w = frame.w.min(sw - SNAP_INSET * 2.0).max(1.0);
    let h = frame.h.min(sh - SNAP_INSET * 2.0).max(1.0);
    Frame { x: frame.x.clamp(0.0, sw - SNAP_INSET - w), y: frame.y.clamp(0.0, sh - SNAP_INSET - h), w, h }
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

/// `site_icon` for a native popover.
pub(crate) fn site_image(url: &str, size: f64) -> crate::native::Node {
    let (letter, host) = site_glyph(url);
    let path = favicon_path(&host);
    let path = (is_web(url) && path.exists()).then_some(path);
    crate::native::Node::Image { path, letter, rgb: crate::native::rgb(site_color(&host)), size, radius: (size / 4.).max(4.) }
}

pub(crate) fn favicon_path(host: &str) -> std::path::PathBuf {
    let name: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    app_support_dir().join("favicons").join(format!("icon-{name}"))
}

/// A new tab: shown as the native new-tab page, its web view kept hidden until it navigates.
pub(crate) fn is_blank(url: &str) -> bool {
    url.is_empty() || url == "about:blank" || url == "nxbrowser://home"
}

fn is_web(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

pub(crate) fn site_color(host: &str) -> Color {
    let mut hash = 0u32;
    for byte in host.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    hsl(f64::from(hash % 360) / 360., 0.62, 0.38)
}

fn full_title(tab: &browser_core::Tab) -> String {
    match tab.title.as_str() {
        "" | "New Tab" => tab_label(&tab.title, &tab.url),
        title => title.to_string(),
    }
}

/// What a window torn off the main one brings: its space, the space's live pages and their window layout.
pub(crate) struct Handoff {
    pub(crate) session: String,
    pub(crate) tab_window: bool,
    pub(crate) extensions: Extensions,
    pages: Vec<(String, LiveTab)>,
    chrome: HashMap<String, TabChrome>,
}


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

/// Whether `at` (screen point) is over the main window's rail.
fn over_main_bar(at: Point) -> bool {
    let Some(((x, y), (_, height))) = crate::main_window().and_then(|main| crate::update(main, |view| (view.shell.origin(), view.shell.size()))) else {
        return false;
    };
    Bounds::new(point(x as f32, y as f32), size(crate::rail::RAIL_W, height as f32)).contains(&at)
}

/// Opens `session` in an OS window of its own whose top-left is at `at` (screen point).
/// Opens `session` in an OS window of its own whose top-left is at `at` (screen point).
fn open_space(model: SharedModel, extensions: Extensions, session: String, tab_window: bool, pages: Vec<(String, LiveTab)>, chrome: HashMap<String, TabChrome>, at: Point) {
    let handoff = Handoff { session, tab_window, extensions, pages, chrome };
    crate::open_browser_window(Some((f64::from(at.x), f64::from(at.y))), (1000., 700.), move |shell| BrowserWindow::new(model, Some(handoff), false, Instant::now(), shell));
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
        debug_assert!(!MODEL_HELD.replace(true), "model locked twice on one thread (deadlock): drop the first guard before calling a helper that locks");
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
        MODEL_HELD.set(false);
    }
}

thread_local! {
    /// Whether this thread holds a `Pinned`: the mutex is not reentrant, so a second `lock` would hang the UI.
    static MODEL_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The profile's color, or the theme accent when it has none.
pub(crate) fn profile_tint(profile: &browser_core::Profile, paint: Paint) -> Color {
    if profile.color.is_empty() { paint.accent } else { parse_hex(&profile.color) }
}

/// `profile_badge` for a native sheet.
pub(crate) fn profile_badge_node(profile: &browser_core::Profile, active: bool, size: f64, paint: Paint) -> crate::native::Node {
    use crate::native::{text, Ink, Node, Weight};
    let [r, g, b] = crate::native::rgb(profile_tint(profile, paint));
    let glyph = if profile.icon.ends_with(".svg") {
        crate::native::asset(&profile.icon).map(|data| Node::Svg { data, size: size * 0.5, ink: Ink::Primary })
    } else {
        let mark = if profile.icon.is_empty() { rail_mark(&profile.name) } else { profile.icon.clone() };
        Some(text(mark, size * 0.45, Weight::Medium, Ink::Primary))
    };
    Node::Swatch { size, fill: Some([r, g, b, if active { 0.3 } else { 0.12 }]), selected: active, action: None, child: glyph.map(Box::new) }
}

/// Media in use by a profile's pages.
#[derive(Clone, Copy, Default)]
pub(crate) struct Activity {
    pub(crate) camera: bool,
    pub(crate) microphone: bool,
    pub(crate) sound: bool,
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
    fn colours_parse_and_convert() {
        assert_eq!(parse_hex("#ff8000"), [1., 128. / 255., 0., 1.]);
        assert_eq!(parse_hex("bogus"), [0., 0., 0., 1.]);
        assert_eq!(hsl(0., 1., 0.5), [1., 0., 0., 1.]);
        assert_eq!(hsl(2. / 3., 1., 0.5), [0., 0., 1., 1.]);
    }

    #[test]
    fn fit_on_stage_brings_a_window_back_whole() {
        let fit = |x, y, w, h| {
            let frame = fit_on_stage(Frame { x, y, w, h }, 800.0, 600.0);
            (frame.x, frame.y, frame.w, frame.h)
        };
        // Hanging off the right and bottom: moved in, size kept.
        assert_eq!(fit(500.0, 400.0, 400.0, 300.0), (392.0, 292.0, 400.0, 300.0));
        // Larger than the stage: shrunk to fit inside its margins.
        assert_eq!(fit(100.0, 50.0, 1200.0, 900.0), (8.0, 8.0, 784.0, 584.0));
        // Already on the stage: untouched.
        assert_eq!(fit(20.0, 30.0, 300.0, 200.0), (20.0, 30.0, 300.0, 200.0));
    }

    fn window_model() -> WindowModel {
        let model = Model::open(browser_storage::Storage::open_in_memory().unwrap()).unwrap();
        WindowModel { shared: std::sync::Arc::new(std::sync::Mutex::new(model)), detached: None }
    }

    #[test]
    fn model_locks_again_once_the_guard_drops() {
        let model = window_model();
        drop(model.lock());
        drop(model.lock());
    }

    #[test]
    #[should_panic(expected = "locked twice")]
    fn second_lock_on_one_thread_panics_instead_of_hanging() {
        let model = window_model();
        let _held = model.lock();
        let _ = model.lock();
    }

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
    fn overlay_drops_page_corner_wedges_it_touches() {
        let stack = vec![("page".to_string(), r(0., 0., 100., 120.), r(0., 0., 100., 120.))];
        let (mut holes, mut notches) = visible_pages(&stack, &r(0., 0., 1000., 1000.));
        assert_eq!(notches.len(), 4);
        // A popover whose edge crosses the bottom-left corner square, but not the corner point.
        cut_overlay(&mut holes, &mut notches, &r(5., 60., 40., 55.), 10.);
        assert!(!notches.contains(&((0., 120.), (16., 104.))));
        assert!(notches.contains(&((100., 120.), (84., 104.))));
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
        // Each window's own corners the hole reaches get a wedge too, unless a front window covers them.
        assert_eq!(notches, vec![
            ((60., 100.), (76., 84.)),
            ((0., 120.), (16., 104.)),
            ((100., 120.), (84., 104.)),
            ((60., 100.), (76., 84.)),
            ((160., 100.), (144., 84.)),
        ]);
        let covered = vec![
            ("back".to_string(), r(0., 0., 100., 120.), r(0., 10., 100., 110.)),
            ("front".to_string(), r(-5., 100., 50., 40.), r(-5., 110., 50., 30.)),
        ];
        assert!(!visible_pages(&covered, &r(0., 0., 1000., 1000.)).1.contains(&((0., 120.), (16., 104.))));
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
        assert_eq!(grip_edges(2., 2., 400., 300.), Some(Edges { left: true, right: false, top: true, bottom: false }));
        assert_eq!(grip_edges(398., 2., 400., 300.), Some(Edges { left: false, right: true, top: true, bottom: false }));
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
}
