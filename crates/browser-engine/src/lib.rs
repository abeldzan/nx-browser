use std::sync::mpsc::Receiver;

/// A tab's page area left uncovered by the chrome overlay.
pub type Hole = (String, LogicalRect);
/// (outer corner, arc centre) of a rounded-corner sliver that also shows the page.
pub type Notch = ((f64, f64), (f64, f64));

#[derive(Clone, Debug, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Navigated {
        tab_id: String,
        url: String,
        title: String,
        loading: bool,
    },
    Failed {
        tab_id: String,
        message: String,
    },
    /// WebKit is holding a download until `save_download(download, path)` says where to write it.
    Download {
        tab_id: String,
        url: String,
        /// The server's or the link's file name.
        filename: String,
        download: u64,
    },
    /// A download finished, or failed with `error` (cancelled included).
    DownloadDone {
        download: u64,
        bytes: u64,
        error: Option<String>,
    },
    /// A site asking for camera/microphone with no saved answer; WebKit waits for `answer_permission(request, ..)`.
    Permission {
        tab_id: String,
        origin: String,
        kind: String,
        request: u64,
    },
    /// A message from the password script in a page frame (JSON with a `kind`); the page waits for
    /// `answer_passwords(request, ..)`. `origin` comes from WebKit, not the page.
    Passwords {
        tab_id: String,
        origin: String,
        request: u64,
        message: String,
    },
    OpenInNewTab {
        url: String,
        /// The tab whose page asked for it; the new tab joins that tab's window.
        from: Option<String>,
    },
    Alert {
        message: String,
    },
    Eval {
        tab_id: String,
        token: String,
        value: String,
    },
    Shortcut(Shortcut),
    /// An extension finished loading, failed, or changed its toolbar button (badge, icon).
    ExtensionChanged {
        id: String,
    },
    /// An extension asks for more permissions; answer with `answer_extension_prompt`.
    ExtensionPrompt {
        prompt: u64,
        id: String,
        request: String,
    },
    ActivateTab {
        tab_id: String,
    },
    CloseTab {
        tab_id: String,
    },
    /// ⌥-clicked link, to show in a floating preview over its page.
    Peek {
        url: String,
    },
    Audio {
        tab_id: String,
        playing: bool,
    },
    /// The page is recording from the camera or microphone.
    Capture {
        tab_id: String,
        camera: bool,
        microphone: bool,
    },
}

/// Browser settings the engine applies to web views.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineSettings {
    pub javascript: bool,
    pub block_popups: bool,
    pub fraud_warnings: bool,
    pub https_only: bool,
    pub block_ads: bool,
    pub inspectable: bool,
    pub user_agent: String,
    pub page_zoom: f64,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            javascript: true,
            block_popups: true,
            fraud_warnings: true,
            https_only: false,
            block_ads: true,
            inspectable: true,
            user_agent: String::new(),
            page_zoom: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    FocusUrl,
    Reload,
    /// Reload bypassing the cache.
    HardReload,
    Back,
    Forward,
    Find,
    Bookmark,
    Settings,
    ReopenTab,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// 1-based position; 9 picks the last tab, as in Chrome.
    SelectTab(usize),
    NextTab,
    PrevTab,
    Print,
    CommandPalette,
    Inspect,
    NewTerminal,
    /// Re-splits the space's windows among the stage.
    TileWindows,
    /// Focuses the nearest window on the stage in that direction.
    FocusWindow(Dir),
    /// Tiled: swaps places with the neighbour that way; free: snaps to that half.
    MoveWindow(Dir),
    /// Closes every tab of the active window.
    CloseWindow,
    MaximizeWindow,
    MinimizeWindow,
    NextSpace,
    PrevSpace,
    NewSpace,
    History,
    Downloads,
    Bookmarks,
    CopyUrl,
    /// Escape; the page still gets the key.
    Dismiss,
}

impl Shortcut {
    /// Keys a focused page gets first and keeps if it handles them (as Chrome does):
    /// Excalidraw uses Cmd+D to duplicate, docs editors take Cmd+F.
    pub fn page_first(self) -> bool {
        matches!(
            self,
            Self::Reload | Self::HardReload | Self::Back | Self::Forward | Self::Find | Self::Bookmark | Self::Print
                | Self::ZoomIn | Self::ZoomOut | Self::ZoomReset | Self::CommandPalette
        )
    }
}

/// A rebindable app shortcut; `keys` is the default, in gpui keystroke form, empty for none.
pub struct Binding {
    /// Settings key; stable across releases.
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub keys: &'static str,
    pub action: Shortcut,
}

const fn bind(id: &'static str, group: &'static str, label: &'static str, keys: &'static str, action: Shortcut) -> Binding {
    Binding { id, group, label, keys, action }
}

/// Every rebindable shortcut, in the order settings lists them.
pub const SHORTCUTS: &[Binding] = &[
    bind("new_tab", "Tabs", "New tab", "cmd-t", Shortcut::NewTab),
    bind("new_terminal", "Tabs", "New terminal", "alt-cmd-t", Shortcut::NewTerminal),
    bind("close_tab", "Tabs", "Close tab", "cmd-w", Shortcut::CloseTab),
    bind("reopen_tab", "Tabs", "Reopen closed tab", "cmd-shift-t", Shortcut::ReopenTab),
    bind("next_tab", "Tabs", "Next tab", "ctrl-tab", Shortcut::NextTab),
    bind("prev_tab", "Tabs", "Previous tab", "ctrl-shift-tab", Shortcut::PrevTab),
    bind("select_tab_1", "Tabs", "Go to tab 1", "cmd-1", Shortcut::SelectTab(1)),
    bind("select_tab_2", "Tabs", "Go to tab 2", "cmd-2", Shortcut::SelectTab(2)),
    bind("select_tab_3", "Tabs", "Go to tab 3", "cmd-3", Shortcut::SelectTab(3)),
    bind("select_tab_4", "Tabs", "Go to tab 4", "cmd-4", Shortcut::SelectTab(4)),
    bind("select_tab_5", "Tabs", "Go to tab 5", "cmd-5", Shortcut::SelectTab(5)),
    bind("select_tab_6", "Tabs", "Go to tab 6", "cmd-6", Shortcut::SelectTab(6)),
    bind("select_tab_7", "Tabs", "Go to tab 7", "cmd-7", Shortcut::SelectTab(7)),
    bind("select_tab_8", "Tabs", "Go to tab 8", "cmd-8", Shortcut::SelectTab(8)),
    bind("select_tab_last", "Tabs", "Go to last tab", "cmd-9", Shortcut::SelectTab(9)),
    bind("focus_url", "Navigation", "Focus address bar", "cmd-l", Shortcut::FocusUrl),
    bind("back", "Navigation", "Go back", "cmd-[", Shortcut::Back),
    bind("forward", "Navigation", "Go forward", "cmd-]", Shortcut::Forward),
    bind("reload", "Navigation", "Reload page", "cmd-r", Shortcut::Reload),
    bind("hard_reload", "Navigation", "Reload, ignoring cache", "cmd-shift-r", Shortcut::HardReload),
    bind("copy_url", "Navigation", "Copy page address", "cmd-shift-c", Shortcut::CopyUrl),
    bind("focus_left", "Windows & spaces", "Focus window to the left", "ctrl-cmd-left", Shortcut::FocusWindow(Dir::Left)),
    bind("focus_right", "Windows & spaces", "Focus window to the right", "ctrl-cmd-right", Shortcut::FocusWindow(Dir::Right)),
    bind("focus_up", "Windows & spaces", "Focus window above", "ctrl-cmd-up", Shortcut::FocusWindow(Dir::Up)),
    bind("focus_down", "Windows & spaces", "Focus window below", "ctrl-cmd-down", Shortcut::FocusWindow(Dir::Down)),
    bind("move_left", "Windows & spaces", "Move window left", "ctrl-cmd-shift-left", Shortcut::MoveWindow(Dir::Left)),
    bind("move_right", "Windows & spaces", "Move window right", "ctrl-cmd-shift-right", Shortcut::MoveWindow(Dir::Right)),
    bind("maximize_window", "Windows & spaces", "Maximize window", "", Shortcut::MaximizeWindow),
    bind("minimize_window", "Windows & spaces", "Minimize window", "", Shortcut::MinimizeWindow),
    bind("close_window", "Windows & spaces", "Close window", "cmd-shift-w", Shortcut::CloseWindow),
    bind("tile_windows", "Windows & spaces", "Tile windows", "", Shortcut::TileWindows),
    bind("next_space", "Windows & spaces", "Next space", "alt-cmd-right", Shortcut::NextSpace),
    bind("prev_space", "Windows & spaces", "Previous space", "alt-cmd-left", Shortcut::PrevSpace),
    bind("new_space", "Windows & spaces", "New space", "ctrl-cmd-n", Shortcut::NewSpace),
    bind("find", "Page", "Find in page", "cmd-f", Shortcut::Find),
    bind("bookmark", "Page", "Bookmark page", "cmd-d", Shortcut::Bookmark),
    bind("print", "Page", "Print page", "cmd-p", Shortcut::Print),
    bind("zoom_in", "Page", "Zoom in", "cmd-=", Shortcut::ZoomIn),
    bind("zoom_out", "Page", "Zoom out", "cmd--", Shortcut::ZoomOut),
    bind("zoom_reset", "Page", "Reset zoom", "cmd-0", Shortcut::ZoomReset),
    bind("inspect", "Page", "Inspect page", "alt-cmd-i", Shortcut::Inspect),
    bind("command_palette", "App", "Command palette", "cmd-k", Shortcut::CommandPalette),
    bind("settings", "App", "Open settings", "cmd-,", Shortcut::Settings),
    bind("history", "App", "Show history", "cmd-y", Shortcut::History),
    bind("downloads", "App", "Show downloads", "alt-cmd-l", Shortcut::Downloads),
    bind("bookmarks", "App", "Show bookmarks", "alt-cmd-b", Shortcut::Bookmarks),
];

pub trait WebEngine {
    fn attach(&mut self, ns_view: *mut std::ffi::c_void) -> anyhow::Result<()>;
    fn set_bounds(&mut self, bounds: LogicalRect);
    fn ensure_tab(&mut self, tab_id: &str, data_store_id: &str, url: &str) -> anyhow::Result<()>;
    fn close_tab(&mut self, tab_id: &str);
    fn suspend_tab(&mut self, tab_id: &str);
    fn show_tab(&mut self, tab_id: Option<&str>);
    fn place_tab(&mut self, tab_id: &str, bounds: Option<LogicalRect>);
    fn navigate(&mut self, tab_id: &str, url: &str) -> anyhow::Result<()>;
    fn back(&mut self, tab_id: &str);
    fn forward(&mut self, tab_id: &str);
    fn reload(&mut self, tab_id: &str);
    fn hard_reload(&mut self, tab_id: &str);
    /// Selects the next match of `query` after the page selection (before it, `backwards`), wrapping around.
    fn find(&mut self, tab_id: &str, query: &str, backwards: bool);
    fn eval(&mut self, tab_id: &str, token: &str, javascript: &str);
    fn focus_chrome(&mut self);
    fn remove_data_store(&mut self, data_store_id: &str);
    fn events(&self) -> &Receiver<EngineEvent>;
    fn frame(&self, tab_id: &str) -> Option<(f64, f64)>;
}
