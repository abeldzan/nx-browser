use crate::native::{button, empty, field, header, section, sheet, text, title, Ink, Key, Node, Pop, Style, Weight, HISTORY_QUERY};
use crate::window::{site_host, site_image, BrowserWindow};
use browser_core::{now_secs, HistoryEntry};

const POPUP_LIMIT: usize = 12;
/// Width of the native popovers.
pub(crate) const POPOVER_W: f64 = 340.;
const MODAL_LIMIT: usize = 200;
const ROW_H: f32 = 30.;

impl BrowserWindow {
    /// Recent visits, dropped from the title bar's history button.
    pub(crate) fn history_popover(&self, pop: &mut Pop) -> Node {
        let entries = self.model.lock().search_history("", POPUP_LIMIT).unwrap_or_default();
        let list = if entries.is_empty() {
            empty(None, "No history yet")
        } else {
            Node::Scroll { max: 340., child: Box::new(Node::Column { gap: 0., pad: (6., 4.), children: history_rows(&entries, pop) }) }
        };
        let all = pop.act(|this| this.open_history_modal());
        Node::Column {
            gap: 0.,
            pad: (0., 0.),
            children: vec![
                header("History", Vec::new()),
                Node::Separator,
                list,
                Node::Separator,
                Node::Column { gap: 0., pad: (8., 8.), children: vec![Node::Row { gap: 0., children: vec![Node::Link { title: "Show all history".into(), action: all }, Node::Spacer] }] },
            ],
        }
    }

    /// Full history with a live title and URL search.
    pub(crate) fn history_sheet(&self, pop: &mut Pop) -> Node {
        let query = self.typed(HISTORY_QUERY);
        let entries = self.model.lock().search_history(&query, MODAL_LIMIT).unwrap_or_default();
        let list = if entries.is_empty() {
            empty(None, if query.trim().is_empty() { "No history yet" } else { "No matches" })
        } else {
            Node::Scroll { max: 380., child: Box::new(Node::Column { gap: 0., pad: (0., 0.), children: history_rows(&entries, pop) }) }
        };
        let done = pop.act(|this| this.close_history_modal());
        // Return in the search box opens nothing; the list is clicked.
        let search = pop.act(|_| {});
        sheet("History", vec![field(HISTORY_QUERY, "", "Search history", search), list], vec![button("Done", done, Style::Primary, Key::Escape)])
    }

    pub(crate) fn open_history_modal(&mut self) {
        self.popover_texts.remove(&HISTORY_QUERY);
        self.popover = None;
        self.history_modal = true;
        self.notify();
    }

    pub(crate) fn close_history_modal(&mut self) {
        self.history_modal = false;
        self.notify();
    }

}

/// History rows under day labels, each opening its page.
fn history_rows(entries: &[HistoryEntry], pop: &mut Pop) -> Vec<Node> {
    let mut rows = Vec::new();
    let mut last = String::new();
    for entry in entries {
        let label = day_label(entry.visited_at, now_secs());
        if label != last {
            rows.push(section(&label));
            last = label;
        }
        let url = entry.url.clone();
        let host = site_host(&entry.url);
        let name = if entry.title.is_empty() { host.clone() } else { entry.title.clone() };
        rows.push(Node::Item {
            height: f64::from(ROW_H),
            click: Some(pop.act(move |this| this.open_history_url(&url))),
            drag: None,
            children: vec![
                site_image(&entry.url, 16.),
                title(name, 12.5, Weight::Regular, Ink::Primary),
                Node::Spacer,
                title(host, 11., Weight::Regular, Ink::Secondary),
                text(clock_label(entry.visited_at), 11., Weight::Regular, Ink::Secondary),
            ],
        });
    }
    rows
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
