use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use block2::RcBlock;
use browser_engine::{EngineEvent, EngineSettings, LogicalRect, ScriptInjection, Shortcut, WebEngine};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSEvent, NSEventMask, NSEventModifierFlags, NSView, NSWindowOrderingMode};
use objc2_core_graphics::{CGColor, CGMutablePath};
use objc2_quartz_core::{kCAFillRuleEvenOdd, CAShapeLayer, CATransaction};
use objc2_foundation::{
    ns_string, NSDate, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
    NSURLRequest, NSUUID,
};
use objc2_web_kit::{
    WKContentWorld, WKFindConfiguration, WKMediaCaptureType, WKNavigation, WKNavigationAction,
    WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse, WKNavigationResponsePolicy,
    WKPermissionDecision, WKScriptMessage, WKScriptMessageHandler, WKSecurityOrigin, WKUIDelegate,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore, WKWindowFeatures,
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

struct DelegateIvars {
    events: Sender<EngineEvent>,
    tab_id: String,
    permissions: Arc<Mutex<HashMap<String, String>>>,
    https_only: Arc<AtomicBool>,
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
                if let Some(url) = navigation_response.response().URL().and_then(|url| url.absoluteString()) {
                    let _ = self.ivars().events.send(EngineEvent::Download {
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
            let _ = self.ivars().events.send(EngineEvent::Failed {
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
            let _ = self.ivars().events.send(EngineEvent::Failed {
                tab_id: self.ivars().tab_id.clone(),
                message: ns_string(&error.localizedDescription()),
            });
        }
    }

    unsafe impl WKUIDelegate for NxWebDelegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        unsafe fn webView_createWebViewWithConfiguration_forNavigationAction_windowFeatures(
            &self,
            _web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            if navigation_action.targetFrame().is_none() {
                if let Some(url) = navigation_action.request().URL().and_then(|url| url.absoluteString()) {
                    let _ = self.ivars().events.send(EngineEvent::OpenInNewTab { url: ns_string(&url) });
                }
            }
            None
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn webView_runJavaScriptAlertPanelWithMessage_initiatedByFrame_completionHandler(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            _frame: &objc2_web_kit::WKFrameInfo,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let _ = self.ivars().events.send(EngineEvent::Alert {
                message: ns_string(message),
            });
            completion_handler.call(());
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
                .ok()
                .and_then(|map| map.get(&key).cloned())
                .unwrap_or_else(|| "deny".into());
            let _ = self.ivars().events.send(EngineEvent::Permission {
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

    unsafe impl WKScriptMessageHandler for NxWebDelegate {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn userContentController_didReceiveScriptMessage(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            let body = message.body();
            let text = match body.downcast::<NSString>() {
                Ok(string) => ns_string(&string),
                Err(body) => {
                    let description: Retained<NSString> = msg_send![&*body, description];
                    ns_string(&description)
                }
            };
            let _ = self.ivars().events.send(EngineEvent::ScriptMessage {
                tab_id: self.ivars().tab_id.clone(),
                body: text,
            });
        }
    }
);

impl NxWebDelegate {
    fn new(mtm: MainThreadMarker, ivars: DelegateIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn emit_page(&self, web_view: &WKWebView, loading: bool) {
        let url = unsafe { web_view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|url| ns_string(&url))
            .unwrap_or_default();
        let title = unsafe { web_view.title() }
            .map(|title| ns_string(&title))
            .unwrap_or_default();
        let _ = self.ivars().events.send(EngineEvent::Navigated {
            tab_id: self.ivars().tab_id.clone(),
            url,
            title,
            loading,
        });
    }
}

struct LiveTab {
    view: Retained<WKWebView>,
    delegate: Retained<NxWebDelegate>,
    worlds: Vec<String>,
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
    _monitor: Option<Retained<AnyObject>>,
    _page_monitor: Option<Retained<AnyObject>>,
}

impl MacEngine {
    pub fn new() -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let permissions = Arc::new(Mutex::new(HashMap::new()));
        let monitor = install_shortcuts(events_tx.clone());
        let page_monitor = install_page_clicks();
        Self {
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
            _monitor: monitor,
            _page_monitor: page_monitor,
        }
    }

    pub fn take_page_click(&mut self) -> Option<String> {
        PAGE_CLICK.lock().ok().and_then(|mut click| click.take())
    }

    pub fn focus_page(&mut self) {
        let Some(tab_id) = self.visible.clone() else {
            return;
        };
        let Some(live) = self.tabs.get(&tab_id) else {
            return;
        };
        if let Some(window) = live.view.window() {
            let view = live.view.clone();
            window.makeFirstResponder(Some(&view));
        }
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
        for live in self.tabs.values() {
            apply_view_settings(&live.view, &settings);
        }
        self.settings = settings;
    }

    /// While a chrome modal is open, pages stay visible but dimmed and stop taking clicks.
    pub fn set_modal(&mut self, on: bool) {
        MODAL.store(on, Ordering::Relaxed);
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

    pub fn set_permissions(&mut self, entries: Vec<(String, String, String)>) {
        if let Ok(mut map) = self.permissions.lock() {
            map.clear();
            for (origin, kind, decision) in entries {
                map.insert(format!("{origin}\n{kind}"), decision);
            }
        }
    }

    /// `notches` are (outer corner, arc centre) points of rounded-corner slivers that also show the page.
    pub fn set_page_holes(&mut self, holes: &[(String, LogicalRect)], notches: &[Notch]) {
        let Some(chrome) = &self.chrome else {
            return;
        };
        let Some(content) = &self.content else {
            return;
        };
        let size = unsafe { content.bounds() }.size;
        if self
            .placed_holes
            .as_ref()
            .is_some_and(|(old, old_notches, old_size)| old.as_slice() == holes && old_notches.as_slice() == notches && *old_size == size)
        {
            return;
        }
        let Some(host) = (unsafe {
            chrome.setWantsLayer(true);
            chrome.layer()
        }) else {
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
        if let Ok(mut shared) = HOLES.lock() {
            *shared = placed;
        }
        // Mask changes must land in the same frame as the GPUI paint, not animate in.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        unsafe {
            let shape = CAShapeLayer::new();
            let path = CGMutablePath::new();
            let bounds = chrome.bounds();
            CGMutablePath::add_rect(Some(&path), std::ptr::null(), bounds);
            let holes = HOLES.lock().ok();
            if let Some(holes) = holes.as_ref() {
                for hole in holes.iter() {
                    let local = chrome.convertRect_fromView(hole.rect, Some(content));
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
            shape.setFillRule(unsafe { kCAFillRuleEvenOdd });
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
        unsafe {
            match &self.chrome {
                Some(chrome) => content.addSubview_positioned_relativeTo(view, NSWindowOrderingMode::Below, Some(chrome)),
                None => content.addSubview(view),
            }
        }
    }
}

impl Default for MacEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl WebEngine for MacEngine {
    fn attach(&mut self, ns_view: *mut c_void) -> anyhow::Result<()> {
        let chrome = unsafe { Retained::retain(ns_view as *mut NSView) }
            .ok_or_else(|| anyhow::anyhow!("GPUI view pointer was null"))?;
        let window = chrome.window().ok_or_else(|| anyhow::anyhow!("GPUI view has no window"))?;
        let content = window
            .contentView()
            .ok_or_else(|| anyhow::anyhow!("window has no content view"))?;
        install_hit_proxy(&chrome);
        self.chrome = Some(chrome);
        self.content = Some(content);
        self.apply_frame();
        Ok(())
    }

    fn set_bounds(&mut self, bounds: LogicalRect) {
        self.bounds = bounds;
        self.apply_frame();
    }

    fn ensure_tab(
        &mut self,
        tab_id: &str,
        data_store_id: &str,
        url: &str,
        scripts: &[ScriptInjection],
    ) -> anyhow::Result<()> {
        if self.tabs.contains_key(tab_id) {
            self.set_scripts(tab_id, scripts);
            return Ok(());
        }
        let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("WebKit must be created on the main thread"))?;
        let configuration = unsafe { WKWebViewConfiguration::new(mtm) };
        let store = data_store(data_store_id, mtm)?;
        unsafe {
            configuration.setWebsiteDataStore(&store);
            let preferences = configuration.preferences();
            preferences.setJavaScriptCanOpenWindowsAutomatically(!self.settings.block_popups);
            preferences.setFraudulentWebsiteWarningEnabled(self.settings.fraud_warnings);
            configuration.defaultWebpagePreferences().setAllowsContentJavaScript(self.settings.javascript);
        }
        let view = unsafe {
            WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                objc2_core_foundation::CGRect {
                    origin: objc2_core_foundation::CGPoint { x: 0.0, y: 0.0 },
                    size: objc2_core_foundation::CGSize { width: 100.0, height: 100.0 },
                },
                &configuration,
            )
        };
        apply_view_settings(&view, &self.settings);
        let delegate = NxWebDelegate::new(
            mtm,
            DelegateIvars {
                events: self.events_tx.clone(),
                tab_id: tab_id.to_string(),
                permissions: self.permissions.clone(),
                https_only: self.https_only.clone(),
            },
        );
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        self.tabs.insert(
            tab_id.to_string(),
            LiveTab {
                view,
                delegate,
                worlds: Vec::new(),
            },
        );
        self.set_scripts(tab_id, scripts);
        self.navigate(tab_id, url)?;
        unsafe { self.tabs.get(tab_id).unwrap().view.setHidden(true) };
        Ok(())
    }

    fn close_tab(&mut self, tab_id: &str) {
        if let Some(live) = self.tabs.remove(tab_id) {
            unsafe { live.view.removeFromSuperview() };
            self.stacked.clear();
        }
        if self.visible.as_deref() == Some(tab_id) {
            self.visible = None;
        }
    }

    fn suspend_tab(&mut self, tab_id: &str) {
        self.close_tab(tab_id);
    }

    fn show_tab(&mut self, tab_id: Option<&str>) {
        let mut inserted = false;
        for (id, live) in &self.tabs {
            let visible = Some(id.as_str()) == tab_id;
            unsafe { live.view.setHidden(!visible) };
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
            unsafe { live.view.setHidden(true) };
            return;
        };
        let Some(content) = self.content.clone() else {
            return;
        };
        let live = self.tabs.get(tab_id).unwrap();
        let height = unsafe { content.bounds() }.size.height;
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
        unsafe {
            if url == "nxbrowser://home" || url.is_empty() {
                live.view.loadHTMLString_baseURL(&NSString::from_str(HOME_HTML), None);
            } else if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) {
                let request = NSURLRequest::requestWithURL(&nsurl);
                live.view.loadRequest(&request);
            }
        }
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

    fn resolve_extension(&mut self, tab_id: &str, world: &str, javascript: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(live) = self.tabs.get(tab_id) else {
            return;
        };
        let content_world = unsafe { WKContentWorld::worldWithName(&NSString::from_str(world), mtm) };
        unsafe {
            live.view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(javascript),
                None,
                &content_world,
                None,
            );
        }
    }

    fn set_scripts(&mut self, tab_id: &str, scripts: &[ScriptInjection]) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(live) = self.tabs.get_mut(tab_id) else {
            return;
        };
        let controller = unsafe { live.view.configuration().userContentController() };
        unsafe { controller.removeAllUserScripts() };
        for script in scripts {
            if script.source.is_empty() {
                continue;
            }
            let world = unsafe { WKContentWorld::worldWithName(&NSString::from_str(&script.world), mtm) };
            if !live.worlds.iter().any(|name| name == &script.world) {
                unsafe {
                    controller.addScriptMessageHandler_contentWorld_name(
                        ProtocolObject::from_ref(&*live.delegate),
                        &world,
                        ns_string!("nxbrowser"),
                    );
                }
                live.worlds.push(script.world.clone());
            }
            let user_script = unsafe {
                WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                    WKUserScript::alloc(mtm),
                    &NSString::from_str(&script.source),
                    WKUserScriptInjectionTime::AtDocumentStart,
                    true,
                    &world,
                )
            };
            unsafe { controller.addUserScript(&user_script) };
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
        let Ok(uuid) = parse_uuid(data_store_id) else {
            return;
        };
        let block = RcBlock::new(|_error: *mut NSError| {});
        unsafe {
            WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(&uuid, &block, mtm);
        }
    }

    fn events(&self) -> &Receiver<EngineEvent> {
        &self.events_rx
    }

    fn frame(&self, tab_id: &str) -> Option<(f64, f64)> {
        let live = self.tabs.get(tab_id)?;
        let size = unsafe { live.view.frame() }.size;
        Some((size.width, size.height))
    }
}

impl MacEngine {
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
        let height = unsafe { content.bounds() }.size.height;
        let width = self.bounds.width.max(1.0);
        let view_height = self.bounds.height.max(1.0);
        let frame = NSRect::new(
            NSPoint::new(self.bounds.x, height - self.bounds.y - view_height),
            NSSize::new(width, view_height),
        );
        unsafe { live.view.setFrame(frame) };
    }
}

fn apply_view_settings(view: &WKWebView, settings: &EngineSettings) {
    let agent = (!settings.user_agent.trim().is_empty()).then(|| NSString::from_str(settings.user_agent.trim()));
    unsafe {
        view.setInspectable(settings.inspectable);
        view.setCustomUserAgent(agent.as_deref());
        view.setPageZoom(settings.page_zoom);
    }
}

fn data_store(id: &str, mtm: MainThreadMarker) -> anyhow::Result<Retained<WKWebsiteDataStore>> {
    let uuid = parse_uuid(id)?;
    Ok(unsafe { WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm) })
}

fn parse_uuid(id: &str) -> anyhow::Result<Retained<NSUUID>> {
    Uuid::parse_str(id)?;
    NSUUID::from_string(&NSString::from_str(id)).context("invalid WebKit data store UUID")
}

fn install_shortcuts(events: Sender<EngineEvent>) -> Option<Retained<AnyObject>> {
    let block = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let event_ref = unsafe { event.as_ref() };
        let flags = event_ref.modifierFlags();
        if flags.contains(NSEventModifierFlags::Command) && !flags.contains(NSEventModifierFlags::Control) {
            if let Some(chars) = event_ref.charactersIgnoringModifiers() {
                let key = ns_string(&chars).to_ascii_lowercase();
                let shortcut = match key.as_str() {
                    "t" => Some(Shortcut::NewTab),
                    "w" => Some(Shortcut::CloseTab),
                    "l" => Some(Shortcut::FocusUrl),
                    "r" => Some(Shortcut::Reload),
                    "f" => Some(Shortcut::Find),
                    "d" => Some(Shortcut::Bookmark),
                    "[" => Some(Shortcut::Back),
                    "]" => Some(Shortcut::Forward),
                    "," => Some(Shortcut::Settings),
                    _ => None,
                };
                if let Some(shortcut) = shortcut {
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

static HOLES: Mutex<Vec<PageHole>> = Mutex::new(Vec::new());
static PAGE_CLICK: Mutex<Option<String>> = Mutex::new(None);
static MODAL: AtomicBool = AtomicBool::new(false);

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
        let point = unsafe { content.convertPoint_fromView(event_ref.locationInWindow(), None) };
        if MODAL.load(Ordering::Relaxed) {
            return event.as_ptr();
        }
        if let Ok(holes) = HOLES.lock() {
            if let Some(hit) = holes.iter().rev().find(|hole| hole_contains(hole.rect, point)) {
                if let Ok(mut click) = PAGE_CLICK.lock() {
                    *click = Some(hit.tab_id.clone());
                }
            }
        }
        event.as_ptr()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &block) };
    std::mem::forget(block);
    monitor
}

fn install_hit_proxy(chrome: &NSView) {
    static PROXY: OnceLock<&'static AnyClass> = OnceLock::new();
    let class = PROXY.get_or_init(|| {
        let name = CStr::from_bytes_with_nul(b"NxPageHitProxy\0").unwrap();
        let mut builder = ClassBuilder::new(name, chrome.class()).expect("NxPageHitProxy");
        unsafe {
            builder.add_method(sel!(hitTest:), hit_test as extern "C-unwind" fn(_, _, _) -> _);
        }
        builder.register()
    });
    unsafe {
        objc2::ffi::object_setClass(std::ptr::from_ref(chrome).cast_mut().cast(), *class);
    }
}

extern "C-unwind" fn hit_test(this: &AnyObject, _cmd: Sel, point: NSPoint) -> *mut AnyObject {
    if let (false, Ok(holes)) = (MODAL.load(Ordering::Relaxed), HOLES.lock()) {
        if holes.iter().any(|hole| hole_contains(hole.rect, point)) {
            return std::ptr::null_mut();
        }
    }
    let result: *mut AnyObject = unsafe { msg_send![super(this, NSView::class()), hitTest: point] };
    result
}

fn ns_string(value: &NSString) -> String {
    objc2::rc::autoreleasepool(|pool| unsafe { value.to_str(pool).to_string() })
}
