# NxBrowser

A fast, keyboard-first web browser for macOS, written in Rust.

NxBrowser draws its interface with [GPUI](https://www.gpui.rs) (the GPU UI framework behind Zed) and renders pages with the system WebKit (`WKWebView`), the same engine as Safari. It runs Chrome Web Store extensions, keeps every profile fully isolated, tiles pages like a window manager, and has a real terminal built in.

> **Status:** early and moving fast. Expect rough edges. macOS only for now.

## Features

### Profiles and privacy
- **Isolated profiles.** Each profile has its own persistent WebKit data store, so cookies, logins, `localStorage` and extension data never leak between them. Give each one a name, icon and color, and switch from the profile rail.
- **Private profiles.** In-memory storage, no history, wiped at launch.
- **Built-in ad and tracker blocking** using WebKit content rules (Peter Lowe's list), plus pop-up blocking, HTTPS-only mode and fraud warnings.
- **Profile picker for outside links.** Set NxBrowser as the default browser, and links opened from other apps can ask which profile to use.
- **Per-site permissions** for camera, microphone and more, with indicators on tabs that are recording or playing sound. Tabs can be muted.

### Spaces, windows and tiling
- **Spaces** group windows into separate workspaces. Switch with ⌥⌘← / ⌥⌘→, or drag a space out to open it in its own OS window.
- **Tiling windows.** Pages sit in windows on a stage that tile automatically as a split tree, like a tiling window manager. You can also let a space float its windows freely, snap them to halves and quarters, or maximize one.
- **Move and focus by keyboard** (⌃⌘ + arrows to focus, ⌃⇧⌘ + arrows to move).
- **Tear off and merge.** Drag a window out into its own OS window, and drag it back to merge.
- **Peek.** ⌥-click a link to preview it in a floating window. Esc closes it.
- **Compact, Safari-style title bar**: one row for navigation, tabs and tools. The active tab turns into the address field.

### Command palette (⌘K)
Inspired by Arc, Vivaldi and Zen. You can:
- search tabs, bookmarks, history, spaces, profiles, extensions and every action
- narrow the search with scopes: `@tabs` / `%`, `@bookmarks` / `*`, `@history` / `^`, `@actions` / `>`, `@spaces`
- flip settings in place (theme, compact mode, JavaScript, ad blocking…)
- do arithmetic with the built-in calculator: type `12*(3+4)` and the result is copied
- search other sites with **bangs**: `!yt lofi`, `!gh gpui`, `!w rust`, `!mdn fetch`, `!crates serde`, `!claude …` and 20 more

### Extensions
- **Chrome Web Store extensions run unmodified** on WebKit's native web extension runtime (the one Safari uses), MV2 and MV3. Install from the store page, by pasting a link or ID, or load an unpacked folder.
- Popups open as native popovers. Options pages and side panels open as floating windows. Extensions can be pinned to the title bar.
- Extensions are enabled per profile, and so is their storage.

### Password manager
- Offers to save and update passwords when you sign in, and autofills them later.
- Passwords are encrypted with AES-256-GCM. The key is kept in the macOS Keychain. Revealing, copying and exporting passwords needs Touch ID or your Mac password.
- Password generator and health checks for weak and reused passwords.
- CSV import and export, compatible with other password managers.

### Built-in terminal
- **Terminal tabs** (⌥⌘T) run your login shell next to your web pages, tile with them, and restore in the same folder.
- Built on `alacritty_terminal`, with mouse reporting, bracketed paste and **kitty graphics protocol** images.
- You can configure the font, size, line height, cursor style, shell and scrollback.

### Everyday browsing
- Address bar suggestions from history, bookmarks and your search engine (DuckDuckGo, Google, Bing, Brave, Startpage or Ecosia).
- A new-tab page with your bookmarks and pinned and recently visited sites, plus history, bookmark and download panels.
- Drag finished downloads straight from the downloads popover into a page or Finder.
- Find in page, per-site zoom, print, reopen closed tabs (⌘⇧T), HTML5 fullscreen.
- Pages can be inspected from Safari's Develop menu (⌥⌘I).

### Performance
- Pages are GPU-rendered through Metal. Embedded web views move in the same frame as the interface.
- WebViews are created lazily, and background tabs are suspended after a configurable time and live-tab limit. Suspended tabs stay in the tab bar but use no memory until you open them again.
- Sessions are saved continuously, so a crash or restart brings every tab back.

### Make it yours
- Light, dark or system theme, any accent color, comfortable or compact density, and reduced motion.
- **Every shortcut can be rebound** in Settings → Shortcuts.

## Keyboard shortcuts

These are the defaults. All of them can be changed in Settings.

| Shortcut | Action |
|---|---|
| ⌘K | Command palette |
| ⌘L | Address bar |
| ⌘T / ⌥⌘T | New tab / new terminal |
| ⌘W / ⇧⌘T | Close tab / reopen closed tab |
| ⌃⇥ / ⌃⇧⇥, ⌘1…⌘9 | Switch tabs |
| ⌘[ / ⌘] | Back / forward |
| ⌘R / ⇧⌘R | Reload / reload ignoring cache |
| ⇧⌘C | Copy page address |
| ⌘F | Find in page |
| ⌘D | Bookmark page |
| ⌘= / ⌘− / ⌘0 | Zoom in / out / reset |
| ⌘P | Print |
| ⌥⌘I | Inspect page |
| ⌃⌘ + arrows | Focus window in that direction |
| ⌃⇧⌘ ← / → | Move window |
| ⇧⌘W | Close window |
| ⌥⌘← / ⌥⌘→ | Previous / next space |
| ⌃⌘N | New space |
| ⌘Y / ⌥⌘L / ⌥⌘B | History / downloads / bookmarks |
| ⌘, | Settings |
| ⌥-click a link | Peek |

## Install

```bash
brew install --cask abeldzan/tap/nxbrowser
brew trust --cask abeldzan/tap/nxbrowser   # lets `brew upgrade` update it
```

Apple Silicon, macOS 14 or newer. The app is ad-hoc signed, not notarized, so the cask clears the download quarantine flag for you. Because of that, macOS may ask again for Keychain and camera/microphone access after an update.

## Building from source

Requirements:
- macOS 14 or newer
- Xcode with the Metal toolchain (GPUI compiles its shaders with it)
- Rust. `rust-toolchain.toml` pins the version, so `rustup` installs it for you.

```bash
git clone <repo-url> nxbrowser
cd nxbrowser
cargo run -p nxbrowser
```

Run it from the repository root so the example extension in `extensions/examples/hello-world` is found. `cargo run` starts the binary inside a small `.app` wrapper (`scripts/run-bundled.sh`), so the app icon and system integration work in development.

To build a standalone app:

```bash
scripts/bundle-macos.sh
open target/NxBrowser.app
```

This produces an ad-hoc signed `NxBrowser.app` with the bundle ID `dev.nxbrowser.app`.

### Tests

```bash
cargo test -p browser-core -p browser-extensions
cargo run -p nxbrowser -- --self-test
```

`--self-test` opens a window and checks that two profiles keep separate `localStorage`, that JavaScript evaluation and web view resizing work, and that the example extension's content script runs. It prints startup and memory figures, then exits.

## Project layout

| Crate | What it does |
|---|---|
| `crates/app` | The `nxbrowser` binary, `Info.plist` and app icon |
| `crates/browser-ui` | The GPUI interface: windows, tiling, palette, settings, passwords, terminal |
| `crates/browser-core` | Platform-independent model: profiles, tabs, sessions, settings, URL handling, password crypto |
| `crates/browser-engine` | The `WebEngine` trait and the shared shortcut table |
| `crates/browser-engine-macos` | The WebKit implementation: `WKWebView`, extensions, downloads, Keychain |
| `crates/browser-extensions` | Manifest parsing, Chrome Web Store install and API polyfills |
| `crates/browser-storage` | SQLite persistence |
| `vendor/gpui-apple` | Patched GPUI macOS backend (synced Metal presents) |

The original spec is in `plan.md`, and progress against it is tracked in `IMPLEMENTATION.md`.

## Known limitations

- **macOS only.** The engine sits behind a trait, but WebKit on macOS is the only backend so far.
- **No passkeys.** WebAuthn in `WKWebView` needs an Apple entitlement that is only granted to paid developer accounts.
- **Some extension APIs are missing**: `chrome.contextMenus`, runtime `action.setIcon` / `setTitle`, and WebSockets from extension service workers. For example, Bitwarden's live sync falls back to periodic sync.
- Ad blocking only blocks network requests. It does not hide page elements.
- The Web Inspector is Safari's (Develop menu). There is no in-app DevTools.

## Contributing

Issues and pull requests are welcome. Before you open a PR, run the tests above and `cargo run -p nxbrowser -- --self-test`.

To refresh the ad-block list, run `scripts/update-blocklist.sh`.

## License

MIT
