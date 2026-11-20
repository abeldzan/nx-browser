//! Native popovers (`NSPopover`) and sheets that the UI describes as a `Node` tree; on macOS 26 they get Liquid Glass.
//! Controls report back to the window as `EngineEvent::Popover*` with the action ids the tree gave them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use browser_engine::{EngineEvent, LogicalRect, Phase};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAppearance,
    NSBackingStoreType, NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControlSize, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSImage, NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLineBreakMode, NSPanel, NSPopover, NSPopoverBehavior, NSPopoverDelegate,
    NSProgressIndicator, NSProgressIndicatorStyle, NSResponder, NSScrollView, NSSecureTextField, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTrackingArea, NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation,
    NSView, NSViewController, NSWindow, NSWindowStyleMask, NSComboBox, NSPopUpButton, NSStepper, NSVisualEffectView,
    NSVisualEffectMaterial, NSVisualEffectBlendingMode, NSWindowDelegate, NSApplication, NSControl, NSEventModifierFlags,
    NSFocusRingType, NSGlassEffectView, NSTextView, NSWindowOrderingMode, NSAnimationContext, NSAnimatablePropertyContainer,
    NSEventMask, NSEventType,
};
use objc2_foundation::{NSArray, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSRectEdge, NSSize, NSString};

/// One piece of a popover or sheet. Sizes are points; `action`s and `field`s are the UI's ids for its handlers.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Stacked top to bottom, each child full width, `gap` apart and inset by `pad` (horizontal, vertical).
    Column { gap: f64, pad: (f64, f64), children: Vec<Node> },
    /// Side by side and vertically centred; a `Spacer` takes the slack.
    Row { gap: f64, children: Vec<Node> },
    Spacer,
    /// Text, truncated at the tail when `shrink` (before its neighbours) unless it wraps.
    Text { text: String, size: f64, weight: Weight, ink: Ink, wrap: Wrap, shrink: bool },
    /// A file's image (a favicon), or a white letter on `rgb` when there is none or it doesn't load.
    Image { path: Option<PathBuf>, letter: String, rgb: [f64; 3], size: f64, radius: f64 },
    /// An SF Symbol.
    Glyph { name: &'static str, size: f64, ink: Ink },
    /// A monochrome SVG icon, tinted.
    Svg { data: &'static [u8], size: f64, ink: Ink },
    Button { title: String, action: u32, style: Style, key: Key, enabled: bool },
    Link { title: String, action: u32 },
    /// A borderless SF Symbol button; `on` tints it with the accent.
    Tool { symbol: &'static str, tip: &'static str, action: u32, on: bool },
    Switch { on: bool, action: u32 },
    /// A text field showing `value` when created; edits arrive as `PopoverText`, Return runs `submit` (and so
    /// does leaving the field, when `blur`).
    Field { field: u32, value: Initial, placeholder: String, submit: u32, secure: bool, blur: bool },
    /// A borderless search field (the command palette's, the find bar's) in `size`pt text, showing `value` when
    /// created; list keys arrive as `EngineEvent::PopoverKey`.
    Search { field: u32, value: Initial, placeholder: String, size: f64 },
    /// A text field with a list to pick from (`NSComboBox`); a pick or Return runs `submit`, as does leaving it.
    Combo { field: u32, value: Initial, items: Vec<String>, submit: u32 },
    /// A pop-up menu of `items` showing `selected`; picking item `i` runs action `first + i`.
    Popup { items: Vec<String>, selected: usize, first: u32 },
    /// `value` beside up/down arrows: down runs `down`, up runs `down + 1`.
    Stepper { value: String, down: u32 },
    /// `child`, with `corner` over its top-right corner (taking no room of its own).
    Overlay { child: Box<Node>, corner: Box<Node> },
    /// `child` at its natural size, centred across the width it is given.
    Center(Box<Node>),
    /// `children` left to right, wrapping onto new rows `gap` apart; rows centred when `center`.
    Flow { gap: f64, center: bool, children: Vec<Node> },
    /// `child` exactly `width` wide; text inside wraps at that width.
    Width(f64, Box<Node>),
    /// `child` exactly `height` tall; a `Spacer` in a column inside takes the slack.
    Height(f64, Box<Node>),
    /// `child`, running `action` on a right-click (the handler asks the engine where the pointer is).
    Menu { action: u32, child: Box<Node> },
    /// `child`, running `enter` when the pointer comes over it and `leave` when it goes; clicks pass through to
    /// whatever is beneath.
    Watch { enter: u32, leave: u32, child: Box<Node> },
    /// `child`, showing `tip` when the pointer rests on it.
    Tip { tip: String, child: Box<Node> },
    /// `child`, whose items report being dragged as `EngineEvent::ItemDrag` with `id` instead of clicking.
    Drag { id: u32, child: Box<Node> },
    /// A column (or a row when `horizontal`) whose items can be dragged along it, the others sliding aside; a drop
    /// sends `EngineEvent::ListReorder` for `list`.
    Reorder { list: u32, gap: f64, horizontal: bool, children: Vec<Node> },
    /// A window's layout: a `sidebar_width` sidebar on the system sidebar material, then `content` scrolling,
    /// both `height` tall.
    Split { sidebar_width: f64, height: f64, sidebar: Box<Node>, content: Box<Node> },
    Progress(f64),
    Separator,
    /// A list row `height` tall: highlights on hover when it has a `click`, and can drag `drag` out as a file.
    Item { height: f64, click: Option<u32>, drag: Option<PathBuf>, children: Vec<Node> },
    /// Shown only while the enclosing `Item` is hovered (`true`), or only while it isn't (`false`).
    Hover(bool, Box<Node>),
    /// `child` at 40% opacity: something switched off.
    Faded(Box<Node>),
    /// A box with a tinted background and rounded corners around `child`.
    Card { tint: Ink, child: Box<Node> },
    /// `child` in a rounded field ringed in the accent colour: a text field being typed in. Its corners are `radius`.
    Ring { radius: f64, child: Box<Node> },
    /// `child` with corners of `radius`, on a tinted fill when `tint`: an item's hover highlight takes the same
    /// corners, so a capsule's contents can be concentric with it.
    Round { radius: f64, tint: Option<Ink>, child: Box<Node> },
    /// `child` on a Liquid Glass capsule with corners of `radius`: a group of controls, like a toolbar's.
    Glass { radius: f64, child: Box<Node> },
    /// A `size` square with rounded corners, filled with `fill` (RGBA) and ringed when `selected`, `child` centred;
    /// pressing it runs `action`. A colour or icon choice, or a profile badge.
    Swatch { size: f64, fill: Option<[f64; 4]>, selected: bool, action: Option<u32>, child: Option<Box<Node>> },
    /// Scrolls `child` once it is taller than `max`.
    Scroll { max: f64, child: Box<Node> },
}

/// What a field shows when it is created. Later builds leave what was typed alone, so it never counts as a change.
#[derive(Clone, Debug, Default)]
pub struct Initial(pub String);

impl PartialEq for Initial {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
    /// The system monospaced font.
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Primary,
    Secondary,
    Tertiary,
    Accent,
    Danger,
}

/// How text takes the available width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wrap {
    /// One line.
    Line,
    /// Wraps, ragged right.
    Left,
    /// Wraps, centred.
    Center,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Style {
    Plain,
    Primary,
    Danger,
}

/// The key that presses a button, as in any macOS dialog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    None,
    Return,
    Escape,
}

/// Which side of the anchor the popover opens on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Below,
    Above,
    Right,
}

enum Host {
    Popover(Retained<NSPopover>),
    /// The sheet, and the window it hangs from.
    Sheet(Retained<NSPanel>, Retained<NSWindow>),
    Window(Retained<NSWindow>),
    /// A view the caller places in its own window (the new tab page), holding the content.
    Embedded(Retained<NSView>),
    /// A floating glass panel, the window it is a child of, and the glass holding the content.
    Panel(Retained<NxPanel>, Retained<NSWindow>, Retained<NSGlassEffectView>),
}

impl Host {
    /// The view showing the content now.
    fn root(&self) -> Option<Retained<NSView>> {
        match self {
            Host::Popover(popover) => popover.contentViewController().map(|controller| controller.view()),
            Host::Sheet(panel, _) => panel.contentView(),
            Host::Window(window) => window.contentView(),
            Host::Panel(_, _, glass) => glass.contentView(),
            Host::Embedded(container) => container.subviews().firstObject(),
        }
    }
}

/// A popover or sheet on screen.
struct Surface {
    host: Host,
    window_no: isize,
    key: u64,
    width: f64,
    content: Node,
    /// Field id to its Return action.
    submits: HashMap<u32, u32>,
    /// Kept across rebuilds so typing survives a content change.
    fields: HashMap<u32, Retained<NSTextField>>,
}

thread_local! {
    static POPOVER: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static SHEET: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static WINDOW: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static PANEL: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static LIST: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static TOAST: RefCell<Option<Surface>> = const { RefCell::new(None) };
    static FIND: RefCell<Option<Surface>> = const { RefCell::new(None) };
    /// Embedded surfaces by key (the new tab pages, by tab id).
    static EMBEDS: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
    /// Search fields, by field id: their arrows, Return, Tab and Esc go to the app as `EngineEvent::PopoverKey`.
    static KEY_FIELDS: RefCell<std::collections::HashSet<u32>> = RefCell::new(std::collections::HashSet::new());
    /// The click count of the last press released on an item, for a handler telling a double-click apart.
    static CLICKS: Cell<isize> = const { Cell::new(0) };
    /// While a `Reorder` row is being dragged: hover swaps would re-lay its stack out from under the drag.
    static REORDERING: Cell<bool> = const { Cell::new(false) };
    /// The last popover to close, by key, and when.
    static CLOSED: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
    static TARGET: Retained<NxPopoverTarget> = {
        let this = NxPopoverTarget::alloc(MainThreadMarker::new().expect("popovers live on the main thread")).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
}

/// Rebuilds `surface` with `content` when it changed, keeping its fields and the one being typed in.
/// Returns false when `surface` shows something else (another key or window) and must be replaced.
fn refresh(slot: &'static std::thread::LocalKey<RefCell<Option<Surface>>>, window_no: isize, key: u64, content: &Node) -> bool {
    let mtm = MainThreadMarker::new().expect("popovers live on the main thread");
    slot.with_borrow_mut(|surface| {
        let Some(surface) = surface.as_mut().filter(|surface| surface.key == key && surface.window_no == window_no) else {
            return false;
        };
        if surface.content == *content {
            return true;
        }
        let editing = surface.fields.iter().find(|(_, field)| field.currentEditor().is_some()).map(|(id, _)| *id);
        // Rebuilt from scratch, so scroll views would jump back to the top (a download's progress rebuilds every frame).
        let mut offsets = Vec::new();
        if let Some(old) = surface.host.root() {
            scroll_offsets(&old, &mut offsets);
        }
        let (view, _) = root(mtm, content, surface.width, true, &mut surface.submits, &mut surface.fields);
        let size = view.fittingSize();
        match &surface.host {
            Host::Popover(popover) => {
                popover.setContentSize(size);
                if let Some(controller) = popover.contentViewController() {
                    controller.setView(&view);
                }
            }
            Host::Sheet(panel, _) => {
                panel.setContentView(Some(&view));
                panel.setContentSize(size);
            }
            Host::Window(window) => {
                window.setContentView(Some(&view));
                window.setContentSize(size);
            }
            Host::Embedded(_) => {}
            Host::Panel(panel, _, glass) => {
                // Grows and shrinks downward: its top edge stays put under the title bar.
                let frame = panel.frame();
                let top = frame.origin.y + frame.size.height;
                let size = NSSize::new(size.width + 2. * GLASS_MARGIN, size.height + 2. * GLASS_MARGIN);
                // Resized, refilled and laid out as one unanimated step: otherwise the glass showed a frame at its
                // old height, poking the field's text up above the panel, and faded it out.
                NSAnimationContext::beginGrouping();
                NSAnimationContext::currentContext().setDuration(0.);
                NSAnimationContext::currentContext().setAllowsImplicitAnimation(false);
                panel.setFrame_display(NSRect::new(NSPoint::new(frame.origin.x, top - size.height), size), false);
                glass.setContentView(Some(&view));
                if let Some(clear) = panel.contentView() {
                    clear.layoutSubtreeIfNeeded();
                }
                panel.display();
                NSAnimationContext::endGrouping();
            }
        }
        view.layoutSubtreeIfNeeded();
        restore_offsets(&view, &mut offsets.into_iter());
        surface.content = content.clone();
        if let Some(field) = editing.and_then(|id| surface.fields.get(&id)) {
            focus(field);
        }
        true
    })
}

/// Puts the caret at the end of `field`, rather than selecting all of it as AppKit does.
fn focus(field: &NSTextField) {
    if let Some(window) = field.window() {
        window.makeFirstResponder(Some(field));
    }
    if let Some(editor) = field.currentEditor() {
        editor.setSelectedRange(NSRange::new(field.stringValue().length(), 0));
    }
}

/// Shows `content` anchored to `anchor` (window coordinates, top-left origin) in `content_view`, or updates the
/// popover already showing under `key` in place. A `transient` one closes on any press outside it; otherwise
/// it stays until `close`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn present(window_no: isize, content_view: &NSView, key: u64, anchor: &LogicalRect, edge: Edge, width: f64, transient: bool, content: &Node) {
    if refresh(&POPOVER, window_no, key, content) {
        return;
    }
    close();
    let mtm = MainThreadMarker::new().expect("popovers live on the main thread");
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let (view, _) = root(mtm, content, width, true, &mut submits, &mut fields);
    let controller = NSViewController::new(mtm);
    controller.setView(&view);
    let popover = NSPopover::new(mtm);
    popover.setBehavior(if transient { NSPopoverBehavior::Transient } else { NSPopoverBehavior::ApplicationDefined });
    popover.setAnimates(true);
    popover.setContentViewController(Some(&controller));
    popover.setContentSize(view.fittingSize());
    TARGET.with(|target| popover.setDelegate(Some(ProtocolObject::from_ref(&**target))));
    let height = content_view.bounds().size.height;
    let flipped = content_view.isFlipped();
    let y = if flipped { anchor.y } else { height - anchor.y - anchor.height };
    let rect = NSRect::new(NSPoint::new(anchor.x, y), NSSize::new(anchor.width.max(1.), anchor.height.max(1.)));
    let side = match (edge, flipped) {
        (Edge::Right, _) => NSRectEdge::MaxX,
        (Edge::Below, true) => NSRectEdge::MaxY,
        (Edge::Below, false) => NSRectEdge::MinY,
        (Edge::Above, true) => NSRectEdge::MinY,
        (Edge::Above, false) => NSRectEdge::MaxY,
    };
    popover.showRelativeToRect_ofView_preferredEdge(rect, content_view, side);
    POPOVER.set(Some(Surface { host: Host::Popover(popover), window_no, key, width, content: content.clone(), submits, fields }));
}

pub(crate) fn close() {
    // Taken first: closing calls the delegate, which looks at `POPOVER`.
    if let Some(surface) = POPOVER.take() {
        CLOSED.set(Some((surface.key, Instant::now())));
        if let Host::Popover(popover) = surface.host {
            popover.close();
        }
    }
}

pub(crate) fn is_shown() -> bool {
    POPOVER.with_borrow(|surface| surface.is_some())
}

/// Whether the popover under `key` closed a moment ago: the press that closed it may be on its own button.
pub(crate) fn just_closed(key: u64) -> bool {
    CLOSED.get().is_some_and(|(closed, at)| closed == key && at.elapsed() < Duration::from_millis(400))
}

/// Shows `content` as a sheet on `parent`, or updates the sheet already showing under `key` in place.
pub(crate) fn present_sheet(window_no: isize, parent: &NSWindow, key: u64, width: f64, content: &Node) {
    if refresh(&SHEET, window_no, key, content) {
        return;
    }
    close_sheet(None);
    let mtm = MainThreadMarker::new().expect("sheets live on the main thread");
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let (view, first) = root(mtm, content, width, true, &mut submits, &mut fields);
    let size = view.fittingSize();
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::ZERO, size),
        NSWindowStyleMask::Titled,
        NSBackingStoreType::Buffered,
        false,
    );
    // Held by `SHEET`; AppKit releasing it on close as well would free it twice.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setContentView(Some(&view));
    panel.setContentSize(size);
    if let Some(first) = &first {
        panel.setInitialFirstResponder(Some(first));
    }
    parent.beginSheet_completionHandler(&panel, None);
    SHEET.set(Some(Surface { host: Host::Sheet(panel, parent.retain()), window_no, key, width, content: content.clone(), submits, fields }));
}

/// Closes the open sheet if browser window `owner` opened it, or whichever is open for None.
pub(crate) fn close_sheet(owner: Option<isize>) {
    let surface = match owner {
        Some(window_no) => take_owned(&SHEET, window_no),
        None => SHEET.take(),
    };
    if let Some(Surface { host: Host::Sheet(panel, parent), .. }) = surface {
        parent.endSheet(&panel);
        panel.orderOut(None);
    }
}

/// The surface in `slot` if browser window `window_no` opened it; another window's stays.
fn take_owned(slot: &'static std::thread::LocalKey<RefCell<Option<Surface>>>, window_no: isize) -> Option<Surface> {
    slot.with_borrow_mut(|surface| surface.take_if(|surface| surface.window_no == window_no))
}

/// The open native window, which sheets hang from while it is up.
pub(crate) fn native_window() -> Option<Retained<NSWindow>> {
    WINDOW.with_borrow(|surface| match surface.as_ref().map(|surface| &surface.host) {
        Some(Host::Window(window)) => Some(window.clone()),
        _ => None,
    })
}

/// Shows `content` in its own window titled `title`, centred on `parent`, or updates the one open under `key`.
pub(crate) fn present_window(window_no: isize, parent: &NSWindow, key: u64, title: &str, width: f64, content: &Node) {
    if refresh(&WINDOW, window_no, key, content) {
        return;
    }
    close_window(None);
    let mtm = MainThreadMarker::new().expect("windows live on the main thread");
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let (view, _) = root(mtm, content, width, true, &mut submits, &mut fields);
    let size = view.fittingSize();
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, size),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // Held by `WINDOW`; AppKit releasing it on close as well would free it twice.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    // The sidebar's glass runs up under the title bar, as in System Settings.
    window.setTitlebarAppearsTransparent(true);
    window.setContentView(Some(&view));
    window.setContentSize(size);
    TARGET.with(|target| window.setDelegate(Some(ProtocolObject::from_ref(&**target))));
    let (outer, own) = (parent.frame(), window.frame());
    window.setFrameOrigin(NSPoint::new(
        outer.origin.x + (outer.size.width - own.size.width) / 2.,
        outer.origin.y + (outer.size.height - own.size.height) / 2.,
    ));
    window.makeKeyAndOrderFront(None);
    WINDOW.set(Some(Surface { host: Host::Window(window), window_no, key, width, content: content.clone(), submits, fields }));
}

/// Closes the native window if browser window `owner` opened it, or whichever is open for None.
pub(crate) fn close_window(owner: Option<isize>) {
    // Taken first: closing calls the delegate, which looks at `WINDOW`.
    let surface = match owner {
        Some(window_no) => take_owned(&WINDOW, window_no),
        None => WINDOW.take(),
    };
    if let Some(Surface { host: Host::Window(window), .. }) = surface {
        window.close();
    }
}

/// Whether `window` is the open native window; Esc and ⌘W close it.
pub(crate) fn is_native_window(window: &NSWindow) -> bool {
    WINDOW.with_borrow(|surface| matches!(surface.as_ref().map(|surface| &surface.host), Some(Host::Window(own)) if std::ptr::eq(&**own, window)))
}

/// Closes the open native window as if from its close button.
pub(crate) fn dismiss_window() {
    if let Some(Surface { host: Host::Window(window), window_no, .. }) = WINDOW.take() {
        window.close();
        crate::webkit::send_event(window_no, EngineEvent::NativeWindowClosed);
    }
}

/// The scroll offset of every scroll view under `view`, in tree order.
fn scroll_offsets(view: &NSView, out: &mut Vec<NSPoint>) {
    if let Some(scroll) = view.downcast_ref::<NSScrollView>() {
        out.push(scroll.contentView().bounds().origin);
    }
    for child in view.subviews().iter() {
        scroll_offsets(&child, out);
    }
}

/// Puts `scroll_offsets` back on the matching scroll views of a rebuilt tree.
fn restore_offsets(view: &NSView, offsets: &mut impl Iterator<Item = NSPoint>) {
    if let Some(scroll) = view.downcast_ref::<NSScrollView>() {
        let Some(point) = offsets.next() else {
            return;
        };
        let clip = scroll.contentView();
        clip.scrollToPoint(point);
        scroll.reflectScrolledClipView(&clip);
    }
    for child in view.subviews().iter() {
        restore_offsets(&child, offsets);
    }
}

/// What a floating glass panel is for, which decides how it behaves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Float {
    /// Takes key for its search field; closes, with `EngineEvent::PanelClosed`, once it loses key (a click outside).
    Palette,
    /// Clickable rows that never take key, so typing stays in the field it hangs from (address suggestions).
    List,
    /// Shows a message only: clicks pass through it, and it fades in and out.
    Toast,
    /// Takes key for its search field like the palette, but stays up when the page is clicked (the find bar).
    Find,
}

/// Where a floating panel's glass goes, in its browser window's coordinates (top-left origin).
#[derive(Clone, Copy)]
pub enum Place {
    /// Top edge `y` down; left edge at `x`, or centred for None.
    Top { x: Option<f64>, y: f64 },
    /// Centred, bottom edge this far above the window's bottom.
    Bottom(f64),
}

fn float_slot(kind: Float) -> &'static std::thread::LocalKey<RefCell<Option<Surface>>> {
    match kind {
        Float::Palette => &PANEL,
        Float::List => &LIST,
        Float::Toast => &TOAST,
        Float::Find => &FIND,
    }
}

/// Shows `content` on a floating glass panel `width` wide at `place` over `parent`, or updates the one of this kind
/// open under `key`. A toast fades in when `animate`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn present_float(kind: Float, window_no: isize, parent: &NSWindow, key: u64, width: f64, place: Place, animate: bool, content: &Node) {
    let slot = float_slot(kind);
    if refresh(slot, window_no, key, content) {
        // Follows what it is placed by (the find bar's page, when its window is dragged).
        slot.with_borrow(|surface| {
            if let Some(Surface { host: Host::Panel(panel, _, _), .. }) = surface {
                let origin = float_origin(place, parent.frame(), panel.frame().size);
                if origin != panel.frame().origin {
                    panel.setFrameOrigin(origin);
                }
            }
        });
        return;
    }
    close_float(kind, None, false);
    let mtm = MainThreadMarker::new().expect("panels live on the main thread");
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let (view, first) = root(mtm, content, width, true, &mut submits, &mut fields);
    let size = view.fittingSize();
    let size = NSSize::new(size.width + 2. * GLASS_MARGIN, size.height + 2. * GLASS_MARGIN);
    let origin = float_origin(place, parent.frame(), size);
    let panel: Retained<NxPanel> = unsafe {
        let panel = NxPanel::alloc(mtm).set_ivars(PanelIvars { takes_key: matches!(kind, Float::Palette | Float::Find) });
        msg_send![super(panel), initWithContentRect: NSRect::new(origin, size), styleMask: NSWindowStyleMask::Borderless, backing: NSBackingStoreType::Buffered, defer: false]
    };
    // Held by its slot; AppKit releasing it on close as well would free it twice.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setHasShadow(false);
    panel.setIgnoresMouseEvents(kind == Float::Toast);
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), NSRect::ZERO);
    glass.setCornerRadius(if kind == Float::Toast { 10. } else { 12. });
    glass.setContentView(Some(&view));
    // Inset in a clear view: the glass's own shadow spills past its edge, and the panel's square edge cut it off
    // into a dark outline.
    let clear = NSView::new(mtm);
    clear.addSubview(&glass);
    glass.setTranslatesAutoresizingMaskIntoConstraints(false);
    for edge in [
        glass.leadingAnchor().constraintEqualToAnchor_constant(&clear.leadingAnchor(), GLASS_MARGIN),
        glass.trailingAnchor().constraintEqualToAnchor_constant(&clear.trailingAnchor(), -GLASS_MARGIN),
        glass.topAnchor().constraintEqualToAnchor_constant(&clear.topAnchor(), GLASS_MARGIN),
        glass.bottomAnchor().constraintEqualToAnchor_constant(&clear.bottomAnchor(), -GLASS_MARGIN),
    ] {
        edge.setActive(true);
    }
    panel.setContentView(Some(&clear));
    if kind == Float::Toast && animate {
        panel.setAlphaValue(0.);
    }
    unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above) };
    if kind == Float::Palette {
        TARGET.with(|target| panel.setDelegate(Some(ProtocolObject::from_ref(&**target))));
    }
    if matches!(kind, Float::Palette | Float::Find) {
        panel.makeKeyAndOrderFront(None);
        if let Some(field) = &first {
            focus(field);
        }
    }
    if kind == Float::Toast && animate {
        fade(&panel, 1., None);
    }
    slot.set(Some(Surface { host: Host::Panel(panel, parent.retain(), glass), window_no, key, width, content: content.clone(), submits, fields }));
}

/// Where a panel `size` big (glass margin included) goes for `place` over a window framed `outer`, in screen points.
fn float_origin(place: Place, outer: NSRect, size: NSSize) -> NSPoint {
    let centred = outer.origin.x + (outer.size.width - size.width) / 2.;
    match place {
        Place::Top { x, y } => NSPoint::new(
            x.map_or(centred, |x| outer.origin.x + x - GLASS_MARGIN),
            outer.origin.y + outer.size.height - y - size.height + GLASS_MARGIN,
        ),
        Place::Bottom(above) => NSPoint::new(centred, outer.origin.y + above - GLASS_MARGIN),
    }
}

/// Makes the panel of this kind key again with the caret in its field (⌘F while the find bar is up).
pub(crate) fn focus_float(kind: Float, owner: isize) {
    float_slot(kind).with_borrow(|surface| {
        if let Some(Surface { host: Host::Panel(panel, _, _), fields, .. }) = surface.as_ref().filter(|surface| surface.window_no == owner) {
            panel.makeKeyAndOrderFront(None);
            if let Some(field) = fields.values().next() {
                focus(field);
            }
        }
    });
}

/// Animates `panel`'s opacity to `alpha` over a toast's fade, then runs `done`.
fn fade(panel: &NxPanel, alpha: f64, done: Option<Box<dyn Fn()>>) {
    let panel = panel.retain();
    let changes = RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
        unsafe { context.as_ref() }.setDuration(0.18);
        let window: &NSWindow = &panel;
        window.animator().setAlphaValue(alpha);
    });
    let done = done.map(|done| RcBlock::new(move || done()));
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, done.as_deref());
}

/// Shows `text` in a toast 18pt above the bottom of `parent`, as wide as the text up to 420pt; nothing changes
/// while the same text is up.
pub(crate) fn show_toast(window_no: isize, parent: &NSWindow, text: &str, animate: bool) {
    let mtm = MainThreadMarker::new().expect("toasts live on the main thread");
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&font(12., Weight::Regular)));
    let width = (label.fittingSize().width.ceil() + 2. * 14.).min(420.);
    let content = Node::Column { gap: 0., pad: (14., 7.), children: vec![Node::Text { text: text.into(), size: 12., weight: Weight::Regular, ink: Ink::Primary, wrap: Wrap::Line, shrink: true }] };
    use std::hash::{Hash, Hasher};
    let mut key = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut key);
    present_float(Float::Toast, window_no, parent, key.finish(), width, Place::Bottom(18.), animate, &content);
}

/// Closes the panel of this kind if browser window `owner` opened it, or whichever is open for None; a toast
/// fades out first when `animate`.
pub(crate) fn close_float(kind: Float, owner: Option<isize>, animate: bool) {
    // Taken first: losing key calls the delegate, which looks at the slot.
    let slot = float_slot(kind);
    let surface = match owner {
        Some(window_no) => take_owned(slot, window_no),
        None => slot.take(),
    };
    let Some(Surface { host: Host::Panel(panel, parent, _), .. }) = surface else {
        return;
    };
    if animate {
        let gone = panel.clone();
        fade(&panel, 0., Some(Box::new(move || {
            parent.removeChildWindow(&gone);
            gone.orderOut(None);
        })));
    } else {
        parent.removeChildWindow(&panel);
        panel.orderOut(None);
    }
}

/// Clear room around the panel's glass for its shadow to fade out in.
const GLASS_MARGIN: f64 = 48.;

/// The browser window that opened `window`, when it is the open panel: keys pressed there run that window's shortcuts.
pub(crate) fn panel_owner(window: &NSWindow) -> Option<isize> {
    [&PANEL, &FIND].into_iter().find_map(|slot| {
        slot.with_borrow(|surface| {
            surface.as_ref().filter(|surface| matches!(&surface.host, Host::Panel(panel, _, _) if std::ptr::eq(Retained::as_ptr(panel).cast::<NSWindow>(), window))).map(|surface| surface.window_no)
        })
    })
}

/// Fills `container` with `content` for browser window `window_no`; rebuilt only when the content or width changes,
/// which it returns. The caller places `container`. Laid out exactly `width` wide (0: as wide as it needs), or when
/// `stretch`, as wide as the container, following it as it resizes; `width` then only bounds wrapping and scrolling
/// at the build, and a new one alone rebuilds nothing.
pub(crate) fn embed(window_no: isize, key: &str, container: &NSView, width: f64, stretch: bool, content: &Node) -> bool {
    let mtm = MainThreadMarker::new().expect("embedded views live on the main thread");
    // Out of the map while rebuilding: moving a focused field ends its editing, which calls back into this module.
    let Some(mut surface) = EMBEDS.with_borrow_mut(|embeds| match embeds.get(key) {
        Some(surface) if surface.content == *content && (stretch || surface.width == width) => None,
        _ => Some(embeds.remove(key).unwrap_or(Surface {
            host: Host::Embedded(container.retain()),
            window_no,
            key: 0,
            width,
            content: Node::Spacer,
            submits: HashMap::new(),
            fields: HashMap::new(),
        })),
    }) else {
        return false;
    };
    let editing = surface.fields.iter().find(|(_, field)| field.currentEditor().is_some()).map(|(id, _)| *id);
    let (view, _) = root(mtm, content, width, !stretch, &mut surface.submits, &mut surface.fields);
    for old in container.subviews().iter() {
        old.removeFromSuperview();
    }
    container.addSubview(&view);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.leadingAnchor().constraintEqualToAnchor(&container.leadingAnchor()).setActive(true);
    view.topAnchor().constraintEqualToAnchor(&container.topAnchor()).setActive(true);
    // Otherwise `root` fixed its width and the caller sizes the container to match.
    if stretch {
        view.trailingAnchor().constraintEqualToAnchor(&container.trailingAnchor()).setActive(true);
    }
    surface.width = width;
    surface.content = content.clone();
    if let Some(field) = editing.and_then(|id| surface.fields.get(&id)) {
        focus(field);
    }
    EMBEDS.with_borrow_mut(|embeds| embeds.insert(key.to_string(), surface));
    true
}

/// Where embedded surface `key`'s clickable items sit in their window (top-left origin), by their action.
pub(crate) fn embed_items(key: &str) -> Vec<(u32, LogicalRect)> {
    fn walk(view: &NSView, content: &NSView, out: &mut Vec<(u32, LogicalRect)>) {
        if let Some(action) = view.downcast_ref::<NxPopoverItem>().and_then(|item| item.ivars().click.get()) {
            let rect = view.convertRect_toView(view.bounds(), Some(content));
            let top = if content.isFlipped() { rect.origin.y } else { content.bounds().size.height - rect.origin.y - rect.size.height };
            out.push((action, LogicalRect { x: rect.origin.x, y: top, width: rect.size.width, height: rect.size.height }));
        }
        for child in view.subviews().iter() {
            walk(&child, content, out);
        }
    }
    let mut out = Vec::new();
    EMBEDS.with_borrow(|embeds| {
        if let Some(Host::Embedded(container)) = embeds.get(key).map(|surface| &surface.host) {
            if let Some(content) = container.window().and_then(|window| window.contentView()) {
                walk(container, &content, &mut out);
            }
        }
    });
    out
}

pub(crate) fn unembed(key: &str) {
    EMBEDS.with_borrow_mut(|embeds| embeds.remove(key));
}

/// Puts the caret in the first text field of embedded surface `key`.
pub(crate) fn focus_embed(key: &str) {
    EMBEDS.with_borrow(|embeds| {
        // Not again while being typed in: the click that started the edit also refocuses the tab.
        if let Some(field) = embeds.get(key).and_then(|surface| surface.fields.values().next()).filter(|field| field.currentEditor().is_none()) {
            focus(field);
        }
    });
}

/// Where text field `field` of an embedded surface sits while it is being edited, in its window's coordinates
/// (top-left origin).
pub(crate) fn field_frame(field: u32) -> Option<LogicalRect> {
    EMBEDS.with_borrow(|embeds| {
        let input = embeds.values().filter_map(|surface| surface.fields.get(&field)).find(|input| input.currentEditor().is_some())?;
        let content = input.window()?.contentView()?;
        let rect = input.convertRect_toView(input.bounds(), Some(&content));
        let top = if content.isFlipped() { rect.origin.y } else { content.bounds().size.height - rect.origin.y - rect.size.height };
        Some(LogicalRect { x: rect.origin.x, y: top, width: rect.size.width, height: rect.size.height })
    })
}

/// Replaces the text in field `field` of the open popover, sheet, window or panel, caret at the end.
pub(crate) fn set_field(field: u32, text: &str) {
    let put = |input: &NSTextField| {
        input.setStringValue(&NSString::from_str(text));
        if let Some(editor) = input.currentEditor() {
            editor.setSelectedRange(NSRange::new(input.stringValue().length(), 0));
        }
    };
    for slot in [&PANEL, &FIND, &WINDOW, &SHEET, &POPOVER] {
        slot.with_borrow(|surface| surface.as_ref().and_then(|surface| surface.fields.get(&field)).map(|input| put(input)));
    }
    EMBEDS.with_borrow(|embeds| embeds.values().filter_map(|surface| surface.fields.get(&field)).for_each(|input| put(input)));
}

/// Field `field`'s Return action, in whichever surface has it.
fn submit_action(field: u32) -> Option<u32> {
    [&PANEL, &FIND, &LIST, &WINDOW, &SHEET, &POPOVER]
        .into_iter()
        .find_map(|slot| peek(slot, |surface| surface.submits.get(&field).copied()))
        .or_else(|| EMBEDS.with(|embeds| embeds.try_borrow().ok()?.values().find_map(|surface| surface.submits.get(&field).copied())))
}

/// `read` of the surface in `slot`, skipping one being rebuilt: moving its focused field ends editing, which calls
/// back in here while `refresh` holds the slot.
fn peek<T>(slot: &'static std::thread::LocalKey<RefCell<Option<Surface>>>, read: impl FnOnce(&Surface) -> Option<T>) -> Option<T> {
    slot.with(|cell| cell.try_borrow().ok().and_then(|surface| surface.as_ref().and_then(read)))
}

/// How many clicks the last press released on an item made: 2 for a double-click.
pub(crate) fn last_clicks() -> isize {
    CLICKS.get()
}

type DragFollower = RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>) -> *mut NSEvent>;

thread_local! {
    /// The monitor following a drag torn off an item, until its release; and its handler, kept past its removal
    /// (made from inside it) until the next drag.
    static FOLLOW: RefCell<(Option<Retained<AnyObject>>, Option<DragFollower>)> = const { RefCell::new((None, None)) };
}

/// Reports the moves and the release of drag `id`, begun in `window`, wherever the pointer goes.
fn follow_drag(window: Retained<NSWindow>, id: u32) {
    let handler: DragFollower = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let up = event_ref.r#type() == NSEventType::LeftMouseUp;
        report_drag(&window, event_ref, id, if up { Phase::End } else { Phase::Move });
        if up {
            if let Some(monitor) = FOLLOW.with(|follow| follow.borrow_mut().0.take()) {
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
        }
        event.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp, &handler) };
    let old = FOLLOW.with(|follow| std::mem::replace(&mut *follow.borrow_mut(), (monitor, Some(handler))));
    if let Some(monitor) = old.0 {
        unsafe { NSEvent::removeMonitor(&monitor) };
    }
}

/// Reports a drag under `id` at `event`, in `window`'s content (top-left origin).
fn report_drag(window: &NSWindow, event: &NSEvent, id: u32, phase: Phase) {
    let Some(content) = window.contentView() else {
        return;
    };
    let local = content.convertPoint_fromView(event.locationInWindow(), None);
    let (x, y) = (local.x, if content.isFlipped() { local.y } else { content.bounds().size.height - local.y });
    let event = EngineEvent::ItemDrag { id, phase, x, y };
    match window.windowNumber() {
        no if crate::webkit::is_browser_window(no) => crate::webkit::send_event(no, event),
        _ => send(event),
    }
}

/// Whether `hit`, or a view between it and `stop`, takes clicks itself: a control, an item, a text editor.
pub(crate) fn interactive(hit: &NSView, stop: &NSView) -> bool {
    let mut view = Some(hit.retain());
    while let Some(current) = view {
        if std::ptr::eq(&*current, stop) {
            return false;
        }
        if current.downcast_ref::<objc2_app_kit::NSControl>().is_some() || current.downcast_ref::<NSTextView>().is_some() || current.downcast_ref::<NxPopoverItem>().is_some() {
            return true;
        }
        view = unsafe { current.superview() };
    }
    false
}

/// Sends `event` to the browser window `view` is in, when it is embedded in one; else as `send` does.
fn send_for(view: &NSView, event: EngineEvent) {
    match view.window().map(|window| window.windowNumber()).filter(|&no| crate::webkit::is_browser_window(no)) {
        Some(window_no) => crate::webkit::send_event(window_no, event),
        None => send(event),
    }
}

fn send(event: EngineEvent) {
    let window_no = [&PANEL, &FIND, &LIST, &WINDOW, &SHEET, &POPOVER].into_iter().find_map(|slot| peek(slot, |surface| Some(surface.window_no)));
    if let Some(window_no) = window_no {
        crate::webkit::send_event(window_no, event);
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxPopoverTarget"]
    struct NxPopoverTarget;

    unsafe impl NSObjectProtocol for NxPopoverTarget {}

    unsafe impl NSPopoverDelegate for NxPopoverTarget {
        #[unsafe(method(popoverWillClose:))]
        fn popover_will_close(&self, notification: &NSNotification) {
            // A transient close (a press outside, Esc); `close()` already took its entry.
            let ours = POPOVER.with_borrow(|surface| match surface.as_ref().map(|surface| &surface.host) {
                Some(Host::Popover(popover)) => notification.object().is_some_and(|object| std::ptr::eq(&*object as *const _ as *const NSPopover, &**popover)),
                _ => false,
            });
            if let Some(surface) = ours.then(|| POPOVER.take()).flatten() {
                CLOSED.set(Some((surface.key, Instant::now())));
                crate::webkit::send_event(surface.window_no, EngineEvent::PopoverClosed);
            }
        }
    }

    unsafe impl NSWindowDelegate for NxPopoverTarget {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, notification: &NSNotification) {
            // A click outside the panel; `close_panel()` already took the entry when the app closed it.
            let object = notification.object();
            let window_no = PANEL.with_borrow(|surface| {
                surface
                    .as_ref()
                    .filter(|surface| matches!(&surface.host, Host::Panel(panel, _, _) if object.as_ref().is_some_and(|object| std::ptr::eq(Retained::as_ptr(object), Retained::as_ptr(panel).cast()))))
                    .map(|surface| surface.window_no)
            });
            if let Some(window_no) = window_no {
                close_float(Float::Palette, Some(window_no), false);
                crate::webkit::send_event(window_no, EngineEvent::PanelClosed);
            }
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            // Its close button; `close_window()` already took the entry when the app closed it.
            if let Some(surface) = WINDOW.take() {
                crate::webkit::send_event(surface.window_no, EngineEvent::NativeWindowClosed);
            }
        }
    }

    unsafe impl NSControlTextEditingDelegate for NxPopoverTarget {
        /// The palette's and find bar's keys: arrows, Return (with ⌘ or ⇧), Tab, Esc, and → or ⌫ at the field's ends.
        /// Other fields keep theirs.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, control: &NSControl, text_view: &NSTextView, command: Sel) -> Bool {
            let field = control.tag() as u32;
            if !KEY_FIELDS.with_borrow(|fields| fields.contains(&field)) {
                return Bool::NO;
            }
            // The key event's own modifiers: the keyboard's live state misses replayed and posted keys.
            let event = NSApplication::sharedApplication(self.mtm()).currentEvent();
            let flags = event.as_ref().map(|event| event.modifierFlags()).unwrap_or(NSEventModifierFlags::empty());
            let (shift_held, command_held) = (flags.contains(NSEventModifierFlags::Shift), flags.contains(NSEventModifierFlags::Command));
            let length = text_view.string().length();
            let at_end = text_view.selectedRange() == NSRange::new(length, 0);
            let key = match command.name().to_str().unwrap_or_default() {
                "moveUp:" => "up",
                "moveDown:" => "down",
                "insertNewline:" if command_held => "cmd-enter",
                "insertNewline:" if shift_held => "shift-enter",
                "insertNewline:" => "enter",
                // ⌘↩ arrives as no command at all.
                "noop:" if command_held && event.as_ref().is_some_and(|event| event.keyCode() == 36) => "cmd-enter",
                "insertTab:" => "tab",
                "cancelOperation:" => "escape",
                "moveRight:" if at_end => "right",
                "deleteBackward:" if length == 0 => "backspace",
                _ => return Bool::NO,
            };
            send_for(control, EngineEvent::PopoverKey { field, key: key.into() });
            Bool::YES
        }
    }

    unsafe impl NSTextFieldDelegate for NxPopoverTarget {}

    impl NxPopoverTarget {
        #[unsafe(method(act:))]
        fn act(&self, sender: &NSView) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Ok(action) = u32::try_from(tag) {
                send_for(sender, EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(picked:))]
        fn picked(&self, sender: &NSPopUpButton) {
            if let Some(action) = sender.selectedItem().and_then(|item| u32::try_from(item.tag()).ok()) {
                send_for(sender, EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(stepped:))]
        fn stepped(&self, sender: &NSStepper) {
            // Parked at 1 between 0 and 2, so the new value says which arrow was pressed.
            let up = sender.integerValue() > 1;
            sender.setIntegerValue(1);
            if let Ok(down) = u32::try_from(sender.tag()) {
                send_for(sender, EngineEvent::PopoverAction(down + u32::from(up)));
            }
        }

        #[unsafe(method(submit:))]
        fn submit(&self, sender: &NSTextField) {
            let field = sender.tag() as u32;
            // A combo box pick changes the text without a text-change notification.
            send_for(sender, EngineEvent::PopoverText { field, text: sender.stringValue().to_string() });
            if let Some(action) = submit_action(field) {
                send_for(sender, EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn text_did_end(&self, notification: &NSNotification) {
            // Search fields report losing the caret, so the address bar knows its edit ended.
            let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) else {
                return;
            };
            let tag = field.tag() as u32;
            // Not while its surface is being rebuilt, which moves the field and focuses it again after.
            let settled = |surface: &Surface| surface.fields.get(&tag).is_some_and(|kept| std::ptr::eq(&**kept, &*field));
            let known = [&PANEL, &FIND, &LIST, &WINDOW, &SHEET, &POPOVER].into_iter().any(|slot| peek(slot, |surface| settled(surface).then_some(())).is_some())
                || EMBEDS.with(|embeds| embeds.try_borrow().is_ok_and(|embeds| embeds.values().any(settled)));
            if known && KEY_FIELDS.with_borrow(|fields| fields.contains(&tag)) {
                send_for(&field, EngineEvent::PopoverKey { field: tag, key: "blur".into() });
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, notification: &NSNotification) {
            let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) else {
                return;
            };
            let text = field.stringValue().to_string();
            send_for(&field, EngineEvent::PopoverText { field: field.tag() as u32, text });
        }
    }
);

/// A `Reorder` row being dragged. The row itself stays in the stack, invisible, moving to the slot under the pointer
/// as AppKit lays the stack out again (the rows it passes slide aside); a glass copy of it follows the pointer.
struct Lift {
    stack: Retained<NSStackView>,
    row: Retained<NSView>,
    /// The copy, in the window's content view above everything.
    ghost: Retained<NSView>,
    /// Where the copy was picked up, in content view coordinates.
    home: NSRect,
    from: usize,
    to: usize,
}

/// Lays `stack` out again, its rows gliding to their new places, then runs `done`.
/// How `arrange_list` last laid out a `Reorder` stack; a freshly built one stands as built.
#[derive(Default)]
struct Arranged {
    widths: Vec<f64>,
    hide: Option<usize>,
    clear: Option<usize>,
    gap: Option<(usize, f64)>,
    spacer: Option<Retained<NSView>>,
}

thread_local! {
    /// Every `Reorder` stack built, by list id (ids repeat across windows), with its arrangement.
    static LISTS: RefCell<Vec<(u32, objc2::rc::Weak<NSStackView>, Arranged)>> = const { RefCell::new(Vec::new()) };
}

/// Lays out `window`'s `Reorder` list `list` for a drag passing over it, animated from how it stood: its items
/// `widths` wide, item `hide` collapsed away (the one being dragged), and a gap open before shown item `gap.0`,
/// `gap.1` wide, where the drag would land; item `clear` kept in its place but unseen (one landing there). Nothing
/// is rebuilt, so the items slide.
pub(crate) fn arrange_list(window: &NSWindow, list: u32, widths: &[f64], hide: Option<usize>, clear: Option<usize>, gap: Option<(usize, f64)>) {
    LISTS.with_borrow_mut(|lists| {
        let ours = |stack: &NSStackView| stack.window().is_some_and(|other| std::ptr::eq(&*other, window));
        let Some((_, weak, arranged)) = lists.iter_mut().find(|(id, stack, _)| *id == list && stack.load().is_some_and(|stack| ours(&stack))) else {
            return;
        };
        let Some(stack) = weak.load() else {
            return;
        };
        if arranged.widths == widths && arranged.hide == hide && arranged.clear == clear && arranged.gap == gap {
            return;
        }
        let spacer = arranged.spacer.clone();
        let is_spacer = |view: &NSView| spacer.as_deref().is_some_and(|spacer| std::ptr::eq(spacer, view));
        let rows = stack.arrangedSubviews().iter().filter(|row| !is_spacer(row)).collect::<Vec<_>>();
        for (index, row) in rows.iter().enumerate() {
            if let (Some(width), Some(constraint)) = (widths.get(index), width_constraint(row)) {
                if constraint.constant() != *width {
                    constraint.setConstant(*width);
                }
            }
            let hidden = hide == Some(index);
            if row.isHidden() != hidden {
                row.setHidden(hidden);
            }
            let alpha = if clear == Some(index) { 0. } else { 1. };
            if row.alphaValue() != alpha {
                row.setAlphaValue(alpha);
            }
        }
        if let Some(spacer) = &arranged.spacer {
            stack.removeArrangedSubview(spacer);
            spacer.removeFromSuperview();
        }
        arranged.spacer = gap.map(|(at, width)| {
            let spacer = NSView::new(stack.mtm());
            spacer.setTranslatesAutoresizingMaskIntoConstraints(false);
            spacer.widthAnchor().constraintEqualToConstant(width).setActive(true);
            spacer.heightAnchor().constraintEqualToConstant(1.).setActive(true);
            // Before the `at`th item still shown, or after them all.
            let shown = rows.iter().filter(|row| !row.isHidden()).nth(at);
            let all = stack.arrangedSubviews();
            let index = shown.and_then(|row| all.iter().position(|other| std::ptr::eq(&*other, &**row))).unwrap_or(all.len());
            stack.insertArrangedSubview_atIndex(&spacer, index as isize);
            spacer
        });
        (arranged.widths, arranged.hide, arranged.clear, arranged.gap) = (widths.to_vec(), hide, clear, gap);
        relayout(&stack, None);
    });
}

/// The fixed-width constraint a `Width` node put on `view`.
fn width_constraint(view: &NSView) -> Option<Retained<objc2_app_kit::NSLayoutConstraint>> {
    view.constraints().iter().find(|constraint| {
        // The view's own constant width: no second item, and the view itself first.
        let (first, second) = unsafe { (constraint.firstItem(), constraint.secondItem()) };
        constraint.firstAttribute() == NSLayoutAttribute::Width && second.is_none() && first.is_some_and(|item| std::ptr::eq(Retained::as_ptr(&item).cast::<NSView>(), view))
    })
}

fn relayout(stack: &NSStackView, done: Option<Box<dyn Fn()>>) {
    let stack = stack.retain();
    let changes = RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
        let context = unsafe { context.as_ref() };
        context.setDuration(0.2);
        context.setAllowsImplicitAnimation(true);
        stack.layoutSubtreeIfNeeded();
    });
    let done = done.map(|done| RcBlock::new(move || done()));
    NSAnimationContext::runAnimationGroup_completionHandler(&changes, done.as_deref());
}

/// A picture of `row` on a glass capsule, at `frame` in `content`, lifted above everything.
fn ghost(row: &NSView, content: &NSView, frame: NSRect) -> Retained<NSView> {
    let mtm = row.mtm();
    let bounds = row.bounds();
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
    glass.setCornerRadius((bounds.size.height.min(bounds.size.width) / 2.).min(12.));
    if let Some(rep) = row.bitmapImageRepForCachingDisplayInRect(bounds) {
        row.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let image = NSImage::initWithSize(NSImage::alloc(), bounds.size);
        image.addRepresentation(&rep);
        let picture = NSImageView::imageViewWithImage(&image, mtm);
        picture.setFrame(bounds);
        glass.setContentView(Some(&picture));
    }
    content.addSubview(&glass);
    Retained::into_super(glass)
}

struct ItemIvars {
    click: Cell<Option<u32>>,
    drag: RefCell<Option<PathBuf>>,
    /// `Hover` children: (view, shown while hovered).
    swaps: RefCell<Vec<(Retained<NSView>, bool)>>,
    press: Cell<Option<NSPoint>>,
    hovered: Cell<bool>,
    /// The corners of its hover highlight.
    radius: Cell<f64>,
    /// The `Reorder` column it can be dragged along, by list id.
    reorder: RefCell<Option<(u32, objc2::rc::Weak<NSStackView>)>>,
    /// While dragged along it.
    lifted: RefCell<Option<Lift>>,
    /// The `Drag` id it reports drags under.
    drags: Cell<Option<u32>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxPopoverItem"]
    #[ivars = ItemIvars]
    struct NxPopoverItem;

    impl NxPopoverItem {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.set_hovered(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.set_hovered(false);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().press.set(Some(event.locationInWindow()));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(start) = self.ivars().press.get() else {
                return;
            };
            let at = event.locationInWindow();
            let stack = self.ivars().reorder.borrow().as_ref().and_then(|(_, stack)| stack.load());
            if let Some(stack) = stack {
                self.drag_row(&stack, event, start, at);
                return;
            }
            if self.ivars().drags.get().is_some() {
                if (at.x - start.x).abs() + (at.y - start.y).abs() > 4. {
                    self.tear_off(event);
                }
                return;
            }

            let Some(path) = self.ivars().drag.borrow().clone() else {
                return;
            };
            if (at.x - start.x).abs() + (at.y - start.y).abs() > 4. {
                self.ivars().press.set(None);
                crate::drag::drag_file(&path);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            CLICKS.set(event.clickCount());
            if self.ivars().lifted.borrow().is_some() {
                self.ivars().press.set(None);
                self.settle(true);
                return;
            }
            if self.ivars().press.take().is_some() {
                if let Some(action) = self.ivars().click.get() {
                    send_for(self, EngineEvent::PopoverAction(action));
                }
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().hovered.get() && self.ivars().click.get().is_some() {
                NSColor::quaternarySystemFillColor().setFill();
                let radius = self.ivars().radius.get();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), radius, radius).fill();
            }
        }
    }
);

impl NxPopoverItem {
    fn new(mtm: MainThreadMarker, click: Option<u32>, drag: Option<PathBuf>) -> Retained<Self> {
        let ivars = ItemIvars {
            click: Cell::new(click),
            drag: RefCell::new(drag),
            swaps: RefCell::default(),
            press: Cell::new(None),
            hovered: Cell::new(false),
            radius: Cell::new(6.),
            reorder: RefCell::default(),
            lifted: RefCell::default(),
            drags: Cell::new(None),
        };
        let this: Retained<Self> = unsafe { msg_send![super(Self::alloc(mtm).set_ivars(ivars)), initWithFrame: NSRect::ZERO] };
        let options = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&this), None) };
        this.addTrackingArea(&area);
        this
    }

    /// Drags the `Reorder` row holding this item along its stack, from `start` to `at` (window coordinates). Pulled
    /// well off the stack's line, an item that is also in a `Drag` tears off instead and reports an `ItemDrag` from then on.
    fn drag_row(&self, stack: &NSStackView, event: &NSEvent, start: NSPoint, at: NSPoint) {
        let horizontal = stack.orientation() == NSUserInterfaceLayoutOrientation::Horizontal;
        let Some(content) = self.window().and_then(|window| window.contentView()) else {
            return;
        };
        let (from, to) = (content.convertPoint_fromView(start, None), content.convertPoint_fromView(at, None));
        let (along, across) = if horizontal { (to.x - from.x, to.y - from.y) } else { (to.y - from.y, to.x - from.x) };
        let tears = self.ivars().drags.get().is_some();
        if self.ivars().lifted.borrow().is_none() {
            if tears && across.abs() > 8. && across.abs() > along.abs() {
                return self.tear_off(event);
            }
            if along.abs() < 4. {
                return;
            }
            let rows = stack.arrangedSubviews();
            let Some(index) = rows.iter().position(|row| self.isDescendantOf(&row)) else {
                return;
            };
            let row = rows.objectAtIndex(index);
            let home = row.convertRect_toView(row.bounds(), Some(&content));
            let ghost = ghost(&row, &content, home);
            row.setAlphaValue(0.);
            REORDERING.set(true);
            *self.ivars().lifted.borrow_mut() = Some(Lift { stack: stack.retain(), row, ghost, home, from: index, to: index });
        }
        if tears && across.abs() > 28. {
            self.settle(false);
            return self.tear_off(event);
        }
        let mut lifted = self.ivars().lifted.borrow_mut();
        let Some(lift) = lifted.as_mut() else {
            return;
        };
        // Along the stack only, and not past its ends.
        let line = stack.convertRect_toView(stack.bounds(), Some(&content));
        let origin = if horizontal {
            NSPoint::new((lift.home.origin.x + along).clamp(line.origin.x, (line.origin.x + line.size.width - lift.home.size.width).max(line.origin.x)), lift.home.origin.y)
        } else {
            NSPoint::new(lift.home.origin.x, (lift.home.origin.y + along).clamp(line.origin.y, (line.origin.y + line.size.height - lift.home.size.height).max(line.origin.y)))
        };
        lift.ghost.setFrameOrigin(origin);
        let centre = |row: &NSView| {
            let rect = row.convertRect_toView(row.bounds(), Some(&content));
            if horizontal { rect.origin.x + rect.size.width / 2. } else { rect.origin.y + rect.size.height / 2. }
        };
        let (low, high) = if horizontal { (origin.x, origin.x + lift.home.size.width) } else { (origin.y, origin.y + lift.home.size.height) };
        // In content coordinates the order runs left to right, or top to bottom: down the y axis unless flipped.
        let forward = horizontal || content.isFlipped();
        // As in AppKit's lists, a row gives way once the dragged one covers half of it, whichever way it moves:
        // its leading edge past the next row's middle, or its trailing edge past the previous one's.
        let (ahead, behind) = if forward { (high, low) } else { (low, high) };
        let passed = |middle: f64, edge: f64, onward: bool| if onward == forward { edge > middle } else { edge < middle };
        let rows = stack.arrangedSubviews().iter().collect::<Vec<_>>();
        let mut before = lift.to;
        while rows.get(before + 1).is_some_and(|next| passed(centre(next), ahead, true)) {
            before += 1;
        }
        // One way at a time: scanning back from a slot already moved forward would meet its own (invisible) row.
        if before == lift.to {
            while before > 0 && rows.get(before - 1).is_some_and(|previous| passed(centre(previous), behind, false)) {
                before -= 1;
            }
        }
        if before != lift.to {
            lift.to = before;
            stack.removeArrangedSubview(&lift.row);
            stack.insertArrangedSubview_atIndex(&lift.row, before as isize);
            relayout(stack, None);
        }
    }

    /// Ends a row drag: the copy glides into the slot the row now holds and the row shows again, then the move goes
    /// out as `ListReorder`. When not `drop` (a tear-off), the row goes back to its own slot and nothing is reported.
    fn settle(&self, drop: bool) {
        let Some(lift) = self.ivars().lifted.take() else {
            return;
        };
        REORDERING.set(false);
        let Lift { stack, row, ghost, from, to, .. } = lift;
        let moved = drop && to != from;
        if !drop && to != from {
            stack.removeArrangedSubview(&row);
            stack.insertArrangedSubview_atIndex(&row, from as isize);
            stack.layoutSubtreeIfNeeded();
        }
        if !drop {
            ghost.removeFromSuperview();
            row.setAlphaValue(1.);
            return;
        }
        stack.layoutSubtreeIfNeeded();
        let list = self.ivars().reorder.borrow().as_ref().map(|(list, _)| *list);
        let target = unsafe { ghost.superview() }.map(|content| row.convertRect_toView(row.bounds(), Some(&content)));
        let item = self.retain();
        let changes = {
            let ghost = ghost.clone();
            RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
                unsafe { context.as_ref() }.setDuration(0.18);
                if let Some(target) = target {
                    ghost.animator().setFrame(target);
                }
            })
        };
        let done = RcBlock::new(move || {
            ghost.removeFromSuperview();
            row.setAlphaValue(1.);
            if let (true, Some(list)) = (moved, list) {
                send_for(&item, EngineEvent::ListReorder { list, from, to });
            }
        });
        NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&done));
    }

    /// Hands the press over to the `Drag` it is in: an `ItemDrag` begins here.
    /// The drag goes on to rebuild this item's surface, taking it out of its window, and AppKit sends a view gone
    /// from its window no more of it: a monitor follows the rest, and the item lets the press go.
    fn tear_off(&self, event: &NSEvent) {
        let Some(id) = self.ivars().drags.get() else {
            return;
        };
        self.ivars().press.set(None);
        let Some(window) = self.window().or_else(|| event.window(self.mtm())) else {
            return;
        };
        report_drag(&window, event, id, Phase::Begin);
        follow_drag(window, id);
    }

    fn set_hovered(&self, hovered: bool) {
        if REORDERING.get() {
            return;
        }
        self.ivars().hovered.set(hovered);
        for (view, on_hover) in self.ivars().swaps.borrow().iter() {
            view.setHidden(*on_hover != hovered);
        }
        self.setNeedsDisplay(true);
    }
}

define_class!(
    /// Runs its action on a right-click anywhere inside, which the views within pass up the responder chain.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxMenuView"]
    #[ivars = Cell<u32>]
    struct NxMenuView;

    impl NxMenuView {
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, _event: &NSEvent) {
            send_for(self, EngineEvent::PopoverAction(self.ivars().get()));
        }
    }
);

impl NxMenuView {
    fn new(mtm: MainThreadMarker, action: u32) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(Cell::new(action))), initWithFrame: NSRect::ZERO] }
    }
}

define_class!(
    /// Reports the pointer coming over it and leaving, as actions; never the target of a click itself.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxWatchView"]
    #[ivars = (u32, u32)]
    struct NxWatchView;

    impl NxWatchView {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            send_for(self, EngineEvent::PopoverAction(self.ivars().0));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            send_for(self, EngineEvent::PopoverAction(self.ivars().1));
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            match hit {
                Some(hit) if !std::ptr::eq(Retained::as_ptr(&hit).cast::<Self>(), self) => Retained::autorelease_ptr(hit),
                _ => std::ptr::null_mut(),
            }
        }
    }
);

impl NxWatchView {
    fn new(mtm: MainThreadMarker, enter: u32, leave: u32) -> Retained<Self> {
        let this: Retained<Self> = unsafe { msg_send![super(Self::alloc(mtm).set_ivars((enter, leave))), initWithFrame: NSRect::ZERO] };
        let options = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&this), None) };
        this.addTrackingArea(&area);
        this
    }
}

pub(crate) struct PanelIvars {
    takes_key: bool,
}

define_class!(
    /// Borderless, so the palette would never take key (and its search field typing) without saying it can; a
    /// list must not, or clicking it would take typing away from the field it hangs from.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxPanel"]
    #[ivars = PanelIvars]
    pub(crate) struct NxPanel;

    impl NxPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            self.ivars().takes_key
        }
    }
);

define_class!(
    /// Scroll views lay their document out from the bottom unless it is flipped.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxFlippedView"]
    struct NxFlippedView;

    impl NxFlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

/// The popover's whole content view, `width` wide.
/// The whole content view, `width` wide, and its first text field.
fn root(
    mtm: MainThreadMarker,
    content: &Node,
    width: f64,
    fixed: bool,
    submits: &mut HashMap<u32, u32>,
    fields: &mut HashMap<u32, Retained<NSTextField>>,
) -> (Retained<NSView>, Option<Retained<NSTextField>>) {
    let mut kept = std::mem::take(fields);
    submits.clear();
    // Width 0 leaves it its natural width; wrapping text and scroll views still need some bound.
    let bound = if width > 0. { width } else { 10_000. };
    let mut build = Build { mtm, submits, fields, kept: &mut kept, swaps: Vec::new(), width: bound, first: None, reorder: None, drags: None };
    let view = build.node(content);
    if fixed && width > 0. {
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
    }
    view.layoutSubtreeIfNeeded();
    (view, build.first)
}

struct Build<'a> {
    mtm: MainThreadMarker,
    submits: &'a mut HashMap<u32, u32>,
    fields: &'a mut HashMap<u32, Retained<NSTextField>>,
    /// Fields from the previous build, reused by id.
    kept: &'a mut HashMap<u32, Retained<NSTextField>>,
    /// `Hover` views met since the innermost `Item` began.
    swaps: Vec<(Retained<NSView>, bool)>,
    /// The width the node being built gets; a scroll view's document and wrapping text need it before layout.
    width: f64,
    /// The first text field built, which a sheet starts with focus in.
    first: Option<Retained<NSTextField>>,
    /// Items met since the innermost `Reorder` began, while inside one.
    reorder: Option<Vec<Retained<NxPopoverItem>>>,
    /// The innermost `Drag` id, while inside one.
    drags: Option<u32>,
}

impl Build<'_> {
    fn node(&mut self, node: &Node) -> Retained<NSView> {
        let mtm = self.mtm;
        let view: Retained<NSView> = match node {
            Node::Column { gap, pad, children } => {
                let outer = self.width;
                self.width -= 2. * pad.0;
                let stack = self.stack(children, NSUserInterfaceLayoutOrientation::Vertical, *gap);
                self.width = outer;
                stack.setAlignment(NSLayoutAttribute::Leading);
                stack.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: pad.1, left: pad.0, bottom: pad.1, right: pad.0 });
                for child in stack.arrangedSubviews().iter() {
                    child.widthAnchor().constraintEqualToAnchor_constant(&stack.widthAnchor(), -2. * pad.0).setActive(true);
                }
                // In a row it takes the free width, like a flex-1 column.
                stack.setHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                Retained::into_super(stack)
            }
            Node::Row { gap, children } => {
                let stack = self.stack(children, NSUserInterfaceLayoutOrientation::Horizontal, *gap);
                stack.setAlignment(NSLayoutAttribute::CenterY);
                // Two spacers share the slack equally, centring what is between them.
                let views = stack.arrangedSubviews();
                let spacers = children.iter().enumerate().filter(|(_, child)| matches!(child, Node::Spacer)).filter(|(index, _)| *index < views.count()).map(|(index, _)| views.objectAtIndex(index)).collect::<Vec<_>>();
                if let [first, second] = spacers.as_slice() {
                    first.widthAnchor().constraintEqualToAnchor(&second.widthAnchor()).setActive(true);
                }
                Retained::into_super(stack)
            }
            Node::Spacer => {
                let view = NSView::new(mtm);
                for orientation in [NSLayoutConstraintOrientation::Horizontal, NSLayoutConstraintOrientation::Vertical] {
                    view.setContentHuggingPriority_forOrientation(1., orientation);
                    view.setContentCompressionResistancePriority_forOrientation(1., orientation);
                }
                view
            }
            Node::Text { text, size, weight, ink, wrap, shrink } => {
                let text = NSString::from_str(text);
                let label = match wrap {
                    Wrap::Left | Wrap::Center => {
                        let label = NSTextField::wrappingLabelWithString(&text, mtm);
                        label.setPreferredMaxLayoutWidth(self.width);
                        label.setSelectable(false);
                        if *wrap == Wrap::Center {
                            label.setAlignment(NSTextAlignment::Center);
                        }
                        label
                    }
                    Wrap::Line => {
                        let label = NSTextField::labelWithString(&text, mtm);
                        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
                        label.setMaximumNumberOfLines(1);
                        let give = if *shrink { 250. } else { 760. };
                        label.setContentCompressionResistancePriority_forOrientation(give, NSLayoutConstraintOrientation::Horizontal);
                        label
                    }
                };
                label.setFont(Some(&font(*size, *weight)));
                label.setTextColor(Some(&color(*ink)));
                Retained::into_super(Retained::into_super(label))
            }
            Node::Image { path, letter, rgb, size, radius } => {
                let image = path.as_ref().and_then(|path| NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(&path.to_string_lossy())));
                let view: Retained<NSView> = match image {
                    Some(image) => {
                        let view = NSImageView::imageViewWithImage(&image, mtm);
                        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                        Retained::into_super(Retained::into_super(view))
                    }
                    None => {
                        let label = NSTextField::labelWithString(&NSString::from_str(letter), mtm);
                        label.setFont(Some(&font((size * 0.55).max(9.), Weight::Semibold)));
                        label.setTextColor(Some(&NSColor::whiteColor()));
                        label.setAlignment(NSTextAlignment::Center);
                        label.setDrawsBackground(true);
                        label.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(rgb[0], rgb[1], rgb[2], 1.)));
                        Retained::into_super(Retained::into_super(label))
                    }
                };
                view.setWantsLayer(true);
                if let Some(layer) = view.layer() {
                    layer.setCornerRadius(*radius);
                    layer.setMasksToBounds(true);
                }
                fixed(&view, *size, *size);
                view
            }
            Node::Glyph { name, size, ink } => {
                let view = NSImageView::new(mtm);
                view.setImage(symbol(name, *size).as_deref());
                view.setContentTintColor(Some(&color(*ink)));
                Retained::into_super(Retained::into_super(view))
            }
            Node::Button { title, action, style, key, enabled } => {
                let button = TARGET.with(|target| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(sel!(act:)), mtm) });
                button.setTag(*action as isize);
                button.setEnabled(*enabled);
                match style {
                    Style::Plain => {}
                    Style::Primary => button.setBezelColor(Some(&NSColor::controlAccentColor())),
                    Style::Danger => button.setBezelColor(Some(&NSColor::systemRedColor())),
                }
                match key {
                    Key::None => {}
                    Key::Return => button.setKeyEquivalent(&NSString::from_str("\r")),
                    Key::Escape => button.setKeyEquivalent(&NSString::from_str("\u{1b}")),
                }
                Retained::into_super(Retained::into_super(button))
            }
            Node::Svg { data, size, ink } => {
                let view = NSImageView::new(mtm);
                if let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(data)) {
                    image.setTemplate(true);
                    view.setImage(Some(&image));
                }
                view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                view.setContentTintColor(Some(&color(*ink)));
                fixed(&view, *size, *size);
                Retained::into_super(Retained::into_super(view))
            }
            Node::Swatch { size, fill, selected, action, child } => {
                let item = self.item(*action, None);
                let tile = NSBox::new(mtm);
                tile.setBoxType(NSBoxType::Custom);
                tile.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
                tile.setCornerRadius(size * 0.27);
                tile.setBorderWidth(if *selected { 2. } else { 0. });
                tile.setBorderColor(&NSColor::labelColor());
                let [r, g, b, a] = fill.unwrap_or([0., 0., 0., 0.]);
                tile.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a));
                tile.setContentViewMargins(NSSize::ZERO);
                pin(&item, &tile, 0.);
                if let Some(child) = child {
                    let inner = self.node(child);
                    if let Some(content) = tile.contentView() {
                        content.addSubview(&inner);
                        inner.centerXAnchor().constraintEqualToAnchor(&content.centerXAnchor()).setActive(true);
                        inner.centerYAnchor().constraintEqualToAnchor(&content.centerYAnchor()).setActive(true);
                    }
                }
                fixed(&item, *size, *size);
                Retained::into_super(item)
            }
            Node::Link { title, action } => {
                let button = TARGET.with(|target| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(sel!(act:)), mtm) });
                button.setTag(*action as isize);
                button.setBordered(false);
                button.setFont(Some(&font(12., Weight::Regular)));
                button.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
                Retained::into_super(Retained::into_super(button))
            }
            Node::Tool { symbol: name, tip, action, on } => {
                let image = symbol(name, 12.).unwrap_or_else(NSImage::new);
                let button = TARGET.with(|target| unsafe { NSButton::buttonWithImage_target_action(&image, Some(target), Some(sel!(act:)), mtm) });
                button.setTag(*action as isize);
                button.setBordered(false);
                button.setToolTip(Some(&NSString::from_str(tip)));
                button.setContentTintColor(Some(&color(if *on { Ink::Accent } else { Ink::Secondary })));
                fixed(&button, 22., 22.);
                Retained::into_super(Retained::into_super(button))
            }
            Node::Switch { on, action } => {
                let switch = NSSwitch::new(mtm);
                switch.setControlSize(NSControlSize::Mini);
                switch.setState(if *on { NSControlStateValueOn } else { NSControlStateValueOff });
                switch.setTag(*action as isize);
                TARGET.with(|target| unsafe {
                    switch.setTarget(Some(target));
                    switch.setAction(Some(sel!(act:)));
                });
                Retained::into_super(Retained::into_super(switch))
            }
            Node::Search { field, value, placeholder, size } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let input = NSTextField::new(mtm);
                    input.setBordered(false);
                    input.setDrawsBackground(false);
                    input.setFocusRingType(NSFocusRingType::None);
                    input.setFont(Some(&font(*size, Weight::Regular)));
                    input.setStringValue(&NSString::from_str(&value.0));
                    self.wire_field(&input, *field, false);
                    input
                });
                input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                KEY_FIELDS.with_borrow_mut(|fields| fields.insert(*field));
                self.fields.insert(*field, input.clone());
                self.first.get_or_insert_with(|| input.clone());
                input.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                Retained::into_super(Retained::into_super(input))
            }
            Node::Overlay { child, corner } => {
                let holder = NSView::new(mtm);
                pin(&holder, &self.node(child), 0.);
                let corner = self.node(corner);
                holder.addSubview(&corner);
                corner.setTranslatesAutoresizingMaskIntoConstraints(false);
                corner.topAnchor().constraintEqualToAnchor(&holder.topAnchor()).setActive(true);
                corner.trailingAnchor().constraintEqualToAnchor(&holder.trailingAnchor()).setActive(true);
                holder.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                holder
            }
            Node::Center(child) => {
                let holder = NSView::new(mtm);
                let view = self.node(child);
                holder.addSubview(&view);
                view.centerXAnchor().constraintEqualToAnchor(&holder.centerXAnchor()).setActive(true);
                view.topAnchor().constraintEqualToAnchor(&holder.topAnchor()).setActive(true);
                view.bottomAnchor().constraintEqualToAnchor(&holder.bottomAnchor()).setActive(true);
                view.widthAnchor().constraintLessThanOrEqualToAnchor(&holder.widthAnchor()).setActive(true);
                holder.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                holder
            }
            Node::Flow { gap, center, children } => {
                // Rows filled greedily by each child's natural width, within the width available here.
                let views = children.iter().map(|child| self.node(child)).collect::<Vec<_>>();
                let mut rows: Vec<Vec<Retained<NSView>>> = Vec::new();
                let mut used = f64::INFINITY;
                for view in views {
                    let width = view.fittingSize().width;
                    if used + gap + width > self.width {
                        rows.push(Vec::new());
                        used = -gap;
                    }
                    used += gap + width;
                    if let Some(row) = rows.last_mut() {
                        row.push(view);
                    }
                }
                let rows = rows
                    .into_iter()
                    .map(|row| {
                        let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&row), mtm);
                        stack.setSpacing(*gap);
                        stack.setAlignment(NSLayoutAttribute::Top);
                        Retained::into_super(stack)
                    })
                    .collect::<Vec<_>>();
                let column = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
                column.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
                column.setSpacing(*gap);
                column.setAlignment(if *center { NSLayoutAttribute::CenterX } else { NSLayoutAttribute::Leading });
                column.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                Retained::into_super(column)
            }
            Node::Combo { field, value, items, submit } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let combo = NSComboBox::new(mtm);
                    let names = items.iter().map(|item| NSString::from_str(item)).collect::<Vec<_>>();
                    // SAFETY: an array of strings is an array of objects.
                    let names: Retained<NSArray> = unsafe { Retained::cast_unchecked(NSArray::from_retained_slice(&names)) };
                    unsafe { combo.addItemsWithObjectValues(&names) };
                    combo.setCompletes(true);
                    combo.setNumberOfVisibleItems(14);
                    let input = Retained::into_super(combo);
                    input.setStringValue(&NSString::from_str(&value.0));
                    self.wire_field(&input, *field, true);
                    input
                });
                self.submits.insert(*field, *submit);
                self.fields.insert(*field, input.clone());
                input.widthAnchor().constraintEqualToConstant(260.).setActive(true);
                Retained::into_super(Retained::into_super(input))
            }
            Node::Popup { items, selected, first } => {
                let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
                for (index, title) in items.iter().enumerate() {
                    popup.addItemWithTitle(&NSString::from_str(title));
                    if let Some(item) = popup.lastItem() {
                        item.setTag((first + index as u32) as isize);
                    }
                }
                popup.selectItemAtIndex(*selected as isize);
                TARGET.with(|target| unsafe {
                    popup.setTarget(Some(target));
                    popup.setAction(Some(sel!(picked:)));
                });
                Retained::into_super(Retained::into_super(Retained::into_super(popup)))
            }
            Node::Stepper { value, down } => {
                let stepper = NSStepper::new(mtm);
                stepper.setMinValue(0.);
                stepper.setMaxValue(2.);
                stepper.setIncrement(1.);
                stepper.setIntegerValue(1);
                stepper.setTag(*down as isize);
                TARGET.with(|target| unsafe {
                    stepper.setTarget(Some(target));
                    stepper.setAction(Some(sel!(stepped:)));
                });
                let label = self.node(&Node::Text { text: value.clone(), size: 13., weight: Weight::Regular, ink: Ink::Primary, wrap: Wrap::Line, shrink: false });
                let views = [label, Retained::into_super(Retained::into_super(stepper))];
                let row = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
                row.setSpacing(6.);
                Retained::into_super(row)
            }
            Node::Width(width, child) => {
                let outer = self.width;
                self.width = *width;
                let view = self.node(child);
                self.width = outer;
                view.widthAnchor().constraintEqualToConstant(*width).setActive(true);
                view
            }
            Node::Height(height, child) => {
                let view = self.node(child);
                view.heightAnchor().constraintEqualToConstant(*height).setActive(true);
                view
            }
            Node::Menu { action, child } => {
                let holder = NxMenuView::new(mtm, *action);
                pin(&holder, &self.node(child), 0.);
                Retained::into_super(holder)
            }
            Node::Watch { enter, leave, child } => {
                let holder = NxWatchView::new(mtm, *enter, *leave);
                pin(&holder, &self.node(child), 0.);
                Retained::into_super(holder)
            }
            Node::Tip { tip, child } => {
                let view = self.node(child);
                view.setToolTip(Some(&NSString::from_str(tip)));
                view
            }
            Node::Drag { id, child } => {
                let outer = self.drags.replace(*id);
                let view = self.node(child);
                self.drags = outer;
                view
            }
            Node::Reorder { list, gap, horizontal, children } => {
                let outer = self.reorder.replace(Vec::new());
                let orientation = if *horizontal { NSUserInterfaceLayoutOrientation::Horizontal } else { NSUserInterfaceLayoutOrientation::Vertical };
                let stack = self.stack(children, orientation, *gap);
                stack.setAlignment(if *horizontal { NSLayoutAttribute::CenterY } else { NSLayoutAttribute::CenterX });
                for item in std::mem::replace(&mut self.reorder, outer).unwrap_or_default() {
                    *item.ivars().reorder.borrow_mut() = Some((*list, objc2::rc::Weak::from_retained(&stack)));
                }
                LISTS.with_borrow_mut(|lists| {
                    lists.retain(|(_, stack, _)| stack.load().is_some());
                    lists.push((*list, objc2::rc::Weak::from_retained(&stack), Arranged::default()));
                });
                Retained::into_super(stack)
            }
            Node::Split { sidebar_width, height, sidebar, content } => {
                let outer = self.width;
                let side = NSVisualEffectView::new(mtm);
                side.setMaterial(NSVisualEffectMaterial::Sidebar);
                side.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
                self.width = *sidebar_width;
                let list = self.node(sidebar);
                side.addSubview(&list);
                list.leadingAnchor().constraintEqualToAnchor(&side.leadingAnchor()).setActive(true);
                list.trailingAnchor().constraintEqualToAnchor(&side.trailingAnchor()).setActive(true);
                list.topAnchor().constraintEqualToAnchor(&side.topAnchor()).setActive(true);
                fixed(&side, *sidebar_width, *height);
                self.width = outer - sidebar_width;
                let page = self.scroll(content, *height, true);
                self.width = outer;
                let split = NSView::new(mtm);
                for view in [Retained::into_super(side), page.clone()] {
                    view.setTranslatesAutoresizingMaskIntoConstraints(false);
                    split.addSubview(&view);
                    view.topAnchor().constraintEqualToAnchor(&split.topAnchor()).setActive(true);
                    view.bottomAnchor().constraintEqualToAnchor(&split.bottomAnchor()).setActive(true);
                }
                let side = &split.subviews().objectAtIndex(0);
                side.leadingAnchor().constraintEqualToAnchor(&split.leadingAnchor()).setActive(true);
                page.leadingAnchor().constraintEqualToAnchor(&side.trailingAnchor()).setActive(true);
                page.trailingAnchor().constraintEqualToAnchor(&split.trailingAnchor()).setActive(true);
                fixed(&split, outer, *height);
                split
            }
            Node::Field { field, value, placeholder, submit, secure, blur } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let input = if *secure { Retained::into_super(NSSecureTextField::new(mtm)) } else { NSTextField::new(mtm) };
                    input.setStringValue(&NSString::from_str(&value.0));
                    self.wire_field(&input, *field, *blur);
                    input
                });
                input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                self.submits.insert(*field, *submit);
                self.fields.insert(*field, input.clone());
                self.first.get_or_insert_with(|| input.clone());
                input.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                Retained::into_super(Retained::into_super(input))
            }
            Node::Progress(fraction) => {
                let bar = NSProgressIndicator::new(mtm);
                bar.setStyle(NSProgressIndicatorStyle::Bar);
                bar.setIndeterminate(false);
                bar.setControlSize(NSControlSize::Small);
                bar.setMaxValue(1.);
                bar.setDoubleValue(*fraction);
                bar.heightAnchor().constraintEqualToConstant(4.).setActive(true);
                Retained::into_super(bar)
            }
            Node::Separator => {
                let line = NSBox::new(mtm);
                line.setBoxType(NSBoxType::Separator);
                Retained::into_super(line)
            }
            Node::Item { height, click, drag, children } => {
                let outer = std::mem::take(&mut self.swaps);
                let item = self.item(*click, drag.clone());
                self.width -= 12.;
                let row = self.node(&Node::Row { gap: 8., children: children.clone() });
                self.width += 12.;
                pin(&item, &row, 6.);
                item.heightAnchor().constraintEqualToConstant(*height).setActive(true);
                for (view, on_hover) in &self.swaps {
                    view.setHidden(*on_hover);
                }
                *item.ivars().swaps.borrow_mut() = std::mem::replace(&mut self.swaps, outer);
                Retained::into_super(item)
            }
            Node::Faded(child) => {
                let view = self.node(child);
                view.setAlphaValue(0.4);
                view
            }
            Node::Hover(on_hover, child) => {
                let view = self.node(child);
                self.swaps.push((view.clone(), *on_hover));
                view
            }
            Node::Card { tint, child } => {
                let card = NSBox::new(mtm);
                card.setBoxType(NSBoxType::Custom);
                card.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
                card.setBorderWidth(0.);
                card.setCornerRadius(8.);
                card.setFillColor(&tinted(*tint, 0.12));
                // The box sits behind the child, not around it: as its content view the child sized nothing and the
                // card collapsed, and pinned inside the box it never got clicks (a box hit-tests only its content view).
                let holder = NSView::new(mtm);
                pin(&holder, &card, 0.);
                pin(&holder, &self.node(child), 0.);
                holder
            }
            Node::Round { radius, tint, child } => {
                let inner = self.node(child);
                if let Some(item) = inner.downcast_ref::<NxPopoverItem>() {
                    item.ivars().radius.set(*radius);
                }
                match tint {
                    None => inner,
                    Some(tint) => {
                        let fill = NSBox::new(mtm);
                        fill.setBoxType(NSBoxType::Custom);
                        fill.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
                        fill.setBorderWidth(0.);
                        fill.setCornerRadius(*radius);
                        fill.setFillColor(&tinted(*tint, 0.14));
                        let holder = NSView::new(mtm);
                        pin(&holder, &fill, 0.);
                        pin(&holder, &inner, 0.);
                        holder
                    }
                }
            }
            Node::Ring { radius, child } => {
                let ring = NSBox::new(mtm);
                ring.setBoxType(NSBoxType::Custom);
                ring.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
                ring.setCornerRadius(*radius);
                ring.setBorderWidth(2.);
                ring.setBorderColor(&NSColor::controlAccentColor());
                ring.setFillColor(&tinted(Ink::Secondary, 0.12));
                // Behind the child, as with `Card`.
                let holder = NSView::new(mtm);
                pin(&holder, &ring, 0.);
                pin(&holder, &self.node(child), 0.);
                holder
            }
            Node::Glass { radius, child } => {
                // Behind the child rather than holding it, as with `Card`, so the child sizes the capsule.
                let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), NSRect::ZERO);
                glass.setCornerRadius(*radius);
                let holder = NSView::new(mtm);
                pin(&holder, &glass, 0.);
                pin(&holder, &self.node(child), 0.);
                holder
            }
            Node::Scroll { max, child } => self.scroll(child, *max, false),
        };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view
    }

    /// `child` in a scroll view as tall as it is up to `height`, or exactly `height` when `exact`.
    fn scroll(&mut self, child: &Node, height: f64, exact: bool) -> Retained<NSView> {
        let scroll = NSScrollView::new(self.mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        // Over the rows while scrolling, as in Apple's popovers, rather than a track cutting off their right end.
        scroll.setScrollerStyle(objc2_app_kit::NSScrollerStyle::Overlay);
        let document: Retained<NxFlippedView> = unsafe { msg_send![super(NxFlippedView::alloc(self.mtm).set_ivars(())), initWithFrame: NSRect::ZERO] };
        let inner = self.node(child);
        pin(&document, &inner, 0.);
        document.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.setDocumentView(Some(&document));
        // A fixed width, not the clip view's: that has none until the popover is on screen, which squashed the rows.
        document.widthAnchor().constraintEqualToConstant(self.width).setActive(true);
        document.layoutSubtreeIfNeeded();
        let height = if exact { height } else { document.fittingSize().height.min(height) };
        scroll.heightAnchor().constraintEqualToConstant(height).setActive(true);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        Retained::into_super(scroll)
    }

    /// Reports `input`'s edits and runs its `submits` action on Return, and on leaving it when `blur`.
    fn wire_field(&self, input: &NSTextField, field: u32, blur: bool) {
        input.setTag(field as isize);
        TARGET.with(|target| unsafe {
            input.setDelegate(Some(ProtocolObject::from_ref(&**target)));
            input.setTarget(Some(target));
            input.setAction(Some(sel!(submit:)));
        });
        if let Some(cell) = input.cell() {
            cell.setSendsActionOnEndEditing(blur);
            // One scrolling line: wrapping, it grew taller once clicked into.
            cell.setWraps(false);
            cell.setScrollable(true);
            cell.setUsesSingleLineMode(true);
        }
    }

    /// A clickable item view, which a `Reorder` being built lets drag.
    fn item(&mut self, click: Option<u32>, drag: Option<PathBuf>) -> Retained<NxPopoverItem> {
        let item = NxPopoverItem::new(self.mtm, click, drag);
        item.ivars().drags.set(self.drags);
        if let Some(items) = &mut self.reorder {
            items.push(item.clone());
        }
        item
    }

    fn stack(&mut self, children: &[Node], orientation: NSUserInterfaceLayoutOrientation, gap: f64) -> Retained<NSStackView> {
        let views = children.iter().map(|child| self.node(child)).collect::<Vec<_>>();
        let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), self.mtm);
        stack.setOrientation(orientation);
        stack.setSpacing(gap);
        // Not the default gravity areas, which never stretch a child: the lowest-hugging one (a `Spacer`) takes the slack.
        stack.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
        // Hidden `Hover` children give up their space; only in rows, as a column pins each child's width to itself.
        stack.setDetachesHiddenViews(orientation == NSUserInterfaceLayoutOrientation::Horizontal);
        stack
    }
}

/// Pins `child` inside `parent`, inset by `inset` left and right.
fn pin(parent: &NSView, child: &NSView, inset: f64) {
    parent.addSubview(child);
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    child.leadingAnchor().constraintEqualToAnchor_constant(&parent.leadingAnchor(), inset).setActive(true);
    child.trailingAnchor().constraintEqualToAnchor_constant(&parent.trailingAnchor(), -inset).setActive(true);
    child.topAnchor().constraintEqualToAnchor(&parent.topAnchor()).setActive(true);
    child.bottomAnchor().constraintEqualToAnchor(&parent.bottomAnchor()).setActive(true);
}

fn fixed(view: &NSView, width: f64, height: f64) {
    view.widthAnchor().constraintEqualToConstant(width).setActive(true);
    view.heightAnchor().constraintEqualToConstant(height).setActive(true);
}

fn font(size: f64, weight: Weight) -> Retained<NSFont> {
    let weight = unsafe {
        match weight {
            Weight::Regular => NSFontWeightRegular,
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
            Weight::Mono => return NSFont::monospacedSystemFontOfSize_weight(size, NSFontWeightRegular),
        }
    };
    NSFont::systemFontOfSize_weight(size, weight)
}

fn color(ink: Ink) -> Retained<NSColor> {
    match ink {
        Ink::Primary => NSColor::labelColor(),
        Ink::Secondary => NSColor::secondaryLabelColor(),
        Ink::Tertiary => NSColor::tertiaryLabelColor(),
        Ink::Accent => NSColor::controlAccentColor(),
        Ink::Danger => NSColor::systemRedColor(),
    }
}

/// `ink` at `alpha`, resolved per appearance at draw time: `colorWithAlphaComponent` on a system color freezes it
/// in the appearance current when called, which on a dark system left light-theme cards white on white.
fn tinted(ink: Ink, alpha: f64) -> Retained<NSColor> {
    let provider = RcBlock::new(move |appearance: std::ptr::NonNull<NSAppearance>| {
        let resolved = RefCell::new(None);
        unsafe { appearance.as_ref() }.performAsCurrentDrawingAppearance(&RcBlock::new(|| *resolved.borrow_mut() = Some(color(ink).colorWithAlphaComponent(alpha))));
        let resolved = resolved.into_inner().expect("performAsCurrentDrawingAppearance runs its block");
        std::ptr::NonNull::new(Retained::autorelease_ptr(resolved)).expect("a color")
    });
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

fn symbol(name: &str, size: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, unsafe { NSFontWeightRegular });
    image.imageWithSymbolConfiguration(&config)
}
