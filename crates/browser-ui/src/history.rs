use crate::settings::measure_overlay;
use crate::window::{paint, site_host, site_icon, BrowserWindow};
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

impl BrowserWindow {
    /// Recent visits, dropped from the title bar's history button.
    pub(crate) fn history_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let entries = self.model.lock().expect("model").search_history("", POPUP_LIMIT).unwrap_or_default();
        let groups = group_entries(&entries);
        let empty = if entries.is_empty() { 72. } else { 0. };
        let height = (112. + entries.len() as f32 * 48. + groups.len() as f32 * 26. + empty).min(440.);
        v_stack()
            .id("history-popover")
            .absolute()
            .top(px(self.titlebar_h() + 6.))
            .right(px(8.))
            .w(px(380.))
            .h(px(height))
            .rounded(px(12.))
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
            .child(measure_overlay(12., cx))
            .child(
                h_stack()
                    .w_full()
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .items_center()
                    .justify_between()
                    .child(
                        v_stack()
                            .gap(px(2.))
                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("History"))
                            .child(div().text_size(px(12.)).text_color(paint.muted).child("Recent")),
                    )
                    .child(icon_button("history-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.history_open = false;
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .px(px(6.))
                    .child(scroll_area("history-list").vertical().child(self.history_groups(&entries, "No history yet", cx))),
            )
            .child(
                h_stack()
                    .w_full()
                    .px(px(12.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(paint.border)
                    .child(
                        div()
                            .id("history-view-all")
                            .text_size(px(12.))
                            .text_color(paint.muted)
                            .cursor_pointer()
                            .hover(|style| style.text_color(paint.fg))
                            .child("View all history")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_history_modal(cx))),
                    ),
            )
    }

    /// Full history with a live title and URL search.
    pub(crate) fn history_modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let query = self.history_query.read(cx).content().to_string();
        let entries = self.model.lock().expect("model").search_history(&query, MODAL_LIMIT).unwrap_or_default();
        let empty = if query.trim().is_empty() { "No history yet" } else { "No matches" };
        let card = v_stack()
            .w(px(560.))
            .h(px(520.))
            .max_w_full()
            .max_h_full()
            .child(
                h_stack()
                    .w_full()
                    .px(px(16.))
                    .py(px(12.))
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(paint.border)
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("History"))
                    .child(icon_button("history-modal-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_history_modal(cx);
                    }))),
            )
            .child(
                div().w_full().px(px(16.)).py(px(10.)).child(
                    text_field(&self.history_query, cx).placeholder("Search history").full_width(true),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .px(px(8.))
                    .pb(px(8.))
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
            return div().px(px(12.)).py(px(20.)).text_color(paint.muted).child(empty);
        }
        let mut column = v_stack().w_full().gap(px(2.)).pb(px(6.));
        let mut last = String::new();
        for entry in entries {
            let label = day_label(entry.visited_at, now_secs());
            if label != last {
                column = column.child(
                    div()
                        .px(px(10.))
                        .pt(px(10.))
                        .pb(px(2.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(paint.muted)
                        .child(label.clone()),
                );
                last = label;
            }
            column = column.child(self.history_row(entry, paint, cx));
        }
        column
    }

    fn history_row(&self, entry: &HistoryEntry, paint: crate::window::Paint, cx: &mut Context<Self>) -> impl IntoElement {
        let url = entry.url.clone();
        let title = if entry.title.is_empty() { site_host(&entry.url) } else { entry.title.clone() };
        let when = clock_label(entry.visited_at);
        h_stack()
            .id(SharedString::from(format!("history-row-{}", entry.id)))
            .w_full()
            .gap(px(10.))
            .px(px(8.))
            .py(px(7.))
            .rounded(px(8.))
            .items_center()
            .cursor_pointer()
            .hover(|style| style.bg(paint.secondary))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.open_history_url(&url, cx)))
            .child(site_icon(&entry.url, 28., false))
            .child(
                v_stack()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(1.))
                    .child(div().truncate().child(title))
                    .child(div().text_size(px(11.)).text_color(paint.muted).truncate().child(site_host(&entry.url))),
            )
            .child(div().flex_none().text_size(px(11.)).text_color(paint.muted).child(when))
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
    if when.year == today.year && when.yday == today.yday {
        return "Today".into();
    }
    let midnight = now - i64::from(today.hour) * 3600 - i64::from(today.min) * 60 - i64::from(today.sec);
    if let Some(yesterday) = local_tm(midnight.saturating_sub(1)) {
        if when.year == yesterday.year && when.yday == yesterday.yday {
            return "Yesterday".into();
        }
    }
    dated(&when)
}

fn dated(when: &LocalTime) -> String {
    const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let weekday = WEEKDAYS.get(when.wday as usize).copied().unwrap_or("Day");
    let month = MONTHS.get(when.mon as usize).copied().unwrap_or("");
    format!("{weekday}, {month} {}", when.mday)
}

fn clock_label(visited: i64) -> String {
    let Some(when) = local_tm(visited) else {
        return String::new();
    };
    let suffix = if when.hour >= 12 { "PM" } else { "AM" };
    let hour = match when.hour % 12 {
        0 => 12,
        hour => hour,
    };
    format!("{hour}:{:02} {suffix}", when.min)
}

#[repr(C)]
struct LocalTime {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const i8,
}

fn local_tm(secs: i64) -> Option<LocalTime> {
    let mut out = LocalTime {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        wday: 0,
        yday: 0,
        isdst: 0,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    let ok = unsafe { localtime_r(&secs, &mut out) };
    if ok.is_null() { None } else { Some(out) }
}

extern "C" {
    fn localtime_r(timep: *const i64, result: *mut LocalTime) -> *mut LocalTime;
}

#[cfg(test)]
mod tests {
    use super::{clock_label, day_label, local_tm};
    use browser_core::now_secs;

    #[test]
    fn labels_today_and_yesterday_in_local_time() {
        let now = now_secs();
        let today = local_tm(now).expect("local time");
        let start = now - i64::from(today.hour) * 3600 - i64::from(today.min) * 60 - i64::from(today.sec);
        assert_eq!(day_label(start + 60, now), "Today");
        assert_eq!(day_label(start - 60, now), "Yesterday");
        let older = day_label(start - 86_400 * 3, now);
        assert!(older.contains(','), "{older}");
        assert!(!clock_label(now).is_empty());
    }
}
