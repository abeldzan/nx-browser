# Implementation tracker

Checked items are implemented in this repository. The spec stays in `plan.md`.

## Decisions

- [x] macOS first, with the engine behind a platform module
- [x] Direct WKWebView instead of Wry, so each profile can use its own persistent `WKWebsiteDataStore`
- [x] WebKit's native web extension runtime (`WKWebExtensionController`, as in Safari), so Chrome Web Store extensions run unmodified
- [x] Many saved profiles, few live WebViews (suspension)

## Sprint 1 — foundation

- [x] Rust workspace with GPUI
- [x] GPUI window with toolbar and content region
- [x] WKWebView embedded in the GPUI window
- [x] WebView bounds follow GPUI layout
- [x] URL bar, back, forward, refresh
- [x] Page title and loading state
- [x] Keyboard focus, scrolling, text selection, and resize path
- [x] Links do not open external windows
- [x] JavaScript execution
- [x] Startup, memory, and idle CPU measurements

## Phase 2 — profiles and isolation

- [x] Profile model and SQLite persistence
- [x] Separate persistent `WKWebsiteDataStore` per profile
- [x] Create, rename, delete, and switch profiles
- [x] Two profiles keep independent website data across restart

## Phase 3 — tabs and sessions

- [x] Create, close, and switch tabs
- [x] Sessions per profile
- [x] Restore tabs after restart
- [x] Tab lifecycle: active, background, suspended, closed
- [x] Suspended tabs drop their WebView and reload when opened

## Phase 4 — extensions

- [x] WebKit runs backgrounds (MV2/MV3), content scripts, popups, options pages and the `chrome.*`/`browser.*` APIs
- [x] One extension controller per profile, so extension storage stays per profile
- [x] Tabs and the browser window exposed to extensions (`tabs`, `windows`, `activeTab`, `action` badges)
- [x] Install from the Chrome Web Store (store page button, pasted link or id) and Load unpacked
- [x] Popups open as a native popover under the clicked button, sized by WebKit to the page; options pages and side panels open as floating windows
- [x] Pin extensions to the title bar
- [x] Example extension
- [x] Suspended tabs stay listed to extensions; `tabs.captureVisibleTab`; manifest `commands` shortcuts; prompted permission grants saved across launches
- [x] Store extensions get a polyfill that defines `Symbol.dispose`/`Symbol.asyncDispose` (missing in this WebKit, needed by `using`), keeps the service worker alive while an extension page is open, and swaps `WebSocket` in service workers for one that fails at once: WebKit runs them on the main thread, where `new WebSocket()` deadlocks the extension's process (Bitwarden's login froze on it)
- [ ] WebSockets from extension service workers (live sync such as Bitwarden's SignalR falls back to periodic sync)
- [ ] Context menus (`chrome.contextMenus`), runtime `action.setIcon`/`setTitle`/`disable`, and `openPanelOnActionClick: false` (a side-panel button never fires `action.onClicked`)

## Phase 5 — performance

- [x] Lazy WebView creation
- [x] Configurable tab suspension
- [x] Background scripts only for enabled extensions
- [x] Navigation updates come from WebKit delegates
- [x] Metrics for startup, process RSS, and WebKit process RSS

## Phase 6 — browser features

- [x] Downloads
- [x] History
- [x] Bookmarks
- [x] Keyboard shortcuts
- [x] Find in page
- [x] Website permission decisions
- [x] Web Inspector (`WKWebView.inspectable`)
- [x] Extension management UI
- [x] Profile import/export
- [x] Crash recovery via session persistence

## Verification

Proven by `cargo test -p browser-core -p browser-extensions` (17 tests) and `cargo run -p nxbrowser -- --self-test` (`SELFTEST OK`):

- Two profiles on the same origin keep separate `localStorage` (`A` vs `B`) in the same process, via distinct `WKWebsiteDataStore` identifiers.
- JavaScript evaluation returns `2`.
- WebView frame follows the content region (`840×577`).
- Startup time and process RSS are recorded (self-test sample: 1292 ms, ~131 MB RSS).
- SQLite restores a profile and its tab URL after the database is reopened.
- The bundled example extension's content script runs on a test page through WebKit's extension runtime.
- Host match patterns and the tab suspension cap behave as coded.

Not exercised by those runs, so treat them as implemented but unproven in a live window:

- Typing in the URL bar, scrolling a real page, and selecting page text.
- Two profiles surviving a full quit and relaunch (WebKit disk store, not just SQLite).
- Downloads, find-in-page, permission prompts, and profile import/export clicks.
- Idle CPU staying near zero. The metric is printed; the 60 fps target is not a pass/fail check.
- Profile export is JSON metadata plus the data-store UUID, not a copy of WebKit’s on-disk store.
- Downloads use `ureq`, not `WKDownload`.
- DevTools is `WKWebView.setInspectable(true)` (Safari Develop menu), not an in-app inspector.
