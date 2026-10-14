mod manifest;
mod permissions;
mod runtime;
mod scripting;

pub use manifest::{load_extension, LoadedExtension};
pub use permissions::host_allowed;
pub use runtime::{BackgroundRuntime, HostFn};
pub use scripting::{bridge_and_content, matches_url};

pub fn load_extension_dirs(dirs: &[std::path::PathBuf]) -> Vec<LoadedExtension> {
    let mut loaded = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(extension) = load_extension(&path) {
                    loaded.push(extension);
                }
            }
        }
        if let Ok(extension) = load_extension(dir) {
            if !loaded.iter().any(|item| item.id == extension.id) {
                loaded.push(extension);
            }
        }
    }
    loaded
}
