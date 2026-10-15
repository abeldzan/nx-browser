//! Web extensions on WebKit's own runtime (`WKWebExtensionController`), the engine Safari uses.
//! WebKit runs backgrounds, content scripts, popups and the `chrome.*`/`browser.*` APIs; this
//! module tells it about our tabs and windows and turns its requests into engine events.
//!
//! Borrow rule: never hold a `State` borrow across a WebKit call, since WebKit calls back into
//! the tab, window and delegate objects below, which borrow it again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::sync::mpsc::Sender;

use block2::{DynBlock, RcBlock};
use browser_engine::EngineEvent;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{NSApplication, NSEvent, NSImage, NSPopover, NSPopoverBehavior, NSPopoverDelegate, NSViewController};
use objc2_foundation::{NSArray, NSDate, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRectEdge, NSSet, NSSize, NSString, NSURL, NSURLRequest, NSUserDefaults, NSNotification, NSNotificationCenter};
use objc2_web_kit::{
    WKWebExtension, WKWebExtensionAction, WKWebExtensionContext, WKWebExtensionContextPermissionStatus,
    WKWebExtensionController, WKWebExtensionControllerConfiguration, WKWebExtensionControllerDelegate,
    WKWebExtensionMatchPattern, WKWebExtensionPermission, WKWebExtensionTab, WKWebExtensionTabChangedProperties,
    WKWebExtensionTabConfiguration, WKWebExtensionWindow, WKWebExtensionWindowConfiguration, WKWebExtensionWindowState,
    WKWebExtensionWindowType, WKWebExtensionContextErrorsDidUpdateNotification, WKNavigationAction, WKSnapshotConfiguration, WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore, WKWebsiteDataRecord, WKWindowFeatures,
    WKWebExtensionDataRecord, WKWebExtensionDataTypeLocal, WKWebExtensionDataTypeSession, WKWebExtensionDataTypeSynchronized,
};

pub const SCHEME: &str = "webkit-extension://";

type TabReply = RcBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>;

enum Prompt {
    Permissions(RcBlock<dyn Fn(NonNull<NSSet<WKWebExtensionPermission>>, *mut NSDate)>, Retained<NSSet<WKWebExtensionPermission>>),
    Urls(RcBlock<dyn Fn(NonNull<NSSet<NSURL>>, *mut NSDate)>, Retained<NSSet<NSURL>>),
    Patterns(RcBlock<dyn Fn(NonNull<NSSet<WKWebExtensionMatchPattern>>, *mut NSDate)>, Retained<NSSet<WKWebExtensionMatchPattern>>),
}

struct Profile {
    controller: Retained<WKWebExtensionController>,
    _delegate: Retained<NxExtDelegate>,
    window: Retained<NxExtWindow>,
    contexts: HashMap<String, Retained<WKWebExtensionContext>>,
    /// Hidden `KEEPALIVE_PAGE` per extension, which holds its background loaded.
    keepalive: HashMap<String, Retained<WKWebView>>,
}

struct State {
    events: Sender<EngineEvent>,
    inspectable: bool,
    /// Enabled extensions by id with the data stores (profiles) they run in, and their parsed form once WebKit has read them.
    wanted: HashMap<String, Vec<String>>,
    /// Extensions whose folder has the polyfill's `KEEPALIVE_PAGE` (store installs).
    keep_alive: std::collections::HashSet<String>,
    parsed: HashMap<String, Retained<WKWebExtension>>,
    /// One controller per website data store, so extension storage stays per profile.
    profiles: HashMap<String, Profile>,
    tabs: Vec<Retained<NxExtTab>>,
    active: Option<String>,
    /// Pending prompts with the profile and extension they are for.
    prompts: HashMap<u64, (String, String, Prompt)>,
    next_prompt: u64,
    /// `tabs.create` replies waiting for the UI to open a tab at that URL.
    pending_tabs: Vec<(String, TabReply)>,
    /// Where the last clicked action button sits, in window points from the top left: x, y, width, height.
    anchor: [f64; 4],
    /// UI delegate of the open popup's page; WebKit holds delegates weakly.
    popup_ui: Option<Retained<NxPopupUI>>,
    /// The popup shown on the last click before WebKit asked for it; see `presentPopupForAction`.
    early_popup: Option<Retained<WKWebExtensionAction>>,
    /// Prints each new extension error to stderr; the extensions panel only shows the first, truncated.
    _errors_observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

pub struct Extensions(Rc<RefCell<State>>);

impl Extensions {
    pub fn new(events: Sender<EngineEvent>) -> Self {
        let shared = Rc::new(RefCell::new(State {
            events,
            inspectable: true,
            wanted: HashMap::new(),
            keep_alive: Default::default(),
            parsed: HashMap::new(),
            profiles: HashMap::new(),
            tabs: Vec::new(),
            active: None,
            prompts: HashMap::new(),
            next_prompt: 1,
            pending_tabs: Vec::new(),
            anchor: [0.0; 4],
            popup_ui: None,
            early_popup: None,
            _errors_observer: observe_errors(),
        }));
        CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(&shared));
        Self(shared)
    }

    pub fn set_inspectable(&self, inspectable: bool) {
        self.0.borrow_mut().inspectable = inspectable;
        let contexts = self.all_contexts();
        for context in contexts {
            unsafe { context.setInspectable(inspectable) };
        }
    }

    /// The controller every page view in this data store is attached to; created on first use.
    pub fn controller(&self, store: &WKWebsiteDataStore, store_id: &str, mtm: MainThreadMarker) -> Option<Retained<WKWebExtensionController>> {
        if let Some(profile) = self.0.borrow().profiles.get(store_id) {
            return Some(profile.controller.clone());
        }
        let uuid = objc2_foundation::NSUUID::from_string(&NSString::from_str(store_id))?;
        let controller = unsafe {
            let configuration = WKWebExtensionControllerConfiguration::configurationWithIdentifier(&uuid, mtm);
            configuration.setDefaultWebsiteDataStore(Some(store));
            // These are Chrome Web Store builds, which pick their code path from the user agent: without
            // " Chrome/" Bitwarden takes its Safari path, which needs its native app and spins forever.
            let pages = configuration.webViewConfiguration();
            pages.setApplicationNameForUserAgent(Some(&NSString::from_str(&format!("Chrome/131.0.0.0 {}", crate::webkit::safari_ua_suffix()))));
            configuration.setWebViewConfiguration(Some(&pages));
            WKWebExtensionController::initWithConfiguration(WKWebExtensionController::alloc(mtm), &configuration)
        };
        let weak = Rc::downgrade(&self.0);
        let delegate = NxExtDelegate::new(mtm, Ivars { shared: weak.clone(), store: store_id.to_string() });
        let window = NxExtWindow::new(mtm, Ivars { shared: weak, store: store_id.to_string() });
        unsafe {
            controller.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            controller.didOpenWindow(ProtocolObject::from_ref(&*window));
            controller.didFocusWindow(Some(ProtocolObject::from_ref(&*window)));
        }
        self.0.borrow_mut().profiles.insert(
            store_id.to_string(),
            Profile { controller: controller.clone(), _delegate: delegate, window, contexts: HashMap::new(), keepalive: HashMap::new() },
        );
        let ids = self.0.borrow().parsed.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            load_context(&self.0, store_id, &id);
        }
        Some(controller)
    }

    /// Reads the extension at `path` and runs it in every profile; replaces a loaded copy.
    /// Starts the extension in the given data stores only; other profiles never see it.
    pub fn load(&self, id: &str, path: &Path, stores: Vec<String>) {
        self.unload(id);
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        self.0.borrow_mut().wanted.insert(id.to_string(), stores);
        if path.join(KEEPALIVE_PAGE).is_file() {
            self.0.borrow_mut().keep_alive.insert(id.to_string());
        }
        let Some(url) = NSURL::from_directory_path(path) else {
            return;
        };
        let weak = Rc::downgrade(&self.0);
        let id = id.to_string();
        let block = RcBlock::new(move |extension: *mut WKWebExtension, error: *mut NSError| {
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let events = shared.borrow().events.clone();
            // Disabled or replaced while WebKit was still reading it.
            if !shared.borrow().wanted.contains_key(&id) {
                return;
            }
            if let Some(extension) = unsafe { Retained::retain(extension) } {
                shared.borrow_mut().parsed.insert(id.clone(), extension);
                let stores = shared.borrow().profiles.keys().cloned().collect::<Vec<_>>();
                for store in stores {
                    load_context(&shared, &store, &id);
                }
            } else if !error.is_null() {
                let message = error_text(unsafe { &*error });
                eprintln!("extension {id} failed to load: {message}");
            }
            let _ = events.send(EngineEvent::ExtensionChanged { id: id.clone() });
        });
        unsafe { WKWebExtension::extensionWithResourceBaseURL_completionHandler(&url, &block, mtm) };
    }

    pub fn unload(&self, id: &str) {
        let removed = {
            let mut state = self.0.borrow_mut();
            state.wanted.remove(id);
            state.parsed.remove(id);
            state.keep_alive.remove(id);
            state
                .profiles
                .values_mut()
                .filter_map(|profile| {
                    profile.keepalive.remove(id);
                    Some((profile.controller.clone(), profile.contexts.remove(id)?))
                })
                .collect::<Vec<_>>()
        };
        for (controller, context) in removed {
            let _ = unsafe { controller.unloadExtensionContext_error(&context) };
        }
    }

    /// Parse and runtime errors for an extension, newest last.
    pub fn errors(&self, id: &str) -> Vec<String> {
        let (extension, contexts) = {
            let state = self.0.borrow();
            let contexts = state.profiles.values().filter_map(|profile| profile.contexts.get(id).cloned()).collect::<Vec<_>>();
            (state.parsed.get(id).cloned(), contexts)
        };
        let mut errors = extension.map(|extension| unsafe { extension.errors() }.to_vec()).unwrap_or_default();
        for context in contexts {
            errors.extend(unsafe { context.errors() }.to_vec());
        }
        let mut messages = errors.iter().map(|error| error_text(error)).collect::<Vec<_>>();
        messages.dedup();
        messages
    }

    /// Deletes what the extension kept in this profile: its `chrome.storage`, its pages' site data
    /// (IndexedDB, localStorage, caches) and the permissions the user granted it when asked.
    pub fn remove_data(&self, id: &str, store_id: &str) {
        NSUserDefaults::standardUserDefaults().removeObjectForKey(&grants_key(store_id, id));
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // shortcut: a profile no page has opened this run has no controller, so its data stays; create one here if that bites.
        let Some(controller) = self.0.borrow().profiles.get(store_id).map(|profile| profile.controller.clone()) else {
            return;
        };
        let types = unsafe { [WKWebExtensionDataTypeLocal, WKWebExtensionDataTypeSession, WKWebExtensionDataTypeSynchronized] };
        let types = NSSet::from_slice(&types.into_iter().flatten().collect::<Vec<_>>());
        let (wanted, target, kinds) = (id.to_string(), controller.clone(), types.clone());
        let block = RcBlock::new(move |records: NonNull<NSArray<WKWebExtensionDataRecord>>| {
            let records = unsafe { records.as_ref() }.iter().filter(|record| unsafe { record.uniqueIdentifier() }.to_string() == wanted).collect::<Vec<_>>();
            if !records.is_empty() {
                unsafe { target.removeDataOfTypes_fromDataRecords_completionHandler(&kinds, &NSArray::from_retained_slice(&records), &RcBlock::new(|| {})) };
            }
        });
        unsafe { controller.fetchDataRecordsOfTypes_completionHandler(&types, &block) };

        // Extension pages store site data under their `webkit-extension://<id>` origin, named by that host.
        let Some(store) = (unsafe { controller.configuration().defaultWebsiteDataStore() }) else {
            return;
        };
        let (wanted, target, kinds) = (id.to_string(), store.clone(), unsafe { WKWebsiteDataStore::allWebsiteDataTypes(mtm) });
        let block = RcBlock::new(move |records: NonNull<NSArray<WKWebsiteDataRecord>>| {
            let records = unsafe { records.as_ref() }.iter().filter(|record| unsafe { record.displayName() }.to_string().eq_ignore_ascii_case(&wanted)).collect::<Vec<_>>();
            if !records.is_empty() {
                unsafe { target.removeDataOfTypes_forDataRecords_completionHandler(&kinds, &NSArray::from_retained_slice(&records), &RcBlock::new(|| {})) };
            }
        });
        unsafe { store.fetchDataRecordsOfTypes_completionHandler(&WKWebsiteDataStore::allWebsiteDataTypes(mtm), &block) };
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.0.borrow().parsed.contains_key(id)
    }

    /// The options page URL, once the extension has loaded in this profile.
    pub fn options_url(&self, id: &str, store_id: &str) -> Option<String> {
        let context = self.context(id, store_id)?;
        unsafe { context.optionsPageURL() }.and_then(|url| url.absoluteString()).map(|url| url.to_string())
    }

    /// Clicks the toolbar button at `anchor`: WebKit fires `action.onClicked`, or the popup opens below the button.
    pub fn perform_action(&self, id: &str, store_id: &str, tab_id: Option<&str>, anchor: [f64; 4]) {
        let Some(context) = self.context(id, store_id) else {
            return;
        };
        self.0.borrow_mut().anchor = anchor;
        let tab = tab_id.and_then(|tab_id| self.tab(tab_id)).map(ProtocolObject::from_retained);
        unsafe { context.performActionForTab(tab.as_deref()) };
        // WebKit loads the popup but may never call `presentPopupForAction` (Bitwarden's never comes), so show it now.
        if let Some(action) = unsafe { context.actionForTab(tab.as_deref()) }.filter(|action| unsafe { action.presentsPopup() }) {
            if present_popup(&self.0, &action, &context) {
                self.0.borrow_mut().early_popup = Some(action);
            }
        }
    }

    pub fn badge(&self, id: &str, store_id: &str, tab_id: Option<&str>) -> String {
        let Some(context) = self.context(id, store_id) else {
            return String::new();
        };
        let tab = tab_id.and_then(|tab_id| self.tab(tab_id));
        unsafe { context.actionForTab(tab.as_deref().map(ProtocolObject::from_ref)) }
            .map(|action| unsafe { action.badgeText() }.to_string())
            .unwrap_or_default()
    }

    pub fn answer_prompt(&self, prompt: u64, allow: bool) {
        let Some((store, id, prompt)) = self.0.borrow_mut().prompts.remove(&prompt) else {
            return;
        };
        let none = std::ptr::null_mut();
        let granted = match prompt {
            Prompt::Permissions(reply, asked) => {
                let set = if allow { asked } else { NSSet::new() };
                reply.call((NonNull::from(&*set), none));
                set.iter().map(|permission| format!("p:{permission}")).collect::<Vec<_>>()
            }
            Prompt::Urls(reply, asked) => {
                let set = if allow { asked } else { NSSet::new() };
                reply.call((NonNull::from(&*set), none));
                set.iter().filter_map(|url| url.absoluteString()).map(|url| format!("u:{url}")).collect()
            }
            Prompt::Patterns(reply, asked) => {
                let set = if allow { asked } else { NSSet::new() };
                reply.call((NonNull::from(&*set), none));
                set.iter().map(|pattern| format!("m:{}", unsafe { pattern.string() })).collect()
            }
        };
        if !granted.is_empty() {
            let mut saved = saved_grants(&store, &id);
            saved.extend(granted);
            saved.sort();
            saved.dedup();
            save_grants(&store, &id, &saved);
        }
    }

    /// Configuration for a `webkit-extension://` page, so it runs as part of its extension.
    pub fn page_configuration(&self, url: &str, store_id: &str) -> Option<Retained<WKWebViewConfiguration>> {
        let id = url.strip_prefix(SCHEME)?.split('/').next()?;
        unsafe { self.context(id, store_id)?.webViewConfiguration() }
    }

    /// A tab came to life, or a suspended one woke up. Extension pages in tabs are tabs too, as in
    /// Chrome: Phantom's connect window only gets its request when its port's `sender.tab` is set.
    pub fn opened(&self, tab_id: &str, store_id: &str, url: &str, view: &WKWebView, mtm: MainThreadMarker) {
        if let Some(tab) = self.tab(tab_id) {
            *tab.ivars().view.borrow_mut() = Some(view.retain());
            self.handle().changed(tab_id);
            return;
        }
        let tab = NxExtTab::new(
            mtm,
            TabIvars {
                shared: Rc::downgrade(&self.0),
                store: store_id.to_string(),
                tab_id: tab_id.to_string(),
                view: RefCell::new(Some(view.retain())),
                parked: RefCell::new((None, None)),
            },
        );
        let (reply, controller) = {
            let mut state = self.0.borrow_mut();
            state.tabs.push(tab.clone());
            // The UI may rewrite the URL (https-only, a trailing slash), so fall back to the oldest request.
            // shortcut: a tab the user opens while a `tabs.create` is pending answers it; tag requests if that bites.
            let index = state.pending_tabs.iter().position(|(pending, _)| pending == url).or((!state.pending_tabs.is_empty()).then_some(0));
            let reply = index.map(|index| state.pending_tabs.remove(index).1);
            (reply, state.profiles.get(store_id).map(|profile| profile.controller.clone()))
        };
        if let Some(controller) = controller {
            unsafe { controller.didOpenTab(ProtocolObject::from_ref(&*tab)) };
        }
        if let Some(reply) = reply {
            let tab: &ProtocolObject<dyn WKWebExtensionTab> = ProtocolObject::from_ref(&*tab);
            reply.call((tab as *const _ as *mut _, std::ptr::null_mut()));
        }
    }

    /// The tab dropped its page to save memory; extensions keep seeing it, with its last URL and title.
    pub fn suspended(&self, tab_id: &str) {
        if let Some(tab) = self.tab(tab_id) {
            if let Some(view) = tab.ivars().view.borrow_mut().take() {
                *tab.ivars().parked.borrow_mut() = (unsafe { view.URL() }, unsafe { view.title() });
            }
        }
    }

    pub fn closed(&self, tab_id: &str) {
        let (tab, controller) = {
            let mut state = self.0.borrow_mut();
            if state.active.as_deref() == Some(tab_id) {
                state.active = None;
            }
            let tab = state.tabs.iter().position(|tab| tab.ivars().tab_id == tab_id).map(|index| state.tabs.remove(index));
            let controller = tab.as_ref().and_then(|tab| state.profiles.get(&tab.ivars().store)).map(|profile| profile.controller.clone());
            (tab, controller)
        };
        if let (Some(tab), Some(controller)) = (tab, controller) {
            unsafe { controller.didCloseTab_windowIsClosing(ProtocolObject::from_ref(&*tab), false) };
        }
    }

    pub fn activated(&self, tab_id: &str) {
        let Some(tab) = self.tab(tab_id) else {
            return;
        };
        let previous = {
            let mut state = self.0.borrow_mut();
            let previous = state.active.replace(tab_id.to_string());
            if previous.as_deref() == Some(tab_id) {
                return;
            }
            previous
        };
        let previous = previous.and_then(|id| self.tab(&id));
        if let Some(controller) = self.tab_controller(&tab) {
            unsafe { controller.didActivateTab_previousActiveTab(ProtocolObject::from_ref(&*tab), previous.as_deref().map(ProtocolObject::from_ref)) };
        }
    }

    fn context(&self, id: &str, store_id: &str) -> Option<Retained<WKWebExtensionContext>> {
        self.0.borrow().profiles.get(store_id)?.contexts.get(id).cloned()
    }

    fn all_contexts(&self) -> Vec<Retained<WKWebExtensionContext>> {
        self.0.borrow().profiles.values().flat_map(|profile| profile.contexts.values().cloned()).collect()
    }

    fn tab(&self, tab_id: &str) -> Option<Retained<NxExtTab>> {
        self.0.borrow().tabs.iter().find(|tab| tab.ivars().tab_id == tab_id).cloned()
    }

    fn tab_controller(&self, tab: &NxExtTab) -> Option<Retained<WKWebExtensionController>> {
        self.0.borrow().profiles.get(&tab.ivars().store).map(|profile| profile.controller.clone())
    }

    pub fn handle(&self) -> TabNotifier {
        TabNotifier(Rc::downgrade(&self.0))
    }
}

/// Lets page delegates report URL, title and loading changes without owning the engine.
#[derive(Clone)]
pub struct TabNotifier(Weak<RefCell<State>>);

impl TabNotifier {
    pub fn changed(&self, tab_id: &str) {
        let Some(shared) = self.0.upgrade() else {
            return;
        };
        let found = {
            let state = shared.borrow();
            state.tabs.iter().find(|tab| tab.ivars().tab_id == tab_id).cloned().and_then(|tab| {
                let controller = state.profiles.get(&tab.ivars().store)?.controller.clone();
                Some((tab, controller))
            })
        };
        if let Some((tab, controller)) = found {
            let changed = WKWebExtensionTabChangedProperties::URL | WKWebExtensionTabChangedProperties::Title | WKWebExtensionTabChangedProperties::Loading;
            unsafe { controller.didChangeTabProperties_forTab(changed, ProtocolObject::from_ref(&*tab)) };
        }
    }
}

thread_local! {
    /// The engine's extension state, for AppKit key hooks that have no engine reference.
    static CURRENT: RefCell<Weak<RefCell<State>>> = const { RefCell::new(Weak::new()) };
}

/// Runs the extension keyboard shortcut (manifest `commands`) for this key event, if there is one.
pub fn perform_command(event: &NSEvent) -> bool {
    let Some(shared) = CURRENT.with(|current| current.borrow().upgrade()) else {
        return false;
    };
    let contexts = {
        let state = shared.borrow();
        let store = state.active.as_ref().and_then(|active| state.tabs.iter().find(|tab| &tab.ivars().tab_id == active)).map(|tab| tab.ivars().store.clone());
        state.profiles.iter().filter(|(id, _)| store.as_ref().is_none_or(|store| store == *id)).flat_map(|(_, profile)| profile.contexts.values().cloned()).collect::<Vec<_>>()
    };
    contexts.iter().any(|context| unsafe { context.performCommandForEvent(event) })
}

/// Makes a context for a parsed extension in one profile, grants what its manifest asks for
/// (installing it is the user's consent) and starts it.
fn load_context(shared: &Rc<RefCell<State>>, store_id: &str, id: &str) {
    let (extension, controller, inspectable) = {
        let state = shared.borrow();
        let (Some(extension), Some(profile)) = (state.parsed.get(id), state.profiles.get(store_id)) else {
            return;
        };
        if profile.contexts.contains_key(id) || !state.wanted.get(id).is_some_and(|stores| stores.iter().any(|store| store == store_id)) {
            return;
        }
        (extension.clone(), profile.controller.clone(), state.inspectable)
    };
    let context = unsafe { WKWebExtensionContext::contextForExtension(&extension) };
    unsafe {
        context.setUniqueIdentifier(&NSString::from_str(id));
        if let Some(base) = NSURL::URLWithString(&NSString::from_str(&format!("{SCHEME}{id}/"))) {
            context.setBaseURL(&base);
        }
        context.setInspectable(inspectable);
        for permission in extension.requestedPermissions().iter() {
            context.setPermissionStatus_forPermission(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &permission);
        }
        for pattern in extension.allRequestedMatchPatterns().iter() {
            context.setPermissionStatus_forMatchPattern(WKWebExtensionContextPermissionStatus::GrantedExplicitly, &pattern);
        }
        // WebKit leaves keeping the user's answers to optional-permission prompts to the app.
        let granted = WKWebExtensionContextPermissionStatus::GrantedExplicitly;
        for grant in saved_grants(store_id, id) {
            match grant.split_once(':') {
                Some(("p", permission)) => context.setPermissionStatus_forPermission(granted, &NSString::from_str(permission)),
                Some(("u", url)) => {
                    if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
                        context.setPermissionStatus_forURL(granted, &url);
                    }
                }
                Some(("m", pattern)) => {
                    if let Some(pattern) = MainThreadMarker::new().and_then(|mtm| WKWebExtensionMatchPattern::matchPatternWithString(&NSString::from_str(pattern), mtm)) {
                        context.setPermissionStatus_forMatchPattern(granted, &pattern);
                    }
                }
                _ => {}
            }
        }
    }
    if let Err(error) = unsafe { controller.loadExtensionContext_error(&context) } {
        eprintln!("extension {id} did not start: {}", error_text(&error));
        return;
    }
    let keepalive = shared.borrow().keep_alive.contains(id).then(|| keepalive_page(&context, id)).flatten();
    if let Some(profile) = shared.borrow_mut().profiles.get_mut(store_id) {
        profile.contexts.insert(id.to_string(), context);
        if let Some(view) = keepalive {
            profile.keepalive.insert(id.to_string(), view);
        }
    }
}

/// Written next to the polyfill by `browser_extensions::patch_missing_apis`.
const KEEPALIVE_PAGE: &str = "nx-keepalive.html";

/// Loads the extension's keep-alive page in a view no window shows. The polyfill in it holds a port
/// the background pings, so WebKit keeps the background loaded as Chrome does while pages use it.
fn keepalive_page(context: &WKWebExtensionContext, id: &str) -> Option<Retained<WKWebView>> {
    let mtm = MainThreadMarker::new()?;
    let url = NSURL::URLWithString(&NSString::from_str(&format!("{SCHEME}{id}/{KEEPALIVE_PAGE}")))?;
    let configuration = unsafe { context.webViewConfiguration() }?;
    let view = unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)), &configuration) };
    unsafe { view.loadRequest(&NSURLRequest::requestWithURL(&url)) };
    Some(view)
}

fn grants_key(store: &str, id: &str) -> Retained<NSString> {
    NSString::from_str(&format!("NxExtensionGrants.{store}.{id}"))
}

/// Permissions the user allowed when prompted, as `p:`/`u:`/`m:` (permission, URL, match pattern) strings.
fn saved_grants(store: &str, id: &str) -> Vec<String> {
    let defaults = NSUserDefaults::standardUserDefaults();
    defaults.stringArrayForKey(&grants_key(store, id)).map(|list| list.iter().map(|grant| grant.to_string()).collect()).unwrap_or_default()
}

fn save_grants(store: &str, id: &str, grants: &[String]) {
    let list = NSArray::from_retained_slice(&grants.iter().map(|grant| NSString::from_str(grant)).collect::<Vec<_>>());
    unsafe { NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&list), &grants_key(store, id)) };
}

fn observe_errors() -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    // The last error printed per context. WebKit may cap or trim the list, so a count can't tell
    // what is new; everything after the last printed message is.
    let printed = RefCell::new(HashMap::<String, String>::new());
    let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
        let Some(context) = unsafe { notification.as_ref() }.object().and_then(|object| object.downcast::<WKWebExtensionContext>().ok()) else {
            return;
        };
        let id = context_id(&context);
        let errors = unsafe { context.errors() }.iter().map(|error| error_text(&error)).collect::<Vec<_>>();
        let mut printed = printed.borrow_mut();
        let start = printed.get(&id).and_then(|last| errors.iter().rposition(|error| error == last)).map_or(0, |index| index + 1);
        for error in &errors[start..] {
            eprintln!("extension {id} error: {error}");
        }
        if let Some(last) = errors.last() {
            printed.insert(id, last.clone());
        }
    });
    unsafe { NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(WKWebExtensionContextErrorsDidUpdateNotification), None, None, &block) }
}

/// Shows the action's popup as a popover under the clicked button; WebKit sizes it to the page.
/// False when it can't be shown, so WebKit drops the popup instead of counting it as open.
fn present_popup(shared: &Rc<RefCell<State>>, action: &WKWebExtensionAction, context: &WKWebExtensionContext) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(content) = app.keyWindow().or_else(|| app.mainWindow()).or_else(|| app.windows().firstObject()).and_then(|window| window.contentView()) else {
        return false;
    };
    let (popover, view, own) = match (unsafe { action.popupPopover() }, unsafe { action.popupWebView() }) {
        (Some(popover), Some(view)) => (popover, view, false),
        // The extension cleared its popup right after `action.openPopup()` (Phantom does), so WebKit has no page left to give.
        _ => match manifest_popup(context, mtm) {
            Some((popover, view)) => (popover, view, true),
            None => return false,
        },
    };
    if popover.isShown() {
        return true;
    }
    let original = if own { None } else { unsafe { view.UIDelegate() } };
    let own_popover = own.then(|| popover.clone());
    let ui = NxPopupUI::new(mtm, PopupIvars { original, events: shared.borrow().events.clone(), action: action.retain(), own_popover });
    unsafe { view.setUIDelegate(Some(ProtocolObject::from_ref(&*ui))) };
    // WebKit only hears of its popover closing once; a popover shown again would close unnoticed.
    popover.setDelegate(Some(ProtocolObject::from_ref(&*ui)));
    let [x, y, w, h] = shared.borrow().anchor;
    shared.borrow_mut().popup_ui = Some(ui);
    let (rect, edge) = if content.isFlipped() {
        (NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), NSRectEdge::MaxY)
    } else {
        (NSRect::new(NSPoint::new(x, content.bounds().size.height - y - h), NSSize::new(w, h)), NSRectEdge::MinY)
    };
    popover.showRelativeToRect_ofView_preferredEdge(rect, &content, edge);
    true
}

/// The popup page the manifest names, in a popover of our own.
fn manifest_popup(context: &WKWebExtensionContext, mtm: MainThreadMarker) -> Option<(Retained<NSPopover>, Retained<WKWebView>)> {
    let manifest = unsafe { context.webExtension().manifest() };
    let path = ["action.default_popup", "browser_action.default_popup"].into_iter().find_map(|key| {
        let value: Option<Retained<AnyObject>> = unsafe { msg_send![&*manifest, valueForKeyPath: &*NSString::from_str(key)] };
        value?.downcast::<NSString>().ok().filter(|path| path.length() > 0)
    })?;
    let url = NSURL::URLWithString_relativeToURL(&path, Some(&*unsafe { context.baseURL() }))?;
    let configuration = unsafe { context.webViewConfiguration() }?;
    // shortcut: a fixed size where WebKit sizes its own popover to the page; measure the page if a popup looks cramped.
    let size = NSSize::new(360.0, 600.0);
    let view = unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), size), &configuration) };
    unsafe { view.loadRequest(&NSURLRequest::requestWithURL(&url)) };
    let controller = NSViewController::new(mtm);
    controller.setView(&view);
    let popover = NSPopover::new(mtm);
    popover.setBehavior(NSPopoverBehavior::Semitransient);
    popover.setContentViewController(Some(&controller));
    popover.setContentSize(size);
    Some((popover, view))
}

/// WebKit hands back `WKNSError`s that forward `NSError` methods, which objc2's debug method check rejects.
fn error_text(error: &NSError) -> String {
    let text: Option<Retained<NSString>> = unsafe { msg_send![error, performSelector: objc2::sel!(localizedDescription)] };
    text.map(|text| text.to_string()).unwrap_or_default()
}

fn send(shared: &Weak<RefCell<State>>, event: EngineEvent) {
    if let Some(shared) = shared.upgrade() {
        let _ = shared.borrow().events.send(event);
    }
}

fn url_string(url: Option<Retained<NSURL>>) -> Option<String> {
    url.and_then(|url| url.absoluteString()).map(|url| url.to_string())
}

fn window_of(shared: &Weak<RefCell<State>>, store: &str) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
    let window = shared.upgrade()?.borrow().profiles.get(store)?.window.clone();
    Some(ProtocolObject::from_retained(window))
}

fn store_tabs(shared: &Weak<RefCell<State>>, store: &str) -> Vec<Retained<NxExtTab>> {
    let Some(shared) = shared.upgrade() else {
        return Vec::new();
    };
    let state = shared.borrow();
    state.tabs.iter().filter(|tab| tab.ivars().store == store).cloned().collect()
}

fn context_id(context: &WKWebExtensionContext) -> String {
    unsafe { context.uniqueIdentifier() }.to_string()
}

fn queue_prompt(shared: &Weak<RefCell<State>>, store: &str, context: &WKWebExtensionContext, request: String, prompt: Prompt) {
    let Some(state) = shared.upgrade() else {
        return;
    };
    let mut state = state.borrow_mut();
    let key = state.next_prompt;
    state.next_prompt += 1;
    state.prompts.insert(key, (store.to_string(), context_id(context), prompt));
    let _ = state.events.send(EngineEvent::ExtensionPrompt { prompt: key, id: context_id(context), request });
}

pub struct Ivars {
    shared: Weak<RefCell<State>>,
    store: String,
}

pub struct TabIvars {
    shared: Weak<RefCell<State>>,
    store: String,
    tab_id: String,
    /// None while the tab is suspended.
    view: RefCell<Option<Retained<WKWebView>>>,
    /// URL and title from before the tab was suspended.
    parked: RefCell<(Option<Retained<NSURL>>, Option<Retained<NSString>>)>,
}

fn done(reply: &DynBlock<dyn Fn(*mut NSError)>) {
    reply.call((std::ptr::null_mut(),));
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxExtTab"]
    #[ivars = TabIvars]
    pub struct NxExtTab;

    unsafe impl NSObjectProtocol for NxExtTab {}

    unsafe impl WKWebExtensionTab for NxExtTab {
        #[unsafe(method_id(windowForWebExtensionContext:))]
        unsafe fn window_for(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_of(&self.ivars().shared, &self.ivars().store)
        }

        #[unsafe(method(indexInWindowForWebExtensionContext:))]
        unsafe fn index_in_window(&self, _context: &WKWebExtensionContext) -> usize {
            store_tabs(&self.ivars().shared, &self.ivars().store).iter().position(|tab| tab.ivars().tab_id == self.ivars().tab_id).unwrap_or(0)
        }

        #[unsafe(method_id(webViewForWebExtensionContext:))]
        unsafe fn web_view(&self, _context: &WKWebExtensionContext) -> Option<Retained<WKWebView>> {
            self.view()
        }

        #[unsafe(method_id(titleForWebExtensionContext:))]
        unsafe fn title(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSString>> {
            match self.view() {
                Some(view) => unsafe { view.title() },
                None => self.ivars().parked.borrow().1.clone(),
            }
        }

        #[unsafe(method_id(urlForWebExtensionContext:))]
        unsafe fn url(&self, _context: &WKWebExtensionContext) -> Option<Retained<NSURL>> {
            match self.view() {
                Some(view) => unsafe { view.URL() },
                None => self.ivars().parked.borrow().0.clone(),
            }
        }

        #[unsafe(method(isLoadingCompleteForWebExtensionContext:))]
        unsafe fn loading_complete(&self, _context: &WKWebExtensionContext) -> bool {
            self.view().is_none_or(|view| !unsafe { view.isLoading() })
        }

        #[unsafe(method(isSelectedForWebExtensionContext:))]
        unsafe fn selected(&self, _context: &WKWebExtensionContext) -> bool {
            self.ivars().shared.upgrade().is_some_and(|state| state.borrow().active.as_deref() == Some(self.ivars().tab_id.as_str()))
        }

        #[unsafe(method(zoomFactorForWebExtensionContext:))]
        unsafe fn zoom(&self, _context: &WKWebExtensionContext) -> f64 {
            self.view().map_or(1.0, |view| unsafe { view.pageZoom() })
        }

        // shortcut: page commands on a suspended tab do nothing; wake it first if extensions rely on them.
        #[unsafe(method(setZoomFactor:forWebExtensionContext:completionHandler:))]
        unsafe fn set_zoom(&self, zoom: f64, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(view) = self.view() {
                unsafe { view.setPageZoom(zoom) };
            }
            done(reply);
        }

        #[unsafe(method(loadURL:forWebExtensionContext:completionHandler:))]
        unsafe fn load_url(&self, url: &NSURL, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(view) = self.view() {
                unsafe { view.loadRequest(&NSURLRequest::requestWithURL(url)) };
            }
            done(reply);
        }

        #[unsafe(method(reloadFromOrigin:forWebExtensionContext:completionHandler:))]
        unsafe fn reload(&self, from_origin: bool, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(view) = self.view() {
                unsafe {
                    if from_origin {
                        view.reloadFromOrigin();
                    } else {
                        view.reload();
                    }
                }
            }
            done(reply);
        }

        #[unsafe(method(goBackForWebExtensionContext:completionHandler:))]
        unsafe fn go_back(&self, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(view) = self.view() {
                unsafe { view.goBack() };
            }
            done(reply);
        }

        #[unsafe(method(goForwardForWebExtensionContext:completionHandler:))]
        unsafe fn go_forward(&self, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(view) = self.view() {
                unsafe { view.goForward() };
            }
            done(reply);
        }

        // `tabs.captureVisibleTab`.
        #[unsafe(method(takeSnapshotUsingConfiguration:forWebExtensionContext:completionHandler:))]
        unsafe fn snapshot(&self, configuration: &WKSnapshotConfiguration, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSImage, *mut NSError)>) {
            match self.view() {
                Some(view) => unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(configuration), reply) },
                None => reply.call((std::ptr::null_mut(), std::ptr::null_mut())),
            }
        }

        #[unsafe(method(activateForWebExtensionContext:completionHandler:))]
        unsafe fn activate(&self, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            send(&self.ivars().shared, EngineEvent::ActivateTab { tab_id: self.ivars().tab_id.clone() });
            done(reply);
        }

        #[unsafe(method(closeForWebExtensionContext:completionHandler:))]
        unsafe fn close(&self, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            send(&self.ivars().shared, EngineEvent::CloseTab { tab_id: self.ivars().tab_id.clone() });
            done(reply);
        }

        // `activeTab`: clicking an extension's button grants it the current page.
        #[unsafe(method(shouldGrantPermissionsOnUserGestureForWebExtensionContext:))]
        unsafe fn grant_on_gesture(&self, _context: &WKWebExtensionContext) -> bool {
            true
        }
    }
);

impl NxExtTab {
    fn new(mtm: MainThreadMarker, ivars: TabIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn view(&self) -> Option<Retained<WKWebView>> {
        self.ivars().view.borrow().clone()
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxExtWindow"]
    #[ivars = Ivars]
    pub struct NxExtWindow;

    unsafe impl NSObjectProtocol for NxExtWindow {}

    unsafe impl WKWebExtensionWindow for NxExtWindow {
        #[unsafe(method_id(tabsForWebExtensionContext:))]
        unsafe fn tabs(&self, _context: &WKWebExtensionContext) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionTab>>> {
            let tabs = store_tabs(&self.ivars().shared, &self.ivars().store).into_iter().map(ProtocolObject::from_retained).collect::<Vec<_>>();
            NSArray::from_retained_slice(&tabs)
        }

        #[unsafe(method_id(activeTabForWebExtensionContext:))]
        unsafe fn active_tab(&self, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionTab>>> {
            let active = self.ivars().shared.upgrade().and_then(|state| state.borrow().active.clone());
            let tabs = store_tabs(&self.ivars().shared, &self.ivars().store);
            let tab = tabs.iter().find(|tab| Some(&tab.ivars().tab_id) == active.as_ref()).or(tabs.first());
            tab.map(|tab| ProtocolObject::from_retained(tab.clone()))
        }

        #[unsafe(method(windowTypeForWebExtensionContext:))]
        unsafe fn window_type(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowType {
            WKWebExtensionWindowType::Normal
        }

        #[unsafe(method(windowStateForWebExtensionContext:))]
        unsafe fn window_state(&self, _context: &WKWebExtensionContext) -> WKWebExtensionWindowState {
            WKWebExtensionWindowState::Normal
        }

        #[unsafe(method(isPrivateForWebExtensionContext:))]
        unsafe fn private(&self, _context: &WKWebExtensionContext) -> bool {
            false
        }

        #[unsafe(method(focusForWebExtensionContext:completionHandler:))]
        unsafe fn focus(&self, _context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            done(reply);
        }
    }
);

impl NxExtWindow {
    fn new(mtm: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxExtDelegate"]
    #[ivars = Ivars]
    struct NxExtDelegate;

    unsafe impl NSObjectProtocol for NxExtDelegate {}

    unsafe impl WKWebExtensionControllerDelegate for NxExtDelegate {
        #[unsafe(method_id(webExtensionController:openWindowsForExtensionContext:))]
        unsafe fn open_windows(&self, _controller: &WKWebExtensionController, _context: &WKWebExtensionContext) -> Retained<NSArray<ProtocolObject<dyn WKWebExtensionWindow>>> {
            let window = window_of(&self.ivars().shared, &self.ivars().store);
            NSArray::from_retained_slice(&window.into_iter().collect::<Vec<_>>())
        }

        #[unsafe(method_id(webExtensionController:focusedWindowForExtensionContext:))]
        unsafe fn focused_window(&self, _controller: &WKWebExtensionController, _context: &WKWebExtensionContext) -> Option<Retained<ProtocolObject<dyn WKWebExtensionWindow>>> {
            window_of(&self.ivars().shared, &self.ivars().store)
        }

        #[unsafe(method(webExtensionController:openNewTabUsingConfiguration:forExtensionContext:completionHandler:))]
        unsafe fn open_new_tab(
            &self,
            _controller: &WKWebExtensionController,
            configuration: &WKWebExtensionTabConfiguration,
            _context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionTab>, *mut NSError)>,
        ) {
            let url = url_string(unsafe { configuration.url() }).unwrap_or_else(|| "nxbrowser://home".into());
            let Some(shared) = self.ivars().shared.upgrade() else {
                return;
            };
            // Built-in pages are not tabs to extensions, so no tab will ever answer.
            if url.starts_with("nxbrowser://") {
                reply.call((std::ptr::null_mut(), std::ptr::null_mut()));
            } else {
                shared.borrow_mut().pending_tabs.push((url.clone(), reply.copy()));
            }
            let _ = shared.borrow().events.send(EngineEvent::OpenInNewTab { url, from: None });
        }

        #[unsafe(method(webExtensionController:openNewWindowUsingConfiguration:forExtensionContext:completionHandler:))]
        unsafe fn open_new_window(
            &self,
            _controller: &WKWebExtensionController,
            configuration: &WKWebExtensionWindowConfiguration,
            _context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(*mut ProtocolObject<dyn WKWebExtensionWindow>, *mut NSError)>,
        ) {
            // Every page here is its own floating window already; open the URLs as pages of the one browser window.
            for url in unsafe { configuration.tabURLs() }.iter() {
                if let Some(url) = url.absoluteString() {
                    send(&self.ivars().shared, EngineEvent::OpenInNewTab { url: url.to_string(), from: None });
                }
            }
            let window = window_of(&self.ivars().shared, &self.ivars().store);
            reply.call((window.map_or(std::ptr::null_mut(), |window| Retained::as_ptr(&window) as *mut _), std::ptr::null_mut()));
        }

        #[unsafe(method(webExtensionController:openOptionsPageForExtensionContext:completionHandler:))]
        unsafe fn open_options(&self, _controller: &WKWebExtensionController, context: &WKWebExtensionContext, reply: &DynBlock<dyn Fn(*mut NSError)>) {
            if let Some(url) = url_string(unsafe { context.optionsPageURL() }) {
                send(&self.ivars().shared, EngineEvent::OpenInNewTab { url, from: None });
            }
            done(reply);
        }

        #[unsafe(method(webExtensionController:promptForPermissions:inTab:forExtensionContext:completionHandler:))]
        unsafe fn prompt_permissions(
            &self,
            _controller: &WKWebExtensionController,
            permissions: &NSSet<WKWebExtensionPermission>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(NonNull<NSSet<WKWebExtensionPermission>>, *mut NSDate)>,
        ) {
            let request = permissions.iter().map(|permission| permission.to_string()).collect::<Vec<_>>().join(", ");
            queue_prompt(&self.ivars().shared, &self.ivars().store, context, request, Prompt::Permissions(reply.copy(), permissions.retain()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionToAccessURLs:inTab:forExtensionContext:completionHandler:))]
        unsafe fn prompt_urls(
            &self,
            _controller: &WKWebExtensionController,
            urls: &NSSet<NSURL>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(NonNull<NSSet<NSURL>>, *mut NSDate)>,
        ) {
            let request = urls.iter().filter_map(|url| url.host()).map(|host| host.to_string()).collect::<Vec<_>>().join(", ");
            queue_prompt(&self.ivars().shared, &self.ivars().store, context, format!("access to {request}"), Prompt::Urls(reply.copy(), urls.retain()));
        }

        #[unsafe(method(webExtensionController:promptForPermissionMatchPatterns:inTab:forExtensionContext:completionHandler:))]
        unsafe fn prompt_patterns(
            &self,
            _controller: &WKWebExtensionController,
            patterns: &NSSet<WKWebExtensionMatchPattern>,
            _tab: Option<&ProtocolObject<dyn WKWebExtensionTab>>,
            context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(NonNull<NSSet<WKWebExtensionMatchPattern>>, *mut NSDate)>,
        ) {
            let request = patterns.iter().map(|pattern| unsafe { pattern.string() }.to_string()).collect::<Vec<_>>().join(", ");
            queue_prompt(&self.ivars().shared, &self.ivars().store, context, format!("access to {request}"), Prompt::Patterns(reply.copy(), patterns.retain()));
        }

        #[unsafe(method(webExtensionController:didUpdateAction:forExtensionContext:))]
        unsafe fn did_update_action(&self, _controller: &WKWebExtensionController, _action: &WKWebExtensionAction, context: &WKWebExtensionContext) {
            send(&self.ivars().shared, EngineEvent::ExtensionChanged { id: context_id(context) });
        }

        #[unsafe(method(webExtensionController:presentPopupForAction:forExtensionContext:completionHandler:))]
        unsafe fn present_popup(
            &self,
            _controller: &WKWebExtensionController,
            action: &WKWebExtensionAction,
            context: &WKWebExtensionContext,
            reply: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let Some(shared) = self.ivars().shared.upgrade() else {
                return done(reply);
            };
            // We showed it on the click and the user has closed it since: WebKit's close came before WebKit
            // counted it as open, so refuse, or it stays "open" and blocks every later popup in the profile.
            let early = shared.borrow_mut().early_popup.take().is_some_and(|early| std::ptr::eq(&*early, action));
            let dismissed = early && unsafe { action.popupPopover() }.is_none_or(|popover| !popover.isShown());
            if !dismissed && present_popup(&shared, action, context) {
                done(reply);
            } else {
                let error = NSError::new(1, &NSString::from_str("NxBrowser"));
                reply.call((Retained::as_ptr(&error) as *mut _,));
            }
        }
    }
);

impl NxExtDelegate {
    fn new(mtm: MainThreadMarker, ivars: Ivars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }
}

pub struct PopupIvars {
    /// WebKit's own delegate for the popup page, which keeps handling everything we don't.
    original: Option<Retained<ProtocolObject<dyn WKUIDelegate>>>,
    events: Sender<EngineEvent>,
    action: Retained<WKWebExtensionAction>,
    /// Set when the popup is our `manifest_popup`, which WebKit doesn't know to close.
    own_popover: Option<Retained<NSPopover>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "NxPopupUI"]
    #[ivars = PopupIvars]
    struct NxPopupUI;

    impl NxPopupUI {
        #[unsafe(method(respondsToSelector:))]
        fn responds_to(&self, selector: Sel) -> bool {
            let own: bool = unsafe { msg_send![super(self), respondsToSelector: selector] };
            own || self.ivars().original.as_ref().is_some_and(|original| unsafe { msg_send![&**original, respondsToSelector: selector] })
        }

        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, _selector: Sel) -> *mut AnyObject {
            self.ivars().original.as_ref().map_or(std::ptr::null_mut(), |original| Retained::as_ptr(original) as *mut AnyObject)
        }
    }

    unsafe impl NSObjectProtocol for NxPopupUI {}

    unsafe impl WKUIDelegate for NxPopupUI {
        // `window.open` and `target=_blank` (sign-in links) open a tab, and the popup closes as it does in Chrome.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        unsafe fn create_web_view(
            &self,
            _web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            navigation_action: &WKNavigationAction,
            _window_features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            if let Some(url) = url_string(unsafe { navigation_action.request() }.URL()) {
                let _ = self.ivars().events.send(EngineEvent::OpenInNewTab { url, from: None });
                self.close();
            }
            None
        }

        #[unsafe(method(webViewDidClose:))]
        unsafe fn did_close(&self, _web_view: &WKWebView) {
            self.close();
        }
    }

    unsafe impl NSPopoverDelegate for NxPopupUI {
        // Tell WebKit, which may still count the popup as open; closing twice is harmless.
        #[unsafe(method(popoverDidClose:))]
        fn popover_did_close(&self, _notification: &NSNotification) {
            unsafe { self.ivars().action.closePopup() };
        }
    }
);

impl NxPopupUI {
    fn new(mtm: MainThreadMarker, ivars: PopupIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn close(&self) {
        if let Some(popover) = &self.ivars().own_popover {
            popover.close();
        }
        unsafe { self.ivars().action.closePopup() };
    }
}
