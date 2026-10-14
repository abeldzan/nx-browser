use anyhow::{bail, Context};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Chrome Web Store extension id from a bare id or a store detail URL.
pub fn store_id(input: &str) -> Option<String> {
    let input = input.trim();
    let is_id = |part: &str| part.len() == 32 && part.bytes().all(|b| (b'a'..=b'p').contains(&b));
    if is_id(input) {
        return Some(input.to_string());
    }
    let rest = input.split_once("://").map_or(input, |(_, rest)| rest);
    let (host, path) = rest.split_once('/')?;
    if host != "chromewebstore.google.com" && host != "chrome.google.com" {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if !path.starts_with("detail/") && !path.starts_with("webstore/detail/") {
        return None;
    }
    path.split('/').find(|part| is_id(part)).map(str::to_string)
}

/// Stable id for an extension loaded from a folder, in Chrome's 32-letter `a`–`p` form.
pub fn local_id(path: &Path) -> String {
    // FNV-1a: std's hashers are not guaranteed stable across releases, and ids are persisted.
    let mut hash: u128 = 0x6c62272e07bb014262b821756295c58d;
    for byte in path.to_string_lossy().bytes() {
        hash ^= byte as u128;
        hash = hash.wrapping_mul(0x0000000001000000000000000000013B);
    }
    (0..32).map(|i| (b'a' + ((hash >> (i * 4)) & 0xf) as u8) as char).collect()
}

/// The zip archive inside a CRX2 or CRX3 package.
pub fn crx_zip(bytes: &[u8]) -> anyhow::Result<&[u8]> {
    let word = |at: usize| -> anyhow::Result<usize> {
        let slice = bytes.get(at..at + 4).context("truncated CRX header")?;
        Ok(u32::from_le_bytes(slice.try_into()?) as usize)
    };
    if bytes.starts_with(b"PK\x03\x04") {
        return Ok(bytes);
    }
    if !bytes.starts_with(b"Cr24") {
        bail!("not a CRX package");
    }
    let start = match word(4)? {
        2 => 16 + word(8)? + word(12)?,
        3 => 12 + word(8)?,
        version => bail!("unsupported CRX version {version}"),
    };
    bytes.get(start..).filter(|zip| zip.starts_with(b"PK")).context("CRX has no zip payload")
}

/// Unpacks a CRX (or zip) into `dest`, replacing what was there.
pub fn unpack_crx(bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    let zip = crx_zip(bytes)?;
    let parent = dest.parent().context("install folder has no parent")?;
    std::fs::create_dir_all(parent)?;
    let archive = parent.join(format!(".{}.zip", dest.file_name().unwrap_or_default().to_string_lossy()));
    let staging = parent.join(format!(".{}.unpack", dest.file_name().unwrap_or_default().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::write(&archive, zip)?;
    // shortcut: ditto is macOS-only; swap for the zip crate if another platform is added.
    let status = std::process::Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&archive).arg(&staging).status();
    let _ = std::fs::remove_file(&archive);
    if !status.map(|status| status.success()).unwrap_or(false) {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("could not unpack extension archive");
    }
    if !staging.join("manifest.json").is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("extension archive has no manifest.json");
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&staging, dest)?;
    Ok(())
}

/// Downloads a Chrome Web Store extension and unpacks it into `root/<id>`. Blocking.
pub fn install_from_store(id: &str, root: &Path) -> anyhow::Result<PathBuf> {
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion=140.0.0.0&acceptformat=crx2,crx3&x=id%3D{id}%26installsource%3Dondemand%26uc"
    );
    let response = ureq::get(&url).timeout(std::time::Duration::from_secs(60)).call().context("download from the Chrome Web Store")?;
    let mut bytes = Vec::new();
    response.into_reader().take(256 * 1024 * 1024).read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        bail!("the Chrome Web Store returned nothing for {id}");
    }
    let dest = root.join(id);
    unpack_crx(&bytes, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";

    #[test]
    fn store_ids_come_from_ids_and_store_urls() {
        assert_eq!(store_id(ID).as_deref(), Some(ID));
        assert_eq!(store_id(&format!("https://chromewebstore.google.com/detail/ublock-origin-lite/{ID}?hl=en")).as_deref(), Some(ID));
        assert_eq!(store_id(&format!("https://chrome.google.com/webstore/detail/x/{ID}")).as_deref(), Some(ID));
        assert_eq!(store_id(&format!("https://example.com/detail/x/{ID}")), None);
        assert_eq!(store_id("not an id"), None);
    }

    #[test]
    fn crx_headers_are_stripped() {
        let zip = b"PK\x03\x04rest";
        let mut crx3 = b"Cr24".to_vec();
        crx3.extend(3u32.to_le_bytes());
        crx3.extend(5u32.to_le_bytes());
        crx3.extend(b"hdr!!");
        crx3.extend(zip);
        assert_eq!(crx_zip(&crx3).unwrap(), zip);
        let mut crx2 = b"Cr24".to_vec();
        crx2.extend(2u32.to_le_bytes());
        crx2.extend(2u32.to_le_bytes());
        crx2.extend(3u32.to_le_bytes());
        crx2.extend(b"kksss");
        crx2.extend(zip);
        assert_eq!(crx_zip(&crx2).unwrap(), zip);
        assert!(crx_zip(b"Cr24\x03\x00\x00\x00").is_err());
    }

    #[test]
    fn local_ids_are_stable_store_shaped() {
        let id = local_id(Path::new("/tmp/ext"));
        assert_eq!(id, local_id(Path::new("/tmp/ext")));
        assert!(store_id(&id).is_some());
        assert_ne!(id, local_id(Path::new("/tmp/other")));
    }
}
