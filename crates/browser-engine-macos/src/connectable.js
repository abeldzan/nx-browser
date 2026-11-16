// Pages an extension lists in `externally_connectable` message it with `chrome.runtime` (claude.ai hands Claude's
// extension its sign-in that way). WebKit gives every page `browser.runtime` and never `chrome`; this adds
// `chrome.runtime` on those pages only, so other sites don't take this for Chrome.
(() => {
  const patterns = __PATTERNS__;
  const g = globalThis;
  if (!g.browser || !g.browser.runtime || (g.chrome && g.chrome.runtime)) return;
  const escape = (text) => text.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*");
  // Chrome match patterns: `<all_urls>` or scheme://host/path, with `*` for any scheme (http/https), host or subdomain.
  const matches = (pattern) => {
    if (pattern === "<all_urls>") return true;
    const parts = /^(\*|[a-z-]+):\/\/([^/]*)(\/.*)$/.exec(pattern);
    if (!parts) return false;
    const scheme = parts[1] === "*" ? "https?" : escape(parts[1]);
    const host = parts[2] === "*" ? "[^/]*" : parts[2].startsWith("*.") ? `(?:[^/]*\\.)?${escape(parts[2].slice(2))}` : escape(parts[2]);
    return new RegExp(`^${scheme}://${host}(?::\\d+)?${escape(parts[3])}$`).test(location.href);
  };
  if (!patterns.some(matches)) return;
  g.chrome = Object.assign(g.chrome || {}, { runtime: g.browser.runtime });
})();
