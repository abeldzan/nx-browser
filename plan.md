# Project Plan: Lightweight GPUI Browser with Isolated Sessions and Custom Extensions

**Goal:** Build a high-performance, memory-efficient desktop browser using Rust and GPUI, supporting multiple isolated browser sessions, persistent authentication, tabs, and a custom extension system.

I recommend starting **macOS-first**, using WKWebView directly or through Wry. Design the browser backend with platform abstractions so Windows and Linux can be supported later.

The first milestone should be a working GPUI application that can open real websites inside its window.

---

# 1. Technology Stack

| Component | Technology | Purpose |
|---|---|---|
| Language | Rust | Core application |
| UI framework | [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui?utm_source=chatgpt.com) | GPU-accelerated native UI |
| Browser integration | [Wry](https://github.com/tauri-apps/wry?utm_source=chatgpt.com) | Native WebView integration |
| macOS browser engine | WKWebView | Website rendering |
| Database | SQLite | Profiles, sessions, extensions |
| Serialization | Serde | Configuration and manifests |
| Async runtime | Tokio | Background operations |
| Extension runtime | JavaScriptCore initially | Extension JavaScript execution |
| Extension format | WebExtensions-inspired | Custom extension compatibility |
| IPC | Rust channels + native WebView messaging | Browser/extension communication |

**Important architectural decision:** The initial implementation will embed a native WebView within the GPUI window. It will not attempt to render webpage pixels through GPUI's GPU renderer.

This avoids a major source of complexity while retaining good native rendering performance.

---

# 2. Architecture

```text
┌───────────────────────────────────────────────────────┐
│                    GPUI Application                   │
│                                                       │
│  ┌─────────────────────────────────────────────────┐  │
│  │ Browser Toolbar                                 │  │
│  │ Back | Forward | Refresh | URL | Extensions      │  │
│  ├────────────┬────────────────────────────────────┤  │
│  │            │                                    │  │
│  │ Profiles   │                                    │  │
│  │            │          Browser View              │  │
│  │ Sessions   │                                    │  │
│  │            │           WKWebView                │  │
│  │ Tabs       │                                    │  │
│  │            │                                    │  │
│  │ Extensions │                                    │  │
│  │            │                                    │  │
│  └────────────┴────────────────────────────────────┘  │
│                                                       │
└───────────────────────────┬───────────────────────────┘
                            │
                            ▼
┌───────────────────────────────────────────────────────┐
│                    Rust Core                          │
│                                                       │
│  Browser Manager                                      │
│  ├── Session Manager                                  │
│  ├── Profile Manager                                  │
│  ├── Tab Manager                                      │
│  ├── WebView Manager                                  │
│  ├── Extension Manager                                │
│  └── Resource Manager                                 │
│                                                       │
├───────────────────────────────────────────────────────┤
│                     Storage                           │
│                                                       │
│  SQLite                                               │
│  ├── Profiles                                         │
│  ├── Sessions                                         │
│  ├── Tabs                                             │
│  ├── Extensions                                       │
│  └── Settings                                         │
│                                                       │
│  Filesystem                                           │
│  ├── Browser Storage                                  │
│  └── Extension Data                                   │
└───────────────────────────────────────────────────────┘
```

---

# 3. Project Structure

Use a Rust workspace with separate crates.

```text
gpui-browser/
│
├── Cargo.toml
├── Cargo.lock
│
├── crates/
│   │
│   ├── app/
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── browser-ui/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── window.rs
│   │       ├── toolbar.rs
│   │       ├── sidebar.rs
│   │       ├── tabs.rs
│   │       └── profiles.rs
│   │
│   ├── browser-core/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── browser.rs
│   │       ├── session.rs
│   │       ├── profile.rs
│   │       ├── tab.rs
│   │       └── lifecycle.rs
│   │
│   ├── browser-engine/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── engine.rs
│   │       ├── webview.rs
│   │       └── events.rs
│   │
│   ├── browser-engine-macos/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── webkit.rs
│   │       ├── datastore.rs
│   │       └── bridge.rs
│   │
│   ├── browser-extensions/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── manifest.rs
│   │       ├── manager.rs
│   │       ├── permissions.rs
│   │       ├── scripting.rs
│   │       └── runtime.rs
│   │
│   └── browser-storage/
│       └── src/
│           ├── lib.rs
│           ├── database.rs
│           ├── profiles.rs
│           └── extensions.rs
│
├── extensions/
│   └── examples/
│       └── hello-world/
│           ├── manifest.json
│           └── content.js
│
└── README.md
```

This structure keeps GPUI-specific code separate from the browser engine and extension implementation.

---

# 4. Development Phases

## Phase 1 — GPUI + WebView Integration

**Priority: Critical**

Build the smallest working browser.

Features:
- Launch GPUI application.
- Create a native application window.
- Embed WKWebView.
- Navigate to websites.
- URL address bar.
- Back and forward navigation.
- Refresh.
- Page title updates.
- Loading state.

### Acceptance criteria

The application must be able to:

1. Open a website.
2. Navigate between pages.
3. Execute JavaScript.
4. Handle mouse and keyboard input.
5. Resize the browser view with the GPUI window.
6. Open links without unexpectedly creating external windows.

**Technical risk:** GPUI and native WebViews have separate rendering and event-handling lifecycles.

Validate native view positioning, focus handling, and resizing before implementing the rest of the application.

**Deliverable:** A minimal, usable browser window.

---

## Phase 2 — Browser Profiles and Session Isolation

**Priority: Critical**

Implement isolated browser profiles.

Each profile should have independent:
- Cookies.
- Local storage.
- IndexedDB.
- Cache where supported.
- Website permissions.
- Authentication state.
- Extension settings.

### Data model

```rust
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    pub storage_path: PathBuf,
    pub created_at: i64,
}

pub struct BrowserSession {
    pub id: String,
    pub profile_id: String,
    pub tabs: Vec<BrowserTab>,
}

pub struct BrowserTab {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}
```

These are application-level models; the engine adapter will own the actual native WebView and website data-store handles.

### Important implementation detail

Use separate persistent `WKWebsiteDataStore` instances for independent profiles.

On supported macOS versions, WebKit provides APIs for creating persistent data stores with distinct identifiers.

Don't assume that separate filesystem paths alone create isolated WKWebView storage.

### Acceptance criteria

Create two profiles:

```text
Profile A
└── Gmail → Account A

Profile B
└── Gmail → Account B
```

Restart the application.

Both accounts must remain logged in independently.

Also verify that logging out of one profile doesn't affect the other.

**Deliverable:** Persistent, isolated browser profiles.

---

## Phase 3 — Tabs and Session Management

**Priority: High**

Implement a session manager supporting multiple tabs and profiles.

Features:
- Create tabs.
- Close tabs.
- Switch tabs.
- Restore tabs after restart.
- Create profiles.
- Rename profiles.
- Delete profiles.
- Switch between profiles.

### Resource management

Don't keep every browser tab running.

Use a lifecycle state machine:

```rust
pub enum TabState {
    Active,
    Background,
    Suspended,
    Closed,
}
```

Recommended behavior:

| State | WebView | Memory usage |
|---|---|---|
| Active | Running | Highest |
| Background | Running | Moderate to high |
| Suspended | Destroyed | Lower |
| Closed | Destroyed | Minimal |

Suspending a tab should save its URL, title, and restorable metadata before releasing its WebView.

When reopened, the page reloads.

**Deliverable:** A browser capable of managing many saved sessions without keeping every webpage in memory.

---

# 5. Phase 4 — Custom Extension System

**Priority: High**

This is where your browser becomes extensible.

I recommend designing an extension format inspired by Manifest V3, without attempting full Chrome extension compatibility initially.

### Example extension

```text
my-extension/
├── manifest.json
├── background.js
├── content.js
└── popup.html
```

Manifest:

```json
{
  "manifest_version": 3,
  "name": "Example Extension",
  "version": "1.0.0",
  "permissions": ["tabs", "storage"],
  "host_permissions": ["https://example.com/*"],
  "content_scripts": [
    {
      "matches": ["https://example.com/*"],
      "js": ["content.js"]
    }
  ]
}
```

### Implement extension APIs incrementally

**Milestone 1: Content scripts**

Support injecting JavaScript into matching webpages.

**Milestone 2: Storage**

Implement:

```javascript
browser.storage.local.get("key");
browser.storage.local.set({ key: "value" });
```

**Milestone 3: Tabs**

Implement:

```javascript
browser.tabs.query({});
browser.tabs.create({ url: "https://example.com" });
browser.tabs.remove(tabId);
```

**Milestone 4: Messaging**

Implement:

```javascript
browser.runtime.sendMessage({
  action: "hello"
});
```

**Milestone 5: Background execution**

Introduce an isolated JavaScript runtime for background extension logic.

Do not give extensions unrestricted access to Rust APIs. Enforce permissions on the native side, isolate extension execution from webpage JavaScript, and associate every request with its originating extension and browser profile.

### Extension architecture

```text
Extension Manifest
        │
        ▼
Extension Loader
        │
        ▼
Permission Validator
        │
        ▼
Extension Runtime
        │
        ├── Content Scripts
        ├── Background Scripts
        ├── Extension Storage
        └── Browser APIs
                    │
                    ▼
             Rust Browser Core
```

**Deliverable:** Install a custom extension that injects a content script, stores settings, and communicates with the browser.

---

# 6. Phase 5 — Performance Optimization

**Priority: High**

Measure performance before introducing optimizations.

### Metrics to track

| Metric | Target |
|---|---|
| Application startup | Under 1 second, excluding cold engine initialization |
| GPUI interface responsiveness | 60 FPS |
| Idle CPU usage | Near zero when no page is active |
| Inactive tab memory | Released after suspension |
| Profile switching | Minimize unnecessary WebView creation |
| Extension overhead | Minimal when extensions are inactive |

These are initial engineering targets, not guaranteed performance characteristics.

### Optimization strategies

**Lazy initialization:** Only create WebViews when needed.

**Tab suspension:** Destroy inactive WebViews after a configurable period.

**Extension lifecycle management:** Don't run unnecessary background scripts.

**Event-driven architecture:** Avoid polling browser state unnecessarily.

**Memory profiling:** Measure native WebKit processes as well as your Rust application's memory.

One important caveat: WKWebView can share WebKit processes and caches internally. Destroying a WebView does not guarantee immediate reclamation of all associated memory.

---

# 7. Phase 6 — Browser Features

Once the core architecture is stable, add:

- Downloads manager.
- Browser history.
- Bookmarks.
- Keyboard shortcuts.
- Find in page.
- Website permissions.
- Developer tools.
- Extension management UI.
- Profile import/export.
- Crash recovery.

Avoid implementing these before session isolation and native WebView integration are proven.

---

# 8. Initial Development Schedule

| Week | Focus | Deliverable |
|---|---|---|
| 1 | GPUI + WKWebView integration | Working browser |
| 2 | Profiles and isolated storage | Multiple independent logins |
| 3 | Tabs and session persistence | Session manager |
| 4 | Extension manifests and content scripts | First working extension |
| 5 | Extension storage, tabs API, messaging | Functional extension runtime |
| 6 | Performance profiling and optimization | Optimized prototype |
| 7 | Browser features and testing | Usable alpha |

This is an aggressive prototype schedule for one experienced Rust developer. Native view integration, WebKit limitations, and extension security could extend it considerably.

---

# 9. First Implementation Sprint

I would begin with a much smaller scope than the full architecture.

### Sprint 1: Establish the foundation

**Task 1:** Create a Rust workspace with GPUI.

**Task 2:** Create a GPUI window with a toolbar and content region.

**Task 3:** Integrate a single WKWebView using Wry or direct macOS bindings.

**Task 4:** Synchronize the WebView bounds with GPUI layout changes.

**Task 5:** Implement URL navigation and basic browser controls.

**Task 6:** Verify keyboard focus, scrolling, text selection, and resizing.

**Task 7:** Measure startup time, memory consumption, and idle CPU usage.

### Sprint completion criteria

You should have a native macOS application that:

- Uses GPUI for its interface.
- Displays real websites inside its window.
- Supports normal browser navigation.
- Doesn't require Chromium to be bundled.
- Provides baseline performance measurements.

Only after this works reliably should you start implementing isolated profiles.

---

# 10. Important Decisions to Make Early

There are three architectural choices I would settle before writing substantial code.

### A. Wry or direct WKWebView integration?

**Start with Wry for a proof of concept.**

However, if Wry's abstractions make persistent profile isolation or native view management difficult, switch to direct WKWebView bindings.

### B. Chrome-compatible extensions or a custom extension API?

**Start with your own WebExtensions-inspired API.**

Implement the most useful APIs first, then gradually increase compatibility.

Do not promise that arbitrary Chrome Web Store extensions will work.

### C. How many active sessions should be supported?

Design for hundreds of **saved** profiles, but benchmark the number of simultaneously active WebViews.

Hundreds of saved profiles are relatively inexpensive. Hundreds of live browser instances are not.

---

## Recommended MVP

For version `0.1.0`, limit the scope to five capabilities:

1. GPUI application with an embedded WKWebView.
2. Multiple persistent, isolated browser profiles.
3. Tabs with automatic suspension.
4. Custom extensions with content scripts and basic storage APIs.
5. Session restoration after application restart.

This is a realistic foundation for a lightweight browser session manager.

**One final recommendation:** Before committing to the complete project, build a two-profile prototype and verify that the target macOS version supports the persistent WebKit data-store isolation you need. That is the most important technical assumption in this architecture.
