pub fn tab_label(title: &str, url: &str) -> String {
    let label = if title.is_empty() || title == "New Tab" { url } else { title };
    // A tab with neither (an older blank-URL row) would collapse to an icon-only pill.
    let label = if label.is_empty() { "New Tab" } else { label };
    if label.chars().count() > 28 {
        format!("{}…", label.chars().take(27).collect::<String>())
    } else {
        label.to_string()
    }
}
