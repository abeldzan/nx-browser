use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SEARCH_ENGINES: [(&str, &str); 6] = [
    ("DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("Google", "https://www.google.com/search?q=%s"),
    ("Bing", "https://www.bing.com/search?q=%s"),
    ("Brave", "https://search.brave.com/search?q=%s"),
    ("Startpage", "https://www.startpage.com/do/search?q=%s"),
    ("Ecosia", "https://www.ecosia.org/search?q=%s"),
];

/// App-wide preferences, stored as one JSON value so new fields default cleanly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub restore_session: bool,
    pub home_page: String,
    /// "system", "light" or "dark".
    pub theme: String,
    pub page_zoom: f64,
    pub search_engine: String,
    pub save_history: bool,
    pub https_only: bool,
    /// Built-in ad and tracker blocking (WebKit content rules).
    pub block_ads: bool,
    pub block_popups: bool,
    pub fraud_warnings: bool,
    pub javascript: bool,
    pub user_agent: String,
    pub download_dir: String,
    pub ask_download_path: bool,
    pub suspend_after_secs: i64,
    pub max_live_tabs: usize,
    pub inspector: bool,
    /// `#rrggbb`; empty means the theme's own accent.
    pub accent: String,
    pub reduce_motion: bool,
    /// "comfortable" or "compact".
    pub density: String,
    /// Page zoom per host, for hosts zoomed away from `page_zoom`.
    pub site_zoom: BTreeMap<String, f64>,
    /// Spaces open in their own window, reopened at launch.
    pub detached_spaces: Vec<String>,
    /// Hidden spaces each holding one window torn off into an OS window of its own; never shown as spaces.
    pub detached_windows: Vec<String>,
    /// Monospace font family for terminal tabs.
    pub terminal_font: String,
    pub terminal_font_size: f64,
    /// Row height as a multiple of the font size.
    pub terminal_line_height: f64,
    /// "block", "beam" or "underline"; programs may still change it while they run.
    pub terminal_cursor: String,
    /// Program run in new terminals; empty means the login shell (`$SHELL`).
    pub terminal_shell: String,
    /// Lines of history kept per terminal.
    pub terminal_scrollback: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            restore_session: true,
            home_page: "nxbrowser://home".into(),
            theme: "system".into(),
            page_zoom: 1.0,
            search_engine: "DuckDuckGo".into(),
            save_history: true,
            https_only: false,
            block_ads: true,
            block_popups: true,
            fraud_warnings: true,
            javascript: true,
            user_agent: String::new(),
            download_dir: crate::downloads_dir().display().to_string(),
            ask_download_path: false,
            suspend_after_secs: 120,
            max_live_tabs: 6,
            inspector: true,
            accent: String::new(),
            reduce_motion: false,
            density: "comfortable".into(),
            site_zoom: BTreeMap::new(),
            detached_spaces: Vec::new(),
            detached_windows: Vec::new(),
            terminal_font: "Menlo".into(),
            terminal_font_size: 12.0,
            terminal_line_height: 1.35,
            terminal_cursor: "block".into(),
            terminal_shell: String::new(),
            terminal_scrollback: 10_000,
        }
    }
}

impl Settings {
    pub fn search_template(&self) -> &'static str {
        SEARCH_ENGINES
            .iter()
            .find(|(name, _)| *name == self.search_engine)
            .unwrap_or(&SEARCH_ENGINES[0])
            .1
    }

    /// The engine's OpenSearch suggest endpoint; `None` when it has none, so typing is never sent elsewhere.
    pub fn suggest_template(&self) -> Option<&'static str> {
        match self.search_template() {
            "https://duckduckgo.com/?q=%s" => Some("https://duckduckgo.com/ac/?type=list&q=%s"),
            "https://www.google.com/search?q=%s" => Some("https://suggestqueries.google.com/complete/search?client=firefox&q=%s"),
            "https://www.bing.com/search?q=%s" => Some("https://api.bing.com/osjson.aspx?query=%s"),
            "https://search.brave.com/search?q=%s" => Some("https://search.brave.com/api/suggest?q=%s"),
            _ => None,
        }
    }

    pub fn zoom_for(&self, host: &str) -> f64 {
        self.site_zoom.get(host).copied().unwrap_or(self.page_zoom)
    }

    pub fn compact(&self) -> bool {
        self.density == "compact"
    }

    pub fn new_tab_url(&self) -> String {
        let home = self.home_page.trim();
        if home.is_empty() { "nxbrowser://home".into() } else { crate::normalize_url(home, self.search_template(), self.https_only) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let parsed: Settings = serde_json::from_str(r#"{"search_engine":"Google"}"#).unwrap();
        assert_eq!(parsed.search_engine, "Google");
        assert_eq!(parsed.max_live_tabs, Settings::default().max_live_tabs);
        assert!(parsed.search_template().starts_with("https://www.google.com/"));
        assert!(!parsed.compact() && !parsed.reduce_motion && parsed.accent.is_empty());
        assert_eq!(parsed.zoom_for("example.com"), parsed.page_zoom);
        assert_eq!((parsed.terminal_font.as_str(), parsed.terminal_scrollback), ("Menlo", 10_000));
        let round: Settings = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(round, parsed);
    }

    #[test]
    fn unknown_engine_falls_back() {
        let settings = Settings { search_engine: "Nope".into(), ..Default::default() };
        assert_eq!(settings.search_template(), SEARCH_ENGINES[0].1);
    }
}
