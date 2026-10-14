# Implementation tracker

Checked items are implemented in this repository. The spec stays in `plan.md`.

## Decisions

- [x] macOS first, with the engine behind a platform module
- [x] Direct WKWebView instead of Wry, so each profile can use its own persistent `WKWebsiteDataStore`
- [x] WebExtensions-inspired API, not Chrome Web Store compatibility
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

- [x] Manifest parsing and permission checks
- [x] Content scripts injected into matching pages, isolated from the page world
- [x] `browser.storage.local` get/set
- [x] `browser.tabs` query/create/remove
- [x] `browser.runtime.sendMessage`
- [x] Background JavaScriptCore runtime
- [x] Example extension

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

Proven by `cargo test -p browser-core -p browser-extensions` (8 tests) and `cargo run -p nxbrowser -- --self-test` (`SELFTEST OK`):

- Two profiles on the same origin keep separate `localStorage` (`A` vs `B`) in the same process, via distinct `WKWebsiteDataStore` identifiers.
- JavaScript evaluation returns `2`.
- WebView frame follows the content region (`840×577`).
- Startup time and process RSS are recorded (self-test sample: 1292 ms, ~131 MB RSS).
- SQLite restores a profile and its tab URL after the database is reopened.
- Extension storage is rejected without the storage permission.
- Host match patterns and the tab suspension cap behave as coded.

Not exercised by those runs, so treat them as implemented but unproven in a live window:

- Typing in the URL bar, scrolling a real page, and selecting page text.
- Two profiles surviving a full quit and relaunch (WebKit disk store, not just SQLite).
- Downloads, find-in-page, permission prompts, and profile import/export clicks.
- Idle CPU staying near zero. The metric is printed; the 60 fps target is not a pass/fail check.
- Profile export is JSON metadata plus the data-store UUID, not a copy of WebKit’s on-disk store.
- Downloads use `ureq`, not `WKDownload`.
- DevTools is `WKWebView.setInspectable(true)` (Safari Develop menu), not an in-app inspector.
