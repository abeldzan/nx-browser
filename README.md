# NxBrowser

A macOS browser shell: GPUI for the chrome, WKWebView for pages. Each profile has its own persistent `WKWebsiteDataStore`, so cookies and website data stay separate. Wry is not used, because it does not expose per-profile WebKit data stores.

Progress against `plan.md` is tracked in `IMPLEMENTATION.md`.

## Run

```bash
cargo run -p nxbrowser
```

Run it from this directory so `extensions/examples/hello-world` is discovered.

```bash
cargo test -p browser-core -p browser-extensions
cargo run -p nxbrowser -- --self-test
```

`--self-test` opens the window, checks that two profiles keep independent `localStorage`, evaluates JavaScript, checks that the web view resizes, prints startup and memory numbers, and exits.

Requires macOS 14 or newer (persistent `WKWebsiteDataStore` identifiers) and the Xcode Metal toolchain, which GPUI uses to compile shaders. The window chrome uses [gpuikit](https://crates.io/crates/gpuikit). `rust-toolchain.toml` selects Rust 1.98.1, which that GPUI build needs.

## Bundle

```bash
scripts/bundle-macos.sh
open target/NxBrowser.app
```

This builds an ad-hoc signed `NxBrowser.app` with the bundle ID `dev.nxbrowser.app`. Inside the bundle, macOS can offer its own Password AutoFill from the Passwords app on login forms. This is best effort, and some sites don't get it.

Passkeys do not work. WebAuthn in WKWebView needs Apple's `com.apple.developer.web-browser.public-key-credential` entitlement, and Apple only grants it to paid developer accounts.

## Shortcuts

| Shortcut | Action |
|---|---|
| Cmd+L | Address bar |
| Cmd+T | New tab |
| Cmd+W | Close tab |
| Cmd+R | Reload |
| Cmd+[ / Cmd+] | Back / forward |
| Cmd+F | Find in page |
| Cmd+D | Bookmark |
| Option+click a link | Peek: preview in a floating window (Esc closes) |

Pages are inspectable from Safari’s Develop menu. Links that ask for a new window open as a new tab in this process.
