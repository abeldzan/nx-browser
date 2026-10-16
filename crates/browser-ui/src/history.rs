use crate::settings::measure_overlay;
use crate::window::{paint, site_host, site_icon, BrowserWindow, Paint};
use browser_core::{now_secs, HistoryEntry};
use gpui::{div, prelude::*, px, ClickEvent, Context, FontWeight, SharedString};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::elements::text_field::text_field;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::traits::control_sized::ControlSized;

const POPUP_LIMIT: usize = 12;
const MODAL_LIMIT: usize = 200;
/// Shared by the history and downloads popovers so their edges line up.
pub(crate) const INSET: f32 = 12.;
pub(crate) const HEADER_H: f32 = 36.;
const ROW_H: f32 = 30.;
const LABEL_H: f32 = 24.;
const FOOTER_H: f32 = 32.;

/// Title on the left, actions on the right, one hairline under it.
pub(crate) fn popover_header(title: &'static str, actions: gpui::Div, paint: Paint) -> gpui::Div {
    h_stack()
        .flex_none()
        .w_full()
        .h(px(HEADER_H))
        .pl(px(INSET))
        .pr(px(6.))
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(paint.border)
        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(title))
        .child(actions.items_center().gap(px(2.)))
}

/// A small text action for popover headers and footers.
pub(crate) fn text_action(id: &'static str, label: &'static str, paint: Paint) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(6.))
        .py(px(2.))
        .rounded(px(5.))
        .text_size(px(12.))
        .text_color(paint.muted)
        .cursor_pointer()
        .hover(|style| style.bg(paint.secondary).text_color(paint.fg))
        .child(label)
}

impl BrowserWindow {
    /// Recent visits, dropped from the title bar's history button.
    pub(crate) fn history_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let entries = self.model.lock().search_history("", POPUP_LIMIT).unwrap_or_default();
        let groups = group_entries(&entries).len() as f32;
        let body = if entries.is_empty() { 56. } else { entries.len() as f32 * ROW_H + groups * LABEL_H + 8. };
        let height = (HEADER_H + body + FOOTER_H + 2.).min(420.);
        v_stack()
            .id("history-popover")
            .absolute()
            .top(px(self.titlebar_h() + 6.))
            .right(px(8.))
            .w(px(340.))
            .h(px(height))
            .rounded(px(10.))
            .overflow_hidden()
            .bg(paint.surface)
            .border_1()
            .border_color(paint.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.history_open = false;
                cx.notify();
            }))
            .child(measure_overlay(10., cx))
            .child(popover_header(
                "History",
                h_stack().child(icon_button("history-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.history_open = false;
                    cx.notify();
                }))),
                paint,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(scroll_area("history-list").vertical().child(self.history_groups(&entries, "No history yet", cx))),
            )
            .child(
                h_stack()
                    .flex_none()
                    .w_full()
                    .h(px(FOOTER_H))
                    .px(px(INSET - 6.))
                    .items_center()
                    .border_t_1()
                    .border_color(paint.border)
                    .child(text_action("history-view-all", "Show all history", paint).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_history_modal(cx)))),
            )
    }

    /// Full history with a live title and URL search.
    pub(crate) fn history_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let query = self.history_query.read(cx).content().to_string();
        let entries = self.model.lock().search_history(&query, MODAL_LIMIT).unwrap_or_default();
        let empty = if query.trim().is_empty() { "No history yet" } else { "No matches" };
        let card = v_stack()
            .w(px(560.))
            .h(px(520.))
            .max_w_full()
            .max_h_full()
            .child(popover_header(
                "History",
                h_stack().child(icon_button("history-modal-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.close_history_modal(cx);
                }))),
                paint,
            ))
            .child(
                div().flex_none().w_full().px(px(INSET)).py(px(8.)).child(
                    text_field(&self.history_query, cx).placeholder("Search history").full_width(true),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(scroll_area("history-modal-list").vertical().child(self.history_groups(&entries, empty, cx))),
            );
        self.modal_shell("history-modal", card, Self::close_history_modal, cx)
    }

    pub(crate) fn open_history_modal(&mut self, cx: &mut Context<Self>) {
        self.history_query.update(cx, |input, cx| input.set_content_silent(String::new(), cx));
        self.history_open = false;
        self.history_modal = true;
        self.focus_history = true;
        cx.notify();
    }

    pub(crate) fn close_history_modal(&mut self, cx: &mut Context<Self>) {
        self.history_modal = false;
        cx.notify();
    }

    fn history_groups(&self, entries: &[HistoryEntry], empty: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let paint = paint(cx);
        if entries.is_empty() {
            return div().w_full().h(px(56.)).flex().items_center().justify_center().text_size(px(12.)).text_color(paint.muted).child(empty);
        }
        // Rows are inset by 6px so their hover fill sits inside the popover's edges, with text still on the 12px line.
        let mut column = v_stack().w_full().px(px(6.)).pb(px(8.));
        let mut last = String::new();
        for entry in entries {
            let label = day_label(entry.visited_at, now_secs());
            if label != last {
                column = column.child(
                    div()
                        .h(px(LABEL_H))
                        .px(px(INSET - 6.))
                        .flex()
                        .items_end()
                        .pb(px(4.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(paint.muted)
                        .child(label.clone()),
                );
                last = label;
            }
            column = column.child(self.history_row(entry, paint, cx));
        }
        column
    }

    fn history_row(&self, entry: &HistoryEntry, paint: Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let url = entry.url.clone();
        let host = site_host(&entry.url);
        let title = if entry.title.is_empty() { host.clone() } else { entry.title.clone() };
        h_stack()
            .id(SharedString::from(format!("history-row-{}", entry.id)))
            .w_full()
            .h(px(ROW_H))
            .gap(px(8.))
            .px(px(INSET - 6.))
            .rounded(px(6.))
            .items_center()
            .text_size(px(12.5))
            .cursor_pointer()
            .hover(|style| style.bg(paint.secondary))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_history_url(&url, cx)))
            .child(div().flex_none().child(site_icon(&entry.url, 16., false)))
            .child(div().flex_1().min_w(px(0.)).truncate().child(title))
            .child(div().flex_none().max_w(px(110.)).truncate().text_size(px(11.)).text_color(paint.muted).child(host))
            .child(h_stack().flex_none().w(px(52.)).justify_end().text_size(px(11.)).text_color(paint.muted).child(clock_label(entry.visited_at)))
    }
}

fn group_entries(entries: &[HistoryEntry]) -> Vec<&str> {
    let mut labels = Vec::new();
    let mut last = String::new();
    let now = now_secs();
    for entry in entries {
        let label = day_label(entry.visited_at, now);
        if label != last {
            labels.push("day");
            last = label;
        }
    }
    labels
}

fn day_label(visited: i64, now: i64) -> String {
    let Some(when) = local_tm(visited) else {
        return "Earlier".into();
    };
    let Some(today) = local_tm(now) else {
        return dated(&when);
    };
    if when.tm_year == today.tm_year && when.tm_yday == today.tm_yday {
        return "Today".into();
    }
    let midnight = now - i64::from(today.tm_hour) * 3600 - i64::from(today.tm_min) * 60 - i64::from(today.tm_sec);
    if let Some(yesterday) = local_tm(midnight.saturating_sub(1)) {
        if when.tm_year == yesterday.tm_year && when.tm_yday == yesterday.tm_yday {
            return "Yesterday".into();
        }
    }
    dated(&when)
}

fn dated(when: &libc::tm) -> String {
    const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let weekday = WEEKDAYS.get(when.tm_wday as usize).copied().unwrap_or("Day");
    let month = MONTHS.get(when.tm_mon as usize).copied().unwrap_or("");
    format!("{weekday}, {month} {}", when.tm_mday)
}

fn clock_label(visited: i64) -> String {
    let Some(when) = local_tm(visited) else {
        return String::new();
    };
    let suffix = if when.tm_hour >= 12 { "PM" } else { "AM" };
    let hour = match when.tm_hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{:02} {suffix}", when.tm_min)
}

fn local_tm(secs: i64) -> Option<libc::tm> {
    // SAFETY: `tm` is plain C data (all-zero is valid) and both pointers outlive the call.
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::localtime_r(&secs, &mut out) };
    if ok.is_null() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::{clock_label, day_label, local_tm};
    use browser_core::now_secs;

    #[test]
    fn labels_today_and_yesterday_in_local_time() {
        let now = now_secs();
        let today = local_tm(now).expect("local time");
        let start = now - i64::from(today.tm_hour) * 3600 - i64::from(today.tm_min) * 60 - i64::from(today.tm_sec);
        assert_eq!(day_label(start + 60, now), "Today");
        assert_eq!(day_label(start - 60, now), "Yesterday");
        let older = day_label(start - 86_400 * 3, now);
        assert!(older.contains(','), "{older}");
        assert!(!clock_label(now).is_empty());
    }
}
