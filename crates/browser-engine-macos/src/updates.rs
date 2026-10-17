//! The app menu's "Check for Updates…" item. GPUI menus are off (see `defuse_gpui_menu_handlers`), so the
//! item has its own target object and calls back into the UI.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSMenuItem;
use objc2_foundation::{ns_string, NSObject, NSObjectProtocol};
use std::cell::RefCell;

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxUpdateMenuTarget"]
    struct NxUpdateMenuTarget;

    unsafe impl NSObjectProtocol for NxUpdateMenuTarget {}

    impl NxUpdateMenuTarget {
        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            CHECK.with_borrow(|check| if let Some((callback, _)) = check { callback() });
        }
    }
);

/// The UI's callback, and the target that calls it: menu items hold their target weakly.
type Check = (Box<dyn Fn()>, Retained<NxUpdateMenuTarget>);

thread_local! {
    static CHECK: RefCell<Option<Check>> = const { RefCell::new(None) };
}

/// Adds "Check for Updates…" to the app menu, calling `callback` when chosen. Call before the first window opens.
pub fn on_check_for_updates(callback: impl Fn() + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let this = NxUpdateMenuTarget::alloc(mtm).set_ivars(());
    let target: Retained<NxUpdateMenuTarget> = unsafe { msg_send![super(this), init] };
    CHECK.set(Some((Box::new(callback), target)));
}

/// The menu item, when the UI asked for one.
pub(crate) fn menu_item(mtm: MainThreadMarker) -> Option<Retained<NSMenuItem>> {
    CHECK.with_borrow(|check| {
        let (_, target) = check.as_ref()?;
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), ns_string!("Check for Updates…"), Some(sel!(checkForUpdates:)), ns_string!(""))
        };
        unsafe { item.setTarget(Some(target)) };
        Some(item)
    })
}
