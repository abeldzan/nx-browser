use serde_json::Value;
use std::path::Path;

const POLYFILL: &str = include_str!("polyfill.js");
const POLYFILL_FILE: &str = "nx-polyfill.js";
const WORKER_FILE: &str = "nx-background.js";

/// Makes a store-installed extension load the polyfill for Chrome-only APIs WebKit lacks, ahead
/// of its background and pages. Idempotent; only for copies we unpacked, never a user's folder.
pub fn patch_missing_apis(dir: &Path) -> anyhow::Result<()> {
    std::fs::write(dir.join(POLYFILL_FILE), POLYFILL)?;
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)?;
    let mut manifest: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    let background = &mut manifest["background"];
    let mut changed = false;
    if let Some(worker) = background["service_worker"].as_str().filter(|worker| *worker != WORKER_FILE).map(str::to_string) {
        let worker = worker.trim_start_matches('/');
        let loader = if background["type"] == "module" {
            format!("import '/{POLYFILL_FILE}';\nimport '/{worker}';\n")
        } else {
            format!("importScripts('/{POLYFILL_FILE}', '/{worker}');\n")
        };
        std::fs::write(dir.join(WORKER_FILE), loader)?;
        background["service_worker"] = WORKER_FILE.into();
        changed = true;
    } else if let Some(scripts) = background["scripts"].as_array_mut() {
        if scripts.first().and_then(Value::as_str) != Some(POLYFILL_FILE) {
            scripts.insert(0, POLYFILL_FILE.into());
            changed = true;
        }
    }
    if changed {
        std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;
    }
    patch_pages(dir)
}

/// Chrome enum constants WebKit lacks, as the values Chrome gives them. They can't be polyfilled:
/// WebKit's API objects take no new properties, and swapping the `chrome` global breaks popups.
/// Also Chrome's extension URL scheme: code that trusts `sender.url` only when it starts with
/// `chrome-extension://<id>/` (Kluff's sign-in) would reject its own pages here.
const CONSTANTS: [(&str, &str); 3] = [
    ("chrome.scripting.ExecutionWorld.ISOLATED", "\"ISOLATED\""),
    ("chrome.scripting.ExecutionWorld.MAIN", "\"MAIN\""),
    ("chrome-extension://", "webkit-extension://"),
];

/// Points `chrome.permissions.`/`browser.permissions.` at the polyfill's `nxPermissions`, falling back
/// to the real object where the polyfill isn't loaded (content scripts). Leaves `window.chrome.permissions.`
/// and the like alone, where wrapping would break the expression.
fn guard_permissions(js: &str) -> String {
    let mut out = String::with_capacity(js.len());
    let mut rest = js;
    while let Some((at, pattern)) = ["chrome.permissions.", "browser.permissions."].into_iter().filter_map(|pattern| Some((rest.find(pattern)?, pattern))).min() {
        out.push_str(&rest[..at]);
        let member = out.chars().next_back().is_some_and(|c| c == '.' || c == '_' || c == '$' || c.is_alphanumeric());
        if member {
            out.push_str(pattern);
        } else {
            out.push_str(&format!("(globalThis.nxPermissions||{}).", &pattern[..pattern.len() - 1]));
        }
        rest = &rest[at + pattern.len()..];
    }
    out.push_str(rest);
    out
}

/// Adds the polyfill as the first script of every extension page (popup, options, side panel,
/// background page) and inlines the `CONSTANTS` in its scripts.
fn patch_pages(dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "_metadata" {
                patch_pages(&path)?;
            }
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "js") {
            let Ok(js) = std::fs::read_to_string(&path) else {
                continue;
            };
            let patched = guard_permissions(&CONSTANTS.iter().fold(js.clone(), |js, (from, to)| js.replace(from, to)));
            if patched != js {
                std::fs::write(&path, patched)?;
            }
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "html") {
            continue;
        }
        let Ok(html) = std::fs::read_to_string(&path) else {
            continue;
        };
        if html.contains(POLYFILL_FILE) {
            continue;
        }
        let tag = format!("<script src=\"/{POLYFILL_FILE}\"></script>");
        let at = html.to_ascii_lowercase().find("<head").and_then(|start| html[start..].find('>').map(|end| start + end + 1)).unwrap_or(0);
        std::fs::write(&path, format!("{}{tag}{}", &html[..at], &html[at..]))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_module_worker_and_pages_once() {
        let dir = std::env::temp_dir().join(format!("nx-polyfill-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("manifest.json"), r#"{"background":{"service_worker":"sw.js","type":"module"}}"#).unwrap();
        std::fs::write(dir.join("src/panel.html"), "<html><head><title>x</title></head></html>").unwrap();
        std::fs::write(dir.join("src/inject.js"), "run({world:chrome.scripting.ExecutionWorld.MAIN})").unwrap();
        std::fs::write(dir.join("src/senders.js"), "u.startsWith(`chrome-extension://${id}/`)").unwrap();
        std::fs::write(dir.join("src/perms.js"), "chrome.permissions.contains(p);window.chrome.permissions.remove(p)").unwrap();
        patch_missing_apis(&dir).unwrap();
        patch_missing_apis(&dir).unwrap();
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["background"]["service_worker"], WORKER_FILE);
        assert_eq!(std::fs::read_to_string(dir.join(WORKER_FILE)).unwrap(), "import '/nx-polyfill.js';\nimport '/sw.js';\n");
        let html = std::fs::read_to_string(dir.join("src/panel.html")).unwrap();
        assert_eq!(html, "<html><head><script src=\"/nx-polyfill.js\"></script><title>x</title></head></html>");
        assert_eq!(std::fs::read_to_string(dir.join("src/inject.js")).unwrap(), "run({world:\"MAIN\"})");
        assert_eq!(std::fs::read_to_string(dir.join("src/senders.js")).unwrap(), "u.startsWith(`webkit-extension://${id}/`)");
        assert_eq!(
            std::fs::read_to_string(dir.join("src/perms.js")).unwrap(),
            "(globalThis.nxPermissions||chrome.permissions).contains(p);window.chrome.permissions.remove(p)"
        );
        assert!(!std::fs::read_to_string(dir.join(POLYFILL_FILE)).unwrap().contains("nxPermissions||"), "the polyfill itself is left alone");
        let _ = std::fs::remove_dir_all(dir);
    }
}
