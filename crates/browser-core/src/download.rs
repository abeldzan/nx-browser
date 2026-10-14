use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct DownloadUpdate {
    pub id: String,
    /// "running" while bytes arrive, then "done" or "error: …".
    pub status: String,
    pub path: String,
    pub received: u64,
    /// From Content-Length; `None` when the server doesn't say.
    pub total: Option<u64>,
}

/// Copies `reader` into `writer`, calling `progress(received)` at most every `every` bytes.
fn copy_with_progress(
    mut reader: impl Read,
    mut writer: impl std::io::Write,
    every: u64,
    mut progress: impl FnMut(u64),
) -> std::io::Result<u64> {
    let mut buf = [0u8; 64 * 1024];
    let (mut received, mut reported) = (0u64, 0u64);
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(received);
        }
        writer.write_all(&buf[..n])?;
        received += n as u64;
        if received - reported >= every {
            reported = received;
            progress(received);
        }
    }
}

pub fn spawn_download(id: String, url: String, dest: PathBuf, tx: Sender<DownloadUpdate>) {
    let runtime = runtime();
    runtime.spawn_blocking(move || {
        let path = dest.display().to_string();
        let mut total = None;
        let result = (|| -> Result<u64, String> {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            // Only connect/read stalls time out; long downloads keep going while bytes flow.
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(30))
                .timeout_read(std::time::Duration::from_secs(60))
                .build();
            let response = agent.get(&url).call().map_err(|err| err.to_string())?;
            total = response.header("Content-Length").and_then(|value| value.parse().ok());
            let file = std::fs::File::create(&dest).map_err(|err| err.to_string())?;
            let mut out = std::io::BufWriter::new(file);
            let received = copy_with_progress(response.into_reader(), &mut out, 256 * 1024, |received| {
                let _ = tx.send(DownloadUpdate { id: id.clone(), status: "running".into(), path: path.clone(), received, total });
            })
            .map_err(|err| err.to_string())?;
            std::io::Write::flush(&mut out).map_err(|err| err.to_string())?;
            Ok(received)
        })();
        let update = match result {
            Ok(received) => DownloadUpdate { id, status: "done".into(), path, received, total: total.or(Some(received)) },
            Err(error) => DownloadUpdate { id, status: format!("error: {error}"), path, received: 0, total },
        };
        let _ = tx.send(update);
    });
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

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reports_in_steps_and_copies_everything() {
        let data = vec![7u8; 300_000];
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let total = copy_with_progress(&data[..], &mut out, 100_000, |n| seen.push(n)).unwrap();
        assert_eq!(total, 300_000);
        assert_eq!(out, data);
        assert!(seen.windows(2).all(|w| w[1] - w[0] >= 100_000) && seen.len() >= 2);
    }
}
