use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct DownloadUpdate {
    pub id: String,
    pub status: String,
    pub path: String,
}

pub fn spawn_download(id: String, url: String, dest: PathBuf, tx: Sender<DownloadUpdate>) {
    let runtime = runtime();
    runtime.spawn_blocking(move || {
        let result = (|| -> Result<(), String> {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            }
            let response = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(60))
                .call()
                .map_err(|err| err.to_string())?;
            let mut file = std::fs::File::create(&dest).map_err(|err| err.to_string())?;
            std::io::copy(&mut response.into_reader(), &mut file).map_err(|err| err.to_string())?;
            Ok(())
        })();
        let update = match result {
            Ok(()) => DownloadUpdate {
                id,
                status: "done".into(),
                path: dest.display().to_string(),
            },
            Err(error) => DownloadUpdate {
                id,
                status: format!("error: {error}"),
                path: dest.display().to_string(),
            },
        };
        let _ = tx.send(update);
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
