//! App-wide update state: checks GitHub in the background, stages the new app, and restarts into it.
//! Every window observes `Updates`, so the title bar's "Restart to Update" appears everywhere at once.

use crate::SharedModel;
use browser_core::update;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::PoisonError;
use std::time::Duration;

/// Gives launch its moment before the first network request.
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Default, PartialEq)]
pub(crate) enum Status {
    /// This build can't update itself: debug, self-test, or not run from an app bundle.
    #[default]
    Unavailable,
    Idle,
    Checking,
    Downloading(String),
    Ready { version: String, app: PathBuf },
    Failed(String),
}

thread_local! {
    static STATUS: RefCell<Status> = RefCell::default();
}

/// Sets up the status; with `enabled`, also the menu item and the periodic background check.
pub(crate) fn init(model: SharedModel, enabled: bool) {
    if !enabled || update::installed_bundle().is_none() {
        return;
    }
    STATUS.set(Status::Idle);
    std::thread::spawn(update::clear_staging);
    browser_engine_macos::on_check_for_updates(|| check(true));
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK);
        loop {
            if model.lock().unwrap_or_else(PoisonError::into_inner).settings.auto_update {
                dispatch2::DispatchQueue::main().exec_async(|| check(false));
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

pub(crate) fn status() -> Status {
    STATUS.with_borrow(Status::clone)
}

/// Looks for a newer release and downloads it. `manual` checks report back in a toast; background ones stay quiet.
pub(crate) fn check(manual: bool) {
    match status() {
        Status::Unavailable => {
            if manual {
                toast("Updates are available in the installed app only");
            }
            return;
        }
        Status::Checking => return,
        Status::Downloading(version) => {
            if manual {
                toast(format!("Downloading NxBrowser {version}…"));
            }
            return;
        }
        Status::Ready { version, .. } => {
            if manual {
                toast(format!("NxBrowser {version} is ready. Restart to update."));
            }
            return;
        }
        Status::Idle | Status::Failed(_) => {}
    }
    set(Status::Checking);
    let main = dispatch2::DispatchQueue::main();
    std::thread::spawn(move || {
        let status = match join(update::check()) {
            Ok(None) => {
                if manual {
                    main.exec_async(|| toast("NxBrowser is up to date"));
                }
                Status::Idle
            }
            Ok(Some(release)) => {
                let version = release.version.clone();
                let shown = version.clone();
                main.exec_async(move || {
                    set(Status::Downloading(shown.clone()));
                    if manual {
                        toast(format!("Downloading NxBrowser {shown}…"));
                    }
                });
                match join(update::prepare(release)) {
                    Ok(app) => Status::Ready { version, app },
                    Err(error) => Status::Failed(error.to_string()),
                }
            }
            Err(error) => Status::Failed(error.to_string()),
        };
        main.exec_async(move || {
            if let (true, Status::Failed(message)) = (manual, &status) {
                toast(message.clone());
            }
            set(status);
        });
    });
}

/// Quits into the staged update; the helper swaps it in and opens it.
pub(crate) fn restart() {
    let Status::Ready { app, .. } = status() else {
        return;
    };
    match update::relaunch_into(&app) {
        Ok(()) => browser_engine_macos::quit(),
        Err(error) => toast(error.to_string()),
    }
}

/// Every window's title bar shows "Restart to Update", so all of them redraw.
fn set(status: Status) {
    STATUS.set(status);
    crate::notify_all();
}

/// A background task's result, its panic or cancellation included.
fn join<T, E: std::error::Error + Send + Sync + 'static>(task: impl std::future::Future<Output = Result<anyhow::Result<T>, E>>) -> anyhow::Result<T> {
    futures::executor::block_on(task)?
}

fn toast(message: impl Into<String>) {
    let Some(no) = browser_engine_macos::key_window().or_else(crate::main_window) else {
        return;
    };
    let message = message.into();
    crate::update(no, |view| {
        view.status = message;
        view.notify();
    });
}
