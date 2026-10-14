mod download;
mod lifecycle;
mod metrics;
mod model;
mod url;

pub use download::{spawn_download, DownloadUpdate};
pub use lifecycle::{suspend_candidates, LifeEvent, TabState};
pub use metrics::{sample_cpu_seconds, snapshot, MetricsSnapshot};
pub use model::{
    Bookmark, BundleBookmark, BundleHistory, BundlePermission, BundleProfile, BundleStorage,
    BundleTab, DownloadRecord, Effect, ExtensionRecord, HistoryEntry, Model, Profile, ProfileBundle,
    Session, Tab,
};
pub use url::normalize_url;

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
