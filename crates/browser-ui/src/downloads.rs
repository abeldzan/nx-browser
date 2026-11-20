use crate::native::{empty, header, text, title, Ink, Node, Pop, Weight};
use crate::window::BrowserWindow;
use std::path::Path;

const ROW_H: f32 = 36.;

impl BrowserWindow {
    pub(crate) fn downloads_popover(&self, pop: &mut Pop) -> Node {
        let downloads = self.model.lock().downloads().unwrap_or_default();
        let empty_list = downloads.is_empty();
        let mut rows = Vec::new();
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
                        Some((received as f64 / total as f64).clamp(0., 1.)),
                    ),
                    _ => (human_bytes(received), Some(0.)),
                }
            } else {
                ("Interrupted".into(), None)
            };
            let mut label = vec![title(name, 12.5, Weight::Regular, if failed { Ink::Secondary } else { Ink::Primary })];
            label.extend(fraction.map(Node::Progress));
            let detail = text(detail, 11., Weight::Regular, if failed { Ink::Danger } else { Ink::Secondary });
            let mut children = vec![
                Node::Glyph { name: "doc", size: 14., ink: if failed { Ink::Danger } else { Ink::Secondary } },
                Node::Column { gap: 3., pad: (0., 0.), children: label },
                // The reveal button takes the size's place on hover.
                if done { Node::Hover(false, Box::new(detail)) } else { detail },
            ];
            if fraction.is_some() {
                let id = entry.id.clone();
                children.push(Node::Tool { symbol: "xmark", tip: "Stop", action: pop.act(move |this| this.cancel_download(&id)), on: false });
            }
            if done {
                let reveal = entry.path.clone();
                children.push(Node::Hover(true, Box::new(Node::Tool { symbol: "magnifyingglass", tip: "Show in Finder", action: pop.act(move |_| open_path(&reveal, true)), on: false })));
            }
            let path = entry.path.clone();
            rows.push(Node::Item {
                height: f64::from(ROW_H),
                click: done.then(|| pop.act(move |_| open_path(&path, false))),
                // Dragging the row out drops the file on a page (or anywhere) as from Finder.
                drag: done.then(|| entry.path.clone().into()),
                children,
            });
        }
        let clear = pop.act(|this| {
            let _ = this.model.lock().clear_finished_downloads();
        });
        let actions = if empty_list { Vec::new() } else { vec![Node::Link { title: "Clear".into(), action: clear }] };
        let list = if empty_list {
            empty(Some("arrow.down.circle"), "No downloads yet")
        } else {
            Node::Scroll { max: 380., child: Box::new(Node::Column { gap: 0., pad: (6., 4.), children: rows }) }
        };
        Node::Column { gap: 0., pad: (0., 0.), children: vec![header("Downloads", actions), Node::Separator, list] }
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
