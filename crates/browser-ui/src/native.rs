//! Building blocks for the native popovers: `Node`s the engine turns into AppKit views, and the handlers they run.

use crate::settings::MenuAction;
use crate::window::BrowserWindow;
pub(crate) use browser_engine_macos::{Initial, Ink, Key, Node, Style, Weight, Wrap};
use gpui::Context;

/// Action ids from here up belong to the open sheet; below, to the open popover.
pub(crate) const SHEET_BASE: u32 = 1 << 20;

// Text field ids, unique across popovers and sheets (the extensions popover's is `extensions::EXT_FIELD`).
pub(crate) const HISTORY_QUERY: u32 = 1;
pub(crate) const BOOKMARK_TITLE: u32 = 2;
pub(crate) const BOOKMARK_URL: u32 = 3;
pub(crate) const PROFILE_NAME: u32 = 4;
pub(crate) const LOGIN_SITE: u32 = 5;
pub(crate) const LOGIN_USER: u32 = 6;
pub(crate) const LOGIN_PASSWORD: u32 = 7;
pub(crate) const LOGIN_NOTE: u32 = 8;
pub(crate) const DIALOG_INPUT: u32 = 9;

/// Collects a popover's or sheet's handlers while its content is built; a control carries the id `act` returns.
#[derive(Default)]
pub(crate) struct Pop {
    pub(crate) actions: Vec<MenuAction>,
    base: u32,
}

impl Pop {
    pub(crate) fn sheet() -> Self {
        Self { actions: Vec::new(), base: SHEET_BASE }
    }

    pub(crate) fn act(&mut self, run: impl Fn(&mut BrowserWindow, &mut Context<BrowserWindow>) + 'static) -> u32 {
        self.actions.push(Box::new(run));
        self.base + (self.actions.len() - 1) as u32
    }
}

/// An app asset (an SVG icon) for the life of the app; each is loaded once.
pub(crate) fn asset(path: &str) -> Option<&'static [u8]> {
    static CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, &'static [u8]>>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let cache = cache.get_or_insert_with(Default::default);
    if let Some(bytes) = cache.get(path) {
        return Some(bytes);
    }
    let bytes: &'static [u8] = match gpui::AssetSource::load(&crate::Assets, path).ok()?? {
        std::borrow::Cow::Borrowed(bytes) => bytes,
        std::borrow::Cow::Owned(bytes) => Box::leak(bytes.into_boxed_slice()),
    };
    cache.insert(path.to_string(), bytes);
    Some(bytes)
}

/// One line of text that keeps its width.
pub(crate) fn text(text: impl Into<String>, size: f64, weight: Weight, ink: Ink) -> Node {
    Node::Text { text: text.into(), size, weight, ink, wrap: Wrap::Line, shrink: false }
}

/// One line of text that truncates first when the row is short of room.
pub(crate) fn title(text: impl Into<String>, size: f64, weight: Weight, ink: Ink) -> Node {
    Node::Text { text: text.into(), size, weight, ink, wrap: Wrap::Line, shrink: true }
}

/// The 36pt header: a bold title, then `actions` on the right.
pub(crate) fn header(name: &str, actions: Vec<Node>) -> Node {
    let mut children = vec![title(name, 13., Weight::Semibold, Ink::Primary), Node::Spacer];
    children.extend(actions);
    Node::Column { gap: 0., pad: (6., 0.), children: vec![Node::Item { height: 36., click: None, drag: None, children: vec![Node::Row { gap: 2., children }] }] }
}

/// A centred, muted message for an empty list, with an optional symbol above it.
pub(crate) fn empty(symbol: Option<&'static str>, message: &str) -> Node {
    let mut children = Vec::new();
    if let Some(name) = symbol {
        children.push(Node::Glyph { name, size: 18., ink: Ink::Secondary });
    }
    children.push(Node::Text { text: message.into(), size: 12., weight: Weight::Regular, ink: Ink::Secondary, wrap: Wrap::Center, shrink: false });
    Node::Column { gap: 6., pad: (24., 20.), children }
}

/// A muted section label above a group of rows.
pub(crate) fn section(label: &str) -> Node {
    Node::Item { height: 24., click: None, drag: None, children: vec![text(label, 11., Weight::Medium, Ink::Secondary)] }
}

/// Text that wraps, ragged right.
pub(crate) fn para(text: impl Into<String>, size: f64, ink: Ink) -> Node {
    Node::Text { text: text.into(), size, weight: Weight::Regular, ink, wrap: Wrap::Left, shrink: false }
}

pub(crate) fn button(title: &str, action: u32, style: Style, key: Key) -> Node {
    Node::Button { title: title.into(), action, style, key }
}

/// A text field that starts out showing `value`.
pub(crate) fn field(field: u32, value: impl Into<String>, placeholder: &str, submit: u32) -> Node {
    Node::Field { field, value: Initial(value.into()), placeholder: placeholder.into(), submit, secure: false }
}

/// A dialog sheet: a bold title, `body`, then `buttons` along the bottom right.
pub(crate) fn sheet(name: impl Into<String>, body: Vec<Node>, buttons: Vec<Node>) -> Node {
    let mut children = vec![text(name, 15., Weight::Semibold, Ink::Primary)];
    children.extend(body);
    let mut row = vec![Node::Spacer];
    row.extend(buttons);
    children.push(Node::Column { gap: 0., pad: (0., 6.), children: vec![Node::Row { gap: 10., children: row }] });
    Node::Column { gap: 10., pad: (20., 20.), children }
}

/// A small muted label above a field.
pub(crate) fn label(text_: &str) -> Node {
    text(text_, 12., Weight::Regular, Ink::Secondary)
}

pub(crate) fn rgb(color: gpui::Hsla) -> [f64; 3] {
    let rgba = gpui::Rgba::from(color);
    [f64::from(rgba.r), f64::from(rgba.g), f64::from(rgba.b)]
}
