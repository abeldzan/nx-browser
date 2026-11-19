//! Native popovers (`NSPopover`) and sheets that the UI describes as a `Node` tree; on macOS 26 they get Liquid Glass.
//! Controls report back to the window as `EngineEvent::Popover*` with the action ids the tree gave them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use browser_engine::{EngineEvent, LogicalRect};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControlSize, NSControlStateValueOff,
    NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSImage, NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLayoutAttribute,
    NSLayoutConstraintOrientation, NSLineBreakMode, NSPanel, NSPopover, NSPopoverBehavior, NSPopoverDelegate,
    NSProgressIndicator, NSProgressIndicatorStyle, NSResponder, NSScrollView, NSSecureTextField, NSStackView, NSSwitch,
    NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTrackingArea, NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation,
    NSView, NSViewController, NSWindow, NSWindowStyleMask,
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
    Button { title: String, action: u32, style: Style, key: Key },
    Link { title: String, action: u32 },
    /// A borderless SF Symbol button; `on` tints it with the accent.
    Tool { symbol: &'static str, tip: &'static str, action: u32, on: bool },
    Switch { on: bool, action: u32 },
    /// A text field showing `value` when created; edits arrive as `PopoverText`, Return runs `submit`.
    Field { field: u32, value: Initial, placeholder: String, submit: u32, secure: bool },
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
        }
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
    close_sheet();
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

pub(crate) fn close_sheet() {
    if let Some(Surface { host: Host::Sheet(panel, parent), .. }) = SHEET.take() {
        parent.endSheet(&panel);
        panel.orderOut(None);
    }
}

/// Replaces the text in field `field` of the open popover or sheet.
pub(crate) fn set_field(field: u32, text: &str) {
    for slot in [&SHEET, &POPOVER] {
        slot.with_borrow(|surface| {
            if let Some(input) = surface.as_ref().and_then(|surface| surface.fields.get(&field)) {
                input.setStringValue(&NSString::from_str(text));
            }
        });
    }
}

/// Field `field`'s Return action, in whichever surface has it.
fn submit_action(field: u32) -> Option<u32> {
    [&SHEET, &POPOVER].into_iter().find_map(|slot| slot.with_borrow(|surface| surface.as_ref().and_then(|surface| surface.submits.get(&field).copied())))
}

fn send(event: EngineEvent) {
    let window_no = [&SHEET, &POPOVER].into_iter().find_map(|slot| slot.with_borrow(|surface| surface.as_ref().map(|surface| surface.window_no)));
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

    unsafe impl NSControlTextEditingDelegate for NxPopoverTarget {}

    unsafe impl NSTextFieldDelegate for NxPopoverTarget {}

    impl NxPopoverTarget {
        #[unsafe(method(act:))]
        fn act(&self, sender: &NSView) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Ok(action) = u32::try_from(tag) {
                send(EngineEvent::PopoverAction(action));
            }
        }

        #[unsafe(method(submit:))]
        fn submit(&self, sender: &NSTextField) {
            let field = sender.tag() as u32;
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
            Node::Button { title, action, style, key } => {
                let button = TARGET.with(|target| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(sel!(act:)), mtm) });
                button.setTag(*action as isize);
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
            Node::Field { field, value, placeholder, submit, secure } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let input = if *secure { Retained::into_super(NSSecureTextField::new(mtm)) } else { NSTextField::new(mtm) };
                    input.setStringValue(&NSString::from_str(&value.0));
                    input.setTag(*field as isize);
                    TARGET.with(|target| unsafe {
                        input.setDelegate(Some(ProtocolObject::from_ref(&**target)));
                        input.setTarget(Some(target));
                        input.setAction(Some(sel!(submit:)));
                    });
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
                card.setContentViewMargins(NSSize::ZERO);
                card.setContentView(Some(&self.node(child)));
                Retained::into_super(card)
            }
            Node::Scroll { max, child } => {
                let scroll = NSScrollView::new(mtm);
                scroll.setDrawsBackground(false);
                scroll.setHasVerticalScroller(true);
                scroll.setAutohidesScrollers(true);
                let document: Retained<NxFlippedView> = unsafe { msg_send![super(NxFlippedView::alloc(mtm).set_ivars(())), initWithFrame: NSRect::ZERO] };
                let inner = self.node(child);
                pin(&document, &inner, 0.);
                document.setTranslatesAutoresizingMaskIntoConstraints(false);
                scroll.setDocumentView(Some(&document));
                // A fixed width, not the clip view's: that has none until the popover is on screen, which squashed the rows.
                document.widthAnchor().constraintEqualToConstant(self.width).setActive(true);
                document.layoutSubtreeIfNeeded();
                let height = document.fittingSize().height.min(*max);
                scroll.heightAnchor().constraintEqualToConstant(height).setActive(true);
                Retained::into_super(scroll)
            }
        };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view
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
