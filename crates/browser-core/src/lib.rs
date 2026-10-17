mod calc;
mod download;
mod lifecycle;
mod metrics;
mod model;
pub mod passwords;
mod settings;
pub mod update;
mod url;

pub use download::{fetch_suggestions, spawn_favicon, unique_path};
pub use lifecycle::{suspend_candidates, TabState};
pub use metrics::{sample_cpu_seconds, snapshot, MetricsSnapshot};
pub use model::{
    Bookmark, BundleBookmark, BundleHistory, BundlePermission, BundleProfile,
    BundleTab, DownloadRecord, Effect, ExtensionRecord, HistoryEntry, Model, Profile, ProfileBundle,
    Session, Tab, PRIVATE_STORE_PREFIX,
};
pub use settings::{Settings, SEARCH_ENGINES};
pub use calc::calc;
pub use url::{bang_home, find_bang, normalize_url, BANGS};

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

pub fn app_support_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join("Library/Application Support/nxbrowser")
}

pub fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(home).join("Downloads")
}
