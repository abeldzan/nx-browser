pub fn matches_url(pattern: &str, url: &str) -> bool {
    if pattern == "<all_urls>" || pattern == "*://*/*" {
        return url.starts_with("http://") || url.starts_with("https://");
    }
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    let (host_pattern, path_pattern) = rest.split_once('/').unwrap_or((rest, "*"));
    let Some((url_scheme, url_rest)) = url.split_once("://") else {
        return false;
    };
    if scheme != "*" && scheme != url_scheme {
        return false;
    }
    let (url_host, url_path) = match url_rest.split_once('/') {
        Some((host, path)) => (host.split('?').next().unwrap_or(host), format!("/{path}")),
        None => (url_rest.split('?').next().unwrap_or(url_rest), "/".into()),
    };
    let host_ok = if host_pattern == "*" {
        true
    } else if let Some(suffix) = host_pattern.strip_prefix("*.") {
        url_host == suffix || url_host.ends_with(&format!(".{suffix}"))
    } else {
        url_host == host_pattern
    };
    if !host_ok {
        return false;
    }
    let path_pattern = if path_pattern.is_empty() {
        "*"
    } else if path_pattern.starts_with('/') {
        path_pattern
    } else {
        return url_path.starts_with(&format!("/{path_pattern}"))
            || path_pattern == "*"
            || format!("/{path_pattern}") == url_path;
    };
    if path_pattern == "/*" || path_pattern == "*" {
        return true;
    }
    if let Some(prefix) = path_pattern.strip_suffix('*') {
        url_path.starts_with(prefix)
    } else {
        url_path == path_pattern
    }
}

pub fn bridge_and_content(extension_id: &str, host_permissions: &[String], content: &str) -> String {
    let patterns = serde_json::to_string(host_permissions).unwrap_or_else(|_| "[]".into());
    format!(
        r#"
(() => {{
  const extensionId = {extension_id:?};
  const patterns = {patterns};
  function nxMatch(url, pattern) {{
    if (pattern === "<all_urls>" || pattern === "*://*/*") return url.startsWith("http://") || url.startsWith("https://");
    try {{
      const u = new URL(url);
      const parts = pattern.split("://");
      if (parts.length !== 2) return false;
      const [scheme, rest] = parts;
      if (scheme !== "*" && scheme !== u.protocol.replace(":", "")) return false;
      const slash = rest.indexOf("/");
      const host = slash === -1 ? rest : rest.slice(0, slash);
      const path = slash === -1 ? "/*" : rest.slice(slash);
      const urlHost = u.host;
      let hostOk = host === "*" || host === urlHost;
      if (!hostOk && host.startsWith("*.")) {{
        const suffix = host.slice(2);
        hostOk = urlHost === suffix || urlHost.endsWith("." + suffix);
      }}
      if (!hostOk) return false;
      if (path === "/*" || path === "*") return true;
      const target = u.pathname + u.search;
      if (path.endsWith("*")) return target.startsWith(path.slice(0, -1));
      return target === path;
    }} catch (e) {{ return false; }}
  }}
  if (!patterns.some((pattern) => nxMatch(location.href, pattern))) return;
  const pending = new Map();
  let nextId = 1;
  function call(method, args) {{
    const id = nextId++;
    return new Promise((resolve, reject) => {{
      pending.set(id, {{ resolve, reject }});
      window.webkit.messageHandlers.nxbrowser.postMessage(JSON.stringify({{ id, extensionId, method, args }}));
    }});
  }}
  globalThis.__nxResolve = (id, ok, value) => {{
    const pendingCall = pending.get(id);
    if (!pendingCall) return;
    pending.delete(id);
    if (ok) pendingCall.resolve(value);
    else pendingCall.reject(new Error(String(value || "extension error")));
  }};
  const listeners = [];
  globalThis.__nxDispatch = (message) => listeners.forEach((fn) => fn(message));
  const browser = {{
    storage: {{ local: {{
      get: (key) => call("storage.get", {{ key: typeof key === "string" ? key : null }}),
      set: (items) => call("storage.set", {{ items }}),
    }} }},
    tabs: {{
      query: () => call("tabs.query", {{}}),
      create: (opts) => call("tabs.create", opts || {{}}),
      remove: (tabId) => call("tabs.remove", {{ tabId }}),
    }},
    runtime: {{
      sendMessage: (message) => call("runtime.sendMessage", {{ message }}),
      onMessage: {{ addListener(fn) {{ listeners.push(fn); }} }},
    }},
  }};
  globalThis.browser = browser;
  globalThis.chrome = browser;
  {content}
}})();
"#
    )
}

pub fn background_prelude() -> &'static str {
    r#"
const listeners = [];
function call(method, args) {
  const raw = nxCall(JSON.stringify({ extensionId: globalThis.__nxExtensionId, method, args }));
  const parsed = JSON.parse(raw);
  if (!parsed.ok) return Promise.reject(new Error(parsed.error || "extension error"));
  return Promise.resolve(parsed.value);
}
globalThis.__nxDispatch = (message) => listeners.forEach((fn) => fn(message));
const browser = {
  storage: { local: {
    get: (key) => call("storage.get", { key: typeof key === "string" ? key : null }),
    set: (items) => call("storage.set", { items }),
  }},
  tabs: {
    query: () => call("tabs.query", {}),
    create: (opts) => call("tabs.create", opts || {}),
    remove: (tabId) => call("tabs.remove", { tabId }),
  },
  runtime: {
    sendMessage: (message) => call("runtime.sendMessage", { message }),
    onMessage: { addListener(fn) { listeners.push(fn); } },
  },
};
globalThis.browser = browser;
globalThis.chrome = browser;
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_patterns_cover_hosts_and_wildcards() {
        assert!(matches_url("https://example.com/*", "https://example.com/inbox"));
        assert!(!matches_url("https://example.com/*", "https://example.org/"));
        assert!(matches_url("*://*/*", "http://localhost:8080/a"));
        assert!(matches_url("https://*.example.com/*", "https://mail.example.com/"));
        assert!(!matches_url("<all_urls>", "nxbrowser://home"));
    }
}
