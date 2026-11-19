//! Native popovers (`NSPopover`, Liquid Glass on macOS 26) that the UI describes as a `Node` tree.
//! Controls report back to the window as `EngineEvent::Popover*` with the action ids the tree gave them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use browser_engine::{EngineEvent, LogicalRect};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSControlSize, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSImage,
    NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLayoutAttribute, NSLayoutConstraintOrientation, NSLineBreakMode,
    NSPopover, NSPopoverBehavior, NSPopoverDelegate, NSProgressIndicator, NSProgressIndicatorStyle, NSResponder,
    NSScrollView, NSStackView, NSSwitch, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTrackingArea, NSTrackingAreaOptions,
    NSUserInterfaceLayoutOrientation, NSView, NSViewController,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSize, NSString};

/// One piece of a popover. Sizes are points; `action`s and `field`s are the UI's ids for its handlers.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Stacked top to bottom, each child full width, `gap` apart and inset by `pad` (horizontal, vertical).
    Column { gap: f64, pad: (f64, f64), children: Vec<Node> },
    /// Side by side and vertically centred; a `Spacer` takes the slack.
    Row { gap: f64, children: Vec<Node> },
    Spacer,
    /// One line, truncated at the tail when `shrink` (before its neighbours); `wrap` instead wraps it at that width, centred.
    Text { text: String, size: f64, weight: Weight, ink: Ink, wrap: Option<f64>, shrink: bool },
    /// A file's image (a favicon), or a white letter on `rgb` when there is none or it doesn't load.
    Image { path: Option<PathBuf>, letter: String, rgb: [f64; 3], size: f64, radius: f64 },
    /// An SF Symbol.
    Glyph { name: &'static str, size: f64, ink: Ink },
    Button { title: String, action: u32, primary: bool },
    Link { title: String, action: u32 },
    /// A borderless SF Symbol button; `on` tints it with the accent.
    Tool { symbol: &'static str, tip: &'static str, action: u32, on: bool },
    Switch { on: bool, action: u32 },
    /// A text field; edits arrive as `PopoverText`, Return runs `submit`. It starts empty: see `set_field`.
    Field { field: u32, placeholder: String, submit: u32 },
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
    /// Scrolls `child` once it is taller than `max`.
    Scroll { max: f64, child: Box<Node> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Primary,
    Secondary,
    Tertiary,
    Accent,
    Danger,
}

/// Which side of the anchor the popover opens on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    Below,
    Right,
}

struct Shown {
    popover: Retained<NSPopover>,
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
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    /// The last popover to close, by key, and when.
    static CLOSED: Cell<Option<(u64, Instant)>> = const { Cell::new(None) };
    static TARGET: Retained<NxPopoverTarget> = {
        let this = NxPopoverTarget::alloc(MainThreadMarker::new().expect("popovers live on the main thread")).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
}

/// Shows `content` anchored to `anchor` (window coordinates, top-left origin) in `content_view`, or updates the
/// popover already showing under `key` in place.
pub(crate) fn present(window_no: isize, content_view: &NSView, key: u64, anchor: &LogicalRect, edge: Edge, width: f64, content: &Node) {
    let mtm = MainThreadMarker::new().expect("popovers live on the main thread");
    let same = SHOWN.with_borrow(|shown| shown.as_ref().map(|shown| (shown.key == key && shown.window_no == window_no, shown.content == *content)));
    match same {
        Some((true, true)) => return,
        Some((true, false)) => {
            SHOWN.with_borrow_mut(|shown| {
                if let Some(shown) = shown {
                    let editing = shown.fields.iter().find(|(_, field)| field.currentEditor().is_some()).map(|(id, _)| *id);
                    let view = root(mtm, content, shown.width, &mut shown.submits, &mut shown.fields);
                    shown.popover.setContentSize(view.fittingSize());
                    if let Some(controller) = shown.popover.contentViewController() {
                        controller.setView(&view);
                    }
                    shown.content = content.clone();
                    if let Some(field) = editing.and_then(|id| shown.fields.get(&id)) {
                        if let Some(window) = field.window() {
                            window.makeFirstResponder(Some(field));
                        }
                    }
                }
            });
            return;
        }
        Some((false, _)) => close(),
        None => {}
    }
    let (mut submits, mut fields) = (HashMap::new(), HashMap::new());
    let view = root(mtm, content, width, &mut submits, &mut fields);
    let controller = NSViewController::new(mtm);
    controller.setView(&view);
    let popover = NSPopover::new(mtm);
    popover.setBehavior(NSPopoverBehavior::Transient);
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
    SHOWN.set(Some(Shown { popover, window_no, key, width, content: content.clone(), submits, fields }));
}

pub(crate) fn close() {
    // Taken first: closing calls the delegate, which looks at `SHOWN`.
    if let Some(shown) = SHOWN.take() {
        CLOSED.set(Some((shown.key, Instant::now())));
        shown.popover.close();
    }
}

pub(crate) fn is_shown() -> bool {
    SHOWN.with_borrow(|shown| shown.is_some())
}

/// Whether the popover under `key` closed a moment ago: the press that closed it may be on its own button.
pub(crate) fn just_closed(key: u64) -> bool {
    CLOSED.get().is_some_and(|(closed, at)| closed == key && at.elapsed() < Duration::from_millis(400))
}

pub(crate) fn set_field(field: u32, text: &str) {
    SHOWN.with_borrow(|shown| {
        if let Some(input) = shown.as_ref().and_then(|shown| shown.fields.get(&field)) {
            input.setStringValue(&NSString::from_str(text));
        }
    });
}

fn send(event: EngineEvent) {
    if let Some(window_no) = SHOWN.with_borrow(|shown| shown.as_ref().map(|shown| shown.window_no)) {
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
            let ours = SHOWN.with_borrow(|shown| {
                shown.as_ref().is_some_and(|shown| notification.object().is_some_and(|object| std::ptr::eq(&*object as *const _ as *const NSPopover, &*shown.popover)))
            });
            if ours {
                let window_no = SHOWN.with_borrow(|shown| shown.as_ref().map(|shown| shown.window_no)).unwrap_or(0);
                if let Some(shown) = SHOWN.take() {
                    CLOSED.set(Some((shown.key, Instant::now())));
                }
                crate::webkit::send_event(window_no, EngineEvent::PopoverClosed);
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
            if let Some(action) = SHOWN.with_borrow(|shown| shown.as_ref().and_then(|shown| shown.submits.get(&field).copied())) {
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
fn root(mtm: MainThreadMarker, content: &Node, width: f64, submits: &mut HashMap<u32, u32>, fields: &mut HashMap<u32, Retained<NSTextField>>) -> Retained<NSView> {
    let mut kept = std::mem::take(fields);
    submits.clear();
    let mut build = Build { mtm, submits, fields, kept: &mut kept, swaps: Vec::new(), width };
    let view = build.node(content);
    view.widthAnchor().constraintEqualToConstant(width).setActive(true);
    view.layoutSubtreeIfNeeded();
    view
}

struct Build<'a> {
    mtm: MainThreadMarker,
    submits: &'a mut HashMap<u32, u32>,
    fields: &'a mut HashMap<u32, Retained<NSTextField>>,
    /// Fields from the previous build, reused by id.
    kept: &'a mut HashMap<u32, Retained<NSTextField>>,
    /// `Hover` views met since the innermost `Item` began.
    swaps: Vec<(Retained<NSView>, bool)>,
    /// The width the node being built gets; a scroll view's document needs it before AppKit lays anything out.
    width: f64,
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
                    Some(width) => {
                        let label = NSTextField::wrappingLabelWithString(&text, mtm);
                        label.setPreferredMaxLayoutWidth(*width);
                        label.setAlignment(NSTextAlignment::Center);
                        label
                    }
                    None => {
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
            Node::Button { title, action, primary } => {
                let button = TARGET.with(|target| unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(sel!(act:)), mtm) });
                button.setTag(*action as isize);
                if *primary {
                    button.setBezelColor(Some(&NSColor::controlAccentColor()));
                }
                Retained::into_super(Retained::into_super(button))
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
            Node::Field { field, placeholder, submit } => {
                let input = self.kept.remove(field).unwrap_or_else(|| {
                    let input = NSTextField::textFieldWithString(&NSString::new(), mtm);
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
