use crate::scripting::bridge_and_content;
use anyhow::Context;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct ManifestFile {
    name: String,
    version: String,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    host_permissions: Vec<String>,
    #[serde(default)]
    content_scripts: Vec<ContentScript>,
    #[serde(default)]
    background: Option<Background>,
}

#[derive(Debug, Deserialize)]
struct ContentScript {
    #[serde(default)]
    matches: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Background {
    #[serde(default)]
    scripts: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct LoadedExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    pub permissions: Vec<String>,
    pub host_permissions: Vec<String>,
    pub matches: Vec<String>,
    pub content_script: String,
    pub background: Option<String>,
}

pub fn load_extension(dir: &Path) -> anyhow::Result<LoadedExtension> {
    let manifest_path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: ManifestFile = serde_json::from_str(&text)?;
    let mut matches = Vec::new();
    let mut sources = Vec::new();
    for script in &manifest.content_scripts {
        matches.extend(script.matches.iter().cloned());
        for file in &script.js {
            let source = std::fs::read_to_string(dir.join(file))
                .with_context(|| format!("read content script {file}"))?;
            sources.push(source);
        }
    }
    let allowed = if manifest.host_permissions.is_empty() {
        matches.clone()
    } else {
        manifest.host_permissions.clone()
    };
    let content_script = if sources.is_empty() {
        String::new()
    } else {
        bridge_and_content(&slug(&manifest.name), &allowed, &sources.join("\n"))
    };
    let background = manifest.background.as_ref().and_then(|background| {
        let mut combined = String::new();
        for file in &background.scripts {
            let Ok(source) = std::fs::read_to_string(dir.join(file)) else {
                return None;
            };
            combined.push_str(&source);
            combined.push('\n');
        }
        if combined.is_empty() {
            None
        } else {
            Some(combined)
        }
    });
    Ok(LoadedExtension {
        id: slug(&manifest.name),
        name: manifest.name,
        version: manifest.version,
        path: dir.to_path_buf(),
        permissions: manifest.permissions,
        host_permissions: manifest.host_permissions,
        matches,
        content_script,
        background,
    })
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}
