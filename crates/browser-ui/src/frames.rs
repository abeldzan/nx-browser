//! Browser windows' frames and the title bar's spaces, as native views: each window's border and title strip
//! (navigation, tab pills or the address field, tools), the space chips, and the previews shown while dragging.
//! Presses and drags on them come back as engine events that run the window move, resize, snap and tab logic.

use super::*;
use crate::native::{text, title, Initial, Ink, Node, Pop, Weight, ADDRESS_QUERY, SPACE_NAME};
use browser_engine::Phase;
use browser_engine_macos::FrameStyle;
use crate::geom::{point, size, Bounds, Point};

/// A press, drag or hover on native chrome, waiting for the next frame to act on it.
pub(crate) enum NativeInput {
    Gesture { key: String, phase: Phase, at: Point, edges: u8, shift: bool },
    Drag { id: u32, phase: Phase, at: Point },
}

/// What a native drag id stands for.
pub(crate) enum DragItem {
    Tab(String),
    Space(String),
}

/// The title strip's glass capsules: 22pt controls 2pt in, so 26pt tall and fully round.
const CAPSULE_R: f64 = 13.;
/// A dragged tab's pill: as wide as its tab, within these, so even an icon-only tab shows its title.
pub(crate) fn drag_pill_w(slot: f64) -> f64 {
    slot.clamp(140., 180.)
}
/// What sits in a capsule (a tab, a space, a hover highlight, the address field): fully round too, and concentric
/// with the capsule around it.
const PILL_R: f64 = CAPSULE_R - 2.;
/// Tab pills narrower than this show only their icon: the close and reload slots, gaps and padding need it all.
const FULL_PILL: f64 = 96.;
/// The narrowest a tab pill gets: its icon, padded.
const MIN_PILL: f64 = 28.;

/// A glass capsule of `buttons` 28pt strip tools, 2pt apart and 2pt in (`group`).
fn capsule_w(buttons: usize) -> f64 {
    if buttons == 0 { 0. } else { 30. * buttons as f64 + 2. }
}

/// `controls` on a small glass capsule, as a macOS 26 toolbar groups its buttons.
fn group(controls: Vec<Node>) -> Node {
    Node::Glass { radius: CAPSULE_R, child: Box::new(Node::Column { gap: 0., pad: (2., 2.), children: vec![Node::Row { gap: 2., children: controls }] }) }
}

/// A title strip button, as the dock's: its symbol in the label colour (the accent while `on`) on a round
/// highlight that shows under the pointer, concentric with the 26pt capsule it sits 2pt into.
fn strip_tool(action: u32, symbol: &'static str, tip: &'static str, on: bool) -> Node {
    let glyph = Node::Glyph { name: symbol, size: 13., ink: if on { Ink::Accent } else { Ink::Primary } };
    let item = Node::Item { height: CAPSULE_R * 2. - 4., click: Some(action), drag: None, children: vec![Node::Center(Box::new(glyph))] };
    Node::Tip { tip: tip.into(), child: Box::new(Node::Width(28., Box::new(Node::Round { radius: PILL_R, tint: None, child: Box::new(item) }))) }
}

/// What a native reorder list holds, in its order: a window's tabs ("" for a gap a drag opened), or spaces.
pub(crate) enum ReorderList {
    Tabs(Vec<String>),
    Spaces(Vec<String>),
}

/// Height of the space chips' capsule in the title bar.
const SPACES_H: f64 = 26.;

impl BrowserWindow {
    /// Builds every shown window's frame for this frame, and records where its tab pills landed.
    pub(crate) fn sync_frames(&mut self, pop: &mut Pop, tabs: &[browser_core::Tab]) {
        let Some(stage) = self.stage_bounds.filter(|_| !self.tab_window && self.self_test.is_none()) else {
            self.engine.keep_window_frames(&[]);
            return;
        };
        let paint = paint();
        let (ox, oy) = (f64::from(stage.origin.x), f64::from(stage.origin.y));
        let carried = self.carried().map(|(id, _)| id);
        let shown = self.shown_tabs(tabs).into_iter().cloned().collect::<Vec<_>>();
        let mut keys = Vec::new();
        for tab in &shown {
            let Some(frame) = self.visual_frame(&tab.id).filter(|_| carried.as_deref() != Some(tab.id.as_str())) else {
                continue;
            };
            let (scale, alpha) = self.window_motion(&tab.id);
            let frame = scaled(frame, scale);
            let maximized = self.chrome.get(&tab.id).is_some_and(|chrome| chrome.maximized);
            let shell = LogicalRect { x: ox + f64::from(frame.x), y: oy + f64::from(frame.y), width: f64::from(frame.w), height: f64::from(frame.h) };
            let group = self.group_id(&tab.id);
            let members = tabs.iter().filter(|other| self.group_of(&other.id) == group).collect::<Vec<_>>();
            let load = if tab.active && self.load_bar > 0. {
                let fade = self.load_done.map_or(1., |at| 1. - (at.elapsed().as_secs_f64() / LOAD_FADE.as_secs_f64()).min(1.));
                (f64::from(self.load_bar), fade)
            } else {
                (0., 0.)
            };
            let style = FrameStyle {
                shell: shell.clone(),
                grip: if maximized { 0. } else { f64::from(GRIP_OUT) },
                title_h: TITLE_H,
                radius: WINDOW_RADIUS,
                body: rgba(paint.secondary),
                line: rgba(paint.border),
                // A neutral hairline on every window: focus shows as depth and an undimmed strip, as in macOS.
                ring: rgba(paint.border),
                accent: rgba(paint.accent),
                load,
                alpha: f64::from(alpha),
                active: tab.active,
            };
            let mut pills = Vec::new();
            let strip = match self.chrome.get(&tab.id).and_then(|chrome| chrome.panel.clone()) {
                Some(ext) => self.panel_strip(&tab.id, &ext, pop),
                None => {
                    let row = self.window_row(tab, &members, shell.width, true, pop, &mut pills);
                    // The 26pt glass capsules 2pt in from the top and 3pt in from the sides, nesting in the window's
                    // 16pt corners.
                    Node::Column { gap: 0., pad: (3., 2.), children: vec![row] }
                }
            };
            self.engine.set_window_frame(&group, style, &strip);
            for (list, widths, hide, clear, gap) in std::mem::take(&mut self.list_arrangements) {
                self.engine.arrange_list(list, &widths, hide, clear, gap);
            }
            for (action, rect) in self.engine.frame_items(&group) {
                if let Some((_, id)) = pills.iter().find(|(click, _)| *click == action) {
                    self.slots.insert(id.clone(), bounds(&rect));
                }
            }
            keys.push(group);
        }
        self.engine.keep_window_frames(&keys);
        if std::mem::take(&mut self.address_caret) {
            if let Some(id) = self.active_tab_id() {
                self.engine.focus_frame_field(&self.group_id(&id));
            }
        }
    }

    /// A window's title strip, laid out as Safari's compact tab bar: back and forward, the tabs on one glass track
    /// (the shown one standing as the address field) and new tab, then bookmark and screenshot, then the window buttons;
    /// `tools` false leaves out all but the tabs and new tab button.
    #[allow(clippy::too_many_arguments)]
    fn window_row(&mut self, tab: &browser_core::Tab, members: &[&browser_core::Tab], width: f64, tools: bool, pop: &mut Pop, pills: &mut Vec<(u32, String)>) -> Node {
        let paint = paint();
        let id = tab.id.clone();
        let tool = |pop: &mut Pop, symbol: &'static str, tip: &'static str, on: bool, run: crate::settings::MenuAction| strip_tool(pop.act(run), symbol, tip, on);
        let badge = self.guest_profile(&tab.id);
        let (mut page, mut window) = if tools { self.window_tools(&tab.id, pop) } else { (Vec::new(), Vec::new()) };
        let nav_most = if tools && !is_terminal(&tab.url) { 2 } else { 0 };
        let (page_most, window_most) = (page.len(), window.len());
        // What fits, fullest first: as the window narrows, bookmark and screenshot go, then forward, then minimize and
        // maximize (close stays), then new tab, so nothing runs past the strip's edge. (back/forward, page tools,
        // window tools, new tab)
        let levels = [
            (nav_most, page_most, window_most, true),
            (nav_most, 0, window_most, true),
            (nav_most.min(1), 0, window_most, true),
            (nav_most.min(1), 0, window_most.min(1), true),
            (0, 0, window_most.min(1), true),
            (0, 0, window_most.min(1), false),
        ];
        let badge_w = if badge.is_some() { 20. } else { 0. };
        let fixed = |(nav, page, window, plus): (usize, usize, usize, bool)| {
            let parts = [capsule_w(nav), badge_w, capsule_w(page), capsule_w(window), capsule_w(usize::from(plus))];
            // The track and its two spacers besides the parts shown, all 6pt apart.
            let shown = parts.iter().filter(|part| **part > 0.).count() + 3;
            parts.iter().sum::<f64>() + 6. * (shown - 1) as f64
        };
        let count = members.len().max(1) as f64;
        // The narrowest track that still shows every tab, as an icon.
        let track_min = 4. + count * MIN_PILL + 2. * (count - 1.);
        // Inside the strip's 3pt side insets, with a point to spare for rounding.
        let avail = width - 7.;
        let (nav, page_n, window_n, plus) = levels.into_iter().find(|level| fixed(*level) + track_min <= avail).unwrap_or(levels[5]);
        let mut row = Vec::new();
        if nav > 0 {
            let (back, forward) = (id.clone(), id.clone());
            let mut buttons = vec![tool(pop, "chevron.left", "Back", false, Box::new(move |this| {
                this.focus_window(&back);
                this.back();
            }))];
            if nav > 1 {
                buttons.push(tool(pop, "chevron.right", "Forward", false, Box::new(move |this| {
                    this.focus_window(&forward);
                    this.forward();
                })));
            }
            row.push(group(buttons));
        }
        if let Some(profile) = badge {
            row.push(profile_badge_node(&profile, true, 20., paint));
        }
        // What the buttons leave. A resize sets the new widths on the strip's views rather than rebuilding it.
        let room = (avail - fixed((nav, page_n, window_n, plus))).floor().max(MIN_PILL + 4.);
        row.push(Node::Spacer);
        row.push(self.tab_track(&tab.id, members, room, pop, pills));
        if plus {
            let new_tab = id.clone();
            // Right after the last tab, as in Safari.
            row.push(group(vec![tool(pop, "plus", "New Tab", false, Box::new(move |this| {
                let shift = this.engine.shift_held();
                this.click_new_tab(&new_tab, shift);
            }))]));
        }
        row.push(Node::Spacer);
        page.truncate(page_n);
        // Close is the last window tool, and the one that stays.
        window.drain(..window_most - window_n);
        if !page.is_empty() {
            row.push(group(page));
        }
        if !window.is_empty() {
            row.push(group(window));
        }
        Node::Row { gap: 6., children: row }
    }

    /// The window's tabs on one glass track at most `room` wide, in its order (with the gap a drag opened): each as
    /// wide as the others, as in Safari, the shown one standing as the address field.
    fn tab_track(&mut self, front: &str, members: &[&browser_core::Tab], room: f64, pop: &mut Pop, pills: &mut Vec<(u32, String)>) -> Node {
        let ids = members.iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        let group = self.group_id(front);
        let count = ids.len().max(1) as f64;
        // As long as Safari's for a few tabs, the whole room for many.
        let track = room.min((count * 220.).max(440.));
        let editing = members.iter().any(|tab| tab.id == front && tab.active) && (self.address_editing || self.address_focused);
        // As in Safari, when the room runs short the shown tab keeps enough for its site, more while its address is
        // typed, and the others close up first, down to their icons. For `n` slots: the shown tab's, the others'.
        let want = if editing { 320. } else { 200. };
        let split = |n: f64| {
            let inner = track - 4. - 2. * (n - 1.);
            if n > 1. && inner / n < want {
                let shown = want.min(inner - MIN_PILL * (n - 1.)).max(inner / n).floor();
                (shown, ((inner - shown) / (n - 1.)).floor())
            } else {
                ((inner / n).floor(), (inner / n).floor())
            }
        };
        let (shown_w, other_w) = split(count);
        let list = pop.act(|_| {});
        self.reorder_lists.push((list, ReorderList::Tabs(ids.clone())));
        // A tab dragged over the row: its own slot closes and a gap opens where it would land; a dropped one keeps
        // its slot unseen while its pill glides into it. The engine slides the row there; the row stays as built.
        let drag = self.reorder.as_ref().filter(|_| self.tab_drag.is_some());
        let hide = drag.and_then(|state| ids.iter().position(|id| *id == state.id));
        let clear = self.landing().and_then(|(landing, ..)| ids.iter().position(|id| id == landing));
        let gap = drag.filter(|state| state.row.as_deref() == Some(group.as_str())).and_then(|state| state.order.iter().position(|id| *id == state.id));
        let slots = count - f64::from(u8::from(hide.is_some())) + f64::from(u8::from(gap.is_some()));
        let (shown_now, other_now) = split(slots.max(1.));
        let widths = ids.iter().map(|id| if id == front { shown_now } else { other_now }.max(MIN_PILL)).collect();
        self.list_arrangements.push((list, widths, hide, clear, gap.map(|at| (at, other_now.max(MIN_PILL)))));
        let mut row = Vec::new();
        for tab in ids.iter().filter_map(|id| members.iter().find(|tab| tab.id == *id)) {
            let shown = tab.id == front;
            let width = if shown { shown_w } else { other_w };
            let node = if shown && editing { self.address_field(tab, pop) } else { self.tab_pill(tab, shown, width, pop, pills) };
            row.push(Node::Width(width.max(MIN_PILL), Box::new(node)));
        }
        let tabs = Node::Reorder { list, gap: 2., horizontal: true, children: row };
        Node::Width(track, Box::new(Node::Glass { radius: CAPSULE_R, child: Box::new(Node::Column { gap: 0., pad: (2., 2.), children: vec![tabs] }) }))
    }

    /// The shown tab while its address is typed: a ringed field with a clear button and reload, as Safari's.
    fn address_field(&mut self, tab: &browser_core::Tab, pop: &mut Pop) -> Node {
        let clear = pop.act(|this| {
            this.set_typed(ADDRESS_QUERY, String::new());
            this.suggestions.clear();
            this.suggestion = None;
            this.address_caret = true;
            this.notify();
        });
        let field = Node::Search { field: ADDRESS_QUERY, value: Initial(self.typed(ADDRESS_QUERY)), placeholder: "Search or enter website name".into(), size: 12. };
        let row = Node::Row { gap: 4., children: vec![Node::Tool { symbol: "xmark.circle.fill", tip: "Clear", action: clear, on: false }, field, self.reload_tool(&tab.id, pop)] };
        Node::Ring { radius: PILL_R, child: Box::new(Node::Height(22., Box::new(Node::Column { gap: 0., pad: (2., 0.), children: vec![row] }))) }
    }

    /// Reload, or Stop while the page loads, as in Safari's address field.
    fn reload_tool(&self, tab_id: &str, pop: &mut Pop) -> Node {
        let loading = self.engine.progress(tab_id).is_some();
        let reload = tab_id.to_string();
        let action = pop.act(move |this| {
            this.focus_window(&reload);
            match this.engine.progress(&reload) {
                Some(_) => this.engine.stop(&reload),
                None => this.reload(),
            }
        });
        Node::Tool { symbol: if loading { "xmark" } else { "arrow.clockwise" }, tip: if loading { "Stop" } else { "Reload" }, action, on: false }
    }

    /// The tab's site icon, or for a terminal running a coding agent, the agent's logo with its status dot.
    pub(crate) fn tab_image(&self, tab: &browser_core::Tab, size: f64) -> Node {
        use terminal::Status;
        let Some(state) = terminal::agent(&tab.id).filter(|_| is_terminal(&tab.url)) else {
            return site_image(&tab.url, size);
        };
        let agent = state.agent;
        let [red, green, blue] = crate::native::rgb(crate::window::hex(agent.color().unwrap_or(0x8E8E93)));
        let letter = agent.name().chars().take(1).collect::<String>();
        let logo = match agent.icon().and_then(crate::native::asset) {
            Some(data) => Node::Svg { data, size, ink: Ink::Primary },
            None => Node::Image { path: None, letter, rgb: [red, green, blue], size, radius: (size / 4.).max(4.) },
        };
        // Coloured as herdr does: amber working, red blocked, teal done, green idle.
        let dot = match state.status {
            Status::Working => [0.96, 0.65, 0.14],
            Status::Blocked => [0.94, 0.27, 0.27],
            Status::Done => [0.18, 0.83, 0.75],
            Status::Idle => [0.13, 0.77, 0.37],
        };
        Node::Overlay { child: Box::new(logo), corner: Box::new(Node::Image { path: None, letter: String::new(), rgb: dot, size: 6., radius: 3. }) }
    }

    /// A tab on the track, `width` wide. The shown one stands as the address field at rest: on a card, its site's
    /// host and a reload button. The others show their icon (a close button on hover, as in Safari) and title.
    fn tab_pill(&mut self, tab: &browser_core::Tab, shown: bool, width: f64, pop: &mut Pop, pills: &mut Vec<(u32, String)>) -> Node {
        let id = tab.id.clone();
        let click = pop.act({
            let id = id.clone();
            move |this| this.click_tab(&id)
        });
        pills.push((click, id.clone()));
        let close = pop.act({
            let id = id.clone();
            move |this| this.close_tabs(vec![id.clone()])
        });
        let children = if width < FULL_PILL {
            // Squeezed by a narrow window or many tabs: the icon alone, as Safari's.
            vec![Node::Center(Box::new(self.tab_image(tab, 14.)))]
        } else {
            self.pill_content(tab, shown, width, close, pop)
        };
        let mut pill = Node::Item { height: 22., click: Some(click), drag: None, children };
        if shown {
            pill = Node::Round { radius: PILL_R, tint: Some(Ink::Secondary), child: Box::new(pill) };
        } else {
            pill = Node::Round { radius: PILL_R, tint: None, child: Box::new(pill) };
        }
        if self.picked(&tab.id) {
            pill = Node::Faded(Box::new(pill));
        }
        let menu = pop.act({
            let id = id.clone();
            move |this| {
                let (x, y) = this.engine.pointer().unwrap_or_default();
                this.open_tab_menu(id.clone(), point(x as f32, y as f32));
            }
        });
        let drag = pop.act(|_| {});
        self.drag_items.push((drag, DragItem::Tab(id)));
        let pill = Node::Tip { tip: full_title(tab), child: Box::new(pill) };
        Node::Drag { id: drag, child: Box::new(Node::Menu { action: menu, child: Box::new(pill) }) }
    }

    /// A tab pill at full size, `width` wide: the close button on hover, icon, title cut to fit, and the shown
    /// tab's reload.
    fn pill_content(&mut self, tab: &browser_core::Tab, shown: bool, width: f64, close: u32, pop: &mut Pop) -> Vec<Node> {
        let id = tab.id.clone();
        // As in Safari: the close button at the tab's leading edge on hover, the icon and title centred between it
        // and a slot of the same width at the trailing edge (the shown tab's reload), so nothing shifts.
        let slot = || Node::Width(18., Box::new(Node::Spacer));
        let mut children = vec![
            Node::Hover(true, Box::new(Node::Width(18., Box::new(Node::Center(Box::new(Node::Tool { symbol: "xmark", tip: "Close Tab", action: close, on: false })))))),
            Node::Hover(false, Box::new(slot())),
            Node::Spacer,
            self.tab_image(tab, 14.),
        ];
        // The shown tab names its site, as Safari's field does; the others their page. Cut to fit, down to the icon.
        let label = match () {
            _ if shown && is_terminal(&tab.url) => tab_label(&tab.title, &tab.url),
            _ if shown && crate::window::is_blank(&tab.url) => "Search or enter website name".to_string(),
            _ if shown => site_host(&tab.url),
            _ => tab_label(&tab.title, &tab.url),
        };
        // Less the two 18pt edge slots, the icon, the gaps and the padding.
        let chars = (((width - 92.) / 6.5).floor().max(0.) as usize).min(48);
        if chars >= 4 {
            let label = if label.chars().count() > chars { format!("{}…", label.chars().take(chars - 1).collect::<String>().trim_end()) } else { label };
            let ink = if shown && crate::window::is_blank(&tab.url) { Ink::Secondary } else if shown { Ink::Primary } else { Ink::Secondary };
            children.push(text(label, 12., Weight::Regular, ink));
        }
        // Plain http: an open padlock, as Safari's "Not Secure".
        if tab.url.starts_with("http://") {
            children.push(Node::Glyph { name: "lock.open", size: 10., ink: Ink::Danger });
        }
        let muted = self.engine.is_muted(&tab.id);
        if muted || self.playing.contains(&tab.id) {
            let toggle = pop.act({
                let id = id.clone();
                move |this| {
                    let mute = !this.engine.is_muted(&id);
                    this.engine.set_muted(&id, mute);
                    this.notify();
                }
            });
            children.push(Node::Tool { symbol: if muted { "speaker.slash.fill" } else { "speaker.wave.2.fill" }, tip: if muted { "Unmute" } else { "Mute" }, action: toggle, on: false });
        }
        children.push(Node::Spacer);
        children.push(if shown && !is_terminal(&tab.url) { Node::Width(18., Box::new(Node::Center(Box::new(self.reload_tool(&tab.id, pop))))) } else { slot() });
        children
    }

    /// Bookmark and screenshot (and Keep as Window for a peek), then minimize, maximize and close.
    fn window_tools(&self, tab_id: &str, pop: &mut Pop) -> (Vec<Node>, Vec<Node>) {
        let maximized = self.chrome.get(tab_id).is_some_and(|chrome| chrome.maximized);
        let peek = self.chrome.get(tab_id).is_some_and(|chrome| chrome.peek);
        let bookmarked = {
            let model = self.model.lock();
            model.tabs.iter().find(|tab| tab.id == tab_id).is_some_and(|tab| model.is_bookmarked(&tab.url).unwrap_or_default())
        };
        let id = tab_id.to_string();
        let mut tools = Vec::new();
        let mut tool = |symbol: &'static str, tip: &'static str, on: bool, run: crate::settings::MenuAction| tools.push(strip_tool(pop.act(run), symbol, tip, on));
        if peek {
            let keep = id.clone();
            tool("macwindow.badge.plus", "Keep as Window", false, Box::new(move |this| {
                // Placed afresh like any new window: tiled, or cascaded in a free space.
                if let Some(entry) = this.chrome.get_mut(&keep) {
                    (entry.peek, entry.frame) = (false, None);
                }
                this.focus_window(&keep);
            }));
        }
        let mark = id.clone();
        tool(if bookmarked { "star.fill" } else { "star" }, "Bookmark", bookmarked, Box::new(move |this| {
            this.focus_window(&mark);
            this.bookmark();
        }));
        let shot = id.clone();
        tool("camera", "Screenshot", false, Box::new(move |this| {
            this.focus_window(&shot);
            let (x, y) = this.engine.pointer().unwrap_or_default();
            this.open_shot_menu(shot.clone(), point(x as f32, y as f32));
        }));
        let page = std::mem::take(&mut tools);
        let mut tool = |symbol: &'static str, tip: &'static str, run: crate::settings::MenuAction| tools.push(strip_tool(pop.act(run), symbol, tip, false));
        let (min, zoom, close) = (id.clone(), id.clone(), id);
        tool("minus", "Minimize", Box::new(move |this| this.minimize_window(&min)));
        tool(if maximized { "arrow.down.right.and.arrow.up.left" } else { "arrow.up.left.and.arrow.down.right" }, if maximized { "Restore" } else { "Maximize" }, Box::new(move |this| this.toggle_maximize(&zoom)));
        tool("xmark", "Close Window", Box::new(move |this| this.close_window(&close)));
        (page, tools)
    }

    /// `[icon] Name ──── [×]`: a side panel needs no address bar or navigation.
    fn panel_strip(&self, tab_id: &str, ext: &str, pop: &mut Pop) -> Node {
        let meta = self.ext_meta.get(ext).cloned().unwrap_or_default();
        let id = tab_id.to_string();
        let close = pop.act(move |this| this.close_tabs(vec![id.clone()]));
        let row = Node::Row {
            gap: 8.,
            children: vec![
                crate::extensions::extension_image(&meta.name, meta.icon.clone(), 16.),
                title(meta.name.clone(), 12., Weight::Regular, Ink::Primary),
                Node::Spacer,
                Node::Tool { symbol: "xmark", tip: "Close", action: close, on: false },
            ],
        };
        Node::Column { gap: 0., pad: (10., 2.), children: vec![row] }
    }

    /// The title bar's centre: this space's chips on glass, or a torn-off window's own tabs.
    pub(crate) fn sync_spaces(&mut self, pop: &mut Pop, bars: &mut Vec<&'static str>, tabs: &[browser_core::Tab]) {
        let width = f64::from(self.viewport().width);
        let content = if self.tab_window {
            let session = self.model.lock().active_session_id.clone();
            let Some((front, _)) = self.window_front(tabs, &session) else {
                return;
            };
            let Some(tab) = tabs.iter().find(|tab| tab.id == front).cloned() else {
                return;
            };
            let group = self.group_id(&front);
            let members = tabs.iter().filter(|other| self.group_of(&other.id) == group).collect::<Vec<_>>();
            let mut pills = Vec::new();
            let row = self.window_row(&tab, &members, width * 0.6, false, pop, &mut pills);
            // Its own glass capsules straight on the title bar, 2pt in, level with the tools on the right.
            let content = Node::Column { gap: 0., pad: (0., 2.), children: vec![row] };
            self.place_spaces(&content, width, false, bars);
            for (list, widths, hide, clear, gap) in std::mem::take(&mut self.list_arrangements) {
                self.engine.arrange_list(list, &widths, hide, clear, gap);
            }
            for (action, rect) in self.engine.bar_items("spaces") {
                if let Some((_, id)) = pills.iter().find(|(click, _)| *click == action) {
                    self.slots.insert(id.clone(), bounds(&rect));
                }
            }
            return;
        } else {
            self.space_chips_row(pop, tabs)
        };
        let switches = content.1;
        self.place_spaces(&content.0, width, true, bars);
        for (action, rect) in self.engine.bar_items("spaces") {
            if let Some((_, id)) = switches.iter().find(|(switch, _)| *switch == action) {
                self.space_chips.push((id.clone(), bounds(&rect)));
            }
        }
        if std::mem::take(&mut self.space_caret) {
            self.engine.focus_bar_field("spaces");
        }
    }

    /// Puts `content` at the dock's left end in the main window, or centred in a torn-off window's title bar, from
    /// the width it took last time: on a glass capsule, or (`glass` off) bare, the title bar's full height.
    fn place_spaces(&mut self, content: &Node, width: f64, glass: bool, bars: &mut Vec<&'static str>) {
        let height = if glass { SPACES_H } else { f64::from(self.titlebar_h()) };
        let (right, top) = match self.dock_at() {
            Some((right, top, _)) => (right - self.toolbar_w - crate::window::DOCK_GAP, top),
            None => ((width + self.spaces_w) / 2., (f64::from(self.titlebar_h()) - height) / 2.),
        };
        self.spaces_w = self.engine.set_bar("spaces", LogicalRect { x: right, y: top, width: 0., height }, glass.then_some(SPACES_H / 2.), content);
        bars.push("spaces");
    }

    /// The space chips (with the field renaming one), a new space button, and each chip's switch action.
    fn space_chips_row(&mut self, pop: &mut Pop, tabs: &[browser_core::Tab]) -> (Node, Vec<(u32, String)>) {
        let sessions = {
            let model = self.model.lock();
            let (spaces, windows) = (&model.settings.detached_spaces, &model.settings.detached_windows);
            let shown = |session: &&browser_core::Session| match &self.model.detached {
                Some(own) => session.id == *own,
                None => !spaces.contains(&session.id) && !windows.contains(&session.id),
            };
            model.profile_sessions().into_iter().filter(shown).cloned().collect::<Vec<_>>()
        };
        let active = self.model.lock().active_session_id.clone();
        let all_tabs = self.model.lock().tabs.clone();
        let closable = sessions.len() > 1;
        let tears = closable || (self.model.detached.is_some() && !self.tab_window);
        let mut chips = Vec::new();
        let mut switches = Vec::new();
        let list = pop.act(|_| {});
        self.reorder_lists.push((list, ReorderList::Spaces(sessions.iter().map(|session| session.id.clone()).collect())));
        for session in &sessions {
            let id = session.id.clone();
            if self.space_rename.as_deref() == Some(id.as_str()) {
                let rename = pop.act({
                    let id = id.clone();
                    move |this| {
                        if this.space_rename.take().is_some() {
                            let name = this.typed(SPACE_NAME);
                            this.with_model(|model| {
                                let _ = model.rename_session(&id, &name);
                            });
                            this.notify();
                        }
                    }
                });
                let field = Node::Field { field: SPACE_NAME, value: Initial(session.name.clone()), placeholder: String::new(), submit: rename, secure: false, blur: true };
                chips.push(Node::Width(110., Box::new(field)));
                continue;
            }
            let switch = pop.act({
                let id = id.clone();
                move |this| {
                    // A double-click renames it; the first click already switched to it.
                    if this.engine.last_clicks() == 2 {
                        let name = this.model.lock().sessions.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
                        this.popover_texts.insert(SPACE_NAME, name);
                        this.space_rename = Some(id.clone());
                        this.space_caret = true;
                        this.notify();
                        return;
                    }
                    this.with_model(|model| {
                        let _ = model.switch_session(&id);
                    });
                    this.refresh_url();
                }
            });
            switches.push((switch, id.clone()));
            let icons = all_tabs.iter().filter(|tab| tab.session_id == id).take(3).map(|tab| site_image(&tab.url, 14.)).collect::<Vec<_>>();
            let mut children = vec![Node::Row { gap: -5., children: icons }, text(session.name.clone(), 12., Weight::Regular, if id == active { Ink::Primary } else { Ink::Secondary })];
            if closable {
                let close = pop.act({
                    let id = id.clone();
                    move |this| {
                        this.with_model(|model| {
                            let _ = model.delete_session(&id);
                        });
                        this.refresh_url();
                    }
                });
                children.push(Node::Hover(true, Box::new(Node::Tool { symbol: "xmark", tip: "Close Space", action: close, on: false })));
            }
            let mut chip = Node::Item { height: 22., click: Some(switch), drag: None, children };
            if id == active {
                chip = Node::Round { radius: PILL_R, tint: Some(Ink::Secondary), child: Box::new(chip) };
            } else {
                chip = Node::Round { radius: PILL_R, tint: None, child: Box::new(chip) };
            }
            if tears {
                let drag = pop.act(|_| {});
                self.drag_items.push((drag, DragItem::Space(id)));
                chip = Node::Drag { id: drag, child: Box::new(chip) };
            }
            chips.push(chip);
        }
        let mut row = vec![Node::Reorder { list, gap: 4., horizontal: true, children: chips }];
        if self.model.detached.is_none() {
            let new_space = pop.act(|this| {
                this.with_model(|model| {
                    let _ = model.create_session();
                });
                this.refresh_url();
            });
            row.push(Node::Tool { symbol: "plus", tip: "New Space", action: new_space, on: false });
        }
        let _ = tabs;
        (Node::Column { gap: 0., pad: (4., 2.), children: vec![Node::Row { gap: 4., children: row }] }, switches)
    }

    /// What follows the pointer while dragging: the snap target, a carried window, a tab pill or a space chip.
    pub(crate) fn sync_drag_views(&mut self, bars: &mut Vec<&'static str>, tabs: &[browser_core::Tab]) {
        let pill_h = f64::from(self.pill_h());
        let snap = self.drop_preview().zip(self.stage_bounds).map(|((_, frame), stage)| LogicalRect {
            x: f64::from(stage.origin.x) + f64::from(frame.x),
            y: f64::from(stage.origin.y) + f64::from(frame.y),
            width: f64::from(frame.w),
            height: f64::from(frame.h),
        });
        let accent = rgba(paint().accent);
        self.engine.set_snap(snap, WINDOW_RADIUS, accent);
        if let Some((id, frame)) = self.carried() {
            // A glass card with the window's sites and title; it holds no size of its own, so as it shrinks and
            // follows the pointer only its frame changes.
            let group = self.group_id(&id);
            let members = tabs.iter().filter(|tab| self.group_of(&tab.id) == group).collect::<Vec<_>>();
            let icons = members.iter().take(3).map(|tab| site_image(&tab.url, 22.)).collect();
            let front = members.iter().find(|tab| tab.id == id).or(members.first());
            let mut lines = vec![Node::Spacer, Node::Center(Box::new(Node::Row { gap: -6., children: icons }))];
            if let Some(tab) = front {
                lines.push(Node::Center(Box::new(title(tab_label(&tab.title, &tab.url), 12., Weight::Semibold, Ink::Primary))));
            }
            if members.len() > 1 {
                lines.push(Node::Center(Box::new(text(format!("{} tabs", members.len()), 11., Weight::Regular, Ink::Secondary))));
            }
            lines.push(Node::Spacer);
            let rect = LogicalRect { x: f64::from(frame.x), y: f64::from(frame.y), width: f64::from(frame.w), height: f64::from(frame.h) };
            let radius = WINDOW_RADIUS.max(rect.height.min(rect.width) / 6.).min(18.);
            self.engine.set_bar("carried", rect, Some(radius), &Node::Column { gap: 6., pad: (12., 8.), children: lines });
            bars.push("carried");
        }
        // The dragged tab's pill under the pointer; once dropped, gliding into its slot.
        let pill = match (&self.tab_drag, self.landing()) {
            (Some((tab, at)), _) => {
                let at = *at - self.tab_grab.get();
                let width = drag_pill_w(self.reorder.as_ref().map_or(160., |state| f64::from(state.size.width)));
                Some((tab.clone(), LogicalRect { x: f64::from(at.x), y: f64::from(at.y), width, height: pill_h }))
            }
            (None, Some((id, from, width, p))) => self.slots.get(id).zip(tabs.iter().find(|tab| tab.id == id)).map(|(slot, tab)| {
                let lerp = |from: f64, to: f64| from + (to - from) * f64::from(p);
                let rect = LogicalRect {
                    x: lerp(f64::from(from.x), f64::from(slot.origin.x)),
                    y: lerp(f64::from(from.y), f64::from(slot.origin.y)),
                    width: lerp(width, f64::from(slot.size.width)),
                    height: lerp(pill_h, f64::from(slot.size.height)),
                };
                (tab.clone(), rect)
            }),
            _ => None,
        };
        if let Some((tab, rect)) = pill {
            let row = Node::Row { gap: 6., children: vec![site_image(&tab.url, 14.), title(tab_label(&tab.title, &tab.url), 12., Weight::Regular, Ink::Primary)] };
            self.engine.set_bar("tab-drag", rect, Some(PILL_R), &Node::Column { gap: 0., pad: (10., (pill_h - 16.) / 2.), children: vec![Node::Center(Box::new(row))] });
            bars.push("tab-drag");
        }
        if let (Some(space), Some(at)) = (&self.space_drag, self.space_drag_at) {
            let name = self.model.lock().sessions.iter().find(|session| session.id == *space).map(|session| session.name.clone()).unwrap_or_default();
            let rect = LogicalRect { x: f64::from(at.x) + 8., y: f64::from(at.y) + 8., width: 140., height: 24. };
            self.engine.set_bar("space-drag", rect, Some(12.), &Node::Column { gap: 0., pad: (10., 4.), children: vec![text(name, 12., Weight::Regular, Ink::Primary)] });
            bars.push("space-drag");
        }
    }

    /// Acts on the presses, drags and hovers native chrome reported since the last frame.
    pub(crate) fn run_native_input(&mut self) {
        for input in std::mem::take(&mut self.native_input) {
            match input {
                NativeInput::Gesture { key, phase, at, edges, shift } => {
                    let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
                    let Some(id) = tabs.iter().find(|tab| self.group_of(&tab.id) == key && self.is_front(&tab.id)).map(|tab| tab.id.clone()) else {
                        continue;
                    };
                    match phase {
                        Phase::Begin if edges != 0 => {
                            let edges = Edges { left: edges & 1 != 0, right: edges & 2 != 0, top: edges & 4 != 0, bottom: edges & 8 != 0 };
                            self.begin_resize(&id, edges, at);
                        }
                        Phase::Begin => self.begin_move(&id, at),
                        Phase::Move if self.gesture.is_some() => self.track_gesture(at, shift),
                        Phase::End if self.gesture.is_some() => {
                            self.track_gesture(at, shift);
                            // Released past the window's edge: the window leaves the app for an OS window of its own.
                            let out = self.torn_off(at).then(|| self.screen_origin() + at);
                            self.finish_gesture(out);
                        }
                        Phase::Enter => {
                            let hover = !self.active_tab_id().is_some_and(|active| self.group_of(&active) == key) && self.model.lock().settings.focus_on_hover;
                            // Not mid-drag, nor while typing an address, which passing over another window would cut off.
                            if hover && self.gesture.is_none() && self.tab_drag.is_none() && !self.palette_open && !self.address_editing {
                                self.focus_window(&id);
                            }
                        }
                        _ => {}
                    }
                }
                // Ids are handed out afresh each build, and a drag rebuilds its row: past its start, a drag
                // belongs to whatever it started on.
                NativeInput::Drag { phase, at, .. } if phase != Phase::Begin && self.tab_drag.is_some() => {
                    let tab = self.tab_drag.as_ref().map(|(tab, _)| tab.id.clone()).unwrap_or_default();
                    self.drag_tab(&tab, phase, at);
                }
                NativeInput::Drag { phase, at, .. } if phase != Phase::Begin && self.space_drag.is_some() => {
                    let space = self.space_drag.clone().unwrap_or_default();
                    self.drag_space(&space, phase, at);
                }
                NativeInput::Drag { id, phase, at } => match self.drag_items.iter().find(|(drag, _)| *drag == id).map(|(_, item)| item) {
                    Some(DragItem::Tab(tab)) => {
                        let tab = tab.clone();
                        self.drag_tab(&tab, phase, at);
                    }
                    Some(DragItem::Space(space)) => {
                        let space = space.clone();
                        self.drag_space(&space, phase, at);
                    }
                    None => {}
                },
            }
        }
    }

    fn drag_tab(&mut self, id: &str, phase: Phase, at: Point) {
        match phase {
            Phase::Begin => {
                let Some(tab) = self.model.lock().tabs.iter().find(|tab| tab.id == id).cloned() else {
                    return;
                };
                let grab = self.slots.get(id).map_or(point(20., 10.), |slot| at - slot.origin);
                self.tab_grab.set(grab);
                self.tab_press = Some(at);
                self.tab_drag = Some((tab, at));
                let rows = self.tab_rows();
                self.preview_drag(id, at - grab, rows, false, 2.0);
            }
            Phase::Move => {
                if let Some(drag) = self.tab_drag.as_mut() {
                    drag.1 = at;
                }
                let rows = self.tab_rows();
                self.preview_drag(id, at - self.tab_grab.get(), rows, false, 2.0);
            }
            Phase::End => {
                self.tab_drag = None;
                if self.torn_off(at) {
                    self.reorder = None;
                    self.detach_tabs(&[id.to_string()], self.screen_origin() + at);
                } else if let Some(target) = self.window_under(at) {
                    self.drop_tab_on(id, &target, at);
                } else {
                    // Dropped on empty stage: a window of its own.
                    self.reorder = None;
                    self.detach_tab(id, at);
                }
            }
            Phase::Enter => {}
        }
        self.notify();
    }

    fn drag_space(&mut self, space: &str, phase: Phase, at: Point) {
        match phase {
            Phase::Begin | Phase::Move => {
                self.space_drag = Some(space.to_string());
                self.space_drag_at = Some(at);
            }
            Phase::End => {
                self.space_drag = None;
                self.space_drag_at = None;
                let inside = Bounds::new(Point::default(), self.viewport()).contains(&at);
                let screen = self.screen_origin() + at;
                match (inside, self.model.detached.is_some()) {
                    (true, _) => {}
                    // A torn-off space goes back only when dropped on the main window's bar.
                    (false, true) => {
                        if over_main_bar(screen) {
                            self.merge_into_main();
                        }
                    }
                    (false, false) => self.open_space_window(space, false, screen),
                }
            }
            Phase::Enter => {}
        }
        self.notify();
    }

    /// Moves the item a native reorder `list` dropped from slot `from` to slot `to`.
    pub(crate) fn reorder_list(&mut self, list: u32, from: usize, to: usize) {
        let Some((_, items)) = self.reorder_lists.iter().find(|(id, _)| *id == list) else {
            return;
        };
        match items {
            ReorderList::Tabs(ids) => {
                let (Some(id), Some(target)) = (ids.get(from).filter(|id| !id.is_empty()).cloned(), ids.get(to).filter(|id| !id.is_empty()).cloned()) else {
                    return;
                };
                // Into the dropped-on tab's place in the space's order: after it moving forward, before it moving back.
                self.with_model(|model| {
                    if let Some(index) = model.session_tabs().iter().position(|tab| tab.id == target) {
                        let _ = model.move_tab(&id, index);
                    }
                });
            }
            ReorderList::Spaces(ids) => {
                let (Some(id), Some(target)) = (ids.get(from).cloned(), ids.get(to).cloned()) else {
                    return;
                };
                self.with_model(|model| {
                    if let Some(index) = model.profile_sessions().iter().position(|session| session.id == target) {
                        let _ = model.move_session(&id, index);
                    }
                });
            }
        }
        self.notify();
    }

    /// The front tab of the frontmost window under `at` (window coordinates).
    fn window_under(&self, at: Point) -> Option<String> {
        let stage = self.stage_bounds?;
        let local = at - stage.origin;
        let tabs = self.model.lock().session_tabs().into_iter().cloned().collect::<Vec<_>>();
        let shown = self.shown_tabs(&tabs).into_iter().map(|tab| tab.id.clone()).collect::<Vec<_>>();
        self.order.iter().rev().filter(|id| shown.contains(id)).find(|id| {
            self.visual_frame(id).is_some_and(|frame| {
                let (x, y) = (f64::from(local.x) as f32, f64::from(local.y) as f32);
                x >= frame.x && x < frame.x + frame.w && y >= frame.y && y < frame.y + frame.h
            })
        }).cloned()
    }
}

fn rgba(color: crate::window::Color) -> [f64; 4] {
    color
}

fn bounds(rect: &LogicalRect) -> Bounds {
    Bounds::new(point(rect.x as f32, rect.y as f32), size(rect.width as f32, rect.height as f32))
}
