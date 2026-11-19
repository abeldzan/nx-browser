//! Native popovers (`NSPopover`) and sheets that the UI describes as a `Node` tree; on macOS 26 they get Liquid Glass.
//! Controls report back to the window as `EngineEvent::Popover*` with the action ids the tree gave them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use browser_engine::{EngineEvent, LogicalRect};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControlSize, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSImage, NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLineBreakMode, NSPanel, NSPopover, NSPopoverBehavior, NSPopoverDelegate,
    NSProgressIndicator, NSProgressIndicatorStyle, NSResponder, NSScrollView, NSSecureTextField, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTrackingArea, NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation,
    NSView, NSViewController, NSWindow, NSWindowStyleMask, NSComboBox, NSPopUpButton, NSStepper, NSVisualEffectView,
    NSVisualEffectMaterial, NSVisualEffectBlendingMode, NSWindowDelegate, NSApplication, NSControl, NSEventModifierFlags,
    NSFocusRingType, NSGlassEffectView, NSTextView, NSWindowOrderingMode, NSAnimationContext, NSAnimatablePropertyContainer,
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
    /// The command palette's borderless, larger search field; list keys arrive as `EngineEvent::PopoverKey`.
    Search { field: u32, placeholder: String },
    /// A text field with a list to pick from (`NSComboBox`); a pick or Return runs `submit`, as does leaving it.
    Combo { field: u32, value: Initial, items: Vec<String>, submit: u32 },
    /// A pop-up menu of `items` showing `selected`; picking item `i` runs action `first + i`.
    Popup { items: Vec<String>, selected: usize, first: u32 },
    /// `value` beside up/down arrows: down runs `down`, up runs `down + 1`.
    Stepper { value: String, down: u32 },
    /// `child` exactly `width` wide; text inside wraps at that width.
    Width(f64, Box<Node>),
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
    Right,
}

enum Host {
    Popover(Retained<NSPopover>),
    /// The sheet, and the window it hangs from.
    Sheet(Retained<NSPanel>, Retained<NSWindow>),
    Window(Retained<NSWindow>),
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
        let (view, _) = root(mtm, content, surface.width, &mut surface.submits, &mut surface.fields);
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
    let (view, _) = root(mtm, content, width, &mut submits, &mut fields);
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
    let (view, first) = root(mtm, content, width, &mut submits, &mut fields);
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
    let (view, _) = root(mtm, content, width, &mut submits, &mut fields);
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
    }
}

/// Shows `content` on a floating glass panel `width` wide at `place` over `parent`, or updates the one of this kind
/// open under `key`. A toast fades in when `animate`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn present_float(kind: Float, window_no: isize, parent: &NSWindow, key: u64, width: f64, place: Place, animate: bool, content: &Node) {
    let slot = float_slot(kind);
    if refresh(slot, window_no, key, content) {
        return;
    }
    close_float(kind, None, false);
    let mtm = MainThreadMarker::new().expect("panels live on the main thread");
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let (view, first) = root(mtm, content, width, &mut submits, &mut fields);
    let size = view.fittingSize();
    let size = NSSize::new(size.width + 2. * GLASS_MARGIN, size.height + 2. * GLASS_MARGIN);
    let outer = parent.frame();
    let centred = outer.origin.x + (outer.size.width - size.width) / 2.;
    let origin = match place {
        Place::Top { x, y } => NSPoint::new(
            x.map_or(centred, |x| outer.origin.x + x - GLASS_MARGIN),
            outer.origin.y + outer.size.height - y - size.height + GLASS_MARGIN,
        ),
        Place::Bottom(above) => NSPoint::new(centred, outer.origin.y + above - GLASS_MARGIN),
    };
    let panel: Retained<NxPanel> = unsafe {
        let panel = NxPanel::alloc(mtm).set_ivars(PanelIvars { takes_key: kind == Float::Palette });
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
    PANEL.with_borrow(|surface| {
        surface.as_ref().filter(|surface| matches!(&surface.host, Host::Panel(panel, _, _) if std::ptr::eq(Retained::as_ptr(panel).cast::<NSWindow>(), window))).map(|surface| surface.window_no)
    })
}

/// Replaces the text in field `field` of the open popover, sheet, window or panel, caret at the end.
pub(crate) fn set_field(field: u32, text: &str) {
    for slot in [&PANEL, &WINDOW, &SHEET, &POPOVER] {
        slot.with_borrow(|surface| {
            if let Some(input) = surface.as_ref().and_then(|surface| surface.fields.get(&field)) {
                input.setStringValue(&NSString::from_str(text));
                if let Some(editor) = input.currentEditor() {
                    editor.setSelectedRange(NSRange::new(input.stringValue().length(), 0));
                }
            }
        });
    }
}

/// Field `field`'s Return action, in whichever surface has it.
fn submit_action(field: u32) -> Option<u32> {
    [&PANEL, &LIST, &WINDOW, &SHEET, &POPOVER].into_iter().find_map(|slot| peek(slot, |surface| surface.submits.get(&field).copied()))
}

/// `read` of the surface in `slot`, skipping one being rebuilt: moving its focused field ends editing, which calls
/// back in here while `refresh` holds the slot.
fn peek<T>(slot: &'static std::thread::LocalKey<RefCell<Option<Surface>>>, read: impl FnOnce(&Surface) -> Option<T>) -> Option<T> {
    slot.with(|cell| cell.try_borrow().ok().and_then(|surface| surface.as_ref().and_then(read)))
}

fn send(event: EngineEvent) {
    let window_no = [&PANEL, &LIST, &WINDOW, &SHEET, &POPOVER].into_iter().find_map(|slot| peek(slot, |surface| Some(surface.window_no)));
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
        /// The palette's list keys: arrows, Return, Tab, Esc, and → or ⌫ at the field's ends. Other fields keep theirs.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, control: &NSControl, text_view: &NSTextView, command: Sel) -> Bool {
            if peek(&PANEL, |surface| surface.fields.values().any(|field| std::ptr::eq(Retained::as_ptr(field).cast::<NSControl>(), control)).then_some(())).is_none() {
                return Bool::NO;
            }
            let length = text_view.string().length();
            let at_end = text_view.selectedRange() == NSRange::new(length, 0);
            let command_held = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Command);
            let key = match command.name().to_str().unwrap_or_default() {
                "moveUp:" => "up",
                "moveDown:" => "down",
                "insertNewline:" if command_held => "cmd-enter",
                "insertNewline:" => "enter",
                // ⌘↩ arrives as no command at all.
                "noop:" if command_held && NSApplication::sharedApplication(self.mtm()).currentEvent().is_some_and(|event| event.keyCode() == 36) => "cmd-enter",
                "insertTab:" => "tab",
                "cancelOperation:" => "escape",
                "moveRight:" if at_end => "right",
                "deleteBackward:" if length == 0 => "backspace",
                _ => return Bool::NO,
            };
            send(EngineEvent::PopoverKey(key.into()));
            Bool::YES
        }
    }

    unsafe impl NSTextFieldDelegate for NxPopoverTarget {}

    impl NxPopoverTarget {
        #[unsafe(method(act:))]
        fn act(&self, sender: &NSView) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Ok(action) = u32::try_from(tag) {
                send(EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(picked:))]
        fn picked(&self, sender: &NSPopUpButton) {
            if let Some(action) = sender.selectedItem().and_then(|item| u32::try_from(item.tag()).ok()) {
                send(EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(stepped:))]
        fn stepped(&self, sender: &NSStepper) {
            // Parked at 1 between 0 and 2, so the new value says which arrow was pressed.
            let up = sender.integerValue() > 1;
            sender.setIntegerValue(1);
            if let Ok(down) = u32::try_from(sender.tag()) {
                send(EngineEvent::PopoverAction(down + u32::from(up)));
            }
        }

        #[unsafe(method(submit:))]
        fn submit(&self, sender: &NSTextField) {
            let field = sender.tag() as u32;
            // A combo box pick changes the text without a text-change notification.
            send(EngineEvent::PopoverText { field, text: sender.stringValue().to_string() });
            if let Some(action) = submit_action(field) {
                send(EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, notification: &NSNotification) {
            let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok()) else {
                return;
            };
            let text = field.stringValue().to_string();
            send(EngineEvent::PopoverText { field: field.tag() as u32, text });
        }
    }
);

struct ItemIvars {
    click: Cell<Option<u32>>,
    drag: RefCell<Option<PathBuf>>,
    /// `Hover` children: (view, shown while hovered).
    swaps: RefCell<Vec<(Retained<NSView>, bool)>>,
    press: Cell<Option<NSPoint>>,
    hovered: Cell<bool>,
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
            let (Some(start), Some(path)) = (self.ivars().press.get(), self.ivars().drag.borrow().clone()) else {
                return;
            };
            let at = event.locationInWindow();
            if (at.x - start.x).abs() + (at.y - start.y).abs() > 4. {
                self.ivars().press.set(None);
                crate::drag::drag_file(&path);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if self.ivars().press.take().is_some() {
                if let Some(action) = self.ivars().click.get() {
                    send(EngineEvent::PopoverAction(action));
                }
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().hovered.get() && self.ivars().click.get().is_some() {
                NSColor::quaternarySystemFillColor().setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 6., 6.).fill();
            }
        }
    }
);

impl NxPopoverItem {
    fn new(mtm: MainThreadMarker, click: Option<u32>, drag: Option<PathBuf>) -> Retained<Self> {
        let ivars = ItemIvars { click: Cell::new(click), drag: RefCell::new(drag), swaps: RefCell::default(), press: Cell::new(None), hovered: Cell::new(false) };
        let this: Retained<Self> = unsafe { msg_send![super(Self::alloc(mtm).set_ivars(ivars)), initWithFrame: NSRect::ZERO] };
        let options = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&this), None) };
        this.addTrackingArea(&area);
        this
    }

    fn set_hovered(&self, hovered: bool) {
        self.ivars().hovered.set(hovered);
        for (view, on_hover) in self.ivars().swaps.borrow().iter() {
            view.setHidden(*on_hover != hovered);
        }
        self.setNeedsDisplay(true);
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
    submits: &mut HashMap<u32, u32>,
    fields: &mut HashMap<u32, Retained<NSTextField>>,
) -> (Retained<NSView>, Option<Retained<NSTextField>>) {
    let mut kept = std::mem::take(fields);
    submits.clear();
    let mut build = Build { mtm, submits, fields, kept: &mut kept, swaps: Vec::new(), width, first: None };
    let view = build.node(content);
    view.widthAnchor().constraintEqualToConstant(width).setActive(true);
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
                Retained::into_super(stack)
            }
            Node::Spacer => {
                let view = NSView::new(mtm);
                view.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                view.setContentCompressionResistancePriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
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
                let item = NxPopoverItem::new(mtm, *action, None);
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
            Node::Search { field, placeholder } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let input = NSTextField::new(mtm);
                    input.setBordered(false);
                    input.setDrawsBackground(false);
                    input.setFocusRingType(NSFocusRingType::None);
                    input.setFont(Some(&font(15., Weight::Regular)));
                    self.wire_field(&input, *field, false);
                    input
                });
                input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                self.fields.insert(*field, input.clone());
                self.first.get_or_insert_with(|| input.clone());
                input.setContentHuggingPriority_forOrientation(1., NSLayoutConstraintOrientation::Horizontal);
                Retained::into_super(Retained::into_super(input))
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
                let item = NxPopoverItem::new(mtm, *click, drag.clone());
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
                card.setFillColor(&color(*tint).colorWithAlphaComponent(0.12));
                // Pinned to the box itself: as its content view the child sized nothing, and the card collapsed.
                pin(&card, &self.node(child), 0.);
                Retained::into_super(card)
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

fn symbol(name: &str, size: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, unsafe { NSFontWeightRegular });
    image.imageWithSymbolConfiguration(&config)
}
