//! `WKScriptMessageHandler` for the JS → host message bridge plus the
//! companion user script that exposes
//! `window.chrome.webview.postMessage` / `addEventListener('message',
//! ...)` shims (matching the WebView2 JS API) on top of WebKit's
//! `window.webkit.messageHandlers.<NAME>` machinery.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{WKScriptMessage, WKScriptMessageHandler, WKUserContentController};

/// JS bridge handler name. JS-side code reaches the host via
/// `window.webkit.messageHandlers.<NAME>.postMessage(...)`. The shim
/// installed in [`HOST_BRIDGE_USER_SCRIPT`] wraps that under a
/// WebView2-compatible `window.chrome.webview` API so consumers can
/// write portable code.
pub(super) const HOST_BRIDGE_HANDLER_NAME: &str = "scryingHostBridge";

/// User-script injected at document start that builds a
/// `window.chrome.webview` shim around
/// `window.webkit.messageHandlers.scryingHostBridge.postMessage`.
///
/// The shim mirrors the WebView2 JS API:
///
/// - JS → host: `window.chrome.webview.postMessage(s)`.
/// - Host → JS: handlers registered via
///   `window.chrome.webview.addEventListener('message', cb)` are
///   invoked with `{data: payload}`. The host calls
///   `window.chrome.webview._dispatchHostMessage(payload)` via
///   `evaluateJavaScript:` (see [`super::WkWebViewProducer::post_web_message`]).
///
/// Idempotent: re-runs (e.g. after a same-document navigation) skip
/// if the shim is already present.
pub(super) const HOST_BRIDGE_USER_SCRIPT: &str = r#"(function() {
    if (window.chrome && window.chrome.webview && window.chrome.webview._dispatchHostMessage) return;
    var listeners = [];
    window.chrome = window.chrome || {};
    window.chrome.webview = {
        postMessage: function(msg) {
            window.webkit.messageHandlers.scryingHostBridge.postMessage(msg);
        },
        addEventListener: function(type, handler) {
            if (type === 'message') listeners.push(handler);
        },
        removeEventListener: function(type, handler) {
            if (type === 'message') {
                var i = listeners.indexOf(handler);
                if (i >= 0) listeners.splice(i, 1);
            }
        }
    };
    Object.defineProperty(window.chrome.webview, '_dispatchHostMessage', {
        value: function(payload) {
            var event = { data: payload, source: window.chrome.webview };
            for (var i = 0; i < listeners.length; i++) {
                try { listeners[i](event); } catch (e) { console.error(e); }
            }
        },
        enumerable: false,
        writable: false,
        configurable: false
    });
})();
"#;

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `ScriptMessageHandler` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Arc<Mutex<VecDeque<String>>>]
    pub(super) struct ScriptMessageHandler;

    unsafe impl NSObjectProtocol for ScriptMessageHandler {}

    // SAFETY: signature matches Apple's `WKScriptMessageHandler` protocol.
    unsafe impl WKScriptMessageHandler for ScriptMessageHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(
            &self,
            _ucc: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            let body = unsafe { message.body() };
            // The trait contract is "string messages." JS senders that
            // need to pass structured payloads should `JSON.stringify`
            // host-side; non-string bodies are dropped here rather
            // than coerced to lossy string forms.
            if let Some(ns_string) = body.downcast_ref::<NSString>()
                && let Ok(mut queue) = self.ivars().lock()
            {
                queue.push_back(ns_string.to_string());
            }
        }
    }
);

impl ScriptMessageHandler {
    pub(super) fn new(
        mtm: MainThreadMarker,
        queue: Arc<Mutex<VecDeque<String>>>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(queue);
        // SAFETY: NSObject's `init` returns a valid initialized instance.
        unsafe { msg_send![super(this), init] }
    }
}
