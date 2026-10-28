//! Temporary dock/quit tracing. Removed after the close-and-dock bug is confirmed.
use std::ffi::CStr;
use std::io::Write;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification, NSApplicationWillTerminateNotification, NSWindow, NSWindowWillCloseNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString};

const LOG_PATH: &str = "/Users/eurika/software/rust_projects/nx-browser/.cursor/debug-ff0b35.log";

static ORIGINAL_REOPEN: OnceLock<Imp> = OnceLock::new();
static INSTALLED: OnceLock<()> = OnceLock::new();

// #region agent log
pub fn debug_log(hypothesis_id: &str, location: &str, message: &str, data_json: &str) {
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(LOG_PATH) else {
        return;
    };
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let _ = writeln!(
        file,
        r#"{{"sessionId":"ff0b35","runId":"post-fix","hypothesisId":"{hypothesis_id}","timestamp":{ts},"location":"{location}","message":"{}","data":{data_json}}}"#,
        json_escape(message),
    );
}
// #endregion

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn ns_to_string(value: &NSString) -> String {
    let ptr = value.UTF8String();
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
}

fn window_snapshot() -> String {
    let Some(mtm) = MainThreadMarker::new() else {
        return r#"{"mainThread":false}"#.to_string();
    };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    let mut items = Vec::new();
    for index in 0..windows.count() {
        let window: Retained<NSWindow> = windows.objectAtIndex(index);
        let frame = window.frame();
        items.push(format!(
            r#"{{"title":"{}","visible":{},"key":{},"mini":{},"alpha":{},"x":{},"y":{},"w":{},"h":{}}}"#,
            json_escape(&ns_to_string(&window.title())),
            window.isVisible(),
            window.isKeyWindow(),
            window.isMiniaturized(),
            window.alphaValue(),
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        ));
    }
    let policy: isize = unsafe { msg_send![&*app, activationPolicy] };
    format!(
        r#"{{"active":{},"policy":{policy},"nsWindows":{},"windows":[{}]}}"#,
        app.isActive(),
        windows.count(),
        items.join(",")
    )
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxDebugLifecycle"]
    struct NxDebugLifecycle;

    impl NxDebugLifecycle {
        #[unsafe(method(note:))]
        fn note(&self, notification: &NSNotification) {
            // #region agent log
            let name = ns_to_string(&notification.name());
            let hypothesis = if name.contains("WillTerminate") {
                "D"
            } else if name.contains("WillClose") {
                "E"
            } else {
                "B"
            };
            debug_log(hypothesis, "debug_lifecycle.rs:note", &name, &window_snapshot());
            // #endregion
        }
    }

    unsafe impl NSObjectProtocol for NxDebugLifecycle {}
);

/// AppKit reads a BOOL here. GPUI's method is registered as void (`ret=v`), so the return
/// register stays 0 and a Dock click never orders a window forward. Return YES after the
/// original, which opens a window when none are visible.
unsafe extern "C-unwind" fn debug_reopen(this: *mut AnyObject, cmd: Sel, sender: *mut AnyObject, has_visible: u8) -> u8 {
    // #region agent log
    debug_log(
        "C",
        "debug_lifecycle.rs:reopen",
        "applicationShouldHandleReopen",
        &format!(r#"{{"hasVisible":{},"app":{}}}"#, has_visible != 0, window_snapshot()),
    );
    // #endregion
    if let Some(original) = ORIGINAL_REOPEN.get().copied() {
        let original: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, u8) = unsafe { std::mem::transmute(original) };
        unsafe { original(this, cmd, sender, has_visible) };
    }
    1
}

/// Records AppKit close, activate, terminate, and dock-reopen events.
pub fn debug_install_lifecycle_log() {
    if INSTALLED.set(()).is_err() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        // #region agent log
        debug_log("B", "debug_lifecycle.rs:install", "not main thread", r#"{"found":false}"#);
        // #endregion
        return;
    };
    let this = NxDebugLifecycle::alloc(mtm).set_ivars(());
    let this: Retained<NxDebugLifecycle> = unsafe { msg_send![super(this), init] };
    let observer = unsafe { &*(Retained::as_ptr(&this) as *const AnyObject) };
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        center.addObserver_selector_name_object(observer, sel!(note:), Some(NSWindowWillCloseNotification), None);
        center.addObserver_selector_name_object(observer, sel!(note:), Some(NSApplicationDidBecomeActiveNotification), None);
        center.addObserver_selector_name_object(observer, sel!(note:), Some(NSApplicationWillTerminateNotification), None);
    }
    std::mem::forget(this);

    let Some(class) = AnyClass::get(c"GPUIApplicationDelegate") else {
        // #region agent log
        debug_log("B", "debug_lifecycle.rs:install", "delegate class missing", r#"{"found":false}"#);
        // #endregion
        return;
    };
    let Some(method) = class.instance_method(sel!(applicationShouldHandleReopen:hasVisibleWindows:)) else {
        // #region agent log
        debug_log("B", "debug_lifecycle.rs:install", "reopen method missing", r#"{"found":false}"#);
        // #endregion
        return;
    };
    let ret = method.return_type();
    let arg = method.argument_type(3);
    let encoding = format!(
        "ret={} arg3={}",
        ret.to_string_lossy(),
        arg.as_ref().map(|value| value.to_string_lossy().into_owned()).unwrap_or_default()
    );
    // #region agent log
    debug_log("B", "debug_lifecycle.rs:install", "reopen method encoding", &format!(r#"{{"found":true,"encoding":"{}"}}"#, json_escape(&encoding)));
    // #endregion
    let imp: Imp = unsafe { std::mem::transmute(debug_reopen as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, u8) -> u8) };
    let previous = unsafe { method.set_implementation(imp) };
    let _ = ORIGINAL_REOPEN.set(previous);
}
