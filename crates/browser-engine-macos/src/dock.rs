use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;

static ORIGINAL_REOPEN: OnceLock<Imp> = OnceLock::new();

/// AppKit reads a BOOL here. GPUI's method is registered as void (`ret=v`), so the return
/// register stays 0 and a Dock click never orders a window forward. Return YES after the
/// original, which opens a window when none are visible.
unsafe extern "C-unwind" fn reopen(this: *mut AnyObject, cmd: Sel, sender: *mut AnyObject, has_visible: u8) -> u8 {
    if let Some(original) = ORIGINAL_REOPEN.get().copied() {
        let original: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, u8) = unsafe { std::mem::transmute(original) };
        unsafe { original(this, cmd, sender, has_visible) };
    }
    1
}

/// Replaces GPUI's dock-reopen handler. Call once GPUI's application delegate is registered.
pub fn install_dock_reopen() {
    if ORIGINAL_REOPEN.get().is_some() {
        return;
    }
    let Some(class) = AnyClass::get(c"GPUIApplicationDelegate") else {
        return;
    };
    let Some(method) = class.instance_method(sel!(applicationShouldHandleReopen:hasVisibleWindows:)) else {
        return;
    };
    let imp: Imp = unsafe { std::mem::transmute(reopen as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, u8) -> u8) };
    let previous = unsafe { method.set_implementation(imp) };
    let _ = ORIGINAL_REOPEN.set(previous);
}
