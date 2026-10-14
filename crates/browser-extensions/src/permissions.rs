use crate::scripting::matches_url;

pub fn host_allowed(patterns: &[String], url: &str) -> bool {
    patterns.iter().any(|pattern| matches_url(pattern, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_permission_gates_the_page() {
        let patterns = vec!["https://example.com/*".into()];
        assert!(host_allowed(&patterns, "https://example.com/inbox"));
        assert!(!host_allowed(&patterns, "https://other.test/"));
    }
}
