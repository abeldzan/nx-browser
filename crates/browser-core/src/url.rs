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
    if let Some(url) = expand_bang(trimmed) {
        return url;
    }
    if trimmed.contains(' ') || !trimmed.contains('.') {
        return search_template.replace("%s", &percent_encode(trimmed));
    }
    format!("https://{trimmed}")
}

/// DuckDuckGo-style bangs as (trigger, site, template); `%s` holds the query.
pub const BANGS: [(&str, &str, &str); 25] = [
    ("g", "Google", "https://www.google.com/search?q=%s"),
    ("gi", "Google Images", "https://www.google.com/search?tbm=isch&q=%s"),
    ("gm", "Google Maps", "https://www.google.com/maps/search/%s"),
    ("yt", "YouTube", "https://www.youtube.com/results?search_query=%s"),
    ("w", "Wikipedia", "https://en.wikipedia.org/w/index.php?search=%s"),
    ("gh", "GitHub", "https://github.com/search?q=%s"),
    ("r", "Reddit", "https://www.reddit.com/search/?q=%s"),
    ("a", "Amazon", "https://www.amazon.com/s?k=%s"),
    ("so", "Stack Overflow", "https://stackoverflow.com/search?q=%s"),
    ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q=%s"),
    ("b", "Bing", "https://www.bing.com/search?q=%s"),
    ("brave", "Brave Search", "https://search.brave.com/search?q=%s"),
    ("mdn", "MDN Web Docs", "https://developer.mozilla.org/en-US/search?q=%s"),
    ("npm", "npm", "https://www.npmjs.com/search?q=%s"),
    ("crates", "crates.io", "https://crates.io/search?q=%s"),
    ("docs", "docs.rs", "https://docs.rs/releases/search?query=%s"),
    ("x", "X", "https://x.com/search?q=%s"),
    ("hn", "Hacker News", "https://hn.algolia.com/?q=%s"),
    ("imdb", "IMDb", "https://www.imdb.com/find/?q=%s"),
    ("tw", "Twitch", "https://www.twitch.tv/search?term=%s"),
    ("osm", "OpenStreetMap", "https://www.openstreetmap.org/search?query=%s"),
    ("tr", "Google Translate", "https://translate.google.com/?text=%s"),
    ("wa", "Wolfram Alpha", "https://www.wolframalpha.com/input?i=%s"),
    ("cgpt", "ChatGPT", "https://chatgpt.com/?q=%s"),
    ("claude", "Claude", "https://claude.ai/new?q=%s"),
];

/// The bang in `input` (`!yt lofi` or `lofi !yt`) as (trigger, site, template) and the query left
/// once the bang word is removed. The first known bang word wins.
pub fn find_bang(input: &str) -> Option<(&'static (&'static str, &'static str, &'static str), String)> {
    let words = input.split_whitespace().collect::<Vec<_>>();
    let (at, bang) = words.iter().enumerate().find_map(|(at, word)| {
        let trigger = word.strip_prefix('!')?.to_ascii_lowercase();
        BANGS.iter().find(|(t, ..)| *t == trigger).map(|bang| (at, bang))
    })?;
    let query = words.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, word)| *word).collect::<Vec<_>>().join(" ");
    Some((bang, query))
}

/// The scheme and host of a bang template, i.e. the site's front page.
pub fn bang_home(template: &str) -> &str {
    let host_end = template[8..].find('/').map_or(template.len(), |at| at + 8);
    &template[..host_end]
}

fn expand_bang(input: &str) -> Option<String> {
    let ((_, _, template), query) = find_bang(input)?;
    if query.is_empty() {
        return Some(bang_home(template).to_string());
    }
    Some(template.replace("%s", &percent_encode(&query)))
}

pub(crate) fn percent_encode(value: &str) -> String {
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

    #[test]
    fn bangs_pick_the_site() {
        assert_eq!(normalize_url("!yt lofi beats", DDG, false), "https://www.youtube.com/results?search_query=lofi+beats");
        assert_eq!(normalize_url("rust lang !G", DDG, false), "https://www.google.com/search?q=rust+lang");
        assert_eq!(normalize_url("!gh", DDG, false), "https://github.com");
        assert_eq!(normalize_url("!nope cats", DDG, false), "https://duckduckgo.com/?q=%21nope+cats");
        assert_eq!(normalize_url("hey!", DDG, false), "https://duckduckgo.com/?q=hey%21");
    }
}
