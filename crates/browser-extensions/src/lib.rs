mod install;
mod manifest;
mod polyfill;

pub use install::{crx_zip, install_from_store, local_id, store_id, unpack_crx};
pub use manifest::{read_manifest, Manifest};
pub use polyfill::{patch_missing_apis, patch_unpacked_copy};

/// Folders directly under `dirs` (or `dirs` themselves) that hold a `manifest.json`.
pub fn discover(dirs: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for dir in dirs {
        if dir.join("manifest.json").is_file() {
            found.push(dir.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("manifest.json").is_file() {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup_by(|a, b| a.canonicalize().ok() == b.canonicalize().ok());
    found
}
