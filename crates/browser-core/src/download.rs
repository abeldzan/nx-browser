use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// `dir/name`, or `name (2).ext` and up when that is taken: WebKit fails a download rather than overwrite.
/// Only the name's last component counts, so a server's file name can't point outside `dir`.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = Path::new(name).file_name().and_then(|name| name.to_str()).filter(|name| !name.starts_with('.')).unwrap_or("download");
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(if n == 1 { name.to_string() } else { format!("{stem} ({n}){ext}") }))
        .find(|path| !path.exists())
        .expect("some name is free")
}

/// Fetches a site icon into `dest` (written whole, then renamed, so readers never see a partial file).
pub fn spawn_favicon(url: String, dest: PathBuf) {
    runtime().spawn_blocking(move || {
        let Ok(response) = ureq::get(&url).timeout(std::time::Duration::from_secs(10)).call() else {
            return;
        };
        let mut bytes = Vec::new();
        // Remote, untrusted file: cap what we keep.
        let read = response.into_reader().take(512 * 1024).read_to_end(&mut bytes);
        if read.is_err() || bytes.is_empty() {
            return;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let partial = dest.with_extension("part");
        if std::fs::write(&partial, &bytes).is_ok() {
            let _ = std::fs::rename(&partial, &dest);
        }
    });
}

/// Fetches search-box completions from an OpenSearch suggest URL (`%s` = query); empty on any failure.
pub fn fetch_suggestions(template: &str, query: &str) -> tokio::task::JoinHandle<Vec<String>> {
    let url = template.replace("%s", &crate::url::percent_encode(query));
    runtime().spawn_blocking(move || {
        let Ok(response) = ureq::get(&url).timeout(std::time::Duration::from_secs(3)).call() else {
            return Vec::new();
        };
        let mut body = String::new();
        // Remote, untrusted body: cap what we read.
        if response.into_reader().take(64 * 1024).read_to_string(&mut body).is_err() {
            return Vec::new();
        }
        parse_opensearch(&body)
    })
}

/// `["query", ["suggestion", ...], ...]`
fn parse_opensearch(body: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(parts)) = serde_json::from_str(body) else {
        return Vec::new();
    };
    let Some(serde_json::Value::Array(list)) = parts.get(1) else {
        return Vec::new();
    };
    list.iter().filter_map(|item| item.as_str().map(String::from)).collect()
}

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        // Only spawn_blocking is used; one async worker is enough.
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opensearch_suggestions() {
        assert_eq!(parse_opensearch(r#"["rust",["rustdesk","rust lang"],[],{}]"#), ["rustdesk", "rust lang"]);
        assert!(parse_opensearch("<html>").is_empty());
        assert!(parse_opensearch(r#"{"a":1}"#).is_empty());
    }

    #[test]
    fn unique_path_skips_taken_names_and_stays_in_dir() {
        let dir = std::env::temp_dir().join(format!("nx-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "logo.svg"), dir.join("logo.svg"));
        std::fs::write(dir.join("logo.svg"), "").unwrap();
        std::fs::write(dir.join("logo (2).svg"), "").unwrap();
        assert_eq!(unique_path(&dir, "logo.svg"), dir.join("logo (3).svg"));
        assert_eq!(unique_path(&dir, "../../etc/passwd"), dir.join("passwd"));
        assert_eq!(unique_path(&dir, ""), dir.join("download"));
        assert_eq!(unique_path(&dir, ".zshrc"), dir.join("download"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
