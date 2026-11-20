//! A terminal tab's screen: an AppKit view that draws what the terminal hands it (cell backgrounds, text runs,
//! images) and hands keys, the mouse, drops and copy/paste back. The engine places it like a page.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use browser_engine::EngineEvent;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, AllocAnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSBezierPath, NSBitmapFormat, NSBitmapImageRep, NSColor, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName, NSFontDescriptor,
    NSFontFamilyAttribute, NSFontManager, NSFontTraitMask, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSResponder, NSStrikethroughStyleAttributeName, NSAttributedStringNSStringDrawing, NSTextInputClient, NSTrackingArea,
    NSTrackingAreaOptions, NSUnderlineStyle, NSUnderlineStyleAttributeName, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSNotFound, NSNumber, NSPoint, NSRange,
    NSRect, NSSize, NSString, NSUInteger, NSURL, NSObject,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// ⌘.
    pub platform: bool,
    pub function: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// A key press: `key` names it (`a`, `enter`, `left`, `f5`), unshifted; `key_char` is what it types, if anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keystroke {
    pub key: String,
    pub key_char: Option<String>,
    pub modifiers: Modifiers,
}

/// What happened on the screen. Positions are in the view (top-left origin, points).
#[derive(Debug)]
pub enum TermInput {
    Key(Keystroke),
    Down { button: MouseButton, x: f64, y: f64, clicks: isize, modifiers: Modifiers },
    /// The pointer moved with `button` held, which goes on past the view's edges.
    Drag { button: MouseButton, x: f64, y: f64, modifiers: Modifiers },
    Up { button: MouseButton, x: f64, y: f64, modifiers: Modifiers },
    /// The pointer moved over the view with no button held.
    Move { x: f64, y: f64, modifiers: Modifiers },
    /// Positive `dy` scrolls toward older output; in points when `precise` (a trackpad), else in lines.
    Scroll { dy: f64, precise: bool, x: f64, y: f64, modifiers: Modifiers },
    /// Files dropped on the screen.
    Drop(Vec<PathBuf>),
    Copy,
    Paste,
    /// The view's size in points, and the screen's backing scale.
    Resize { width: f64, height: f64, scale: f64 },
}

/// The terminal behind a screen.
pub trait TermHost {
    /// Handles `input`; false if it went unused (a right press then opens the window's terminal menu, an unused
    /// key goes on up the responder chain).
    fn input(&self, input: TermInput) -> bool;
    /// What to draw now; asked only when AppKit draws, so a flood of output costs one paint per display frame.
    fn paint(&self) -> Option<TermPaint>;
}

/// An image a program showed (kitty graphics), kept as a bitmap so each paint just draws it.
pub struct TermImage(Retained<NSBitmapImageRep>);

impl TermImage {
    /// From `width` × `height` RGBA pixels, not premultiplied.
    pub fn new(width: u32, height: u32, rgba: &[u8]) -> Option<Self> {
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                width as isize,
                height as isize,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                NSBitmapFormat::AlphaNonpremultiplied,
                width as isize * 4,
                32,
            )
        }?;
        let data = rep.bitmapData();
        if data.is_null() || rgba.len() != width as usize * height as usize * 4 {
            return None;
        }
        unsafe { std::ptr::copy_nonoverlapping(rgba.as_ptr(), data, rgba.len()) };
        Some(Self(rep))
    }
}

/// A run of text on one row, starting at its cell's top-left.
pub struct TermSpan {
    pub x: f64,
    pub y: f64,
    pub text: String,
    /// 0 regular, 1 bold, 2 italic, 3 bold italic.
    pub font: usize,
    pub color: [f64; 4],
    /// 0 none, 1 straight, 2 curly (drawn dotted).
    pub underline: u8,
    pub strike: bool,
}

/// One paint of the screen, in view points (top-left origin); colours are sRGB with alpha.
pub struct TermPaint {
    pub font: String,
    pub size: f64,
    /// Row height; text is centred in it.
    pub line_h: f64,
    pub background: [f64; 4],
    /// Cell backgrounds, selection and cursor bars: (x, y, width, height, colour).
    pub rects: Vec<(f64, f64, f64, f64, [f64; 4])>,
    pub spans: Vec<TermSpan>,
    pub images: Vec<(f64, f64, f64, f64, std::rc::Rc<TermImage>)>,
}

/// A font family and size, and its regular, bold, italic and bold italic faces.
type Fonts = (String, f64, [Retained<NSFont>; 4]);

pub(crate) struct TermIvars {
    host: Box<dyn TermHost>,
    /// The tab it belongs to, for the window's terminal menu.
    tab_id: RefCell<String>,
    fonts: RefCell<Option<Fonts>>,
    /// Text an input method is composing (dead keys, CJK); typed once it commits.
    marked: RefCell<String>,
    held: Cell<Option<MouseButton>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NxTermView"]
    #[ivars = TermIvars]
    pub(crate) struct NxTermView;

    impl NxTermView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(paint) = self.ivars().host.paint() {
                self.draw(&paint);
            }
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.report_size();
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {
            self.report_size();
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            self.report_size();
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            let modifiers = modifiers(flags);
            let special = special_key(event.keyCode(), modifiers);
            // Chords and named keys go straight to the program; plain typing goes through the input method so
            // dead keys and CJK compose, unless it is mid-composition.
            if self.ivars().marked.borrow().is_empty() && (modifiers.control || modifiers.platform || modifiers.alt || special.is_some()) {
                let key = special.map(str::to_string).or_else(|| event.charactersByApplyingModifiers(NSEventModifierFlags::empty()).map(|key| key.to_string().to_lowercase()));
                let key_char = event.characters().map(|text| text.to_string()).filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
                let used = key.is_some_and(|key| self.ivars().host.input(TermInput::Key(Keystroke { key, key_char, modifiers })));
                if !used {
                    let _: () = unsafe { msg_send![super(self), keyDown: event] };
                }
                return;
            }
            self.interpretKeyEvents(&NSArray::from_slice(&[event]));
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.press(event, MouseButton::Left);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            if !self.press(event, MouseButton::Right) {
                self.held(None);
                self.open_menu(event);
            }
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.press(event, MouseButton::Middle);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.drag(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.drag(event);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.drag(event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.release(event);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.release(event);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.release(event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let (x, y) = self.local(event);
            self.ivars().host.input(TermInput::Move { x, y, modifiers: modifiers(event.modifierFlags()) });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (x, y) = self.local(event);
            let (dy, precise) = (event.scrollingDeltaY(), event.hasPreciseScrollingDeltas());
            self.ivars().host.input(TermInput::Scroll { dy, precise, x, y, modifiers: modifiers(event.modifierFlags()) });
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.ivars().host.input(TermInput::Copy);
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.ivars().host.input(TermInput::Paste);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> Bool {
            let urls = unsafe { info.draggingPasteboard().readObjectsForClasses_options(&NSArray::from_slice(&[NSURL::class()]), None) };
            let paths = urls.map(|urls| urls.iter().filter_map(|url| url.downcast::<NSURL>().ok()?.path()).map(|path| PathBuf::from(path.to_string())).collect::<Vec<_>>()).unwrap_or_default();
            if paths.is_empty() {
                return Bool::NO;
            }
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            Bool::new(self.ivars().host.input(TermInput::Drop(paths)))
        }
    }

    unsafe impl NSObjectProtocol for NxTermView {}

    unsafe impl NSTextInputClient for NxTermView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insertText_replacementRange(&self, string: &AnyObject, _range: NSRange) {
            self.ivars().marked.borrow_mut().clear();
            let text = text_of(string);
            if !text.is_empty() {
                self.ivars().host.input(TermInput::Key(Keystroke { key: text.clone(), key_char: Some(text), modifiers: Modifiers::default() }));
            }
        }

        // Named keys never get here (see `keyDown:`); whatever else an input method asks for is ignored, unheard.
        #[unsafe(method(doCommandBySelector:))]
        unsafe fn doCommandBySelector(&self, _selector: Sel) {}

        // shortcut: the composing text isn't drawn at the cursor, only typed once it commits; draw it if CJK input needs it.
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn setMarkedText_selectedRange_replacementRange(&self, string: &AnyObject, _selected: NSRange, _replacement: NSRange) {
            *self.ivars().marked.borrow_mut() = text_of(string);
        }

        #[unsafe(method(unmarkText))]
        fn unmarkText(&self) {
            self.ivars().marked.borrow_mut().clear();
        }

        #[unsafe(method(selectedRange))]
        fn selectedRange(&self) -> NSRange {
            NSRange::new(NSNotFound as NSUInteger, 0)
        }

        #[unsafe(method(markedRange))]
        fn markedRange(&self) -> NSRange {
            match self.ivars().marked.borrow().encode_utf16().count() {
                0 => NSRange::new(NSNotFound as NSUInteger, 0),
                len => NSRange::new(0, len),
            }
        }

        #[unsafe(method(hasMarkedText))]
        fn hasMarkedText(&self) -> bool {
            !self.ivars().marked.borrow().is_empty()
        }

        #[unsafe(method(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributedSubstringForProposedRange_actualRange(&self, _range: NSRange, _actual: *mut NSRange) -> *mut NSAttributedString {
            std::ptr::null_mut()
        }

        #[unsafe(method(validAttributesForMarkedText))]
        fn validAttributesForMarkedText(&self) -> *mut NSArray<NSAttributedStringKey> {
            Retained::autorelease_return(NSArray::new())
        }

        // shortcut: candidate windows open at the screen's corner rather than the cursor; pass the cursor cell if it matters.
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn firstRectForCharacterRange_actualRange(&self, _range: NSRange, _actual: *mut NSRange) -> NSRect {
            match self.window() {
                Some(window) => window.convertRectToScreen(self.convertRect_toView(self.bounds(), None)),
                None => NSRect::ZERO,
            }
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn characterIndexForPoint(&self, _point: NSPoint) -> NSUInteger {
            NSNotFound as NSUInteger
        }
    }
);

/// A terminal's screen view, placed by `MacEngine::set_screen`.
pub struct TermSurface(Retained<NxTermView>);

impl TermSurface {
    pub fn new(host: Box<dyn TermHost>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let ivars = TermIvars { host, tab_id: RefCell::default(), fonts: RefCell::default(), marked: RefCell::default(), held: Cell::new(None) };
        let view: Retained<NxTermView> = unsafe { msg_send![super(NxTermView::alloc(mtm).set_ivars(ivars)), initWithFrame: NSRect::ZERO] };
        let options = NSTrackingAreaOptions::MouseMoved | NSTrackingAreaOptions::ActiveInKeyWindow | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe { NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(&view), None) };
        view.addTrackingArea(&area);
        view.registerForDraggedTypes(&NSArray::from_slice(&[ns_string("public.file-url").as_ref()]));
        Some(Self(view))
    }

    /// Draws it again soon, with what the host paints then.
    pub fn redraw(&self) {
        self.0.setNeedsDisplay(true);
    }

    pub(crate) fn view(&self, tab_id: &str) -> Retained<NSView> {
        if *self.0.ivars().tab_id.borrow() != tab_id {
            *self.0.ivars().tab_id.borrow_mut() = tab_id.to_string();
        }
        Retained::into_super(self.0.clone())
    }
}

/// The cell size for `font` at `size` points: the advance of `m` wide, `size × line_height` tall (rounded).
pub fn cell_size(font: &str, size: f64, line_height: f64) -> (f64, f64) {
    let width = MainThreadMarker::new()
        .map(|mtm| {
            let font = family_font(font, size, mtm);
            let attributes = text_attributes(&font, [0., 0., 0., 1.], 0, false);
            unsafe { NSAttributedString::new_with_attributes(&NSString::from_str("m"), &attributes) }.size().width
        })
        .filter(|width| *width > 1.)
        .unwrap_or(size * 0.6);
    (width, (size * line_height).round())
}

/// The plain text on the pasteboard; `Err(true)` when it holds something else (an image), `Err(false)` when empty.
pub fn clipboard_text() -> Result<String, bool> {
    let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
    match pasteboard.stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString }) {
        Some(text) => Ok(text.to_string()),
        None => Err(pasteboard.types().is_some_and(|types| types.count() > 0)),
    }
}

pub fn set_clipboard_text(text: &str) {
    let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { objc2_app_kit::NSPasteboardTypeString });
}

impl NxTermView {
    /// A press: keys come here, then the host; false if the host didn't use it.
    fn press(&self, event: &NSEvent, button: MouseButton) -> bool {
        if let Some(window) = self.window() {
            window.makeFirstResponder(Some(self));
        }
        self.held(Some(button));
        let (x, y) = self.local(event);
        self.ivars().host.input(TermInput::Down { button, x, y, clicks: event.clickCount(), modifiers: modifiers(event.modifierFlags()) })
    }

    fn drag(&self, event: &NSEvent) {
        if let Some(button) = self.ivars().held.get() {
            let (x, y) = self.local(event);
            self.ivars().host.input(TermInput::Drag { button, x, y, modifiers: modifiers(event.modifierFlags()) });
        }
    }

    fn release(&self, event: &NSEvent) {
        if let Some(button) = self.ivars().held.take() {
            let (x, y) = self.local(event);
            self.ivars().host.input(TermInput::Up { button, x, y, modifiers: modifiers(event.modifierFlags()) });
        }
    }

    fn held(&self, button: Option<MouseButton>) {
        self.ivars().held.set(button);
    }

    fn local(&self, event: &NSEvent) -> (f64, f64) {
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        (point.x, point.y)
    }

    fn report_size(&self) {
        let size = self.bounds().size;
        let scale = self.window().map_or(2., |window| window.backingScaleFactor());
        if size.width > 0. && size.height > 0. {
            self.ivars().host.input(TermInput::Resize { width: size.width, height: size.height, scale });
        }
    }

    fn open_menu(&self, event: &NSEvent) {
        let Some(window) = self.window() else {
            return;
        };
        let Some(content) = window.contentView() else {
            return;
        };
        let local = content.convertPoint_fromView(event.locationInWindow(), None);
        let y = if content.isFlipped() { local.y } else { content.bounds().size.height - local.y };
        let tab_id = self.ivars().tab_id.borrow().clone();
        crate::webkit::send_event(window.windowNumber(), EngineEvent::TerminalMenu { tab_id, x: local.x, y });
    }

    fn draw(&self, paint: &TermPaint) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let fill = |x: f64, y: f64, w: f64, h: f64, color: [f64; 4]| {
            ns_color(color).setFill();
            NSBezierPath::fillRect(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
        };
        let bounds = self.bounds();
        fill(0., 0., bounds.size.width, bounds.size.height, paint.background);
        for &(x, y, w, h, color) in &paint.rects {
            fill(x, y, w, h, color);
        }
        for (x, y, w, h, image) in &paint.images {
            let rect = NSRect::new(NSPoint::new(*x, *y), NSSize::new(*w, *h));
            unsafe { image.0.drawInRect_fromRect_operation_fraction_respectFlipped_hints(rect, NSRect::ZERO, NSCompositingOperation::SourceOver, 1., true, None) };
        }
        let mut fonts = self.ivars().fonts.borrow_mut();
        if fonts.as_ref().is_none_or(|(family, size, _)| *family != paint.font || *size != paint.size) {
            let regular = family_font(&paint.font, paint.size, mtm);
            let manager = NSFontManager::sharedFontManager(mtm);
            let bold = manager.convertFont_toHaveTrait(&regular, NSFontTraitMask::BoldFontMask);
            let italic = manager.convertFont_toHaveTrait(&regular, NSFontTraitMask::ItalicFontMask);
            let both = manager.convertFont_toHaveTrait(&bold, NSFontTraitMask::ItalicFontMask);
            *fonts = Some((paint.font.clone(), paint.size, [regular, bold, italic, both]));
        }
        let Some((_, _, fonts)) = fonts.as_ref() else {
            return;
        };
        for span in &paint.spans {
            let font = &fonts[span.font.min(3)];
            let text_h = font.ascender() - font.descender() + font.leading();
            let attributes = text_attributes(font, span.color, span.underline, span.strike);
            let text = unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(&span.text), &attributes) };
            text.drawAtPoint(NSPoint::new(span.x, span.y + ((paint.line_h - text_h) / 2.).round()));
        }
    }
}

fn family_font(family: &str, size: f64, mtm: MainThreadMarker) -> Retained<NSFont> {
    let _ = mtm;
    let attributes = NSDictionary::from_slices(&[unsafe { NSFontFamilyAttribute }], &[NSString::from_str(family).as_ref() as &AnyObject]);
    let descriptor = unsafe { NSFontDescriptor::fontDescriptorWithFontAttributes(Some(&attributes)) };
    NSFont::fontWithDescriptor_size(&descriptor, size).unwrap_or_else(|| NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular }))
}

fn text_attributes(font: &NSFont, color: [f64; 4], underline: u8, strike: bool) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let mut keys: Vec<&NSAttributedStringKey> = unsafe { vec![NSFontAttributeName, NSForegroundColorAttributeName] };
    let color = ns_color(color);
    let mut values: Vec<Retained<AnyObject>> = vec![Retained::into_super(Retained::into_super(font.retain())), Retained::into_super(Retained::into_super(color))];
    if underline > 0 {
        let style = if underline == 2 { NSUnderlineStyle::Single.0 | NSUnderlineStyle::PatternDot.0 } else { NSUnderlineStyle::Single.0 };
        keys.push(unsafe { NSUnderlineStyleAttributeName });
        values.push(Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_isize(style)))));
    }
    if strike {
        keys.push(unsafe { NSStrikethroughStyleAttributeName });
        values.push(Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_isize(NSUnderlineStyle::Single.0)))));
    }
    let values = values.iter().map(|value| &**value).collect::<Vec<_>>();
    NSDictionary::from_slices(&keys, &values)
}

fn ns_color([red, green, blue, alpha]: [f64; 4]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha)
}

fn ns_string(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// The text of an `NSString` or `NSAttributedString` an input method hands over.
fn text_of(string: &AnyObject) -> String {
    if let Some(attributed) = string.downcast_ref::<NSAttributedString>() {
        return attributed.string().to_string();
    }
    string.downcast_ref::<NSString>().map(|text| text.to_string()).unwrap_or_default()
}

fn modifiers(flags: NSEventModifierFlags) -> Modifiers {
    Modifiers {
        shift: flags.contains(NSEventModifierFlags::Shift),
        control: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        platform: flags.contains(NSEventModifierFlags::Command),
        // AppKit sets it on arrows and other navigation keys too, which would read as Fn+arrow.
        function: false,
    }
}

/// The name of a key that types no text of its own; space only as part of a chord.
fn special_key(code: u16, modifiers: Modifiers) -> Option<&'static str> {
    Some(match code {
        36 | 76 => "enter",
        48 => "tab",
        49 if modifiers.control || modifiers.alt => "space",
        51 => "backspace",
        53 => "escape",
        114 => "insert",
        117 => "delete",
        115 => "home",
        119 => "end",
        116 => "pageup",
        121 => "pagedown",
        123 => "left",
        124 => "right",
        125 => "down",
        126 => "up",
        122 => "f1",
        120 => "f2",
        99 => "f3",
        118 => "f4",
        96 => "f5",
        97 => "f6",
        98 => "f7",
        100 => "f8",
        101 => "f9",
        109 => "f10",
        103 => "f11",
        111 => "f12",
        105 => "f13",
        107 => "f14",
        113 => "f15",
        106 => "f16",
        64 => "f17",
        79 => "f18",
        80 => "f19",
        90 => "f20",
        _ => return None,
    })
}
