use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use anyhow::Context;
use block2::RcBlock;
use browser_engine::{EngineEvent, EngineSettings, LogicalRect, Shortcut, WebEngine};
use crate::extensions::{Extensions, TabNotifier, SCHEME};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSAutoresizingMaskOptions, NSTrackingArea, NSTrackingAreaOptions, NSEvent, NSMenu, NSMenuItem, NSModalResponseOK, NSOpenPanel, NSPrintInfo, NSEventMask, NSEventModifierFlags, NSResponder, NSView, NSWindowOrderingMode};
use objc2_core_graphics::{CGColor, CGMutablePath};
use objc2_quartz_core::{kCAFillRuleEvenOdd, CACornerMask, CAShapeLayer, CATransaction};
use objc2_foundation::{
    ns_string, NSArray, NSBundle, NSData, NSDate, NSDictionary, NSError, NSHTTPCookie, NSObject, NSObjectProtocol,
    NSKeyValueObservingOptions, NSNumber, NSObjectNSKeyValueObserverRegistration, NSPoint, NSPropertyListFormat, NSPropertyListMutabilityOptions, NSPropertyListSerialization, NSRect, NSSize, NSString, NSURL, NSURLRequest, NSURLResponse, NSUUID,
};
use objc2_web_kit::{
    WKContentWorld, WKFindConfiguration, WKMediaCaptureType, WKNavigation, WKNavigationAction,
    WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy,
    WKPermissionDecision, WKScriptMessage, WKScriptMessageHandler, WKSecurityOrigin, WKUIDelegate,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    WKAudiovisualMediaTypes, WKWebsiteDataStore, WKWindowFeatures,
};
use uuid::Uuid;

const HOME_HTML: &str = r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>NxBrowser</title></head>
<body style="margin:0;font-family:-apple-system,BlinkMacSystemFont,sans-serif;background:#1c1c1e;color:#f5f5f7">
<main style="max-width:40rem;margin:4rem auto;padding:0 1.5rem">
<h1>NxBrowser</h1>
<p>This tab is running in the profile's own WebKit data store.</p>
<p>Enter a URL in the toolbar and press Return.</p>
</main>
</body>
</html>"#;

const OBSERVED_KEYS: [&str; 2] = ["URL", "title"];

thread_local! {
    /// Popup views WebKit is already loading, keyed by the URL their new tab is created with.
    /// shortcut: a popup whose tab is never created stays here; evict if that ever matters.
    static POPUPS: RefCell<HashMap<String, Retained<WKWebView>>> = RefCell::new(HashMap::new());
    static PRIVATE_STORES: RefCell<HashMap<String, Retained<WKWebsiteDataStore>>> = RefCell::new(HashMap::new());
}

define_class!(
    /// WebKit's page view, with the right-click menu's "… in New Window" items saying "… in New Tab":
    /// they open in the page's own window (see `OpenInNewTab::from`).
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct NxWebView;

    impl NxWebView {
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, event: &NSEvent) {
            for item in menu.itemArray().iter() {
                let title = item.title().to_string();
                if title.contains("New Window") {
                    item.setTitle(&NSString::from_str(&title.replace("New Window", "New Tab")));
                }
            }
            let _: () = unsafe { msg_send![super(self), willOpenMenu: menu, withEvent: event] };
        }

        /// WebKit re-sends Cmd keys the page ignored down the responder chain, which ends in the system beep;
        /// the page-first ones are run by the key script instead, so stop them here.
        /// Plain keys the page ignored (arrows on YouTube, which seeks without preventDefault) come back the
        /// same way as the same NSEvent object; nothing past the page wants them, so drop them too.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            thread_local! {
                // Retained so a fresh event can't reuse the address and pass for a resend.
                static LAST_KEY_DOWN: RefCell<Option<Retained<NSEvent>>> = const { RefCell::new(None) };
            }
            let resent = LAST_KEY_DOWN.with_borrow_mut(|last| {
                let resent = last.as_deref().is_some_and(|last| std::ptr::eq(last, event));
                *last = Some(objc2::Message::retain(event));
                resent
            });
            let flags = event.modifierFlags();
            let page_first = flags.contains(NSEventModifierFlags::Command)
                && !flags.contains(NSEventModifierFlags::Control)
                && event.charactersIgnoringModifiers().is_some_and(|chars| PAGE_FIRST_KEYS.contains(&ns_string(&chars).to_ascii_lowercase().as_str()));
            let ignored_plain_key = resent && !flags.contains(NSEventModifierFlags::Command);
            if !page_first && !ignored_plain_key {
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }
    }
);

struct DelegateIvars {
    /// Swapped when the page moves to another window's engine.
    events: RefCell<Sender<EngineEvent>>,
    tab_id: String,
    permissions: Arc<Mutex<HashMap<String, String>>>,
    https_only: Arc<AtomicBool>,
    extensions: TabNotifier,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxWebDelegate"]
    #[ivars = DelegateIvars]
    struct NxWebDelegate;

    unsafe impl NSObjectProtocol for NxWebDelegate {}

    unsafe impl WKNavigationDelegate for NxWebDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        unsafe fn webView_decidePolicyForNavigationAction_decisionHandler(
            &self,
            web_view: &WKWebView,
            navigation_action: &WKNavigationAction,
            decision_handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // An extension page (side panel, options) sending the user to a site, e.g. to log in:
            // open the site in a tab and leave the extension page where it is.
            let main_frame = navigation_action.targetFrame().is_some_and(|frame| frame.isMainFrame());
            let from_extension = web_view.URL().and_then(|url| url.absoluteString()).is_some_and(|url| ns_string(&url).starts_with(crate::EXTENSION_SCHEME));
            let target = navigation_action.request().URL().and_then(|url| url.absoluteString()).map(|url| ns_string(&url));
            if let (true, true, Some(url)) = (main_frame, from_extension, target.filter(|url| url.starts_with("https://") || url.starts_with("http://"))) {
                decision_handler.call((WKNavigationActionPolicy::Cancel,));
                let _ = self.ivars().events.borrow().send(EngineEvent::OpenInNewTab { url, from: None });
                return;
            }
            if self.ivars().https_only.load(Ordering::Relaxed) {
                let main_frame = navigation_action.targetFrame().is_some_and(|frame| frame.isMainFrame());
                let url = navigation_action.request().URL().and_then(|url| url.absoluteString()).map(|url| ns_string(&url));
                if let (true, Some(rest)) = (main_frame, url.as_deref().and_then(|url| url.strip_prefix("http://"))) {
                    decision_handler.call((WKNavigationActionPolicy::Cancel,));
                    if let Some(secure) = NSURL::URLWithString(&NSString::from_str(&format!("https://{rest}"))) {
                        web_view.loadRequest(&NSURLRequest::requestWithURL(&secure));
                    }
                    return;
                }
            }
            decision_handler.call((WKNavigationActionPolicy::Allow,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        unsafe fn webView_decidePolicyForNavigationResponse_decisionHandler(
            &self,
            _web_view: &WKWebView,
            navigation_response: &WKNavigationResponse,
            decision_handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            if navigation_response.isForMainFrame() && !navigation_response.canShowMIMEType() {
                // The generated `response()` assumes non-null, but WebKit can hand back nil; a panic here aborts the app.
                let response: Option<Retained<NSURLResponse>> = msg_send![navigation_response, response];
                if let Some(url) = response.and_then(|response| response.URL()).and_then(|url| url.absoluteString()) {
                    let _ = self.ivars().events.borrow().send(EngineEvent::Download {
                        tab_id: self.ivars().tab_id.clone(),
                        url: ns_string(&url),
                    });
                }
                decision_handler.call((WKNavigationResponsePolicy::Cancel,));
                return;
            }
            decision_handler.call((WKNavigationResponsePolicy::Allow,));
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        unsafe fn webView_didStartProvisionalNavigation(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
        ) {
            self.emit_page(web_view, true);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        unsafe fn webView_didFinishNavigation(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.emit_page(web_view, false);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        unsafe fn webView_didFailProvisionalNavigation_withError(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let _ = self.ivars().events.borrow().send(EngineEvent::Failed {
                tab_id: self.ivars().tab_id.clone(),
                message: ns_string(&error.localizedDescription()),
            });
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        unsafe fn webView_didFailNavigation_withError(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let _ = self.ivars().events.borrow().send(EngineEvent::Failed {
                tab_id: self.ivars().tab_id.clone(),
                message: ns_string(&error.localizedDescription()),
            });
        }
    }

    unsafe impl WKUIDelegate for NxWebDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        unsafe fn webView_createWebViewWithConfiguration_forNavigationAction_windowFeatures(
            &self,
            web_view: &WKWebView,
            configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            self.open_popup(web_view, configuration, navigation_action)
        }

        #[unsafe(method(webViewDidClose:))]
        unsafe fn webViewDidClose(&self, _web_view: &WKWebView) {
            let _ = self.ivars().events.borrow().send(EngineEvent::CloseTab { tab_id: self.ivars().tab_id.clone() });
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn webView_runJavaScriptAlertPanelWithMessage_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            _frame: &objc2_web_kit::WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let _ = self.ivars().events.borrow().send(EngineEvent::Alert {
                message: ns_string(message),
            });
            completion_handler.call(());
        }

        // `<input type=file>`: without this WebKit on macOS shows no picker at all.
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        unsafe fn webView_runOpenPanelWithParameters_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            parameters: &objc2_web_kit::WKOpenPanelParameters,
            _frame: &objc2_web_kit::WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            let panel = NSOpenPanel::openPanel(MainThreadMarker::from(self));
            panel.setAllowsMultipleSelection(parameters.allowsMultipleSelection());
            panel.setCanChooseDirectories(parameters.allowsDirectories());
            let urls = (panel.runModal() == NSModalResponseOK).then(|| panel.URLs());
            completion_handler.call((urls.as_ref().map_or(std::ptr::null_mut(), |urls| Retained::as_ptr(urls).cast_mut()),));
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        unsafe fn webView_requestMediaCapturePermissionForOrigin_initiatedByFrame_type_decisionHandler(
            &self,
            _web_view: &WKWebView,
            origin: &WKSecurityOrigin,
            _frame: &objc2_web_kit::WKFrameInfo,
            kind: WKMediaCaptureType,
            decision_handler: &block2::DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            let kind = match kind {
                WKMediaCaptureType::Microphone => "microphone",
                WKMediaCaptureType::CameraAndMicrophone => "camera-microphone",
                _ => "camera",
            };
            let origin = format!("{}://{}", ns_string(&origin.protocol()), ns_string(&origin.host()));
            let key = format!("{origin}\n{kind}");
            let decision = self
                .ivars()
                .permissions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .cloned()
                .unwrap_or_else(|| "deny".into());
            let _ = self.ivars().events.borrow().send(EngineEvent::Permission {
                tab_id: self.ivars().tab_id.clone(),
                origin,
                kind: kind.into(),
            });
            decision_handler.call((if decision == "allow" {
                WKPermissionDecision::Grant
            } else {
                WKPermissionDecision::Deny
            },));
        }
    }

    impl NxWebDelegate {
        /// KVO on the view's `URL` and `title`: in-page navigations (pushState, as on YouTube) never
        /// reach the navigation delegate, so the address bar would lag behind them.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        unsafe fn observe_value(
            &self,
            _key_path: Option<&NSString>,
            object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            if let Some(web_view) = object.and_then(|object| object.downcast_ref::<WKWebView>()) {
                self.emit_page(web_view, web_view.isLoading());
            }
        }
    }

    unsafe impl WKScriptMessageHandler for NxWebDelegate {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn userContentController_didReceiveScriptMessage(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            if ns_string(&message.name()) == "nxShortcut" {
                if let Some(shortcut) = message.body().downcast::<NSString>().ok().and_then(|key| shortcut_for(&ns_string(&key), false)) {
                    let _ = self.ivars().events.borrow().send(EngineEvent::Shortcut(shortcut));
                }
            }
        }
    }
);

impl NxWebDelegate {
    fn new(mtm: MainThreadMarker, ivars: DelegateIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn open_popup(&self, web_view: &WKWebView, configuration: &WKWebViewConfiguration, navigation_action: &WKNavigationAction) -> Option<Retained<WKWebView>> {
        if unsafe { navigation_action.targetFrame() }.is_some() {
            return None;
        }
        let Some(url) = unsafe { navigation_action.request() }.URL().and_then(|url| url.absoluteString()).map(|url| ns_string(&url)) else {
            return None;
        };
        let from_extension = unsafe { web_view.URL() }.and_then(|url| url.absoluteString()).is_some_and(|url| ns_string(&url).starts_with(crate::EXTENSION_SCHEME));
        // Web popups get a real view built from WebKit's configuration so `window.opener` works:
        // OAuth sign-ins (Figma, Google, ...) post their result back through it. `ensure_tab` adopts it.
        let popup = (!from_extension).then(|| {
            let frame = objc2_core_foundation::CGRect {
                origin: objc2_core_foundation::CGPoint { x: 0.0, y: 0.0 },
                size: objc2_core_foundation::CGSize { width: 100.0, height: 100.0 },
            };
            let view: Retained<NxWebView> = unsafe { msg_send![NxWebView::alloc(MainThreadMarker::from(self)), initWithFrame: frame, configuration: configuration] };
            let view = Retained::into_super(view);
            POPUPS.with_borrow_mut(|popups| popups.insert(url.clone(), view.clone()));
            view
        });
        let _ = self.ivars().events.borrow().send(EngineEvent::OpenInNewTab { url, from: Some(self.ivars().tab_id.clone()) });
        popup
    }

    fn emit_page(&self, web_view: &WKWebView, loading: bool) {
        let url = unsafe { web_view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|url| ns_string(&url))
            .unwrap_or_default();
        let title = unsafe { web_view.title() }
            .map(|title| ns_string(&title))
            .unwrap_or_default();
        let _ = self.ivars().events.borrow().send(EngineEvent::Navigated {
            tab_id: self.ivars().tab_id.clone(),
            url,
            title,
            loading,
        });
        self.ivars().extensions.changed(&self.ivars().tab_id);
    }
}

/// A page with its delegate, handed between window engines with `take_tab` / `adopt_tab`.
pub struct LiveTab {
    view: Retained<WKWebView>,
    delegate: Retained<NxWebDelegate>,
}

pub struct MacEngine {
    events_tx: Sender<EngineEvent>,
    events_rx: Receiver<EngineEvent>,
    tabs: HashMap<String, LiveTab>,
    visible: Option<String>,
    content: Option<Retained<NSView>>,
    chrome: Option<Retained<NSView>>,
    bounds: LogicalRect,
    stacked: Vec<String>,
    placed_holes: Option<(Vec<(String, LogicalRect)>, Vec<Notch>, NSSize)>,
    permissions: Arc<Mutex<HashMap<String, String>>>,
    settings: EngineSettings,
    https_only: Arc<AtomicBool>,
    dimmers: HashMap<String, Retained<NSView>>,
    /// `windowNumber` of the window this engine is attached to; keys its entry in `WINDOWS`.
    window_no: isize,
    /// Where session cookies are snapshotted, as `<dir>/<data store id>.plist`; None when not restoring.
    cookie_dir: Option<PathBuf>,
    /// Stores with a snapshot still to restore before their first page loads.
    cookie_restore: HashSet<String>,
    /// Stores that had a tab this run; only their in-memory session cookies are current.
    cookie_used: HashSet<String>,
    /// Page loads held until a store's session cookies are back, per store.
    held_loads: Rc<RefCell<HashMap<String, Vec<(Retained<WKWebView>, String)>>>>,
    /// Tabs whose next page must not start media on its own (restored at launch or woken from suspension).
    hold_media: HashSet<String>,
    extensions: Extensions,
}

impl MacEngine {
    /// `extensions` is the app's one extension host, shared by every window; `None` creates it.
    pub fn new(extensions: Option<Extensions>) -> Self {
        if let Ok(home) = std::env::var("HOME") {
            if let Err(error) = unify_data_root(&std::path::Path::new(&home).join("Library/WebKit")) {
                eprintln!("could not unify WebKit data stores: {error}");
            }
        }
        let (events_tx, events_rx) = mpsc::channel();
        let permissions = Arc::new(Mutex::new(HashMap::new()));
        // App-wide monitors; they route each event to its own window's engine through `WINDOWS`.
        static MONITORS: OnceLock<()> = OnceLock::new();
        MONITORS.get_or_init(|| {
            std::mem::forget(install_shortcuts());
            std::mem::forget(install_page_clicks());
        });
        Self {
            extensions: extensions.unwrap_or_else(|| Extensions::new(events_tx.clone())),
            events_tx,
            events_rx,
            tabs: HashMap::new(),
            visible: None,
            content: None,
            chrome: None,
            bounds: LogicalRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 },
            stacked: Vec::new(),
            placed_holes: None,
            permissions,
            settings: EngineSettings::default(),
            https_only: Arc::new(AtomicBool::new(false)),
            dimmers: HashMap::new(),
            window_no: 0,
            cookie_dir: None,
            cookie_restore: HashSet::new(),
            cookie_used: HashSet::new(),
            held_loads: Rc::new(RefCell::new(HashMap::new())),
            hold_media: HashSet::new(),
        }
    }

    /// The next page created for each of these tabs waits for a click before playing media,
    /// so a video that was playing when the app quit comes back paused.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    pub fn hold_media(&mut self, tab_ids: impl IntoIterator<Item = String>) {
        self.hold_media.extend(tab_ids);
    }

    /// WebKit drops session cookies (no expiry) on quit, logging users out of sites that use them.
    /// With `restore`, snapshots in `dir` are put back before a store's first page loads and kept
    /// current by `save_session_cookies`; without it the snapshots are deleted.
    pub fn set_session_cookie_dir(&mut self, dir: PathBuf, restore: bool) {
        if !restore {
            let _ = std::fs::remove_dir_all(&dir);
            self.cookie_dir = None;
            return;
        }
        self.cookie_restore = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "plist"))
            .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
            .collect();
        self.cookie_dir = Some(dir);
    }

    /// Snapshots the session cookies of every store used this run.
    pub fn save_session_cookies(&mut self) {
        let (Some(dir), Some(mtm)) = (self.cookie_dir.clone(), MainThreadMarker::new()) else {
            return;
        };
        for id in &self.cookie_used {
            // Saving mid-restore would overwrite the snapshot with the cookies not yet put back.
            if self.held_loads.borrow().contains_key(id) {
                continue;
            }
            let Ok(store) = data_store(id, mtm) else {
                continue;
            };
            let path = dir.join(format!("{id}.plist"));
            let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
                write_session_cookies(&path, unsafe { cookies.as_ref() });
            });
            unsafe { store.httpCookieStore().getAllCookies(&block) };
        }
    }

    fn begin_cookie_restore(&self, id: &str, store: &WKWebsiteDataStore) {
        let Some(cookies) = self.cookie_dir.as_ref().and_then(|dir| read_session_cookies(&dir.join(format!("{id}.plist")))) else {
            return;
        };
        if cookies.is_empty() {
            return;
        }
        self.held_loads.borrow_mut().insert(id.to_string(), Vec::new());
        let held = Rc::clone(&self.held_loads);
        let id = id.to_string();
        let done = RcBlock::new(move || {
            let loads = held.borrow_mut().remove(&id).unwrap_or_default();
            for (view, url) in loads {
                load_url(&view, &url);
            }
        });
        unsafe { store.httpCookieStore().setCookies_completionHandler(&cookies, Some(&done)) };
    }

    /// Takes a live page out of this window, still loaded, for another window's engine to `adopt_tab`.
    pub fn take_tab(&mut self, tab_id: &str) -> Option<LiveTab> {
        let live = self.tabs.remove(tab_id)?;
        live.view.removeFromSuperview();
        if let Some(dimmer) = self.dimmers.remove(tab_id) {
            dimmer.removeFromSuperview();
        }
        self.stacked.clear();
        if self.visible.as_deref() == Some(tab_id) {
            self.visible = None;
        }
        Some(live)
    }

    /// Takes over a page from another window; it shows on the next `place_tab`.
    pub fn adopt_tab(&mut self, tab_id: &str, live: LiveTab) {
        *live.delegate.ivars().events.borrow_mut() = self.events_tx.clone();
        live.view.setHidden(true);
        self.tabs.insert(tab_id.to_string(), live);
    }

    pub fn take_page_click(&mut self) -> Option<String> {
        with_window(self.window_no, |state| state.click.take()).flatten()
    }

    /// Makes the tab's page first responder, so keys and page shortcuts reach it without a click.
    pub fn focus_page(&mut self, tab_id: &str) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        if let Some(window) = live.view.window() {
            let view = live.view.clone();
            window.makeFirstResponder(Some(&view));
        }
    }

    pub fn set_zoom(&mut self, tab_id: &str, zoom: f64) {
        if let Some(live) = self.tabs.get(tab_id) {
            if (unsafe { live.view.pageZoom() } - zoom).abs() > f64::EPSILON {
                unsafe { live.view.setPageZoom(zoom) };
            }
        }
    }

    /// Load progress in 0..=1 while the tab's page is loading.
    pub fn progress(&self, tab_id: &str) -> Option<f64> {
        let live = self.tabs.get(tab_id)?;
        unsafe { live.view.isLoading() }.then(|| unsafe { live.view.estimatedProgress() })
    }

    /// Fades the page view; it is a native view, so GPUI opacity does not reach it.
    pub fn set_alpha(&mut self, tab_id: &str, alpha: f64) {
        if let Some(live) = self.tabs.get(tab_id) {
            live.view.setAlphaValue(alpha);
        }
    }

    /// Opens WebKit's native Web Inspector for the tab (private `_inspector`; needs developer extras on).
    pub fn inspect(&mut self, tab_id: &str) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        if !live.view.respondsToSelector(sel!(_inspector)) {
            return;
        }
        unsafe {
            let inspector: *mut AnyObject = msg_send![&*live.view, _inspector];
            if let Some(inspector) = inspector.as_ref() {
                let _: () = msg_send![inspector, show];
            }
        }
    }

    pub fn print(&mut self, tab_id: &str) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let Some(window) = live.view.window() else {
            return;
        };
        let operation = unsafe { live.view.printOperationWithPrintInfo(&NSPrintInfo::sharedPrintInfo()) };
        // WebKit's print view starts zero-sized and prints blank pages unless given the page's size.
        if let Some(view) = operation.view() {
            view.setFrame(live.view.bounds());
        }
        unsafe {
            operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(&window, None, None, std::ptr::null_mut())
        };
    }

    pub fn load_html(&mut self, tab_id: &str, html: &str, base_url: Option<&str>) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let base = base_url.and_then(|url| NSURL::URLWithString(&NSString::from_str(url)));
        unsafe {
            live.view.loadHTMLString_baseURL(&NSString::from_str(html), base.as_deref());
        }
    }

    /// Zoom, user agent and inspector apply to open tabs; the rest to tabs created afterwards.
    pub fn set_settings(&mut self, settings: EngineSettings) {
        self.https_only.store(settings.https_only, Ordering::Relaxed);
        self.extensions.set_inspectable(settings.inspectable);
        for live in self.tabs.values() {
            apply_view_settings(&live.view, &settings);
        }
        self.settings = settings;
    }

    /// While a chrome modal is open, pages stay visible but dimmed and stop taking clicks.
    pub fn set_modal(&mut self, on: bool) {
        with_window(self.window_no, |state| state.modal = on);
        // Dim layers live inside each page view: sibling views do not reliably draw over WebKit's layers.
        self.dimmers.retain(|id, dimmer| {
            let keep = on && self.tabs.contains_key(id);
            if !keep {
                dimmer.removeFromSuperview();
            }
            keep
        });
        let Some(mtm) = MainThreadMarker::new().filter(|_| on) else {
            return;
        };
        for (id, live) in &self.tabs {
            if self.dimmers.contains_key(id) {
                continue;
            }
            let dimmer = NSView::initWithFrame(NSView::alloc(mtm), live.view.bounds());
            dimmer.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
            dimmer.setWantsLayer(true);
            if let Some(layer) = dimmer.layer() {
                layer.setBackgroundColor(Some(&CGColor::new_generic_gray(0.0, 0.45)));
            }
            live.view.addSubview(&dimmer);
            self.dimmers.insert(id.clone(), dimmer);
        }
    }

    /// Removes cookies, caches and site storage from one profile's data store.
    pub fn clear_site_data(&mut self, data_store_id: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Ok(store) = data_store(data_store_id, mtm) else {
            return;
        };
        let block = RcBlock::new(|| {});
        unsafe {
            store.removeDataOfTypes_modifiedSince_completionHandler(
                &WKWebsiteDataStore::allWebsiteDataTypes(mtm),
                &NSDate::distantPast(),
                &block,
            );
        }
    }

    /// Forces the app (and so every page's prefers-color-scheme) light or dark; None follows the system.
    pub fn set_appearance(&mut self, dark: Option<bool>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let appearance = dark.and_then(|dark| {
            NSAppearance::appearanceNamed(unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } })
        });
        NSApplication::sharedApplication(mtm).setAppearance(appearance.as_deref());
    }

    pub fn set_permissions(&mut self, entries: Vec<(String, String, String)>) {
        let mut map = self.permissions.lock().unwrap_or_else(PoisonError::into_inner);
        map.clear();
        for (origin, kind, decision) in entries {
            map.insert(format!("{origin}\n{kind}"), decision);
        }
    }

    /// `notches` are (outer corner, arc centre) points of rounded-corner slivers that also show the page.
    /// Areas (window coordinates, top-left origin) that hit-test to the chrome even over a page.
    pub fn set_grab_zones(&mut self, zones: &[LogicalRect]) {
        let Some(content) = &self.content else {
            return;
        };
        let height = content.bounds().size.height;
        let rects = zones
            .iter()
            .map(|zone| NSRect::new(NSPoint::new(zone.x, height - zone.y - zone.height), NSSize::new(zone.width, zone.height)))
            .collect();
        with_window(self.window_no, |state| state.grabs = rects);
    }

    pub fn set_page_holes(&mut self, holes: &[(String, LogicalRect)], notches: &[Notch]) {
        let Some(chrome) = &self.chrome else {
            return;
        };
        let Some(content) = &self.content else {
            return;
        };
        let size = content.bounds().size;
        if self
            .placed_holes
            .as_ref()
            .is_some_and(|(old, old_notches, old_size)| old.as_slice() == holes && old_notches.as_slice() == notches && *old_size == size)
        {
            return;
        }
        chrome.setWantsLayer(true);
        let Some(host) = chrome.layer() else {
            return;
        };
        self.placed_holes = Some((holes.to_vec(), notches.to_vec(), size));
        let height = size.height;
        let mut placed = Vec::with_capacity(holes.len());
        for (tab_id, hole) in holes {
            let rect = NSRect::new(
                NSPoint::new(hole.x, height - hole.y - hole.height.max(1.0)),
                NSSize::new(hole.width.max(1.0), hole.height.max(1.0)),
            );
            placed.push(PageHole { tab_id: tab_id.clone(), rect });
        }
        let rects = placed.iter().map(|hole| hole.rect).collect::<Vec<_>>();
        with_window(self.window_no, |state| state.holes = placed);
        // Mask changes must land in the same frame as the GPUI paint, not animate in.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        unsafe {
            let shape = CAShapeLayer::new();
            let path = CGMutablePath::new();
            let bounds = chrome.bounds();
            CGMutablePath::add_rect(Some(&path), std::ptr::null(), bounds);
            {
                for hole in rects {
                    let local = chrome.convertRect_fromView(hole, Some(content));
                    let rect = if chrome.isFlipped() == host.isGeometryFlipped() {
                        local
                    } else {
                        NSRect::new(
                            NSPoint::new(local.origin.x, bounds.size.height - local.origin.y - local.size.height),
                            local.size,
                        )
                    };
                    CGMutablePath::add_rect(Some(&path), std::ptr::null(), rect);
                }
            }
            let flip = chrome.isFlipped() != host.isGeometryFlipped();
            let point = |(x, y): (f64, f64)| {
                let local = chrome.convertPoint_fromView(NSPoint::new(x, height - y), Some(content));
                (local.x, if flip { bounds.size.height - local.y } else { local.y })
            };
            for &(corner, center) in notches {
                let (ox, oy) = point(corner);
                let (cx, cy) = point(center);
                let radius = (ox - cx).abs();
                CGMutablePath::move_to_point(Some(&path), std::ptr::null(), ox, cy);
                CGMutablePath::add_arc_to_point(Some(&path), std::ptr::null(), ox, oy, cx, oy, radius);
                CGMutablePath::add_line_to_point(Some(&path), std::ptr::null(), ox, oy);
                CGMutablePath::close_subpath(Some(&path));
            }
            shape.setPath(Some(&path));
            shape.setFillRule(kCAFillRuleEvenOdd);
            shape.setFrame(bounds);
            host.setMask(Some(&shape));
        }
        CATransaction::commit();
    }

    /// Orders live page views back to front, directly below the GPUI chrome view.
    pub fn stack_tabs(&mut self, ids: &[String]) {
        if self.stacked == ids {
            return;
        }
        self.stacked = ids.to_vec();
        for id in ids {
            if let Some(live) = self.tabs.get(id) {
                self.insert_below_chrome(&live.view);
            }
        }
    }

    fn insert_below_chrome(&self, view: &NSView) {
        let Some(content) = &self.content else {
            return;
        };
        match &self.chrome {
            Some(chrome) => content.addSubview_positioned_relativeTo(view, NSWindowOrderingMode::Below, Some(chrome)),
            None => content.addSubview(view),
        }
    }
}

impl Default for MacEngine {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Drop for MacEngine {
    fn drop(&mut self) {
        // A closed window's pages stop; their tabs stay in the model and load again wherever they are shown next.
        for id in self.tabs.keys().cloned().collect::<Vec<_>>() {
            self.extensions.suspended(&id);
            self.drop_view(&id);
        }
        if let Some(windows) = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            windows.remove(&self.window_no);
        }
    }
}

impl WebEngine for MacEngine {
    fn attach(&mut self, ns_view: *mut c_void) -> anyhow::Result<()> {
        let chrome = unsafe { Retained::retain(ns_view as *mut NSView) }
            .ok_or_else(|| anyhow::anyhow!("GPUI view pointer was null"))?;
        let window = chrome.window().ok_or_else(|| anyhow::anyhow!("GPUI view has no window"))?;
        self.window_no = window.windowNumber();
        let events = self.events_tx.clone();
        with_window(self.window_no, |state| state.events = Some(events));
        let content = window
            .contentView()
            .ok_or_else(|| anyhow::anyhow!("window has no content view"))?;
        install_hit_proxy(&chrome);
        if let Some(mtm) = MainThreadMarker::new() {
            install_edit_menu(mtm);
            track_mouse_moves(&chrome, mtm);
        }
        self.chrome = Some(chrome);
        self.content = Some(content);
        self.apply_frame();
        Ok(())
    }

    fn set_bounds(&mut self, bounds: LogicalRect) {
        self.bounds = bounds;
        self.apply_frame();
    }

    fn ensure_tab(&mut self, tab_id: &str, data_store_id: &str, url: &str) -> anyhow::Result<()> {
        if self.tabs.contains_key(tab_id) {
            return Ok(());
        }
        let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("WebKit must be created on the main thread"))?;
        let store = data_store(data_store_id, mtm)?;
        let popup = POPUPS.with_borrow_mut(|popups| popups.remove(url));
        let adopted = popup.is_some();
        let view = match popup {
            Some(view) => view,
            None => self.new_view(tab_id, data_store_id, url, &store, mtm)?,
        };
        apply_view_settings(&view, &self.settings);
        detach_inspector(&view, mtm);
        let delegate = self.page_delegate(tab_id, mtm);
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            // A popup shares its opener's content controller, which already has the handler and script.
            if !adopted {
                view.configuration().userContentController().addScriptMessageHandler_contentWorld_name(
                    ProtocolObject::from_ref(&*delegate),
                    &key_world(mtm),
                    ns_string!("nxShortcut"),
                );
                add_key_script(&view.configuration().userContentController(), mtm);
            }
        }
        observe(&view, &delegate);
        self.extensions.opened(tab_id, data_store_id, url, &view, mtm);
        self.tabs.insert(tab_id.to_string(), LiveTab { view: view.clone(), delegate });
        self.cookie_used.insert(data_store_id.to_string());
        if self.cookie_restore.remove(data_store_id) {
            self.begin_cookie_restore(data_store_id, &store);
        }
        // An adopted popup is already loading its page.
        let held = adopted || self.held_loads.borrow_mut().get_mut(data_store_id).map(|loads| loads.push((view.clone(), url.to_string()))).is_some();
        if !held {
            self.navigate(tab_id, url)?;
        }
        view.setHidden(true);
        Ok(())
    }

    fn close_tab(&mut self, tab_id: &str) {
        self.extensions.closed(tab_id);
        self.drop_view(tab_id);
    }

    fn suspend_tab(&mut self, tab_id: &str) {
        self.hold_media.insert(tab_id.to_string());
        self.extensions.suspended(tab_id);
        self.drop_view(tab_id);
    }

    fn show_tab(&mut self, tab_id: Option<&str>) {
        let mut inserted = false;
        for (id, live) in &self.tabs {
            let visible = Some(id.as_str()) == tab_id;
            live.view.setHidden(!visible);
            if visible && unsafe { live.view.superview() }.is_none() {
                self.insert_below_chrome(&live.view);
                inserted = true;
            }
        }
        if inserted {
            self.stacked.clear();
        }
        self.visible = tab_id.map(str::to_string);
        self.apply_frame();
    }

    fn place_tab(&mut self, tab_id: &str, bounds: Option<LogicalRect>) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let Some(bounds) = bounds else {
            live.view.setHidden(true);
            return;
        };
        let Some(content) = self.content.clone() else {
            return;
        };
        let height = content.bounds().size.height;
        let frame = NSRect::new(
            NSPoint::new(bounds.x, height - bounds.y - bounds.height.max(1.0)),
            NSSize::new(bounds.width.max(1.0), bounds.height.max(1.0)),
        );
        let inserted = unsafe {
            if live.view.isHidden() {
                live.view.setHidden(false);
            }
            if live.view.frame() != frame {
                live.view.setFrame(frame);
            }
            let detached = live.view.superview().is_none();
            if detached {
                round_bottom_corners(&live.view);
                self.insert_below_chrome(&live.view);
            }
            detached
        };
        if inserted {
            self.stacked.clear();
        }
        self.visible = Some(tab_id.to_string());
    }

    fn navigate(&mut self, tab_id: &str, url: &str) -> anyhow::Result<()> {
        let Some(live) = self.tabs.get(tab_id) else {
            return Ok(());
        };
        load_url(&live.view, url);
        Ok(())
    }

    fn back(&mut self, tab_id: &str) {
        if let Some(live) = self.tabs.get(tab_id) {
            unsafe { live.view.goBack() };
        }
    }

    fn forward(&mut self, tab_id: &str) {
        if let Some(live) = self.tabs.get(tab_id) {
            unsafe { live.view.goForward() };
        }
    }

    fn reload(&mut self, tab_id: &str) {
        if let Some(live) = self.tabs.get(tab_id) {
            unsafe { live.view.reload() };
        }
    }

    fn find(&mut self, tab_id: &str, query: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        if query.is_empty() {
            return;
        }
        let config = unsafe { WKFindConfiguration::new(mtm) };
        let block = RcBlock::new(|_result: std::ptr::NonNull<objc2_web_kit::WKFindResult>| {});
        unsafe {
            live.view.findString_withConfiguration_completionHandler(
                &NSString::from_str(query),
                Some(&config),
                &block,
            );
        }
    }

    fn eval(&mut self, tab_id: &str, token: &str, javascript: &str) {
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let tx = self.events_tx.clone();
        let tab_id = tab_id.to_string();
        let token = token.to_string();
        let block = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let text = if !error.is_null() {
                let description = unsafe { &*error }.localizedDescription();
                ns_string(&description)
            } else if value.is_null() {
                "null".into()
            } else {
                let description: Retained<NSString> = unsafe { msg_send![&*value, description] };
                ns_string(&description)
            };
            let _ = tx.send(EngineEvent::Eval {
                tab_id: tab_id.clone(),
                token: token.clone(),
                value: text,
            });
        });
        unsafe {
            live.view.evaluateJavaScript_completionHandler(&NSString::from_str(javascript), Some(&block));
        }
    }

    fn focus_chrome(&mut self) {
        let Some(chrome) = &self.chrome else {
            return;
        };
        if let Some(window) = chrome.window() {
            window.makeFirstResponder(Some(chrome));
        }
    }

    fn remove_data_store(&mut self, data_store_id: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        PRIVATE_STORES.with_borrow_mut(|stores| stores.remove(data_store_id));
        let Ok(uuid) = parse_uuid(data_store_id) else {
            return;
        };
        let block = RcBlock::new(|_error: *mut NSError| {});
        unsafe {
            WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&uuid, &block, mtm);
        }
        self.cookie_used.remove(data_store_id);
        if let Some(dir) = &self.cookie_dir {
            let _ = std::fs::remove_file(dir.join(format!("{data_store_id}.plist")));
        }
    }

    fn events(&self) -> &Receiver<EngineEvent> {
        &self.events_rx
    }

    fn frame(&self, tab_id: &str) -> Option<(f64, f64)> {
        let live = self.tabs.get(tab_id)?;
        let size = live.view.frame().size;
        Some((size.width, size.height))
    }
}

impl MacEngine {
    fn drop_view(&mut self, tab_id: &str) {
        if let Some(live) = self.tabs.remove(tab_id) {
            for key in OBSERVED_KEYS {
                unsafe { NSObject::removeObserver_forKeyPath(&live.view, &live.delegate, &NSString::from_str(key)) };
            }
            live.view.removeFromSuperview();
            self.stacked.clear();
        }
        if self.visible.as_deref() == Some(tab_id) {
            self.visible = None;
        }
    }

    fn new_view(&mut self, tab_id: &str, data_store_id: &str, url: &str, store: &WKWebsiteDataStore, mtm: MainThreadMarker) -> anyhow::Result<Retained<WKWebView>> {
        let configuration = if url.starts_with(SCHEME) {
            // Retried every tick by the caller until the extension has loaded.
            self.extensions.page_configuration(url, data_store_id).context("extension not loaded yet")?
        } else {
            let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
            let controller = self.extensions.controller(store, data_store_id, mtm);
            unsafe { configuration.setWebExtensionController(controller.as_deref()) };
            configuration
        };
        // Sites like YouTube Music sniff for the Safari suffix that bare WKWebView omits.
        unsafe { configuration.setApplicationNameForUserAgent(Some(&NSString::from_str(&safari_ua_suffix()))) };
        unsafe {
            configuration.setWebsiteDataStore(store);
            let preferences = configuration.preferences();
            preferences.setJavaScriptCanOpenWindowsAutomatically(!self.settings.block_popups);
            preferences.setFraudulentWebsiteWarningEnabled(self.settings.fraud_warnings);
            preferences.setElementFullscreenEnabled(true);
            configuration.defaultWebpagePreferences().setAllowsContentJavaScript(self.settings.javascript);
            if self.hold_media.remove(tab_id) {
                configuration.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::All);
            }
        }
        let frame = objc2_core_foundation::CGRect {
            origin: objc2_core_foundation::CGPoint { x: 0.0, y: 0.0 },
            size: objc2_core_foundation::CGSize { width: 100.0, height: 100.0 },
        };
        let view: Retained<NxWebView> = unsafe { msg_send![NxWebView::alloc(mtm), initWithFrame: frame, configuration: &*configuration] };
        Ok(Retained::into_super(view))
    }

    fn page_delegate(&self, tab_id: &str, mtm: MainThreadMarker) -> Retained<NxWebDelegate> {
        NxWebDelegate::new(
            mtm,
            DelegateIvars {
                events: RefCell::new(self.events_tx.clone()),
                tab_id: tab_id.to_string(),
                permissions: self.permissions.clone(),
                https_only: self.https_only.clone(),
                extensions: self.extensions.handle(),
            },
        )
    }

    /// Shows an extension popup WebKit already built and loaded; its own delegates stay in place.
    fn apply_frame(&self) {
        let Some(content) = &self.content else {
            return;
        };
        let Some(tab_id) = &self.visible else {
            return;
        };
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let height = content.bounds().size.height;
        let width = self.bounds.width.max(1.0);
        let view_height = self.bounds.height.max(1.0);
        let frame = NSRect::new(
            NSPoint::new(self.bounds.x, height - self.bounds.y - view_height),
            NSSize::new(width, view_height),
        );
        live.view.setFrame(frame);
    }
}

/// KVO on `URL` and `title`, so in-page navigations reach the address bar.
fn observe(view: &WKWebView, delegate: &NxWebDelegate) {
    for key in OBSERVED_KEYS {
        unsafe { NSObject::addObserver_forKeyPath_options_context(view, delegate, &NSString::from_str(key), NSKeyValueObservingOptions::New, std::ptr::null_mut()) };
    }
}

/// WebKit docks the inspector into the web view's superview (our shared tab container), resizing
/// views our layout owns. A hidden zero-size attachment view makes it unable to dock, so it opens
/// in its own window. WebKit holds the view weakly; as our subview it lives as long as the page.
fn detach_inspector(view: &WKWebView, mtm: MainThreadMarker) {
    if !view.respondsToSelector(sel!(_setInspectorAttachmentView:)) {
        return;
    }
    let anchor = NSView::new(mtm);
    anchor.setHidden(true);
    view.addSubview(&anchor);
    let _: () = unsafe { msg_send![view, _setInspectorAttachmentView: &*anchor] };
}

fn apply_view_settings(view: &WKWebView, settings: &EngineSettings) {
    let agent = (!settings.user_agent.trim().is_empty()).then(|| NSString::from_str(settings.user_agent.trim()));
    unsafe {
        view.setInspectable(settings.inspectable);
        // Private key: adds Inspect Element to the page's context menu (isInspectable only covers Safari's Develop menu).
        let _: () = msg_send![&*view.configuration().preferences(), setValue: &*NSNumber::new_bool(settings.inspectable), forKey: ns_string!("developerExtrasEnabled")];
        view.setCustomUserAgent(agent.as_deref());
        view.setPageZoom(settings.page_zoom);
    }
}

pub(crate) fn safari_ua_suffix() -> String {
    let version = NSBundle::bundleWithPath(ns_string!("/Applications/Safari.app"))
        .and_then(|bundle| bundle.objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString")))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
        .unwrap_or_else(|| "26.0".to_string());
    format!("Version/{version} Safari/605.1.15")
}

fn load_url(view: &WKWebView, url: &str) {
    unsafe {
        if url == "nxbrowser://home" || url.is_empty() {
            view.loadHTMLString_baseURL(&NSString::from_str(HOME_HTML), None);
        } else if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) {
            let request = NSURLRequest::requestWithURL(&nsurl);
            view.loadRequest(&request);
        }
    }
}

/// Writes the session-only cookies as a binary plist of their property dictionaries, readable only by the user.
fn write_session_cookies(path: &Path, cookies: &NSArray<NSHTTPCookie>) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let props = cookies.iter().filter(|cookie| cookie.isSessionOnly()).filter_map(|cookie| cookie.properties()).collect::<Vec<_>>();
    if props.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let array = NSArray::from_retained_slice(&props);
    let Ok(data) = (unsafe { NSPropertyListSerialization::dataWithPropertyList_format_options_error(&array, NSPropertyListFormat::BinaryFormat_v1_0, 0) }) else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let temp = path.with_extension("tmp");
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .and_then(|mut file| file.write_all(&data.to_vec()));
    if written.is_ok() {
        let _ = std::fs::rename(&temp, path);
    }
}

fn read_session_cookies(path: &Path) -> Option<Retained<NSArray<NSHTTPCookie>>> {
    let data = NSData::with_bytes(&std::fs::read(path).ok()?);
    let plist = unsafe { NSPropertyListSerialization::propertyListWithData_options_format_error(&data, NSPropertyListMutabilityOptions::Immutable, std::ptr::null_mut()) }.ok()?;
    let list = plist.downcast::<NSArray>().ok()?;
    let cookies = list
        .iter()
        .filter_map(|item| item.downcast::<NSDictionary>().ok())
        .filter_map(|props| unsafe { NSHTTPCookie::cookieWithProperties(&*(Retained::as_ptr(&props) as *const NSDictionary<NSString, AnyObject>)) })
        .collect::<Vec<_>>();
    Some(NSArray::from_retained_slice(&cookies))
}

/// WebKit keys profile data by bundle id (`dev.nxbrowser.app`) when bundled and by process name
/// (`nxbrowser`) under `cargo run`, so logins made in one vanished in the other. Merges the
/// unbundled root into the bundled one and leaves a symlink so both launch modes share it.
fn unify_data_root(webkit: &std::path::Path) -> std::io::Result<()> {
    use std::fs;
    let canonical = webkit.join("dev.nxbrowser.app/WebsiteDataStore");
    let legacy = webkit.join("nxbrowser/WebsiteDataStore");
    match fs::symlink_metadata(&legacy) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(_) => {
            fs::create_dir_all(&canonical)?;
            fs::create_dir_all(webkit.join("nxbrowser"))?;
            return std::os::unix::fs::symlink(&canonical, &legacy);
        }
    }
    fs::create_dir_all(&canonical)?;
    let cookies = |dir: &std::path::Path| fs::metadata(dir.join("Cookies/Cookies.binarycookies")).and_then(|m| m.modified()).ok();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // The losing copy of a store is kept here, never deleted.
    let old = webkit.join(format!("nxbrowser/WebsiteDataStore.old-{stamp}"));
    for entry in fs::read_dir(&legacy)? {
        let entry = entry?;
        let name = entry.file_name();
        let target = canonical.join(&name);
        if target.exists() {
            fs::create_dir_all(&old)?;
            if cookies(&target) >= cookies(&entry.path()) {
                fs::rename(entry.path(), old.join(&name))?;
                continue;
            }
            fs::rename(&target, old.join(&name))?;
        }
        fs::rename(entry.path(), &target)?;
    }
    fs::remove_dir(&legacy)?;
    std::os::unix::fs::symlink(&canonical, &legacy)
}

/// The window's corner radius less the page inset in browser-ui, so the page's corners follow the window's.
const PAGE_RADIUS: f64 = 9.0;

/// Rounds a page's bottom corners; the top ones meet the title bar and stay square.
fn round_bottom_corners(view: &NSView) {
    view.setWantsLayer(true);
    let Some(layer) = view.layer() else {
        return;
    };
    // In a flipped layer (WKWebView draws top-down) the bottom is max Y.
    let bottom = if layer.contentsAreFlipped() {
        CACornerMask::LayerMinXMaxYCorner | CACornerMask::LayerMaxXMaxYCorner
    } else {
        CACornerMask::LayerMinXMinYCorner | CACornerMask::LayerMaxXMinYCorner
    };
    layer.setCornerRadius(PAGE_RADIUS);
    layer.setMaskedCorners(bottom);
    layer.setMasksToBounds(true);
}

fn data_store(id: &str, mtm: MainThreadMarker) -> anyhow::Result<Retained<WKWebsiteDataStore>> {
    // Must match `browser_core::PRIVATE_STORE_PREFIX`; this crate does not depend on browser-core.
    if id.starts_with("private-") {
        // One in-memory store per private profile, so its tabs share cookies; dropped with the profile.
        return Ok(PRIVATE_STORES.with_borrow_mut(|stores| {
            stores.entry(id.to_string()).or_insert_with(|| unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) }).clone()
        }));
    }
    let uuid = parse_uuid(id)?;
    Ok(unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm) })
}

fn parse_uuid(id: &str) -> anyhow::Result<Retained<NSUUID>> {
    Uuid::parse_str(id)?;
    NSUUID::from_string(&NSString::from_str(id)).context("invalid WebKit data store UUID")
}

fn install_shortcuts() -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let window = MainThreadMarker::new().and_then(|mtm| event_ref.window(mtm));
        let Some(events) = window.and_then(|window| with_window(window.windowNumber(), |state| state.events.clone()).flatten()) else {
            return event.as_ptr();
        };
        let flags = event_ref.modifierFlags();
        let shift = flags.contains(NSEventModifierFlags::Shift);
        if flags.contains(NSEventModifierFlags::Control) && !flags.contains(NSEventModifierFlags::Command)
            && event_ref.charactersIgnoringModifiers().is_some_and(|chars| ns_string(&chars) == "\t") {
                let _ = events.send(EngineEvent::Shortcut(if shift { Shortcut::PrevTab } else { Shortcut::NextTab }));
                return std::ptr::null_mut();
            }
        if flags.contains(NSEventModifierFlags::Command) && !flags.contains(NSEventModifierFlags::Control) {
            if let Some(chars) = event_ref.charactersIgnoringModifiers() {
                let key = ns_string(&chars).to_ascii_lowercase();
                // A focused page gets these first; the key script reports them back if it ignores them.
                if key == "i" && flags.contains(NSEventModifierFlags::Option) {
                    let _ = events.send(EngineEvent::Shortcut(Shortcut::Inspect));
                    return std::ptr::null_mut();
                }
                if key == "t" && flags.contains(NSEventModifierFlags::Option) {
                    let _ = events.send(EngineEvent::Shortcut(Shortcut::NewTerminal));
                    return std::ptr::null_mut();
                }
                let page_first = PAGE_FIRST_KEYS.contains(&key.as_str()) && page_has_focus(event_ref);
                if let Some(shortcut) = shortcut_for(&key, shift).filter(|_| !page_first) {
                    let _ = events.send(EngineEvent::Shortcut(shortcut));
                    return std::ptr::null_mut();
                }
            }
        }
        event.as_ptr()
    });
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };
    std::mem::forget(block);
    monitor
}

type Notch = ((f64, f64), (f64, f64));

struct PageHole {
    tab_id: String,
    rect: NSRect,
}

/// What the app-wide event monitors and hit-test need to know about one browser window.
#[derive(Default)]
struct WindowState {
    holes: Vec<PageHole>,
    /// Window resize rims (content coordinates); they belong to the chrome even where a page lies beneath.
    grabs: Vec<NSRect>,
    /// A chrome modal is open; pages take no clicks.
    modal: bool,
    /// Tab whose page was clicked since the engine last asked.
    click: Option<String>,
    /// Where this window's shortcuts go.
    events: Option<Sender<EngineEvent>>,
}

/// Per browser window, by `windowNumber`.
static WINDOWS: Mutex<Option<HashMap<isize, WindowState>>> = Mutex::new(None);

fn with_window<T>(window_no: isize, f: impl FnOnce(&mut WindowState) -> T) -> Option<T> {
    let mut windows = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner);
    Some(f(windows.get_or_insert_with(HashMap::new).entry(window_no).or_default()))
}

/// Whether `point` (content coordinates) is over a page and outside the chrome's grab rims.
fn over_page(state: &WindowState, point: NSPoint) -> Option<&PageHole> {
    if state.modal || state.grabs.iter().any(|rect| hole_contains(*rect, point)) {
        return None;
    }
    state.holes.iter().rev().find(|hole| hole_contains(hole.rect, point))
}

fn hole_contains(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.width
        && point.y < rect.origin.y + rect.size.height
}

fn install_page_clicks() -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let Some(mtm) = MainThreadMarker::new() else {
            return event.as_ptr();
        };
        let Some(window) = event_ref.window(mtm) else {
            return event.as_ptr();
        };
        let Some(content) = window.contentView() else {
            return event.as_ptr();
        };
        let point = content.convertPoint_fromView(event_ref.locationInWindow(), None);
        with_window(window.windowNumber(), |state| {
            // The latest click wins: a page click still waiting for the UI tick would blur an address field opened since.
            state.click = over_page(state, point).map(|hit| hit.tab_id.clone());
        });
        event.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &block) };
    std::mem::forget(block);
    monitor
}

/// AppKit sends mouse-moved events only to the first responder, so once a page took focus GPUI saw no
/// moves and its hover states (the window dock, buttons) went dead. A tracking area delivers them regardless.
fn track_mouse_moves(chrome: &NSView, mtm: MainThreadMarker) {
    let options = NSTrackingAreaOptions::MouseMoved | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect;
    let area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(mtm.alloc(), NSRect::ZERO, options, Some(chrome), None)
    };
    chrome.addTrackingArea(&area);
}

static CHROME_CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

fn install_hit_proxy(chrome: &NSView) {
    static PROXY: OnceLock<&'static AnyClass> = OnceLock::new();
    let class = PROXY.get_or_init(|| {
        let original = *CHROME_CLASS.get_or_init(|| chrome.class());
        let name = c"NxPageHitProxy";
        let mut builder = ClassBuilder::new(name, original).expect("NxPageHitProxy");
        unsafe {
            builder.add_method(sel!(hitTest:), hit_test as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(performKeyEquivalent:), key_equivalent as extern "C-unwind" fn(_, _, _) -> _);
            // AppKit picks a drop target with `_hitTest:dragTypes:`, not `hitTest:`, so pages beneath the chrome
            // took every file drop. Without that private selector drops just fall back to the window as before.
            builder.add_method(sel!(_hitTest:dragTypes:), drag_hit_test as extern "C-unwind" fn(_, _, _, _) -> _);
            builder.add_method(sel!(draggingEntered:), drag_entered as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(draggingUpdated:), drag_updated as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(draggingExited:), drag_exited as extern "C-unwind" fn(_, _, _));
            builder.add_method(sel!(performDragOperation:), drag_perform as extern "C-unwind" fn(_, _, _) -> _);
            builder.add_method(sel!(concludeDragOperation:), drag_conclude as extern "C-unwind" fn(_, _, _));
        }
        builder.register()
    });
    unsafe {
        objc2::ffi::object_setClass(std::ptr::from_ref(chrome).cast_mut().cast(), *class);
        let types = NSArray::from_slice(&[ns_string!("public.file-url")]);
        let _: () = msg_send![chrome, registerForDraggedTypes: &*types];
    }
}

/// The chrome takes file drags except over a page showing through it; GPUI handles them on the window.
extern "C-unwind" fn drag_hit_test(this: &AnyObject, _cmd: Sel, point: *mut NSPoint, _types: *mut AnyObject) -> *mut AnyObject {
    let view = unsafe { &*std::ptr::from_ref(this).cast::<NSView>() };
    let (Some(window), Some(point)) = (view.window(), unsafe { point.as_ref() }) else {
        return std::ptr::null_mut();
    };
    if !hole_contains(view.frame(), *point) || with_window(window.windowNumber(), |state| over_page(state, *point).is_some()) == Some(true) {
        return std::ptr::null_mut();
    }
    std::ptr::from_ref(this).cast_mut()
}

fn drag_window(this: &AnyObject) -> Option<Retained<objc2_app_kit::NSWindow>> {
    unsafe { &*std::ptr::from_ref(this).cast::<NSView>() }.window()
}

extern "C-unwind" fn drag_entered(this: &AnyObject, _cmd: Sel, info: *mut AnyObject) -> usize {
    drag_window(this).map_or(0, |window| unsafe { msg_send![&*window, draggingEntered: info] })
}

extern "C-unwind" fn drag_updated(this: &AnyObject, _cmd: Sel, info: *mut AnyObject) -> usize {
    drag_window(this).map_or(0, |window| unsafe { msg_send![&*window, draggingUpdated: info] })
}

extern "C-unwind" fn drag_exited(this: &AnyObject, _cmd: Sel, info: *mut AnyObject) {
    if let Some(window) = drag_window(this) {
        let _: () = unsafe { msg_send![&*window, draggingExited: info] };
    }
}

extern "C-unwind" fn drag_perform(this: &AnyObject, _cmd: Sel, info: *mut AnyObject) -> Bool {
    drag_window(this).map_or(Bool::NO, |window| unsafe { msg_send![&*window, performDragOperation: info] })
}

extern "C-unwind" fn drag_conclude(this: &AnyObject, _cmd: Sel, info: *mut AnyObject) {
    if let Some(window) = drag_window(this) {
        let _: () = unsafe { msg_send![&*window, concludeDragOperation: info] };
    }
}

extern "C-unwind" fn hit_test(this: &AnyObject, _cmd: Sel, point: NSPoint) -> *mut AnyObject {
    let view = unsafe { &*std::ptr::from_ref(this).cast::<NSView>() };
    if let Some(window) = view.window() {
        if with_window(window.windowNumber(), |state| over_page(state, point).is_some()) == Some(true) {
            return std::ptr::null_mut();
        }
    }
    let result: *mut AnyObject = unsafe { msg_send![super(this, NSView::class()), hitTest: point] };
    result
}

/// AppKit offers Cmd-key combos to every view, so GPUI took Cmd+Z/C/V/D… even while a page had focus.
/// While a page is focused they go to the page (and the Edit menu) instead.
extern "C-unwind" fn key_equivalent(this: &AnyObject, _cmd: Sel, event: &NSEvent) -> Bool {
    if crate::extensions::perform_command(event) {
        return Bool::YES;
    }
    if page_has_focus(event) {
        return Bool::NO;
    }
    let original = *CHROME_CLASS.get().expect("chrome class");
    unsafe { msg_send![super(this, original), performKeyEquivalent: event] }
}

/// Whether the event's window has a web page (or a view inside one) as first responder.
fn page_has_focus(event: &NSEvent) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(responder) = event.window(mtm).and_then(|window| window.firstResponder()) else {
        return false;
    };
    let mut view = responder.downcast::<NSView>().ok();
    while let Some(current) = view {
        if current.isKindOfClass(WKWebView::class()) {
            return true;
        }
        view = unsafe { current.superview() };
    }
    false
}

/// App shortcuts. Ones a page may claim are offered to the page first while it has focus
/// (as Chrome does): Excalidraw uses Cmd+D to duplicate, docs editors take Cmd+F.
fn shortcut_for(key: &str, shift: bool) -> Option<Shortcut> {
    match key {
        "t" if shift => Some(Shortcut::ReopenTab),
        "t" => Some(Shortcut::NewTab),
        "w" => Some(Shortcut::CloseTab),
        "l" => Some(Shortcut::FocusUrl),
        "r" => Some(Shortcut::Reload),
        "f" => Some(Shortcut::Find),
        "d" => Some(Shortcut::Bookmark),
        "[" => Some(Shortcut::Back),
        "]" => Some(Shortcut::Forward),
        "," => Some(Shortcut::Settings),
        "p" => Some(Shortcut::Print),
        "k" => Some(Shortcut::CommandPalette),
        "=" | "+" => Some(Shortcut::ZoomIn),
        "-" => Some(Shortcut::ZoomOut),
        "0" => Some(Shortcut::ZoomReset),
        _ => key.parse::<usize>().ok().filter(|n| (1..=9).contains(n)).map(Shortcut::SelectTab),
    }
}

const PAGE_FIRST_KEYS: [&str; 11] = ["d", "f", "r", "[", "]", "p", "=", "+", "-", "0", "k"];

/// Runs in its own world; reports page-first shortcuts the page did not `preventDefault`.
const KEY_FALLBACK_JS: &str = r#"addEventListener('keydown', (e) => {
  const key = (e.key || '').toLowerCase();
  if (!e.metaKey || e.ctrlKey || !['d', 'f', 'r', '[', ']', 'p', '=', '+', '-', '0', 'k'].includes(key)) return;
  setTimeout(() => { if (!e.defaultPrevented) webkit.messageHandlers.nxShortcut.postMessage(key); }, 0);
}, true);"#;

fn key_world(mtm: MainThreadMarker) -> Retained<WKContentWorld> {
    unsafe { WKContentWorld::worldWithName(ns_string!("nxbrowser-keys"), mtm) }
}

fn add_key_script(controller: &WKUserContentController, mtm: MainThreadMarker) {
    let script = unsafe {
        WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
            WKUserScript::alloc(mtm),
            &NSString::from_str(KEY_FALLBACK_JS),
            WKUserScriptInjectionTime::AtDocumentStart,
            true,
            &key_world(mtm),
        )
    };
    unsafe { controller.addUserScript(&script) };
}

/// GPUI sets no menu bar, and WebKit's copy/paste/undo/select-all only run as Edit menu actions,
/// so pages never got them. Items have no target: they go to whatever has focus.
fn install_edit_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let main = app.mainMenu().unwrap_or_else(|| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
        let app_item = NSMenuItem::new(mtm);
        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("NxBrowser"));
        app_menu.addItem(&menu_item(mtm, "Quit NxBrowser", sel!(terminate:), "q", false));
        app_item.setSubmenu(Some(&app_menu));
        menu.addItem(&app_item);
        app.setMainMenu(Some(&menu));
        menu
    });
    if main.itemWithTitle(ns_string!("Edit")).is_some() {
        return;
    }
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
    edit.addItem(&menu_item(mtm, "Undo", sel!(undo:), "z", false));
    edit.addItem(&menu_item(mtm, "Redo", sel!(redo:), "z", true));
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    edit.addItem(&menu_item(mtm, "Cut", sel!(cut:), "x", false));
    edit.addItem(&menu_item(mtm, "Copy", sel!(copy:), "c", false));
    edit.addItem(&menu_item(mtm, "Paste", sel!(paste:), "v", false));
    edit.addItem(&menu_item(mtm, "Select All", sel!(selectAll:), "a", false));
    let item = NSMenuItem::new(mtm);
    item.setTitle(ns_string!("Edit"));
    item.setSubmenu(Some(&edit));
    main.addItem(&item);
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str, shift: bool) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(action), &NSString::from_str(key))
    };
    if shift {
        item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    }
    item
}

fn ns_string(value: &NSString) -> String {
    objc2::rc::autoreleasepool(|pool| unsafe { value.to_str(pool).to_string() })
}

#[cfg(test)]
mod tests {
    use super::unify_data_root;
    use std::fs;

    #[test]
    fn unbundled_stores_merge_into_bundled_root() {
        let root = std::env::temp_dir().join(format!("nx-webkit-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("nxbrowser/WebsiteDataStore");
        let canonical = root.join("dev.nxbrowser.app/WebsiteDataStore");
        let write = |dir: &std::path::Path, store: &str, body: &str| {
            fs::create_dir_all(dir.join(store).join("Cookies")).unwrap();
            fs::write(dir.join(store).join("Cookies/Cookies.binarycookies"), body).unwrap();
        };
        write(&canonical, "shared", "bundled-older");
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&legacy, "shared", "unbundled-newer");
        write(&legacy, "only-legacy", "legacy");

        unify_data_root(&root).unwrap();
        unify_data_root(&root).unwrap();

        let read = |store: &str| fs::read_to_string(canonical.join(store).join("Cookies/Cookies.binarycookies")).unwrap();
        assert_eq!(read("shared"), "unbundled-newer");
        assert_eq!(read("only-legacy"), "legacy");
        assert!(fs::symlink_metadata(&legacy).unwrap().file_type().is_symlink());
        assert_eq!(read_via(&legacy, "shared"), "unbundled-newer");
        let kept = fs::read_dir(root.join("nxbrowser")).unwrap().filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().starts_with("WebsiteDataStore.old-"));
        assert!(kept, "replaced store must be kept, not deleted");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn session_cookies_round_trip_and_persistent_ones_are_skipped() {
        use objc2_foundation::{NSDictionary, NSHTTPCookie, NSString, NSURL};
        let url = NSURL::URLWithString(&NSString::from_str("https://example.com/")).unwrap();
        let cookies = |header: &str| {
            let fields = NSDictionary::from_slices(&[&*NSString::from_str("Set-Cookie")], &[&*NSString::from_str(header)]);
            NSHTTPCookie::cookiesWithResponseHeaderFields_forURL(&fields, &url)
        };
        let path = std::env::temp_dir().join(format!("nx-cookies-{}/store.plist", uuid::Uuid::new_v4()));
        super::write_session_cookies(&path, &cookies("sid=abc; Path=/; Secure; HttpOnly"));
        let back = super::read_session_cookies(&path).unwrap();
        assert_eq!(back.len(), 1);
        let cookie = back.objectAtIndex(0);
        assert_eq!((cookie.name().to_string(), cookie.value().to_string()), ("sid".into(), "abc".into()));
        assert!(cookie.isSessionOnly() && cookie.isSecure() && cookie.isHTTPOnly());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Only persistent cookies left: WebKit keeps those itself, so the snapshot goes away.
        super::write_session_cookies(&path, &cookies("keep=1; Path=/; Max-Age=3600"));
        assert!(!path.exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn read_via(dir: &std::path::Path, store: &str) -> String {
        fs::read_to_string(dir.join(store).join("Cookies/Cookies.binarycookies")).unwrap()
    }
}
