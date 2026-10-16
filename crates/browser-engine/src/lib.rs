use std::sync::mpsc::Receiver;

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
    Download {
        tab_id: String,
        url: String,
    },
    Permission {
        tab_id: String,
        origin: String,
        kind: String,
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

#[derive(Clone, Copy, Debug)]
pub enum Shortcut {
    NewTab,
    CloseTab,
    FocusUrl,
    Reload,
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
    /// Re-splits the space's windows among the stage; palette and dock only, no key.
    TileWindows,
    /// Escape; the page still gets the key.
    Dismiss,
}

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
    fn find(&mut self, tab_id: &str, query: &str);
    fn eval(&mut self, tab_id: &str, token: &str, javascript: &str);
    fn focus_chrome(&mut self);
    fn remove_data_store(&mut self, data_store_id: &str);
    fn events(&self) -> &Receiver<EngineEvent>;
    fn frame(&self, tab_id: &str) -> Option<(f64, f64)>;
}
