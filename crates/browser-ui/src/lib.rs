mod profiles;
mod sidebar;
mod tabs;
mod toolbar;
mod window;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use std::sync::{Arc, Mutex};
use window::BrowserWindow;

pub fn run() {
    let self_test = std::env::args().any(|arg| arg == "--self-test");
    let started = std::time::Instant::now();
    Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(gpuikit::assets())
        .run(move |cx: &mut App| {
        gpuikit::init(cx);
        gpuikit::input::bind_input_keys(cx, None);
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("NxBrowser".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(860.), px(560.))),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| BrowserWindow::new(self_test, started, cx)),
        )
        .expect("open window");
        cx.activate(true);
    });
}

pub(crate) type SharedModel = Arc<Mutex<browser_core::Model>>;
