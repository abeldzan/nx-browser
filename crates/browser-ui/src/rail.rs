//! The profile rail down the window's left side: a native sidebar over the GPUI view.

use crate::native::{Ink, Node, Pop};
use crate::window::{paint, profile_badge_node, BrowserWindow, Popover};
use browser_engine::LogicalRect;
use gpui::{px, Context, Window};

/// The sidebar's width, flush with the window's left edge, top and foot.
pub(crate) const RAIL_W: f32 = 50.;
const BADGE: f64 = 30.;
/// Bottom tools: their 30pt rows, the gaps and the separator.
const TOOLS_H: f64 = 3. * BADGE + 4. * 6. + 1.;
/// The rail's `Reorder` list, in `EngineEvent::ListReorder`.
pub(crate) const RAIL_LIST: u32 = 1;

impl BrowserWindow {
    /// Builds the rail for this frame, and records where its badges and Downloads button landed.
    pub(crate) fn sync_rail(&mut self, pop: &mut Pop, bars: &mut Vec<&'static str>, window: &Window, cx: &mut Context<Self>) {
        // A space or window torn off into its own OS window has no rail.
        if self.model.detached.is_some() {
            return;
        }
        let paint = paint(cx);
        let (profiles, active) = {
            let model = self.model.lock();
            (model.profiles.clone(), model.active_profile_id.clone())
        };
        let activity = self.profile_activity();
        let width = f64::from(RAIL_W);
        let height = f64::from(window.viewport_size().height).max(TOOLS_H + BADGE);
        // The window's close, minimize and zoom buttons, stacked at the sidebar's head.
        let lights = self.engine.place_traffic_lights((width / 2., 14.), true) + 10.;
        let mut switches = Vec::new();
        let badges = profiles
            .iter()
            .enumerate()
            .map(|(index, profile)| {
                let id = profile.id.clone();
                let switch = pop.act(move |this, cx| {
                    this.with_model(|model| {
                        let _ = model.switch_profile(&id);
                    });
                    this.refresh_url(cx);
                });
                switches.push((switch, profile.id.clone()));
                let mut badge = profile_badge_node(profile, profile.id == active, BADGE, paint);
                if let Node::Swatch { action, .. } = &mut badge {
                    *action = Some(switch);
                }
                // Privacy dots in the corner: green camera, orange microphone, grey sound.
                let state = activity.get(&profile.id).copied().unwrap_or_default();
                let dot = |rgb: [f64; 3]| Node::Image { path: None, letter: String::new(), rgb, size: 5., radius: 2.5 };
                let dots = [(state.camera, [0.19, 0.82, 0.35]), (state.microphone, [1., 0.62, 0.04]), (state.sound, [0.56, 0.56, 0.58])]
                    .into_iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, rgb)| dot(rgb))
                    .collect::<Vec<_>>();
                let id = profile.id.clone();
                let menu = pop.act(move |this, _| {
                    let (x, y) = this.engine.pointer().unwrap_or_default();
                    this.open_rail_menu(id.clone(), index, gpui::point(px(x as f32), px(y as f32)));
                });
                let child = if dots.is_empty() { badge } else { Node::Overlay { child: Box::new(badge), corner: Box::new(Node::Row { gap: 2., children: dots }) } };
                Node::Menu { action: menu, child: Box::new(child) }
            })
            .collect();
        let tool = |pop: &mut Pop, symbol: &'static str, on: bool, run: fn(&mut Self, &mut Context<Self>)| -> (u32, Node) {
            let action = pop.act(run);
            let item = Node::Item { height: BADGE, click: Some(action), drag: None, children: vec![Node::Center(Box::new(Node::Glyph { name: symbol, size: 15., ink: Ink::Secondary }))] };
            (action, if on { Node::Card { tint: Ink::Accent, child: Box::new(item) } } else { item })
        };
        let (_, new_profile) = tool(pop, "plus", false, |this, cx| this.open_new_profile(cx));
        let (downloads, mut downloads_tool) = tool(pop, "arrow.down.circle", self.popover == Some(Popover::Downloads), |this, cx| this.toggle_popover(Popover::Downloads, cx));
        // `web_downloads` holds exactly the downloads still being written.
        if !self.web_downloads.is_empty() {
            let [r, g, b] = crate::native::rgb(paint.accent);
            let dot = Node::Image { path: None, letter: String::new(), rgb: [r, g, b], size: 6., radius: 3. };
            downloads_tool = Node::Overlay { child: Box::new(downloads_tool), corner: Box::new(dot) };
        }
        let (_, settings) = tool(pop, "gearshape", self.settings_open, |this, cx| this.toggle_settings(cx));
        let content = Node::Height(
            height,
            Box::new(Node::Column {
                gap: 6.,
                pad: ((width - BADGE) / 2., 6.),
                children: vec![
                    // Room for the lights: the column's padding and gap come on top.
                    Node::Height(lights - 12., Box::new(Node::Spacer)),
                    Node::Scroll { max: height - TOOLS_H - 12. - lights, child: Box::new(Node::Reorder { list: RAIL_LIST, gap: 8., horizontal: false, children: badges }) },
                    Node::Spacer,
                    Node::Separator,
                    new_profile,
                    downloads_tool,
                    settings,
                ],
            }),
        );
        let frame = LogicalRect { x: 0., y: 0., width, height };
        self.engine.set_sidebar("rail", frame, &content);
        bars.push("rail");
        // Where the badges are, for a window dragged over the rail, and Downloads, for its popover.
        let rect = |frame: LogicalRect| gpui::Bounds::new(gpui::point(px(frame.x as f32), px(frame.y as f32)), gpui::size(px(frame.width as f32), px(frame.height as f32)));
        for (action, frame) in self.engine.bar_items("rail") {
            if action == downloads {
                self.popover_anchors.insert(Popover::Downloads, rect(frame));
            } else if let Some((_, id)) = switches.iter().find(|(switch, _)| *switch == action) {
                self.rail_badges.push((id.clone(), rect(frame)));
            }
        }
    }

    /// A profile badge dragged from slot `from` to `to`.
    pub(crate) fn reorder_rail(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let id = self.model.lock().profiles.get(from).map(|profile| profile.id.clone());
        if let Some(id) = id {
            self.with_model(|model| {
                let _ = model.move_profile(&id, to);
            });
            cx.notify();
        }
    }
}
