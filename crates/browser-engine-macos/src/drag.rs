use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSWorkspace};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL};
use std::path::Path;

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxFileDragSource"]
    struct NxFileDragSource;

    unsafe impl NSObjectProtocol for NxFileDragSource {}

    unsafe impl NSDraggingSource for NxFileDragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

thread_local! {
    static SOURCE: Retained<NxFileDragSource> = {
        let this = NxFileDragSource::alloc(MainThreadMarker::new().expect("drags start on the main thread")).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
}

/// Drags the file at `path` out of the window as Finder would, from the mouse event being handled,
/// so a page's dropzone (or any app) takes it.
pub fn drag_file(path: &Path) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else {
        return;
    };
    let Some(view) = event.window(mtm).and_then(|window| window.contentView()) else {
        return;
    };
    let path = NSString::from_str(&path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&path);
    let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
    let at = view.convertPoint_fromView(event.locationInWindow(), None);
    unsafe { item.setDraggingFrame_contents(NSRect::new(NSPoint::new(at.x - 16., at.y - 16.), NSSize::new(32., 32.)), Some(&icon)) };
    SOURCE.with(|source| view.beginDraggingSessionWithItems_event_source(&NSArray::from_retained_slice(&[item]), &event, ProtocolObject::from_ref(&**source)));
}
