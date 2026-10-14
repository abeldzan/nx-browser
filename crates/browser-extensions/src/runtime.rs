use crate::scripting::background_prelude;
use std::collections::HashMap;
use std::sync::Arc;

pub type HostFn = Arc<dyn Fn(&str) -> String + Send + Sync>;

pub struct BackgroundRuntime {
    host: HostFn,
    contexts: HashMap<String, ContextSlot>,
}

enum ContextSlot {
    #[cfg(target_os = "macos")]
    Native(objc2::rc::Retained<objc2_javascript_core::JSContext>),
    #[cfg(not(target_os = "macos"))]
    Missing,
}

impl BackgroundRuntime {
    pub fn new(host: HostFn) -> Self {
        Self {
            host,
            contexts: HashMap::new(),
        }
    }

    pub fn set_host(&mut self, host: HostFn) {
        self.host = host;
    }

    pub fn start(&mut self, extension_id: &str, script: &str) {
        #[cfg(target_os = "macos")]
        {
            let context = unsafe { objc2_javascript_core::JSContext::new() };
            install_host(&context, extension_id, self.host.clone());
            let prelude = format!(
                "globalThis.__nxExtensionId = {};\n{}",
                serde_json::to_string(extension_id).unwrap_or_else(|_| "\"\"".into()),
                background_prelude()
            );
            eval(&context, &prelude);
            eval(&context, script);
            self.contexts
                .insert(extension_id.to_string(), ContextSlot::Native(context));
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (extension_id, script);
        }
    }

    pub fn stop(&mut self, extension_id: &str) {
        self.contexts.remove(extension_id);
    }

    pub fn dispatch(&mut self, extension_id: &str, message_json: &str) {
        #[cfg(target_os = "macos")]
        if let Some(ContextSlot::Native(context)) = self.contexts.get(extension_id) {
            let source = format!("globalThis.__nxDispatch({message_json});");
            eval(context, &source);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (extension_id, message_json);
        }
    }
}

impl Default for BackgroundRuntime {
    fn default() -> Self {
        Self::new(Arc::new(|_| {
            r#"{"ok":false,"error":"no host","value":null}"#.into()
        }))
    }
}

#[cfg(target_os = "macos")]
fn eval(context: &objc2_javascript_core::JSContext, source: &str) {
    let script = objc2_foundation::NSString::from_str(source);
    unsafe { context.evaluateScript(Some(&script)) };
}

#[cfg(target_os = "macos")]
fn install_host(context: &objc2_javascript_core::JSContext, _extension_id: &str, host: HostFn) {
    let host = host.clone();
    let block = block2::RcBlock::new(move |payload: *mut objc2_foundation::NSString| -> *mut objc2_foundation::NSString {
        if payload.is_null() {
            return std::ptr::null_mut();
        }
        let text = ns_to_string(unsafe { &*payload });
        let reply = host(&text);
        let result = objc2_foundation::NSString::from_str(&reply);
        objc2::rc::Retained::autorelease_ptr(result)
    });
    let key = objc2_foundation::NSString::from_str("nxCall");
    let ptr = block2::RcBlock::as_ptr(&block) as *const objc2::runtime::AnyObject;
    unsafe {
        context.setObject_forKeyedSubscript(Some(&*ptr), Some(&*key));
    }
    std::mem::forget(block);
}

#[cfg(target_os = "macos")]
fn ns_to_string(value: &objc2_foundation::NSString) -> String {
    objc2::rc::autoreleasepool(|pool| unsafe { value.to_str(pool).to_string() })
}

