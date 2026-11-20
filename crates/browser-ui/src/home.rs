//! The new tab page: native views the engine puts where a blank tab's page would go.

use crate::native::{text, title, Initial, Ink, Node, Pop, Weight, HOME_QUERY};
use crate::window::{paint, site_host, site_image, BrowserWindow, Popover};
use browser_core::Bookmark;
use browser_engine::WebEngine;
use std::collections::HashSet;

/// Bookmarks along the top before the rest collapse into "N more".
const BAR_MARKS: usize = 12;

impl BrowserWindow {
    /// Builds the new tab page of every blank tab in this space, and drops the ones no longer blank.
    pub(crate) fn sync_homes(&mut self) {
        let blank = self.model.lock().session_tabs().into_iter().filter(|tab| crate::window::is_blank(&tab.url)).map(|tab| tab.id.clone()).collect::<Vec<_>>();
        // Terminal screens go where pages would, as new tab pages do.
        let terminals = self.model.lock().session_tabs().into_iter().filter(|tab| crate::terminal::is_terminal(&tab.url)).map(|tab| (tab.id.clone(), tab.url.clone())).collect::<Vec<_>>();
        self.engine.keep_homes(&blank.iter().cloned().chain(terminals.iter().map(|(id, _)| id.clone())).collect::<Vec<_>>());
        for (id, url) in &terminals {
            if let Some(screen) = crate::terminal::screen_for(id, url) {
                self.engine.set_screen(id, &screen);
            }
        }
        if blank.is_empty() {
            self.home_actions.clear();
            return;
        }
        let (bookmarks, recent) = {
            let model = self.model.lock();
            let bookmarks = model.bookmarks().unwrap_or_default();
            // Pinned sites already have a tile; recent skips them too.
            let mut hosts = bookmarks.iter().filter(|b| b.pinned).map(|b| site_host(&b.url)).collect::<HashSet<_>>();
            let recent = model.history().unwrap_or_default().into_iter().filter(|entry| hosts.insert(site_host(&entry.url))).take(8).map(|entry| (entry.url, entry.title)).collect::<Vec<_>>();
            (bookmarks, recent)
        };
        let (pinned, saved): (Vec<_>, Vec<_>) = bookmarks.into_iter().partition(|b| b.pinned);
        let [r, g, b] = crate::native::rgb(paint().bg);
        let mut pop = Pop::home();
        for id in blank {
            let (width, height) = self.page_sizes.get(&id).copied().unwrap_or((800., 600.));
            // The page stretches with its tab; it is rebuilt only every 32pt, to flow the tiles and bookmarks again.
            let width = ((width / 32.).floor() * 32.).max(32.);
            let page = home_page(&id, width, height, &pinned, &saved, &recent, &mut pop);
            self.engine.set_home(&id, width, [r, g, b, 1.], &page);
        }
        self.home_actions = pop.actions;
    }

    /// Arrows, Return, Tab and Esc in a new tab page's search field.
    pub(crate) fn home_key(&mut self, key: &str) {
        let count = self.suggestions.len();
        match key {
            "down" if count > 0 => self.suggestion = Some(self.suggestion.map_or(0, |at| (at + 1) % count)),
            "up" if count > 0 => self.suggestion = Some(self.suggestion.map_or(count - 1, |at| (at + count - 1) % count)),
            "enter" => {
                let picked = self.suggestion.and_then(|at| self.suggestions.get(at)).map(|(url, _, _)| url.clone());
                let typed = self.typed(HOME_QUERY);
                if let Some(target) = picked.or_else(|| (!typed.trim().is_empty()).then_some(typed)) {
                    self.set_typed(HOME_QUERY, String::new());
                    self.commit_navigation(&target);
                }
            }
            // Tab fills the picked suggestion, or the first, into the field to edit before going.
            "tab" if count > 0 => {
                let text = self.suggestions[self.suggestion.unwrap_or(0).min(count - 1)].0.clone();
                self.set_typed(HOME_QUERY, text.clone());
                self.update_suggestions(text);
            }
            "escape" if count > 0 => self.suggestions.clear(),
            "escape" => self.engine.focus_chrome(),
            _ => return,
        }
        self.notify();
    }
}

/// Tab `id`'s new tab page for a page area `width` × `height`.
fn home_page(id: &str, width: f64, height: f64, pinned: &[Bookmark], saved: &[Bookmark], recent: &[(String, String)], pop: &mut Pop) -> Node {
    let open = |pop: &mut Pop, url: &str| {
        let (id, url) = (id.to_string(), url.to_string());
        pop.act(move |this| {
            this.focus_window(&id);
            this.commit_navigation(&url);
        })
    };
    // A click anywhere on the pill, not just on the line of text, starts typing.
    let start = {
        let id = id.to_string();
        pop.act(move |this| this.engine.focus_home(&id))
    };
    let field = Node::Card {
        tint: Ink::Tertiary,
        child: Box::new(Node::Item {
            height: 44.,
            click: Some(start),
            drag: None,
            children: vec![
                Node::Glyph { name: "magnifyingglass", size: 15., ink: Ink::Secondary },
                Node::Search { field: HOME_QUERY, value: Initial(String::new()), placeholder: "Search or enter an address".into(), size: 16. },
            ],
        }),
    };
    let mut tiles = Vec::new();
    for bookmark in pinned.iter().rev() {
        let edit = bookmark.clone();
        let unpin = bookmark.clone();
        let tools = vec![
            Node::Tool { symbol: "pencil", tip: "Edit", action: pop.act(move |this| this.open_bookmark_editor(Some(&edit))), on: false },
            Node::Tool {
                symbol: "xmark",
                tip: "Unpin",
                action: pop.act(move |this| {
                    let _ = this.model.lock().save_bookmark(Some(&unpin.id), &unpin.url, &unpin.title, false);
                }),
                on: false,
            },
        ];
        tiles.push(tile(open(pop, &bookmark.url), &bookmark.url, &bookmark.title, tools));
    }
    tiles.push(Node::Width(
        88.,
        Box::new(Node::Item {
            height: 74.,
            click: Some(pop.act(|this| this.open_bookmark_editor(None))),
            drag: None,
            children: vec![Node::Column {
                gap: 6.,
                pad: (0., 0.),
                children: vec![
                    Node::Center(Box::new(Node::Glyph { name: "plus.circle.fill", size: 28., ink: Ink::Tertiary })),
                    Node::Center(Box::new(text("Add", 11., Weight::Regular, Ink::Secondary))),
                ],
            }],
        }),
    ));
    let mut column = vec![field, grid("Pinned", tiles)];
    if !recent.is_empty() {
        let tiles = recent
            .iter()
            .map(|(url, name)| {
                let (pin_url, pin_title) = (url.clone(), name.clone());
                let pin = pop.act(move |this| {
                    let _ = this.model.lock().save_bookmark(None, &pin_url, &pin_title, true);
                });
                tile(open(pop, url), url, name, vec![Node::Tool { symbol: "pin", tip: "Pin", action: pin, on: false }])
            })
            .collect();
        column.push(grid("Recent", tiles));
    }
    // Bookmarks as a bar of chips along the top, like a bookmarks bar.
    let mut chips = saved
        .iter()
        .take(BAR_MARKS)
        .map(|bookmark| {
            let name = if bookmark.title.is_empty() { site_host(&bookmark.url) } else { bookmark.title.clone() };
            let name = if name.chars().count() > 24 { format!("{}…", name.chars().take(23).collect::<String>()) } else { name };
            Node::Item { height: 26., click: Some(open(pop, &bookmark.url)), drag: None, children: vec![site_image(&bookmark.url, 14.), text(name, 12., Weight::Regular, Ink::Primary)] }
        })
        .collect::<Vec<_>>();
    if saved.len() > BAR_MARKS {
        let more = pop.act(|this| {
            this.popover = Some(Popover::Bookmarks);
            this.notify();
        });
        chips.push(Node::Item { height: 26., click: Some(more), drag: None, children: vec![text(format!("{} more", saved.len() - BAR_MARKS), 12., Weight::Regular, Ink::Secondary)] });
    }
    let content = (width - 48.).clamp(200., 640.);
    let top = (height * 0.14).round().max(16.);
    let mut page = Vec::new();
    if !chips.is_empty() {
        page.push(Node::Flow { gap: 6., center: true, children: chips });
    }
    // A `Height`, which a resize sets on the page in place.
    page.push(Node::Height(top, Box::new(Node::Spacer)));
    page.push(Node::Center(Box::new(Node::Width(content, Box::new(Node::Column { gap: 20., pad: (0., 0.), children: column })))));
    Node::Column { gap: 0., pad: (24., 12.), children: page }
}

/// A site tile: its icon over its name, opening `open` on click, with `tools` in the corner on hover.
fn tile(open: u32, url: &str, name: &str, tools: Vec<Node>) -> Node {
    let name = if name.is_empty() { site_host(url) } else { name.to_string() };
    Node::Width(
        88.,
        Box::new(Node::Item {
            height: 74.,
            click: Some(open),
            drag: None,
            children: vec![Node::Overlay {
                child: Box::new(Node::Column {
                    gap: 6.,
                    pad: (0., 0.),
                    children: vec![Node::Center(Box::new(site_image(url, 32.))), Node::Center(Box::new(title(name, 11., Weight::Regular, Ink::Primary)))],
                }),
                corner: Box::new(Node::Hover(true, Box::new(Node::Width(22., Box::new(Node::Column { gap: 0., pad: (0., 0.), children: tools }))))),
            }],
        }),
    )
}

/// A muted label over a wrapping grid of tiles.
fn grid(label: &str, tiles: Vec<Node>) -> Node {
    Node::Column { gap: 6., pad: (0., 0.), children: vec![crate::native::section(label), Node::Flow { gap: 8., center: false, children: tiles }] }
}
