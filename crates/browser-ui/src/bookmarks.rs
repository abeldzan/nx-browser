use crate::native::{button, empty, field, header, label, sheet, title, Ink, Key, Node, Pop, Style, Weight, BOOKMARK_TITLE, BOOKMARK_URL};
use crate::window::{site_host, site_image, BrowserWindow};
use browser_core::{normalize_url, Bookmark};

const ROW_H: f32 = 30.;

impl BrowserWindow {
    /// Every bookmark, dropped from the title bar's bookmark button; hover a row to pin, edit or remove it.
    pub(crate) fn bookmarks_popover(&self, pop: &mut Pop) -> Node {
        let bookmarks = self.model.lock().bookmarks().unwrap_or_default();
        let list = if bookmarks.is_empty() {
            empty(None, "No bookmarks yet. Press ⌘D on a page.")
        } else {
            let rows = bookmarks.iter().map(|bookmark| bookmark_row(bookmark, pop)).collect();
            Node::Scroll { max: 380., child: Box::new(Node::Column { gap: 0., pad: (6., 6.), children: rows }) }
        };
        Node::Column { gap: 0., pad: (0., 0.), children: vec![header("Bookmarks", Vec::new()), Node::Separator, list] }
    }

    /// `None` adds a new pinned site; otherwise edits that bookmark.
    pub(crate) fn open_bookmark_editor(&mut self, bookmark: Option<&Bookmark>) {
        let (title, url) = bookmark.map(|b| (b.title.clone(), b.url.clone())).unwrap_or_default();
        self.popover_texts.insert(BOOKMARK_TITLE, title);
        self.popover_texts.insert(BOOKMARK_URL, url);
        self.bookmark_edit = Some(bookmark.map_or((None, true), |b| (Some(b.id.clone()), b.pinned)));
        self.notify();
    }

    pub(crate) fn save_bookmark_edit(&mut self) {
        let Some((id, pinned)) = self.bookmark_edit.clone() else {
            return;
        };
        let raw = self.typed(BOOKMARK_URL).trim().to_string();
        if raw.is_empty() {
            return;
        }
        let title = self.typed(BOOKMARK_TITLE).trim().to_string();
        let url = {
            let model = self.model.lock();
            normalize_url(&raw, model.settings.search_template(), model.settings.https_only)
        };
        let title = if title.is_empty() { site_host(&url) } else { title };
        let _ = self.model.lock().save_bookmark(id.as_deref(), &url, &title, pinned);
        self.bookmark_edit = None;
        self.notify();
    }

    pub(crate) fn bookmark_sheet(&self, pop: &mut Pop) -> Node {
        let id = self.bookmark_edit.clone().and_then(|(id, _)| id);
        let save = pop.act(|this| this.save_bookmark_edit());
        let cancel = pop.act(|this| this.bookmark_edit = None);
        let mut buttons = Vec::new();
        if let Some(id) = &id {
            let id = id.clone();
            buttons.push(button(
                "Remove",
                pop.act(move |this| {
                    let _ = this.model.lock().delete_bookmark(&id);
                    this.bookmark_edit = None;
                }),
                Style::Danger,
                Key::None,
            ));
            buttons.push(Node::Spacer);
        }
        buttons.push(button("Cancel", cancel, Style::Plain, Key::Escape));
        buttons.push(button("Save", save, Style::Primary, Key::Return));
        sheet(
            if id.is_some() { "Edit site" } else { "Pin a site" },
            vec![
                label("Name"),
                field(BOOKMARK_TITLE, self.typed(BOOKMARK_TITLE), "Name", save),
                label("Address"),
                field(BOOKMARK_URL, self.typed(BOOKMARK_URL), "example.com", save),
            ],
            buttons,
        )
    }
}

fn bookmark_row(bookmark: &Bookmark, pop: &mut Pop) -> Node {
    let host = site_host(&bookmark.url);
    let name = if bookmark.title.is_empty() { host.clone() } else { bookmark.title.clone() };
    let (open, pin, edit, remove) = (bookmark.url.clone(), bookmark.clone(), bookmark.clone(), bookmark.id.clone());
    let actions = vec![
        Node::Tool {
            symbol: if bookmark.pinned { "pin.fill" } else { "pin" },
            tip: if bookmark.pinned { "Unpin from Start Page" } else { "Pin to Start Page" },
            action: pop.act(move |this| {
                let _ = this.model.lock().save_bookmark(Some(&pin.id), &pin.url, &pin.title, !pin.pinned);
            }),
            on: bookmark.pinned,
        },
        Node::Tool {
            symbol: "pencil",
            tip: "Edit",
            action: pop.act(move |this| {
                this.popover = None;
                this.open_bookmark_editor(Some(&edit));
            }),
            on: false,
        },
        Node::Tool {
            symbol: "xmark",
            tip: "Remove",
            action: pop.act(move |this| {
                let _ = this.model.lock().delete_bookmark(&remove);
            }),
            on: false,
        },
    ];
    Node::Item {
        height: f64::from(ROW_H),
        click: Some(pop.act(move |this| {
            this.popover = None;
            this.open_history_url(&open);
        })),
        drag: None,
        children: vec![
            site_image(&bookmark.url, 16.),
            title(name, 12.5, Weight::Regular, Ink::Primary),
            Node::Spacer,
            // The actions take the host's place on hover.
            Node::Hover(false, Box::new(title(host, 11., Weight::Regular, Ink::Secondary))),
            Node::Hover(true, Box::new(Node::Row { gap: 0., children: actions })),
        ],
    }
}
