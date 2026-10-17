//! App-wide update state: checks GitHub in the background, stages the new app, and restarts into it.
//! Every window observes `Updates`, so the title bar's "Restart to Update" appears everywhere at once.

use crate::window::BrowserWindow;
use crate::SharedModel;
use browser_core::update;
use futures::StreamExt;
use gpui::{App, BorrowAppContext, Global, SharedString};
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

#[derive(Default)]
pub(crate) struct Updates {
    pub(crate) status: Status,
}

impl Global for Updates {}

/// Sets up the global; with `enabled`, also the menu item and the periodic background check.
pub(crate) fn init(model: SharedModel, enabled: bool, cx: &mut App) {
    cx.set_global(Updates::default());
    if !enabled || update::installed_bundle().is_none() {
        return;
    }
    cx.global_mut::<Updates>().status = Status::Idle;
    std::thread::spawn(update::clear_staging);

    let (ask_tx, mut asks) = futures::channel::mpsc::unbounded::<()>();
    browser_engine_macos::on_check_for_updates(move || {
        let _ = ask_tx.unbounded_send(());
    });
    cx.spawn(async move |cx| {
        while asks.next().await.is_some() {
            cx.update(|cx| check(true, cx));
        }
    })
    .detach();

    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            if model.lock().unwrap_or_else(PoisonError::into_inner).settings.auto_update {
                cx.update(|cx| check(false, cx));
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

pub(crate) fn status(cx: &App) -> &Status {
    &cx.global::<Updates>().status
}

/// Looks for a newer release and downloads it. `manual` checks report back in a toast; background ones stay quiet.
pub(crate) fn check(manual: bool, cx: &mut App) {
    match status(cx) {
        Status::Unavailable => {
            if manual {
                toast("Updates are available in the installed app only", cx);
            }
            return;
        }
        Status::Checking => return,
        Status::Downloading(version) => {
            if manual {
                toast(format!("Downloading NxBrowser {version}…"), cx);
            }
            return;
        }
        Status::Ready { version, .. } => {
            if manual {
                toast(format!("NxBrowser {version} is ready. Restart to update."), cx);
            }
            return;
        }
        Status::Idle | Status::Failed(_) => {}
    }
    set(Status::Checking, cx);
    cx.spawn(async move |cx| {
        let status = match join(update::check()).await {
            Ok(None) => {
                if manual {
                    cx.update(|cx| toast("NxBrowser is up to date", cx));
                }
                Status::Idle
            }
            Ok(Some(release)) => {
                let version = release.version.clone();
                cx.update(|cx| {
                    set(Status::Downloading(version.clone()), cx);
                    if manual {
                        toast(format!("Downloading NxBrowser {version}…"), cx);
                    }
                });
                match join(update::prepare(release)).await {
                    Ok(app) => Status::Ready { version, app },
                    Err(error) => Status::Failed(error.to_string()),
                }
            }
            Err(error) => Status::Failed(error.to_string()),
        };
        cx.update(|cx| {
            if let (true, Status::Failed(message)) = (manual, &status) {
                toast(message.clone(), cx);
            }
            set(status, cx);
        });
    })
    .detach();
}

/// Quits into the staged update; the helper swaps it in and opens it.
pub(crate) fn restart(cx: &mut App) {
    let Status::Ready { app, .. } = status(cx) else {
        return;
    };
    match update::relaunch_into(app) {
        Ok(()) => cx.quit(),
        Err(error) => toast(error.to_string(), cx),
    }
}

fn set(status: Status, cx: &mut App) {
    cx.update_global::<Updates, _>(|updates, _| updates.status = status);
}

/// A background task's result, its panic or cancellation included.
async fn join<T, E: std::error::Error + Send + Sync + 'static>(task: impl std::future::Future<Output = Result<anyhow::Result<T>, E>>) -> anyhow::Result<T> {
    task.await?
}

fn toast(message: impl Into<SharedString>, cx: &mut App) {
    let Some(window) = cx.active_window().and_then(|window| window.downcast::<BrowserWindow>()) else {
        return;
    };
    let message = message.into();
    let _ = window.update(cx, |view, _, cx| {
        view.status = message;
        cx.notify();
    });
}
