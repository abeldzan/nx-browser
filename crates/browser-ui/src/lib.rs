mod downloads;
mod extensions;
mod history;
mod palette;
mod profiles;
mod settings;
mod sidebar;
mod tabs;
mod terminal;
mod ui;
mod window;

use gpui::{
    App, AppContext, Application, Bounds, Pixels, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use std::sync::{Arc, Mutex};
use window::BrowserWindow;

pub fn run() {
    let self_test = std::env::args().any(|arg| arg == "--self-test");
    let started = std::time::Instant::now();
    Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(Assets)
        .run(move |cx: &mut App| {
        gpuikit::init(cx);
        gpuikit::input::bind_input_keys(cx, None);
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
        terminal::set_prefs(&model.settings, cx);
        // Listing fonts takes over a second; done now, the settings font list opens instantly.
        std::thread::spawn(|| terminal::monospace_fonts().len());
        let model: SharedModel = Arc::new(Mutex::new(model));
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        open_browser_window(cx, bounds, move |cx| BrowserWindow::new(model, None, self_test, started, cx));
        cx.activate(true);
    });
}

pub(crate) fn open_browser_window(cx: &mut App, bounds: Bounds<Pixels>, build: impl FnOnce(&mut gpui::Context<BrowserWindow>) -> BrowserWindow + 'static) {
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("NxBrowser".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(9.))),
            }),
            // Tabs live in the titlebar; empty titlebar space moves the window itself.
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(860.), px(560.))),
            ..Default::default()
        },
        |_, cx| cx.new(build),
    )
    .expect("open window");
}

/// gpuikit's assets plus the app's own icons (Radix has no puzzle piece or microphone).
struct Assets;

pub(crate) const PUZZLE_ICON: &str = "icons/app/puzzle.svg";
pub(crate) const MIC_ICON: &str = "icons/app/mic.svg";

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if path == PUZZLE_ICON {
            return Ok(Some(include_bytes!("../assets/puzzle.svg").as_slice().into()));
        }
        if path == MIC_ICON {
            return Ok(Some(include_bytes!("../assets/mic.svg").as_slice().into()));
        }
        gpuikit::assets().load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        gpuikit::assets().list(path)
    }
}

pub(crate) type SharedModel = Arc<Mutex<browser_core::Model>>;
