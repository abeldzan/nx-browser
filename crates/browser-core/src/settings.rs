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
        let round: Settings = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(round, parsed);
    }

    #[test]
    fn unknown_engine_falls_back() {
        let settings = Settings { search_engine: "Nope".into(), ..Default::default() };
        assert_eq!(settings.search_template(), SEARCH_ENGINES[0].1);
    }
}
