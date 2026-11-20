mod bookmarks;
mod downloads;
mod extensions;
mod history;
mod home;
mod rail;
mod toolbar;
mod native;
mod palette;
mod passwords;
mod settings;
mod shortcuts;
mod tabs;
mod terminal;
mod update;
mod window;
mod geom;

use browser_engine_macos::{AppHost, ShellWindow, WindowHost};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use window::BrowserWindow;

pub fn run() {
    let self_test = std::env::args().any(|arg| arg == "--self-test");
    let host = Host { self_test, started: Instant::now(), model: RefCell::new(None), links: RefCell::new(Vec::new()) };
    browser_engine_macos::run(Box::new(host), !self_test);
}

struct Host {
    self_test: bool,
    started: Instant,
    /// Filled in once launch has opened the database.
    model: RefCell<Option<SharedModel>>,
    /// Links that arrived before launch finished.
    links: RefCell<Vec<String>>,
}

impl AppHost for Host {
    fn launched(&self) {
        let storage = browser_storage::Storage::open(&browser_core::app_support_dir().join("nxbrowser.sqlite"))
            .expect("open browser database");
        let mut model = browser_core::Model::open(storage).expect("open browser model");
        if !model.settings.restore_session {
            // Start fresh: closing the last tab leaves one home tab, then open the new tab page in it.
            for id in model.session_tabs().iter().map(|tab| tab.id.clone()).collect::<Vec<_>>() {
                let _ = model.close_tab(&id);
            }
            let _ = model.navigate_active(&model.settings.new_tab_url());
            let _ = model.update_settings(|s| {
                s.detached_spaces.clear();
                s.detached_windows.clear();
            });
        }
        // Saved spaces that were deleted since, or the space the main window opens on, are not reopened.
        let live = model.sessions.iter().map(|session| session.id.clone()).collect::<Vec<_>>();
        let main = model.active_session_id.clone();
        let _ = model.update_settings(|s| {
            s.detached_spaces.retain(|id| live.contains(id) && *id != main);
            s.detached_windows.retain(|id| live.contains(id) && *id != main);
        });
        model.request_show_active();
        terminal::set_prefs(&model.settings);
        // Listing fonts takes over a second; done now, the settings font lists open instantly.
        std::thread::spawn(|| terminal::monospace_fonts().len());
        let model: SharedModel = Arc::new(Mutex::new(model));
        *self.model.borrow_mut() = Some(model.clone());
        update::init(model.clone(), !self.self_test);
        let (self_test, started) = (self.self_test, self.started);
        open_browser_window(None, (1100., 760.), move |shell| BrowserWindow::new(model, None, self_test, started, shell));
        browser_engine_macos::activate();
        let links = std::mem::take(&mut *self.links.borrow_mut());
        if !links.is_empty() {
            self.open_urls(links);
        }
    }

    fn open_urls(&self, urls: Vec<String>) {
        let Some(model) = self.model.borrow().clone() else {
            return self.links.borrow_mut().extend(urls);
        };
        // The main window was closed: links reopen it rather than vanish.
        let main = main_window().or_else(|| open_browser_window(None, (1100., 760.), move |shell| BrowserWindow::new(model, None, false, Instant::now(), shell)));
        if let Some(main) = main {
            update(main, |view| {
                view.open_external(urls);
                view.shell.activate();
                view.notify();
            });
        }
        browser_engine_macos::activate();
    }

    fn reopen(&self, _visible: bool) {
        let first = WINDOWS.with_borrow(|windows| windows.first().map(|(no, _)| *no));
        match browser_engine_macos::key_window().or(first) {
            Some(no) => {
                update(no, |view| view.shell.activate());
            }
            None => {
                if let Some(model) = self.model.borrow().clone() {
                    open_browser_window(None, (1100., 760.), move |shell| BrowserWindow::new(model, None, false, Instant::now(), shell));
                }
            }
        }
        browser_engine_macos::activate();
    }
}

thread_local! {
    /// The open browser windows by window number, oldest first.
    static WINDOWS: RefCell<Vec<(isize, Rc<RefCell<BrowserWindow>>)>> = RefCell::default();
}

/// What a window's AppKit side tells its `BrowserWindow`, found by window number.
struct Win(Rc<Cell<isize>>);

impl WindowHost for Win {
    fn frame(&self) {
        let no = self.0.get();
        if update(no, BrowserWindow::frame).is_none() {
            // Busy further up the stack (it asked AppKit for something that reported back): next turn.
            later(no, |_| {});
        }
    }

    fn should_close(&self) -> bool {
        update(self.0.get(), |view| view.should_close()).unwrap_or(true)
    }

    fn closed(&self) {
        let no = self.0.get();
        // Dropped on a later turn, as this can come from inside the window's own frame.
        let gone = SendAnyway(WINDOWS.with_borrow_mut(|windows| windows.iter().position(|(n, _)| *n == no).map(|at| windows.remove(at))));
        dispatch2::DispatchQueue::main().exec_async(move || drop(gone));
    }

    fn dragged(&self, moved: bool, at: (f64, f64)) {
        update(self.0.get(), |view| view.dragged(moved, at));
    }

    fn shift(&self, down: bool) {
        update(self.0.get(), |view| view.shift(down));
    }
}

/// Carries a main-thread value through a block that runs on the main thread too.
pub(crate) struct SendAnyway<T>(pub(crate) T);

// SAFETY: only ever sent from the main thread to the main queue, so it never leaves the main thread.
unsafe impl<T> Send for SendAnyway<T> {}

/// Opens a browser window `size` points big with its top-left at `at` (screen points) or centred, holding what
/// `build` makes of it; returns its number.
pub(crate) fn open_browser_window(at: Option<(f64, f64)>, size: (f64, f64), build: impl FnOnce(ShellWindow) -> BrowserWindow) -> Option<isize> {
    let number = Rc::new(Cell::new(0));
    let shell = ShellWindow::open(at, size, (860., 560.), Box::new(Win(number.clone())))?;
    let no = shell.number();
    number.set(no);
    let view = Rc::new(RefCell::new(build(shell)));
    WINDOWS.with_borrow_mut(|windows| windows.push((no, view)));
    update(no, BrowserWindow::frame);
    Some(no)
}

/// Runs `f` on window `no`, unless it is gone or already in use further up the stack.
pub(crate) fn update<T>(no: isize, f: impl FnOnce(&mut BrowserWindow) -> T) -> Option<T> {
    let view = WINDOWS.with_borrow(|windows| windows.iter().find(|(n, _)| *n == no).map(|(_, view)| view.clone()))?;
    let mut view = view.try_borrow_mut().ok()?;
    Some(f(&mut view))
}

/// Runs `f` on window `no` on a later turn of the main thread (from any thread), then redraws it.
pub(crate) fn later(no: isize, f: impl FnOnce(&mut BrowserWindow) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || run_later(no, f));
}

fn run_later(no: isize, f: impl FnOnce(&mut BrowserWindow) + Send + 'static) {
    let busy = WINDOWS.with_borrow(|windows| windows.iter().any(|(n, view)| *n == no && view.try_borrow_mut().is_err()));
    if busy {
        return later(no, f);
    }
    update(no, |view| {
        f(view);
        view.frame();
    });
}

/// Runs `f` on a later turn of the main thread, outside whatever is borrowed now.
pub(crate) fn defer(f: impl FnOnce() + 'static) {
    let f = SendAnyway(f);
    dispatch2::DispatchQueue::main().exec_async(move || {
        let f = f;
        (f.0)()
    });
}

/// Runs `f` on window `no` every `interval` while it is open.
pub(crate) fn every(no: isize, interval: std::time::Duration, f: fn(&mut BrowserWindow)) {
    let Ok(when) = dispatch2::DispatchTime::try_from(interval) else {
        return;
    };
    let _ = dispatch2::DispatchQueue::main().after(when, move || {
        let open = WINDOWS.with_borrow(|windows| windows.iter().any(|(n, _)| *n == no));
        if open {
            update(no, f);
            every(no, interval, f);
        }
    });
}

/// Every open window's number, oldest first.
pub(crate) fn windows() -> Vec<isize> {
    WINDOWS.with_borrow(|windows| windows.iter().map(|(no, _)| *no).collect())
}

/// The main window (the one with the rail, not a torn-off space), if open.
pub(crate) fn main_window() -> Option<isize> {
    windows().into_iter().find(|no| update(*no, |view| view.model.detached.is_none()).unwrap_or(false))
}

/// Redraws every window: app-wide state (the update status) changed.
pub(crate) fn notify_all() {
    for no in windows() {
        update(no, |view| view.notify());
    }
}

pub(crate) const PUZZLE_ICON: &str = "icons/app/puzzle.svg";
pub(crate) const MIC_ICON: &str = "icons/app/mic.svg";

pub(crate) type SharedModel = Arc<Mutex<browser_core::Model>>;
