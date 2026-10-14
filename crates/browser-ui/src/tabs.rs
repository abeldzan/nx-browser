pub fn tab_label(title: &str, url: &str) -> String {
    let label = if title.is_empty() || title == "New Tab" { url } else { title };
    if label.chars().count() > 28 {
        format!("{}…", label.chars().take(27).collect::<String>())
    } else {
        label.to_string()
    }
}
