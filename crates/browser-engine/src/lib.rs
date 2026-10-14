use std::sync::mpsc::Receiver;

#[derive(Clone, Debug, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug)]
pub struct ScriptInjection {
    pub world: String,
    pub source: String,
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
    ScriptMessage {
        tab_id: String,
        body: String,
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
}

pub trait WebEngine {
    fn attach(&mut self, ns_view: *mut std::ffi::c_void) -> anyhow::Result<()>;
    fn set_bounds(&mut self, bounds: LogicalRect);
    fn ensure_tab(&mut self, tab_id: &str, data_store_id: &str, url: &str, scripts: &[ScriptInjection]) -> anyhow::Result<()>;
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
    fn resolve_extension(&mut self, tab_id: &str, world: &str, javascript: &str);
    fn set_scripts(&mut self, tab_id: &str, scripts: &[ScriptInjection]);
    fn focus_chrome(&mut self);
    fn remove_data_store(&mut self, data_store_id: &str);
    fn events(&self) -> &Receiver<EngineEvent>;
    fn frame(&self, tab_id: &str) -> Option<(f64, f64)>;
}
