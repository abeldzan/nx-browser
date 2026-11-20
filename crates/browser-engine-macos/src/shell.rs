//! The app and its browser windows, straight on AppKit: NSApplication's delegate, each window, and the clear view
//! at the top of a window's content (the chrome view the engine stacks pages beneath and native bars above).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use block2::RcBlock;
use browser_engine::LogicalRect;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSModalResponseOK,
    NSOpenPanel, NSResponder, NSSavePanel, NSScreen, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSArray, NSNotification, NSObject, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSURL,
    NSUserDefaults,
};

/// What the app tells its owner.
pub trait AppHost {
    fn launched(&self);
    /// Links opened from other apps (NxBrowser as the system browser).
    fn open_urls(&self, urls: Vec<String>);
    /// A Dock click; `visible` when a window is on screen.
    fn reopen(&self, visible: bool);
}

/// What a browser window tells its owner. Each call comes from AppKit with nothing of the owner's borrowed.
pub trait WindowHost {
    /// Lays the window out and syncs its native views now: a live resize, an animation frame, a scheduled redraw.
    fn frame(&self);
    /// The close button or ⌘W; false keeps the window open.
    fn should_close(&self) -> bool;
    fn closed(&self);
    /// A press on the drag area moved the window (or didn't); `at` is the pointer in the window after, top-left origin.
    fn dragged(&self, moved: bool, at: (f64, f64));
    /// Shift went down or up with no other key.
    fn shift(&self, down: bool);
}

struct App {
    host: Box<dyn AppHost>,
    /// Quit once the last window closes.
    quit_when_empty: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Each browser window's chrome view, by window number.
    static ROOTS: RefCell<HashMap<isize, Retained<NxRootView>>> = RefCell::default();
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxAppDelegate"]
    struct NxAppDelegate;

    unsafe impl NSObjectProtocol for NxAppDelegate {}

    unsafe impl NSApplicationDelegate for NxAppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            with_app(|app| app.host.launched());
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let urls = urls.iter().filter_map(|url| url.absoluteString()).map(|url| url.to_string()).collect();
            with_app(|app| app.host.open_urls(urls));
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, visible: bool) -> bool {
            with_app(|app| app.host.reopen(visible));
            true
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            // The process was staying in the Dock with no window, and a Dock click had nothing to focus.
            APP.with_borrow(|app| app.as_ref().is_some_and(|app| app.quit_when_empty))
        }
    }
);

/// Runs `f` on the app host without holding the borrow while AppKit calls back in.
fn with_app(f: impl FnOnce(&App)) {
    let app = APP.take();
    if let Some(app) = &app {
        f(app);
    }
    APP.set(app);
}

/// Runs the app until it quits; `host` hears when it has launched, links arriving and Dock clicks.
pub fn run(host: Box<dyn AppHost>, quit_when_empty: bool) {
    let mtm = MainThreadMarker::new().expect("the app runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    APP.set(Some(App { host, quit_when_empty }));
    let delegate: Retained<NxAppDelegate> = unsafe { msg_send![NxAppDelegate::alloc(mtm), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    std::mem::forget(delegate);
    watch_shift();
    app.run();
}

pub fn quit() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).terminate(None);
    }
}

/// Brings the app to the front.
pub fn activate() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).activate();
    }
}

/// The key window's number, if it is one of ours.
pub fn key_window() -> Option<isize> {
    let mtm = MainThreadMarker::new()?;
    let number = NSApplication::sharedApplication(mtm).keyWindow()?.windowNumber();
    ROOTS.with_borrow(|roots| roots.contains_key(&number)).then_some(number)
}

/// Shift alone changes the snap or swap a window drag shows, with no move to report it.
fn watch_shift() {
    let block = RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        if let Some(window) = MainThreadMarker::new().and_then(|mtm| event_ref.window(mtm)) {
            let shift = event_ref.modifierFlags().contains(NSEventModifierFlags::Shift);
            if let Some(root) = ROOTS.with_borrow(|roots| roots.get(&window.windowNumber()).cloned()) {
                root.ivars().host.shift(shift);
            }
        }
        event.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &block) };
    std::mem::forget(block);
    std::mem::forget(monitor);
}

pub(crate) struct RootIvars {
    host: Box<dyn WindowHost>,
    /// Where a press moves the window (a title bar's job), top-left origin.
    zones: RefCell<Vec<LogicalRect>>,
    /// Ticks once per display frame while animating.
    link: RefCell<Option<Retained<AnyObject>>>,
    animating: Cell<bool>,
}

define_class!(
    /// The clear view over a window's content; presses on it outside the drag areas do nothing.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxRootView"]
    #[ivars = RootIvars]
    pub(crate) struct NxRootView;

    impl NxRootView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// A clear view would otherwise let AppKit move the window from anywhere.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            false
        }

        /// Claims the title bar strip, so AppKit neither drags the window from it nor holds clicks there to tell a
        /// double-click apart; the drag areas move the window themselves.
        #[unsafe(method(_opaqueRectForWindowMoveWhenInTitlebar))]
        fn opaque_rect_for_window_move_when_in_titlebar(&self) -> NSRect {
            self.bounds()
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let Some(window) = self.window() else {
                return;
            };
            let at = self.convertPoint_fromView(event.locationInWindow(), None);
            if !self.ivars().zones.borrow().iter().any(|zone| at.x >= zone.x && at.y >= zone.y && at.x < zone.x + zone.width && at.y < zone.y + zone.height) {
                return;
            }
            if event.clickCount() == 2 {
                return double_click_title_bar(&window);
            }
            let before = window.frame().origin;
            // AppKit runs the move in its own loop and returns on release.
            window.performWindowDragWithEvent(event);
            let after = self.convertPoint_fromView(window.mouseLocationOutsideOfEventStream(), None);
            self.ivars().host.dragged(window.frame().origin != before, (after.x, after.y));
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            self.ivars().host.frame();
        }

        #[unsafe(method(step:))]
        fn step(&self, _link: &AnyObject) {
            self.ivars().host.frame();
        }
    }

    unsafe impl NSObjectProtocol for NxRootView {}

    unsafe impl NSWindowDelegate for NxRootView {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().host.should_close()
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            if let Some(window) = notification.object().and_then(|object| object.downcast::<NSWindow>().ok()) {
                ROOTS.with_borrow_mut(|roots| roots.remove(&window.windowNumber()));
                window.setDelegate(None);
            }
            self.set_animating(false);
            self.ivars().host.closed();
        }

        /// Laid out in the resize itself, so pages, frames and bars move in the very frame the window does.
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            self.ivars().host.frame();
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            self.ivars().host.frame();
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            self.ivars().host.frame();
        }
    }
);

impl NxRootView {
    fn set_animating(&self, on: bool) {
        if self.ivars().animating.replace(on) == on {
            return;
        }
        let mut link = self.ivars().link.borrow_mut();
        if on && link.is_none() {
            let created: Retained<AnyObject> = unsafe { msg_send![self, displayLinkWithTarget: self, selector: sel!(step:)] };
            unsafe {
                let _: () = msg_send![&created, addToRunLoop: &*NSRunLoop::mainRunLoop(), forMode: NSRunLoopCommonModes];
            }
            *link = Some(created);
        }
        if let Some(link) = link.as_ref() {
            let _: () = unsafe { msg_send![link, setPaused: !on] };
        }
        if !on && self.window().is_none() {
            if let Some(link) = link.take() {
                let _: () = unsafe { msg_send![&link, invalidate] };
            }
        }
    }
}

/// What the user set a title bar double-click to do: zoom (the default), minimize, or nothing.
fn double_click_title_bar(window: &NSWindow) {
    let action = NSUserDefaults::standardUserDefaults().stringForKey(&NSString::from_str("AppleActionOnDoubleClick")).map(|action| action.to_string());
    match action.as_deref() {
        Some("Minimize") => window.miniaturize(None),
        Some("None") => {}
        _ => window.zoom(None),
    }
}

/// A browser window: an `NSWindow` whose content is the engine's to fill, with the chrome view on top.
pub struct ShellWindow {
    window: Retained<NSWindow>,
    root: Retained<NxRootView>,
}

impl ShellWindow {
    /// Opens a `size` window with its top-left at `at` (screen points, top-left origin of the main screen), or
    /// centred; it can't be made smaller than `min`.
    pub fn open(at: Option<(f64, f64)>, size: (f64, f64), min: (f64, f64), host: Box<dyn WindowHost>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable | NSWindowStyleMask::FullSizeContentView;
        let rect = NSRect::new(NSPoint::ZERO, NSSize::new(size.0, size.1));
        let window = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(NSWindow::alloc(mtm), rect, style, NSBackingStoreType::Buffered, false) };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("NxBrowser"));
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setContentMinSize(NSSize::new(min.0, min.1));
        // The engine puts Liquid Glass or a wallpaper under the chrome, which paints no background of its own.
        window.setOpaque(false);
        // Not clearColor, which breaks the window's shadow.
        window.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0., 0., 0., 0.0001)));
        let ivars = RootIvars { host, zones: RefCell::default(), link: RefCell::default(), animating: Cell::new(false) };
        let root: Retained<NxRootView> = unsafe { msg_send![super(NxRootView::alloc(mtm).set_ivars(ivars)), initWithFrame: rect] };
        root.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        root.setWantsLayer(true);
        let content = window.contentView()?;
        root.setFrame(content.bounds());
        content.addSubview(&root);
        window.setDelegate(Some(ProtocolObject::from_ref(&*root)));
        match at {
            Some((x, y)) => {
                let top = NSScreen::screens(mtm).firstObject().map_or(0., |screen| screen.frame().size.height);
                window.setFrameTopLeftPoint(NSPoint::new(x, top - y));
            }
            None => window.center(),
        }
        ROOTS.with_borrow_mut(|roots| roots.insert(window.windowNumber(), root.clone()));
        window.makeKeyAndOrderFront(None);
        window.makeFirstResponder(Some(&root));
        Some(Self { window, root })
    }

    /// The chrome view, for `WebEngine::attach`.
    pub fn chrome(&self) -> *mut c_void {
        Retained::as_ptr(&self.root).cast_mut().cast()
    }

    pub fn number(&self) -> isize {
        self.window.windowNumber()
    }

    /// The content's size in points.
    pub fn size(&self) -> (f64, f64) {
        let size = self.root.bounds().size;
        (size.width, size.height)
    }

    /// The window's top-left in screen points, top-left origin of the main screen.
    pub fn origin(&self) -> (f64, f64) {
        let frame = self.window.frame();
        let top = NSScreen::screens(self.window.mtm()).firstObject().map_or(0., |screen| screen.frame().size.height);
        (frame.origin.x, top - frame.origin.y - frame.size.height)
    }

    pub fn close(&self) {
        self.window.close();
    }

    pub fn activate(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn is_dark(&self) -> bool {
        let names = NSArray::from_slice(unsafe { &[NSAppearanceNameAqua, NSAppearanceNameDarkAqua] });
        self.window.effectiveAppearance().bestMatchFromAppearancesWithNames(&names).is_some_and(|name| &*name == unsafe { NSAppearanceNameDarkAqua })
    }

    /// Calls `WindowHost::frame` every display frame while `on`.
    pub fn animate(&self, on: bool) {
        self.root.set_animating(on);
    }

    /// Where a press moves the window, in its content (top-left origin).
    pub fn set_drag_zones(&self, zones: Vec<LogicalRect>) {
        *self.root.ivars().zones.borrow_mut() = zones;
    }
}

/// Asks for a folder (`folders`) or a file in a sheet on window `window_no`; `done` gets the pick, or None.
pub fn choose_path(window_no: isize, folders: bool, done: impl FnOnce(Option<PathBuf>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        return done(None);
    };
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(folders);
    panel.setCanChooseFiles(!folders);
    panel.setAllowsMultipleSelection(false);
    let picked = panel.clone();
    let done = Cell::new(Some(done));
    let handler = RcBlock::new(move |response: isize| {
        let path = (response == NSModalResponseOK).then(|| picked.URLs().firstObject().and_then(|url| url.path())).flatten().map(|path| PathBuf::from(path.to_string()));
        if let Some(done) = done.take() {
            done(path);
        }
    });
    sheet(window_no, &panel, &handler);
}

/// Asks where to save, starting in `dir` with `name` filled in, in a sheet on window `window_no`.
pub fn save_path(window_no: isize, dir: &Path, name: Option<&str>, done: impl FnOnce(Option<PathBuf>) + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        return done(None);
    };
    let panel = NSSavePanel::savePanel(mtm);
    if let Some(url) = NSURL::from_directory_path(dir) {
        panel.setDirectoryURL(Some(&url));
    }
    if let Some(name) = name {
        panel.setNameFieldStringValue(&NSString::from_str(name));
    }
    let picked = panel.clone();
    let done = Cell::new(Some(done));
    let handler = RcBlock::new(move |response: isize| {
        let path = (response == NSModalResponseOK).then(|| picked.URL().and_then(|url| url.path())).flatten().map(|path| PathBuf::from(path.to_string()));
        if let Some(done) = done.take() {
            done(path);
        }
    });
    sheet(window_no, &panel, &handler);
}

/// Shows `panel` as a sheet on window `window_no` (a modal loop inside an event handler would let timers re-enter),
/// or on its own when that window is gone.
fn sheet(window_no: isize, panel: &NSSavePanel, handler: &RcBlock<dyn Fn(isize)>) {
    let window = ROOTS.with_borrow(|roots| roots.get(&window_no).and_then(|root| root.window()));
    match window {
        Some(window) => panel.beginSheetModalForWindow_completionHandler(&window, handler),
        None => panel.beginWithCompletionHandler(handler),
    }
}
