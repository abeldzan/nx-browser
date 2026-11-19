//! Building blocks for the native popovers: `Node`s the engine turns into AppKit views, and the handlers they run.

use crate::settings::MenuAction;
use crate::window::BrowserWindow;
pub(crate) use browser_engine_macos::{Ink, Node, Weight};
use gpui::Context;

/// Collects a popover's handlers while its content is built; a control carries the id `act` returns.
#[derive(Default)]
pub(crate) struct Pop {
    pub(crate) actions: Vec<MenuAction>,
}

impl Pop {
    pub(crate) fn act(&mut self, run: impl Fn(&mut BrowserWindow, &mut Context<BrowserWindow>) + 'static) -> u32 {
        self.actions.push(Box::new(run));
        (self.actions.len() - 1) as u32
    }
}

/// One line of text that keeps its width.
pub(crate) fn text(text: impl Into<String>, size: f64, weight: Weight, ink: Ink) -> Node {
    Node::Text { text: text.into(), size, weight, ink, wrap: None, shrink: false }
}

/// One line of text that truncates first when the row is short of room.
pub(crate) fn title(text: impl Into<String>, size: f64, weight: Weight, ink: Ink) -> Node {
    Node::Text { text: text.into(), size, weight, ink, wrap: None, shrink: true }
}

/// The 36pt header: a bold title, then `actions` on the right.
pub(crate) fn header(name: &str, actions: Vec<Node>) -> Node {
    let mut children = vec![title(name, 13., Weight::Semibold, Ink::Primary), Node::Spacer];
    children.extend(actions);
    Node::Column { gap: 0., pad: (6., 0.), children: vec![Node::Item { height: 36., click: None, drag: None, children: vec![Node::Row { gap: 2., children }] }] }
}

/// A centred, muted message for an empty list, with an optional symbol above it.
pub(crate) fn empty(symbol: Option<&'static str>, message: &str, width: f64) -> Node {
    let mut children = Vec::new();
    if let Some(name) = symbol {
        children.push(Node::Glyph { name, size: 18., ink: Ink::Secondary });
    }
    children.push(Node::Text { text: message.into(), size: 12., weight: Weight::Regular, ink: Ink::Secondary, wrap: Some(width - 48.), shrink: false });
    Node::Column { gap: 6., pad: (24., 20.), children }
}

/// A muted section label above a group of rows.
pub(crate) fn section(label: &str) -> Node {
    Node::Item { height: 24., click: None, drag: None, children: vec![text(label, 11., Weight::Medium, Ink::Secondary)] }
}

pub(crate) fn rgb(color: gpui::Hsla) -> [f64; 3] {
    let rgba = gpui::Rgba::from(color);
    [f64::from(rgba.r), f64::from(rgba.g), f64::from(rgba.b)]
}
