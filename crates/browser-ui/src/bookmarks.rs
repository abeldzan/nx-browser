use crate::history::POPOVER_W;
use crate::native::{empty, header, title, Ink, Node, Pop, Weight};
use crate::ui::{action_button, Kind};
use crate::window::{paint, site_host, site_image, BrowserWindow};
use browser_core::{normalize_url, Bookmark};
use gpui::{div, prelude::*, px, Context, FontWeight};
use gpuikit::elements::text_field::text_field;
use gpuikit::layout::{h_stack, v_stack};

const ROW_H: f32 = 30.;

impl BrowserWindow {
    /// Every bookmark, dropped from the title bar's bookmark button; hover a row to pin, edit or remove it.
    pub(crate) fn bookmarks_popover(&self, pop: &mut Pop) -> Node {
        let bookmarks = self.model.lock().bookmarks().unwrap_or_default();
        let list = if bookmarks.is_empty() {
            empty(None, "No bookmarks yet. Press ⌘D on a page.", POPOVER_W)
        } else {
            let rows = bookmarks.iter().map(|bookmark| bookmark_row(bookmark, pop)).collect();
            Node::Scroll { max: 380., child: Box::new(Node::Column { gap: 0., pad: (6., 6.), children: rows }) }
        };
        Node::Column { gap: 0., pad: (0., 0.), children: vec![header("Bookmarks", Vec::new()), Node::Separator, list] }
    }

    /// `None` adds a new pinned site; otherwise edits that bookmark.
    pub(crate) fn open_bookmark_editor(&mut self, bookmark: Option<&Bookmark>, cx: &mut Context<Self>) {
        let (title, url) = bookmark.map(|b| (b.title.clone(), b.url.clone())).unwrap_or_default();
        self.bookmark_title.update(cx, |input, cx| input.set_content_silent(title, cx));
        self.bookmark_url.update(cx, |input, cx| input.set_content_silent(url, cx));
        self.bookmark_edit = Some(bookmark.map_or((None, true), |b| (Some(b.id.clone()), b.pinned)));
        cx.notify();
    }

    pub(crate) fn save_bookmark_edit(&mut self, cx: &mut Context<Self>) {
        let Some((id, pinned)) = self.bookmark_edit.clone() else {
            return;
        };
        let raw = self.bookmark_url.read(cx).content().trim().to_string();
        if raw.is_empty() {
            return;
        }
        let title = self.bookmark_title.read(cx).content().trim().to_string();
        let url = {
            let model = self.model.lock();
            normalize_url(&raw, model.settings.search_template(), model.settings.https_only)
        };
        let title = if title.is_empty() { site_host(&url) } else { title };
        let _ = self.model.lock().save_bookmark(id.as_deref(), &url, &title, pinned);
        self.bookmark_edit = None;
        cx.notify();
    }

    pub(crate) fn bookmark_editor(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let paint = paint(cx);
        let id = self.bookmark_edit.clone().and_then(|(id, _)| id);
        let label = |text: &'static str| div().text_size(paint.text(12.)).text_color(paint.muted).child(text);
        let card = v_stack()
            .w(px(400.))
            .max_w_full()
            .gap(px(8.))
            .p(px(24.))
            .child(div().pb(px(6.)).text_size(paint.text(15.)).font_weight(FontWeight::SEMIBOLD).child(if id.is_some() { "Edit site" } else { "Pin a site" }))
            .child(label("Name"))
            .child(text_field(&self.bookmark_title, cx).placeholder("Name").full_width(true))
            .child(label("Address"))
            .child(text_field(&self.bookmark_url, cx).placeholder("example.com").full_width(true))
            .child(
                h_stack()
                    .pt(px(16.))
                    .gap(px(10.))
                    .when_some(id, |row, id| {
                        row.child(action_button("bookmark-delete", "Remove", Kind::Danger, true, cx, move |this, cx| {
                            let _ = this.model.lock().delete_bookmark(&id);
                            this.bookmark_edit = None;
                            cx.notify();
                        }))
                    })
                    .child(div().flex_1())
                    .child(action_button("bookmark-cancel", "Cancel", Kind::Secondary, true, cx, |this, cx| {
                        this.bookmark_edit = None;
                        cx.notify();
                    }))
                    .child(action_button("bookmark-save", "Save", Kind::Primary, true, cx, |this, cx| this.save_bookmark_edit(cx))),
            );
        self.modal_shell("bookmark-editor", card, |this, cx| {
            this.bookmark_edit = None;
            cx.notify();
        }, cx)
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
            action: pop.act(move |this, _| {
                let _ = this.model.lock().save_bookmark(Some(&pin.id), &pin.url, &pin.title, !pin.pinned);
            }),
            on: bookmark.pinned,
        },
        Node::Tool {
            symbol: "pencil",
            tip: "Edit",
            action: pop.act(move |this, cx| {
                this.popover = None;
                this.open_bookmark_editor(Some(&edit), cx);
            }),
            on: false,
        },
        Node::Tool {
            symbol: "xmark",
            tip: "Remove",
            action: pop.act(move |this, _| {
                let _ = this.model.lock().delete_bookmark(&remove);
            }),
            on: false,
        },
    ];
    Node::Item {
        height: f64::from(ROW_H),
        click: Some(pop.act(move |this, cx| {
            this.popover = None;
            this.open_history_url(&open, cx);
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
