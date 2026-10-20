use crate::history::{popover_header, HEADER_H, INSET};
use crate::settings::measure_overlay;
use crate::ui::{action_button, Kind};
use crate::window::{paint, site_host, site_icon, BrowserWindow};
use browser_core::{normalize_url, Bookmark};
use gpui::{div, prelude::*, px, ClickEvent, Context, FontWeight, SharedString};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::traits::control_sized::ControlSized;

const ROW_H: f32 = 30.;

impl BrowserWindow {
    /// Every bookmark, dropped from the title bar's bookmark button; hover a row to pin, edit or remove it.
    pub(crate) fn bookmarks_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let bookmarks = self.model.lock().bookmarks().unwrap_or_default();
        let body = if bookmarks.is_empty() { 56. } else { bookmarks.len() as f32 * ROW_H + 12. };
        let mut list = v_stack().w_full().p(px(6.));
        for bookmark in &bookmarks {
            list = list.child(self.bookmark_row(bookmark, cx));
        }
        let list = if bookmarks.is_empty() {
            div()
                .w_full()
                .h(px(56.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(paint.text(12.))
                .text_color(paint.muted)
                .child("No bookmarks yet. Press ⌘D on a page.")
        } else {
            list
        };
        v_stack()
            .id("bookmarks-popover")
            .absolute()
            .top(px(self.titlebar_h() + 6.))
            .right(px(8.))
            .w(px(340.))
            .h(px((HEADER_H + body + 2.).min(420.)))
            .rounded(px(10.))
            .overflow_hidden()
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, event, _, cx| this.dismiss_popover(event, cx)))
            .child(measure_overlay(10., cx))
            .child(popover_header(
                "Bookmarks",
                h_stack().child(icon_button("bookmarks-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.popover = None;
                    cx.notify();
                }))),
                paint,
            ))
            .child(div().flex_1().min_h(px(0.)).overflow_hidden().child(scroll_area("bookmarks-list").vertical().full_height(true).child(list)))
    }

    fn bookmark_row(&self, bookmark: &Bookmark, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let group = SharedString::from(format!("bookmark-{}", bookmark.id));
        let host = site_host(&bookmark.url);
        let title = if bookmark.title.is_empty() { host.clone() } else { bookmark.title.clone() };
        let open = bookmark.url.clone();
        let (pin, edit, remove) = (bookmark.clone(), bookmark.clone(), bookmark.id.clone());
        let pin_icon = if bookmark.pinned { Icons::drawing_pin_filled() } else { Icons::drawing_pin() };
        h_stack()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .w_full()
            .h(px(ROW_H))
            .gap(px(8.))
            .px(px(INSET - 6.))
            .rounded(px(6.))
            .items_center()
            .text_size(paint.text(12.5))
            .cursor_pointer()
            .hover(|style| style.bg(paint.secondary))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.popover = None;
                this.open_history_url(&open, cx);
            }))
            .child(div().flex_none().child(site_icon(&bookmark.url, 16., false)))
            .child(div().flex_1().min_w(px(0.)).truncate().child(title))
            .child(div().flex_none().max_w(px(110.)).truncate().text_size(paint.text(11.)).text_color(paint.muted).group_hover(group.clone(), |style| style.opacity(0.)).child(host))
            // The actions take the host's place on hover.
            .child(
                h_stack()
                    .absolute()
                    .right(px(4.))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .child(icon_button(SharedString::from(format!("bookmark-pin-{}", bookmark.id)), pin_icon).small().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            let _ = this.model.lock().save_bookmark(Some(&pin.id), &pin.url, &pin.title, !pin.pinned);
                            cx.notify();
                        },
                    )))
                    .child(icon_button(SharedString::from(format!("bookmark-edit-{}", bookmark.id)), Icons::pencil_1()).small().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.popover = None;
                            this.open_bookmark_editor(Some(&edit), cx);
                        },
                    )))
                    .child(icon_button(SharedString::from(format!("bookmark-remove-{}", bookmark.id)), Icons::cross_2()).small().on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            let _ = this.model.lock().delete_bookmark(&remove);
                            cx.notify();
                        },
                    ))),
            )
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
