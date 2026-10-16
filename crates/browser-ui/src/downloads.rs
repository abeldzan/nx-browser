use crate::settings::measure_overlay;
use crate::window::{paint, BrowserWindow};
use gpui::{div, prelude::*, px, relative, ClickEvent, Context, SharedString};
use crate::history::{popover_header, text_action, INSET};
use gpuikit::elements::icon_button::icon_button;
use gpuikit::elements::scroll_area::scroll_area;
use gpuikit::icons::Icons;
use gpuikit::layout::{h_stack, v_stack};
use gpuikit::theme::{ActiveTheme, Themeable};
use gpuikit::traits::control_sized::ControlSized;
use std::path::Path;

const ROW_H: f32 = 36.;

impl BrowserWindow {
    pub(crate) fn downloads_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paint = paint(cx);
        let danger = cx.theme().danger();
        let downloads = self.model.lock().downloads().unwrap_or_default();
        let empty = downloads.is_empty();
        // Rows are inset by 6px so their hover fill sits inside the popover, with text on the 12px line.
        let mut list = v_stack().w_full().px(px(6.)).py(px(4.));
        for entry in downloads {
            let name = Path::new(&entry.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| entry.url.clone());
            let progress = self.download_progress.get(&entry.id).copied();
            let done = entry.status == "done";
            let failed = entry.status.starts_with("error");
            let (detail, fraction) = if done {
                let size = progress.and_then(|(_, total)| total).or_else(|| std::fs::metadata(&entry.path).ok().map(|meta| meta.len()));
                (size.map(human_bytes).unwrap_or_else(|| "Done".into()), None)
            } else if failed {
                (entry.status.trim_start_matches("error: ").to_string(), None)
            } else if let Some((received, total)) = progress {
                match total {
                    Some(total) if total > 0 => (
                        format!("{} / {}", human_bytes(received), human_bytes(total)),
                        Some((received as f32 / total as f32).clamp(0., 1.)),
                    ),
                    _ => (human_bytes(received), Some(0.)),
                }
            } else {
                ("Interrupted".into(), None)
            };
            let path = entry.path.clone();
            let reveal = entry.path.clone();
            let group = SharedString::from(format!("download-{}", entry.id));
            list = list.child(
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
                    .text_size(px(12.5))
                    .when(done, |row| {
                        row.cursor_pointer()
                            .hover(|style| style.bg(paint.secondary))
                            .on_click(move |_: &ClickEvent, _, _| open_path(&path, false))
                    })
                    .child(Icons::file().flex_none().size(px(16.)).text_color(if failed { danger } else { paint.muted }))
                    .child(
                        v_stack()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(3.))
                            .child(div().truncate().when(failed, |name| name.text_color(paint.muted)).child(name))
                            .when_some(fraction, |column, fraction| {
                                column.child(
                                    div()
                                        .w_full()
                                        .h(px(3.))
                                        .rounded_full()
                                        .bg(paint.border)
                                        .child(div().h_full().w(relative(fraction)).rounded_full().bg(paint.accent)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(150.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(if failed { danger } else { paint.muted })
                            // The reveal button takes the size's place on hover.
                            .when(done, |detail| detail.group_hover(group.clone(), |style| style.opacity(0.)))
                            .child(detail),
                    )
                    .when(done, |row| {
                        row.child(
                            div().absolute().right(px(INSET - 10.)).opacity(0.).group_hover(group, |style| style.opacity(1.)).child(
                                icon_button(SharedString::from(format!("reveal-{}", entry.id)), Icons::magnifying_glass())
                                    .small()
                                    .on_click(move |_: &ClickEvent, _, cx| {
                                        cx.stop_propagation();
                                        open_path(&reveal, true);
                                    }),
                            ),
                        )
                    }),
            );
        }
        v_stack()
            .id("downloads-popover")
            .absolute()
            .left(px(64.))
            .bottom(px(12.))
            .w(px(340.))
            .max_h(px(420.))
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
                "Downloads",
                h_stack()
                    .when(!empty, |actions| {
                        actions.child(text_action("downloads-clear", "Clear", paint).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            let _ = this.model.lock().clear_finished_downloads();
                            cx.notify();
                        })))
                    })
                    .child(icon_button("downloads-close", Icons::cross_2()).small().on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.popover = None;
                        cx.notify();
                    }))),
                paint,
            ))
            .when(empty, |popover| {
                popover.child(
                    v_stack()
                        .h(px(88.))
                        .gap(px(6.))
                        .items_center()
                        .justify_center()
                        .text_size(px(12.))
                        .text_color(paint.muted)
                        .child(Icons::download().size(px(18.)).text_color(paint.muted))
                        .child("No downloads yet"),
                )
            })
            .when(!empty, |popover| {
                popover.child(div().flex_1().min_h(px(0.)).overflow_hidden().child(scroll_area("downloads-list").vertical().child(list)))
            })
    }
}

/// Opens a downloaded file, or reveals it in Finder.
fn open_path(path: &str, reveal: bool) {
    let mut command = std::process::Command::new("open");
    if reveal {
        command.arg("-R");
    }
    let _ = command.arg(path).spawn();
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn formats_sizes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
