use crate::window::{paint, BrowserWindow};
use gpui::{div, prelude::*, px, ClickEvent, Context, ElementId, FontWeight, SharedString};
use gpuikit::theme::{ActiveTheme, Themeable};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// The app's standard 32px button; gpuikit's tallest is 24px, which reads as cramped in dialogs.
pub(crate) fn action_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Kind,
    enabled: bool,
    cx: &mut Context<BrowserWindow>,
    on_click: impl Fn(&mut BrowserWindow, &mut Context<BrowserWindow>) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let paint = paint(cx);
    let danger = cx.theme().danger();
    let (bg, fg, border) = match kind {
        Kind::Primary => (paint.accent, paint.bg, paint.accent),
        Kind::Secondary => (paint.secondary, paint.fg, paint.border),
        Kind::Danger => (danger, gpui::white(), danger),
    };
    div()
        .id(id.into())
        .h(px(32.))
        .px(px(14.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .child(label.into())
        .when(!enabled, |button| button.opacity(0.45))
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(0.88))
                .active(|style| style.opacity(0.75))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| on_click(this, cx)))
        })
}
