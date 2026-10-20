//! Self-update from GitHub releases: find a newer `NxBrowser.dmg`, stage its app in the background,
//! and swap it in on restart. The app is ad-hoc signed, so this checks the download's digest and the
//! staged bundle itself rather than an Apple notarization.

use anyhow::{bail, Context};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tokio::task::JoinHandle;

const LATEST: &str = "https://api.github.com/repos/abeldzan/nx-browser/releases/latest";
const ASSET: &str = "NxBrowser.dmg";
const BUNDLE_ID: &str = "dev.nxbrowser.app";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// Hex SHA-256 of the DMG, from GitHub's asset `digest`.
    pub sha256: Option<String>,
}

/// The running `.app`; `None` for debug builds and bare binaries, which must never replace themselves.
pub fn installed_bundle() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.ancestors().nth(3)?;
    let inside = exe.parent()?.ends_with("Contents/MacOS") && bundle.extension().is_some_and(|ext| ext == "app");
    inside.then(|| bundle.to_path_buf())
}

/// The latest release when it is newer than this build.
pub fn check() -> JoinHandle<anyhow::Result<Option<Release>>> {
    crate::download::runtime().spawn_blocking(|| {
        let response = ureq::get(LATEST)
            .set("User-Agent", &format!("NxBrowser/{VERSION}"))
            .set("Accept", "application/vnd.github+json")
            .timeout(std::time::Duration::from_secs(10))
            .call()
            .context("Couldn't reach GitHub")?;
        let mut body = String::new();
        // Remote body: cap what we read.
        response.into_reader().take(1024 * 1024).read_to_string(&mut body)?;
        parse_release(&body, VERSION)
    })
}

fn parse_release(json: &str, current: &str) -> anyhow::Result<Option<Release>> {
    let value: serde_json::Value = serde_json::from_str(json).context("Unexpected reply from GitHub")?;
    let tag = value["tag_name"].as_str().context("Release has no tag")?;
    if !newer(tag, current) {
        return Ok(None);
    }
    let asset = value["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|asset| asset["name"] == ASSET))
        .with_context(|| format!("Release {tag} has no {ASSET}"))?;
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url: asset["browser_download_url"].as_str().context("Asset has no download URL")?.to_string(),
        sha256: asset["digest"].as_str().and_then(|digest| digest.strip_prefix("sha256:")).map(str::to_lowercase),
    }))
}

/// Dotted numeric versions, `v` prefix ignored; anything unparsable is never newer.
fn newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(str::parse::<u64>).collect::<Result<Vec<_>, _>>();
    match (parse(candidate), parse(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

fn staging_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Caches").join(BUNDLE_ID).join("Update")
}

/// Drops what an earlier run downloaded or staged.
pub fn clear_staging() {
    let _ = std::fs::remove_dir_all(staging_dir());
}

/// Downloads, checks and unpacks `release` into the caches folder; returns the staged `.app`.
pub fn prepare(release: Release) -> JoinHandle<anyhow::Result<PathBuf>> {
    crate::download::runtime().spawn_blocking(move || {
        let bundle = installed_bundle().context("Not running from an app bundle")?;
        let folder = bundle.parent().context("App bundle has no folder")?;
        let probe = folder.join(".nxbrowser-update-probe");
        if std::fs::write(&probe, b"").is_err() {
            bail!("Can't replace NxBrowser in {}. Update with `brew upgrade --cask nxbrowser`.", folder.display());
        }
        let _ = std::fs::remove_file(&probe);
        stage(&release, &staging_dir())
    })
}

/// Downloads `release` into `dir`, checks it, and unpacks its app there.
fn stage(release: &Release, dir: &Path) -> anyhow::Result<PathBuf> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let dmg = dir.join(ASSET);
    let response = ureq::get(&release.url)
        .set("User-Agent", &format!("NxBrowser/{VERSION}"))
        .timeout(std::time::Duration::from_secs(15 * 60))
        .call()
        .context("Couldn't download the update")?;
    // A DMG of a browser is tens of MB; 1 GB stops a runaway reply from filling the disk.
    std::io::copy(&mut response.into_reader().take(1 << 30), &mut std::fs::File::create(&dmg)?).context("Couldn't download the update")?;
    if let Some(expected) = &release.sha256 {
        let sum = run(Command::new("/usr/bin/shasum").args(["-a", "256"]).arg(&dmg))?;
        if sum.split_whitespace().next() != Some(expected.as_str()) {
            bail!("The downloaded update is damaged (checksum mismatch)");
        }
    }

    let mount = dir.join("mount");
    run(Command::new("/usr/bin/hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mount).arg(&dmg))?;
    let app = dir.join("NxBrowser.app");
    let copied = run(Command::new("/usr/bin/ditto").arg(mount.join("NxBrowser.app")).arg(&app));
    let _ = run(Command::new("/usr/bin/hdiutil").args(["detach", "-force"]).arg(&mount));
    copied?;
    let _ = std::fs::remove_file(&dmg);

    let plist = app.join("Contents/Info.plist");
    let read = |key: &str| run(Command::new("/usr/libexec/PlistBuddy").arg("-c").arg(format!("Print {key}")).arg(&plist)).map(|v| v.trim().to_string());
    if read("CFBundleIdentifier")? != BUNDLE_ID || read("CFBundleShortVersionString")? != release.version {
        bail!("The downloaded update isn't NxBrowser {}", release.version);
    }
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&app)).context("The downloaded update's signature is broken")?;
    Ok(app)
}

/// Starts a helper that waits for this process to exit, swaps `staged` in for the installed app and opens it.
/// The caller quits right after.
pub fn relaunch_into(staged: &Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let bundle = installed_bundle().context("Not running from an app bundle")?;
    // $1 pid, $2 staged app, $3 installed app. Puts the old app back if the new one can't be moved in.
    const SWAP: &str = r#"
while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
rm -rf "$3.old"
mv "$3" "$3.old" || { open "$3"; exit 1; }
if mv "$2" "$3"; then rm -rf "$3.old"; else mv "$3.old" "$3"; fi
open "$3"
"#;
    Command::new("/bin/sh")
        .args(["-c", SWAP, "nxbrowser-update", &std::process::id().to_string()])
        .arg(staged)
        .arg(&bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Its own process group, so it outlives the app.
        .process_group(0)
        .spawn()
        .context("Couldn't start the updater")?;
    Ok(())
}

/// Runs a system tool to completion; its stdout, or its stderr as the error.
fn run(command: &mut Command) -> anyhow::Result<String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let out = command.stdin(Stdio::null()).output().with_context(|| format!("Couldn't run {program}"))?;
    if !out.status.success() {
        bail!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(newer("0.2.0", "0.1.99"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("v0.2.0-beta", "0.1.0"));
    }

    /// The real thing against the latest GitHub release: `cargo test -p browser-core -- --ignored stages_latest`.
    #[test]
    #[ignore = "network, hdiutil"]
    fn stages_latest_release() {
        let json = ureq::get(LATEST).set("User-Agent", "NxBrowser-test").call().unwrap().into_string().unwrap();
        let release = parse_release(&json, "0.0.0").unwrap().unwrap();
        let dir = std::env::temp_dir().join(format!("nx-update-{}", std::process::id()));
        let app = stage(&release, &dir).unwrap();
        assert!(app.join("Contents/MacOS/nxbrowser").is_file());
        assert!(!dir.join("mount").join("NxBrowser.app").exists(), "DMG left mounted");
        let mut broken = release;
        broken.sha256 = Some("0".repeat(64));
        assert!(stage(&broken, &dir).unwrap_err().to_string().contains("checksum"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parses_latest_release() {
        let json = r#"{"tag_name":"v0.2.0","assets":[
            {"name":"notes.txt","browser_download_url":"https://x/notes.txt"},
            {"name":"NxBrowser.dmg","browser_download_url":"https://x/NxBrowser.dmg","digest":"sha256:ABC123"}]}"#;
        assert_eq!(
            parse_release(json, "0.1.0").unwrap(),
            Some(Release { version: "0.2.0".into(), url: "https://x/NxBrowser.dmg".into(), sha256: Some("abc123".into()) })
        );
        assert_eq!(parse_release(json, "0.2.0").unwrap(), None);
        let no_digest = r#"{"tag_name":"v0.2.0","assets":[{"name":"NxBrowser.dmg","browser_download_url":"https://x/d"}]}"#;
        assert_eq!(parse_release(no_digest, "0.1.0").unwrap().unwrap().sha256, None);
        let no_dmg = r#"{"tag_name":"v0.2.0","assets":[{"name":"NxBrowser.zip","browser_download_url":"https://x/z"}]}"#;
        assert!(parse_release(no_dmg, "0.1.0").is_err());
    }
}
