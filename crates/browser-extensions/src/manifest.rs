use anyhow::Context;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The manifest fields the browser chrome shows; WebKit handles everything that runs.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: Option<PathBuf>,
    pub popup: bool,
    pub side_panel: Option<String>,
}

pub fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
    let path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    // Chrome tolerates a UTF-8 BOM; serde_json does not.
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    let messages = json["default_locale"]
        .as_str()
        .and_then(|locale| std::fs::read_to_string(dir.join("_locales").join(locale).join("messages.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')).ok());
    let text = |value: &Value| localize(value.as_str().unwrap_or(""), messages.as_ref());
    let action = if json["action"].is_object() { &json["action"] } else { &json["browser_action"] };
    let icon = best_icon(&action["default_icon"]).or_else(|| best_icon(&json["icons"])).map(|file| dir.join(file.trim_start_matches('/')));
    Ok(Manifest {
        name: text(&json["name"]),
        version: text(&json["version"]),
        description: text(&json["description"]),
        icon: icon.filter(|path| path.is_file()),
        popup: action["default_popup"].as_str().is_some_and(|page| !page.is_empty()),
        side_panel: json["side_panel"]["default_path"].as_str().map(str::to_string),
    })
}

/// Resolves `__MSG_key__` against the default locale's messages, case-insensitively as Chrome does.
fn localize(value: &str, messages: Option<&Value>) -> String {
    let Some(key) = value.strip_prefix("__MSG_").and_then(|rest| rest.strip_suffix("__")) else {
        return value.to_string();
    };
    messages
        .and_then(|messages| messages.as_object())
        .and_then(|map| map.iter().find(|(name, _)| name.eq_ignore_ascii_case(key)))
        .and_then(|(_, entry)| entry["message"].as_str())
        .unwrap_or(value)
        .to_string()
}

/// A string icon, or the largest size from a `{ "16": path, ... }` map.
fn best_icon(value: &Value) -> Option<String> {
    if let Some(path) = value.as_str() {
        return Some(path.to_string());
    }
    value
        .as_object()?
        .iter()
        .filter_map(|(size, path)| Some((size.parse::<u32>().ok()?, path.as_str()?)))
        .max_by_key(|(size, _)| *size)
        .map(|(_, path)| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_reads_localized_name_icon_and_panels() {
        let dir = std::env::temp_dir().join(format!("nx-manifest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("_locales/en")).unwrap();
        std::fs::write(dir.join("icon128.png"), b"png").unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"name":"__MSG_appName__","version":"1.2","default_locale":"en","icons":{"16":"icon16.png","128":"icon128.png"},
               "action":{"default_popup":"popup.html"},"side_panel":{"default_path":"panel.html"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("_locales/en/messages.json"), r#"{"APPNAME":{"message":"Hello"}}"#).unwrap();
        let manifest = read_manifest(&dir).unwrap();
        assert_eq!(manifest.name, "Hello");
        assert_eq!(manifest.icon, Some(dir.join("icon128.png")));
        assert!(manifest.popup);
        assert_eq!(manifest.side_panel.as_deref(), Some("panel.html"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
