mod downloads;
mod extensions;
mod history;
mod palette;
mod profiles;
mod settings;
mod sidebar;
mod tabs;
mod ui;
mod window;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
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
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("NxBrowser".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.), px(13.))),
                }),
                // Tabs live in the titlebar; empty titlebar space moves the window itself.
                app_owns_titlebar_drag: true,
                window_min_size: Some(size(px(860.), px(560.))),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| BrowserWindow::new(self_test, started, cx)),
        )
        .expect("open window");
        cx.activate(true);
    });
}

/// gpuikit's assets plus the app's own icons (Radix has no puzzle piece for extensions).
struct Assets;

pub(crate) const PUZZLE_ICON: &str = "icons/app/puzzle.svg";

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if path == PUZZLE_ICON {
            return Ok(Some(include_bytes!("../assets/puzzle.svg").as_slice().into()));
        }
        gpuikit::assets().load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        gpuikit::assets().list(path)
    }
}

pub(crate) type SharedModel = Arc<Mutex<browser_core::Model>>;
