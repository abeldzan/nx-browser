/// `search_template` holds `%s` for the query; `https_only` upgrades plain http.
pub fn normalize_url(input: &str, search_template: &str, https_only: bool) -> String {
    let trimmed = input.trim();
    if https_only {
        if let Some(rest) = trimmed.strip_prefix("http://") {
            return format!("https://{rest}");
        }
    }
    if trimmed.is_empty() {
        return "nxbrowser://home".into();
    }
    if trimmed.starts_with("nxbrowser://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("about:")
        || trimmed.starts_with("data:")
        || trimmed.starts_with("file://")
    {
        return trimmed.to_string();
    }
    if trimmed.contains(' ') || !trimmed.contains('.') {
        return search_template.replace("%s", &percent_encode(trimmed));
    }
    format!("https://{trimmed}")
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: &str = "https://duckduckgo.com/?q=%s";

    #[test]
    fn adds_https_and_keeps_special_schemes() {
        assert_eq!(normalize_url("example.com", DDG, false), "https://example.com");
        assert_eq!(normalize_url("https://example.com/a", DDG, false), "https://example.com/a");
        assert_eq!(normalize_url("nxbrowser://home", DDG, false), "nxbrowser://home");
        assert!(normalize_url("rust browser", DDG, false).starts_with("https://duckduckgo.com/?q="));
    }

    #[test]
    fn uses_search_template_and_https_only() {
        assert_eq!(
            normalize_url("rust lang", "https://www.google.com/search?q=%s", false),
            "https://www.google.com/search?q=rust+lang"
        );
        assert_eq!(normalize_url("http://example.com", DDG, true), "https://example.com");
        assert_eq!(normalize_url("http://example.com", DDG, false), "http://example.com");
    }
}
