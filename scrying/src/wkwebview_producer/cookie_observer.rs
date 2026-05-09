//! `WKHTTPCookieStoreObserver` plumbing for the cookie-change
//! callback API.
//!
//! Apple's protocol fires `cookiesDidChangeInCookieStore:` whenever
//! anything mutates the producer's cookie store — page-side
//! `document.cookie` writes, network responses' `Set-Cookie`
//! headers, host calls to [`super::WkWebViewProducer::set_cookie`] /
//! [`super::WkWebViewProducer::delete_cookie`]. The protocol does
//! *not* convey what changed; the callback is a "go re-fetch the
//! cookies you care about" pulse. Hosts pair it with
//! `request_all_cookies` / `poll_cookies`.
//!
//! The observer is always registered on the producer's
//! `WKHTTPCookieStore` for the producer's lifetime; the registered
//! closure (held behind a `Mutex<Option<...>>`) is what gates
//! whether anything actually happens on each callback. Cheaper than
//! re-registering on every `set_cookie_change_handler` /
//! `clear_cookie_change_handler` flip, and keeps the producer drop
//! sequence simple.

use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol};
use objc2_web_kit::{WKHTTPCookieStore, WKHTTPCookieStoreObserver};

/// Closure invoked when the producer's cookie store fires
/// `cookiesDidChangeInCookieStore:`. The callback receives no
/// arguments — Apple's protocol doesn't convey what changed; pair
/// it with [`super::WkWebViewProducer::request_all_cookies`] to
/// observe the new state.
///
/// Fires on the main thread (Apple's contract). Closures must be
/// `Send + Sync` because the slot is shared via `Arc<Mutex<...>>`,
/// even though delivery is single-threaded.
pub type CookieChangeHandlerFn = Box<dyn Fn() + Send + Sync + 'static>;

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `CookieStoreObserver` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Arc<Mutex<Option<CookieChangeHandlerFn>>>]
    pub(super) struct CookieStoreObserver;

    unsafe impl NSObjectProtocol for CookieStoreObserver {}

    // SAFETY: signature matches Apple's `WKHTTPCookieStoreObserver`
    // protocol. `cookie_store` is unused — the protocol carries no
    // delta info, so re-fetching is the host's responsibility.
    unsafe impl WKHTTPCookieStoreObserver for CookieStoreObserver {
        #[unsafe(method(cookiesDidChangeInCookieStore:))]
        fn cookies_did_change(&self, _cookie_store: &WKHTTPCookieStore) {
            let slot = self.ivars();
            if let Ok(guard) = slot.lock()
                && let Some(handler) = guard.as_ref()
            {
                handler();
            }
        }
    }
);

impl CookieStoreObserver {
    pub(super) fn new(
        mtm: MainThreadMarker,
        slot: Arc<Mutex<Option<CookieChangeHandlerFn>>>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(slot);
        // SAFETY: NSObject's `init` returns a valid initialized instance.
        unsafe { msg_send![super(this), init] }
    }
}
