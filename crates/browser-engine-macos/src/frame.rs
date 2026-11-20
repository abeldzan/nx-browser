//! A browser window's frame inside the app window, stacked just above its page and below the GPUI view: the
//! border, the title strip holding native controls, and the rims that resize it. Presses on the strip's empty
//! space and on the rims, and the pointer coming over the window, become `EngineEvent::WindowGesture`.

use std::cell::{Cell, RefCell};

use browser_engine::{EngineEvent, LogicalRect, Phase};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSCursor, NSCursorFrameResizeDirections, NSCursorFrameResizePosition, NSEvent, NSEventModifierFlags,
    NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectView,
};
use objc2_quartz_core::CACornerMask;
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize};

/// How a window frame is drawn and where; colours are sRGB with alpha.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameStyle {
    /// The window's own rect, in the content's coordinates (top-left origin).
    pub shell: LogicalRect,
    /// How far the resize rims reach outside the shell; 0 when it can't be resized (maximized).
    pub grip: f64,
    pub title_h: f64,
    pub radius: f64,
    pub body: [f64; 4],
    pub line: [f64; 4],
    pub ring: [f64; 4],
    pub accent: [f64; 4],
    /// The loading bar along the strip's foot: how far along, and how opaque.
    pub load: (f64, f64),
    pub alpha: f64,
    /// The focused window. The others look inactive as macOS windows do: their strip dimmed, its material inactive.
    pub active: bool,
}

/// How far the rims reach in from the shell's edge.
const GRIP_IN: f64 = 3.;
/// Corners take presses this far along each edge from the rim's outer corner.
const GRIP_CORNER: f64 = 12.;

pub(crate) struct FrameIvars {
    key: String,
    window_no: isize,
    style: RefCell<FrameStyle>,
    /// The strip's backing: the sidebar material, as the app's own sidebar has.
    material: Retained<NSVisualEffectView>,
    /// The view holding the strip's controls.
    strip: Retained<NSView>,
    /// The loading bar along the strip's foot.
    load: Retained<NSView>,
    /// The 1pt ring along the shell's edge and the body-coloured one just inside it: layer borders, not a drawn
    /// bitmap the size of the window, so a resize costs no redraw.
    ring: Retained<NSView>,
    edge: Retained<NSView>,
    /// The edges of the press under way (0: a move), if any.
    press: Cell<Option<u8>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxFrameView"]
    #[ivars = FrameIvars]
    pub(crate) struct NxFrameView;

    impl NxFrameView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The strip's controls take their own presses; its empty space and the rims are this view's; the page
        /// area passes through to the page (or the GPUI terminal) beneath.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            let Some(hit) = hit else {
                return std::ptr::null_mut();
            };
            if crate::popover::interactive(&hit, self) {
                return Retained::autorelease_ptr(hit);
            }
            let local = match unsafe { self.superview() } {
                Some(parent) => self.convertPoint_fromView(point, Some(&parent)),
                None => point,
            };
            let style = self.ivars().style.borrow();
            let shell = self.shell_rect(&style);
            let in_strip = local.y >= shell.origin.y && local.y < shell.origin.y + style.title_h && local.x >= shell.origin.x && local.x < shell.origin.x + shell.size.width;
            if self.edges_at(local, &style) != 0 || in_strip {
                return Retained::autorelease_ptr(self.retain()).cast();
            }
            std::ptr::null_mut()
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let local = self.convertPoint_fromView(event.locationInWindow(), None);
            let edges = self.edges_at(local, &self.ivars().style.borrow());
            self.ivars().press.set(Some(edges));
            self.report(event, Phase::Begin, edges);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if let Some(edges) = self.ivars().press.get() {
                self.report(event, Phase::Move, edges);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            if let Some(edges) = self.ivars().press.take() {
                self.report(event, Phase::End, edges);
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            self.report(event, Phase::Enter, 0);
        }

        #[unsafe(method(resetCursorRects))]
        // The deprecated cursors are the fallback for macOS 14, which lacks their replacement.
        #[allow(deprecated)]
        fn reset_cursor_rects(&self) {
            let style = self.ivars().style.borrow();
            if style.grip <= 0. {
                return;
            }
            let bounds = self.bounds();
            let (w, h) = (bounds.size.width, bounds.size.height);
            let band = style.grip + GRIP_IN;
            let corner = style.grip + GRIP_CORNER;
            // Frame resize cursors arrived in macOS 15; before that, the plain sideways and up-down ones.
            let framed = NSCursor::class().responds_to(sel!(frameResizeCursorFromPosition:inDirections:));
            let cursor = |position: NSCursorFrameResizePosition| match position {
                _ if framed => NSCursor::frameResizeCursorFromPosition_inDirections(position, NSCursorFrameResizeDirections::All),
                NSCursorFrameResizePosition::Top | NSCursorFrameResizePosition::Bottom => NSCursor::resizeUpDownCursor(),
                _ => NSCursor::resizeLeftRightCursor(),
            };
            let rect = |x: f64, y: f64, width: f64, height: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
            for (area, position) in [
                (rect(0., corner, band, h - 2. * corner), NSCursorFrameResizePosition::Left),
                (rect(w - band, corner, band, h - 2. * corner), NSCursorFrameResizePosition::Right),
                (rect(corner, 0., w - 2. * corner, band), NSCursorFrameResizePosition::Top),
                (rect(corner, h - band, w - 2. * corner, band), NSCursorFrameResizePosition::Bottom),
                (rect(0., 0., corner, corner), NSCursorFrameResizePosition::TopLeft),
                (rect(w - corner, 0., corner, corner), NSCursorFrameResizePosition::TopRight),
                (rect(0., h - corner, corner, corner), NSCursorFrameResizePosition::BottomLeft),
                (rect(w - corner, h - corner, corner, corner), NSCursorFrameResizePosition::BottomRight),
            ] {
                self.addCursorRect_cursor(area, &cursor(position));
            }
        }
    }
);

define_class!(
    /// Holds native chrome over the GPUI view, passing presses on anything but its controls through to it.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxPassView"]
    pub(crate) struct NxPassView;

    impl NxPassView {
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            match hit {
                Some(hit) if crate::popover::interactive(&hit, self) => Retained::autorelease_ptr(hit),
                _ => std::ptr::null_mut(),
            }
        }
    }
);

impl NxPassView {
    pub(crate) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(())), initWithFrame: NSRect::ZERO] }
    }
}

impl NxFrameView {
    pub(crate) fn new(mtm: MainThreadMarker, key: &str, window_no: isize, style: FrameStyle) -> Retained<Self> {
        let material = NSVisualEffectView::new(mtm);
        material.setMaterial(NSVisualEffectMaterial::Sidebar);
        material.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        material.setWantsLayer(true);
        if let Some(layer) = material.layer() {
            // Rounded along the top only, nesting in the window's corners; the page meets its foot.
            layer.setMaskedCorners(CACornerMask::LayerMinXMaxYCorner | CACornerMask::LayerMaxXMaxYCorner);
        }
        let strip = NSView::new(mtm);
        let load = NSView::new(mtm);
        load.setWantsLayer(true);
        let [ring, edge] = [(); 2].map(|_| {
            let view = NSView::new(mtm);
            view.setWantsLayer(true);
            if let Some(layer) = view.layer() {
                layer.setBorderWidth(1.);
            }
            view
        });
        let ivars = FrameIvars {
            key: key.to_string(),
            window_no,
            style: RefCell::new(style.clone()),
            material: material.clone(),
            strip: strip.clone(),
            load: load.clone(),
            ring: ring.clone(),
            edge: edge.clone(),
            press: Cell::new(None),
        };
        let this: Retained<Self> = unsafe { msg_send![super(Self::alloc(mtm).set_ivars(ivars)), initWithFrame: NSRect::ZERO] };
        this.addSubview(&ring);
        this.addSubview(&edge);
        this.addSubview(&material);
        this.addSubview(&strip);
        this.addSubview(&load);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited | NSTrackingAreaOptions::ActiveInKeyWindow | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&this), None) };
        this.addTrackingArea(&area);
        this.paint_rings(&style);
        this
    }

    /// The style it was last given.
    pub(crate) fn style(&self) -> FrameStyle {
        self.ivars().style.borrow().clone()
    }

    /// The view the strip's content goes in.
    pub(crate) fn strip(&self) -> &NSView {
        &self.ivars().strip
    }

    /// Moves and restyles the frame for `style`; `height` is the content view's, to flip `shell` into it.
    pub(crate) fn apply(&self, style: FrameStyle, height: f64) {
        let grip = style.grip;
        let shell = &style.shell;
        let frame = NSRect::new(NSPoint::new(shell.x - grip, height - shell.y - shell.height - grip), NSSize::new(shell.width + 2. * grip, shell.height + 2. * grip));
        let strip = NSRect::new(NSPoint::new(grip, grip), NSSize::new(shell.width, style.title_h));
        // A move alone redraws nothing: the layer just goes along.
        let resized = {
            let old = self.ivars().style.borrow();
            let at = |style: &FrameStyle| LogicalRect { x: 0., y: 0., ..style.shell.clone() };
            self.frame().size != frame.size || FrameStyle { shell: at(&old), ..old.clone() } != FrameStyle { shell: at(&style), ..style.clone() }
        };
        if self.frame() != frame {
            self.setFrame(frame);
        }
        if self.ivars().strip.frame() != strip {
            self.ivars().strip.setFrame(strip);
            // 1pt in from the top and sides, leaving the window's ring drawn beneath it showing.
            self.ivars().material.setFrame(NSRect::new(NSPoint::new(grip + 1., grip + 1.), NSSize::new((shell.width - 2.).max(0.), (style.title_h - 1.).max(0.))));
        }
        if let Some(layer) = self.ivars().material.layer().filter(|layer| layer.cornerRadius() != style.radius - 1.) {
            layer.setCornerRadius(style.radius - 1.);
        }
        let (along, opacity) = style.load;
        let load = NSRect::new(NSPoint::new(grip + 1., grip + style.title_h - 2.), NSSize::new(((shell.width - 2.) * along.min(1.)).max(0.), 2.));
        if self.ivars().load.frame() != load {
            self.ivars().load.setFrame(load);
        }
        if let Some(layer) = self.ivars().load.layer() {
            let [red, green, blue, alpha] = style.accent;
            layer.setBackgroundColor(Some(&CGColor::new_srgb(red, green, blue, alpha * opacity)));
        }
        let dim = if style.active { 1. } else { 0.55 };
        if self.ivars().strip.alphaValue() != dim {
            self.ivars().strip.setAlphaValue(dim);
        }
        if self.alphaValue() != style.alpha {
            self.setAlphaValue(style.alpha);
        }
        let moved = self.ivars().style.borrow().shell != style.shell;
        if moved || resized {
            *self.ivars().style.borrow_mut() = style;
            if let Some(window) = self.window() {
                window.invalidateCursorRectsForView(self);
            }
        }
        if resized {
            self.paint_rings(&self.ivars().style.borrow());
        }
    }

    fn paint_rings(&self, style: &FrameStyle) {
        let shell = self.shell_rect(style);
        // The page sits 1pt in (and the GPUI terminal screen over it): only that edge is body, not the page area.
        for (view, rect, radius, color) in [(&self.ivars().ring, shell, style.radius, style.ring), (&self.ivars().edge, inset(shell, 1.), style.radius - 1., style.body)] {
            view.setFrame(rect);
            if let Some(layer) = view.layer() {
                let [red, green, blue, alpha] = color;
                layer.setCornerRadius(radius.max(0.));
                layer.setBorderColor(Some(&CGColor::new_srgb(red, green, blue, alpha)));
            }
        }
    }

    fn shell_rect(&self, style: &FrameStyle) -> NSRect {
        NSRect::new(NSPoint::new(style.grip, style.grip), NSSize::new(style.shell.width, style.shell.height))
    }

    /// The rim edges under `local` (this view's coordinates): 1 left, 2 right, 4 top, 8 bottom; 0 off the rims.
    fn edges_at(&self, local: NSPoint, style: &FrameStyle) -> u8 {
        if style.grip <= 0. {
            return 0;
        }
        let size = self.bounds().size;
        let (x, y, w, h) = (local.x, local.y, size.width, size.height);
        if x < 0. || y < 0. || x > w || y > h {
            return 0;
        }
        let (band, corner) = (style.grip + GRIP_IN, style.grip + GRIP_CORNER);
        let (mut left, mut right, mut top, mut bottom) = (x < band, x > w - band, y < band, y > h - band);
        if top || bottom {
            left |= x < corner;
            right |= x > w - corner;
        }
        if left || right {
            top |= y < corner;
            bottom |= y > h - corner;
        }
        u8::from(left) | u8::from(right) << 1 | u8::from(top) << 2 | u8::from(bottom) << 3
    }

    /// Whether a press on it is under way: it still gets the rest of that drag, so it must not be dropped yet.
    pub(crate) fn pressed(&self) -> bool {
        self.ivars().press.get().is_some()
    }

    fn report(&self, event: &NSEvent, phase: Phase, edges: u8) {
        // A window being carried off loses its frame from the stage mid-drag; the event still knows its window.
        let Some(content) = self.window().or_else(|| event.window(self.mtm())).and_then(|window| window.contentView()) else {
            return;
        };
        let local = content.convertPoint_fromView(event.locationInWindow(), None);
        let (x, y) = (local.x, if content.isFlipped() { local.y } else { content.bounds().size.height - local.y });
        let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
        let key = self.ivars().key.clone();
        crate::webkit::send_event(self.ivars().window_no, EngineEvent::WindowGesture { key, phase, x, y, edges, shift });
    }
}

fn inset(rect: NSRect, by: f64) -> NSRect {
    NSRect::new(NSPoint::new(rect.origin.x + by, rect.origin.y + by), NSSize::new(rect.size.width - 2. * by, rect.size.height - 2. * by))
}
