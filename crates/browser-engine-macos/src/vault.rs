//! Where the password key lives (the login Keychain) and the Touch ID / Mac password gate.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};
use security_framework::passwords::{get_generic_password, set_generic_password};
use security_framework::random::SecRandom;
use std::cell::RefCell;
use std::sync::Mutex;

const SERVICE: &str = "nxbrowser Safe Storage";
const ACCOUNT: &str = "nxbrowser";
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// The key that encrypts saved passwords, created on first use.
pub fn vault_key() -> anyhow::Result<[u8; 32]> {
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => bytes.try_into().map_err(|_| anyhow::anyhow!("Keychain item \"{SERVICE}\" is not a 32-byte key")),
        // Only a missing item makes a new key: after a denied or failed read, a fresh key would orphan every saved password.
        Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => {
            let mut key = [0u8; 32];
            SecRandom::default().copy_bytes(&mut key)?;
            set_generic_password(SERVICE, ACCOUNT, &key)?;
            Ok(key)
        }
        Err(error) => Err(anyhow::anyhow!("Keychain: {error}")),
    }
}

thread_local! {
    /// Kept alive until the next ask: the context must outlive its evaluation.
    static CONTEXT: RefCell<Option<Retained<LAContext>>> = const { RefCell::new(None) };
}

/// Asks for Touch ID or the Mac password; `done` runs once, off the main thread, with the outcome.
pub fn authenticate(reason: &str, done: impl FnOnce(bool) + Send + 'static) {
    let done = Mutex::new(Some(done));
    let reply = RcBlock::new(move |ok: Bool, _error: *mut NSError| {
        if let Some(done) = done.lock().ok().and_then(|mut done| done.take()) {
            done(ok.as_bool());
        }
    });
    let context = unsafe { LAContext::new() };
    // SAFETY: the block only holds a Mutex around a Send closure.
    unsafe {
        context.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(reason), &reply)
    };
    CONTEXT.with_borrow_mut(|slot| *slot = Some(context));
}
