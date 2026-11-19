//! The tools at the title bar's right end: pinned extensions, find, history, bookmarks and extensions, in one
//! Liquid Glass capsule over the GPUI view.

use crate::native::{text, Ink, Key, Node, Pop, Style, Weight};
use crate::window::{BrowserWindow, Popover};
use browser_engine::LogicalRect;
use gpui::{px, Context, Window};

const BUTTON: f64 = 28.;
const HEIGHT: f64 = 26.;

impl BrowserWindow {
    pub(crate) fn sync_toolbar(&mut self, pop: &mut Pop, bars: &mut Vec<&'static str>, window: &Window, cx: &mut Context<Self>) {
        let mut tools = Vec::new();
        if self.store_page_id().is_some() {
            let add = pop.act(|this, cx| {
                if let Some(id) = this.store_page_id() {
                    this.install_from_store(id);
                }
                cx.notify();
            });
            tools.push(Node::Button { title: "Add to NxBrowser".into(), action: add, style: Style::Primary, key: Key::None, enabled: true });
        }
        let (store, tab) = (self.active_store(), self.active_tab_id());
        let pinned = self.model.lock().extensions.iter().filter(|extension| extension.enabled && extension.pinned).cloned().collect::<Vec<_>>();
        for extension in pinned {
            let id = extension.id.clone();
            let icon = self.ext_meta.get(&id).and_then(|meta| meta.icon.clone());
            let badge = self.engine.extensions().badge(&id, &store, tab.as_deref());
            let open = pop.act(move |this, cx| {
                let x = this.engine.pointer().map_or(0., |(x, _)| x as f32);
                this.open_extension(&id, x, cx);
            });
            let item = button(open, crate::extensions::extension_image(&extension.name, icon, 16.));
            tools.push(if badge.is_empty() { item } else { Node::Overlay { child: Box::new(item), corner: Box::new(text(badge, 8., Weight::Semibold, Ink::Accent)) } });
        }
        if self.model.detached.is_some() {
            // Torn-off windows: the visible way back, besides dropping the title bar on the main one.
            let merge = pop.act(|this, cx| {
                this.merge_requested = true;
                cx.notify();
            });
            tools.push(button(merge, glyph("rectangle.portrait.and.arrow.right")));
        }
        if matches!(crate::update::status(cx), crate::update::Status::Ready { .. }) {
            let restart = pop.act(|_, cx| crate::update::restart(cx));
            tools.push(Node::Button { title: "Restart to Update".into(), action: restart, style: Style::Primary, key: Key::None, enabled: true });
        }
        let find = pop.act(|this, cx| {
            if this.find_here() {
                this.close_find();
            } else {
                this.open_find();
            }
            cx.notify();
        });
        tools.push(selected(self.find_here(), button(find, glyph("magnifyingglass"))));
        if !self.tab_window {
            let windows = pop.act(|this, _| this.open_windows_menu());
            tools.push(button(windows, glyph("square.on.square")));
        }
        let mut anchors = Vec::new();
        for (kind, icon, on) in [
            (Popover::History, glyph("clock"), self.popover == Some(Popover::History) || self.history_modal),
            (Popover::Bookmarks, glyph("bookmark"), self.popover == Some(Popover::Bookmarks)),
            (Popover::Extensions, Node::Svg { data: crate::native::asset(crate::PUZZLE_ICON).unwrap_or_default(), size: 14., ink: Ink::Secondary }, self.popover == Some(Popover::Extensions)),
        ] {
            let toggle = pop.act(move |this, cx| this.toggle_popover(kind, cx));
            anchors.push((toggle, kind));
            tools.push(selected(on, button(toggle, icon)));
        }
        let content = Node::Column { gap: 0., pad: (4., (HEIGHT - 22.) / 2.), children: vec![Node::Row { gap: 2., children: tools }] };
        // The dock's right end in the main window; a torn-off window's title bar's right end.
        let (right, top, open) = match self.dock_at(window) {
            Some((right, top, open)) => (right, top + (crate::window::DOCK_H - HEIGHT) / 2., open - top),
            None => (f64::from(window.viewport_size().width) - 8., (f64::from(self.titlebar_h()) - HEIGHT) / 2., 0.),
        };
        self.toolbar_w = self.engine.set_bar("toolbar", LogicalRect { x: right, y: top, width: 0., height: HEIGHT }, Some(HEIGHT / 2.), &content);
        bars.push("toolbar");
        for (action, frame) in self.engine.bar_items("toolbar") {
            if let Some((_, kind)) = anchors.iter().find(|(toggle, _)| *toggle == action) {
                // Where the button is once the dock is up, which it is while what it opened is.
                let y = frame.y + open;
                let bounds = gpui::Bounds::new(gpui::point(px(frame.x as f32), px(y as f32)), gpui::size(px(frame.width as f32), px(frame.height as f32)));
                self.popover_anchors.insert(*kind, bounds);
            }
        }
    }
}

impl BrowserWindow {
    /// This space's windows, as Safari's tab overview button lists tabs: picking one brings it forward (back from
    /// minimized too); then tiling on or off.
    fn open_windows_menu(&mut self) {
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let mut rows = Vec::new();
        let mut seen = Vec::new();
        for tab in tabs.iter().filter(|tab| self.is_front(&tab.id)) {
            let group = self.group_id(&tab.id);
            if seen.contains(&group) {
                continue;
            }
            seen.push(group);
            let minimized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.minimized);
            let label = crate::tabs::tab_label(&tab.title, &tab.url);
            let label = if minimized { format!("{label} (Minimized)") } else { label };
            let id = tab.id.clone();
            rows.push(crate::settings::menu_item(label, true, move |this, cx| this.focus_window(&id, cx)));
        }
        rows.push(None);
        let session = self.model.lock().active_session_id.clone();
        let label = if self.free_space() { "Tile Windows" } else { "Float Windows" };
        rows.push(crate::settings::menu_item(label, true, move |this, cx| this.toggle_free(&session, cx)));
        let (x, y) = self.engine.pointer().unwrap_or_default();
        self.show_menu(rows, gpui::point(px(x as f32), px(y as f32)));
    }
}

fn glyph(name: &'static str) -> Node {
    Node::Glyph { name, size: 13., ink: Ink::Secondary }
}

/// A square tool button showing `icon`.
fn button(action: u32, icon: Node) -> Node {
    Node::Width(BUTTON, Box::new(Node::Round { radius: HEIGHT / 2. - 2., tint: None, child: Box::new(Node::Item { height: 22., click: Some(action), drag: None, children: vec![Node::Center(Box::new(icon))] }) }))
}

/// `tool`, marked with an accent wash while what it opens is open.
fn selected(on: bool, tool: Node) -> Node {
    // Concentric with the 26pt capsule, 2pt in, as the window strips' tabs are.
    let radius = HEIGHT / 2. - 2.;
    Node::Round { radius, tint: on.then_some(Ink::Accent), child: Box::new(tool) }
}
