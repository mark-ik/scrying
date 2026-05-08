//! Custom URL-scheme handler. WebKit only accepts a
//! `WKURLSchemeHandler` registered on the
//! `WKWebViewConfiguration` *before* the WKWebView is initialized;
//! [`super::WkWebViewProducer::new_with_url_schemes`] feeds them in
//! at construction time.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_foundation::{
    MainThreadMarker, NSData, NSObject, NSObjectProtocol, NSString, NSURLResponse,
};
use objc2_web_kit::{WKURLSchemeHandler, WKURLSchemeTask, WKWebView};

/// Response served by a [`UrlSchemeHandlerFn`] — MIME type plus the
/// raw bytes that should appear as the resource body to the WebView.
#[derive(Clone, Debug)]
pub struct UrlSchemeResponse {
    pub mime_type: String,
    pub body: Vec<u8>,
}

/// Closure type registered on a config / producer to serve resources
/// for a custom URL scheme (e.g. `mere://settings`). The closure
/// receives the full URL of the request and returns a body to deliver
/// to the WebView. `Arc` so the config can be cloned and the handler
/// can be shared across re-creates of the producer.
pub type UrlSchemeHandlerFn =
    Arc<dyn Fn(&str) -> UrlSchemeResponse + Send + Sync + 'static>;

// `WKURLSchemeHandler` delegate class. Holds one handler closure
// keyed to one scheme; multiple schemes get multiple delegate
// instances. WebKit calls `webView:startURLSchemeTask:` on the main
// thread when a request for the registered scheme starts; we invoke
// the handler synchronously and feed the result back through the
// task's `didReceiveResponse:` / `didReceiveData:` / `didFinish`
// callbacks.
define_class!(
    // SAFETY:
    // - Superclass NSObject has no subclassing requirements.
    // - `SchemeHandler` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = UrlSchemeHandlerFn]
    pub(super) struct SchemeHandler;

    unsafe impl NSObjectProtocol for SchemeHandler {}

    unsafe impl WKURLSchemeHandler for SchemeHandler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start_task(
            &self,
            _webview: &WKWebView,
            url_scheme_task: &ProtocolObject<dyn WKURLSchemeTask>,
        ) {
            let request = unsafe { url_scheme_task.request() };
            let url = request.URL();
            let url_str = url
                .as_ref()
                .and_then(|u| u.absoluteString())
                .map(|s| s.to_string())
                .unwrap_or_default();
            let response = (self.ivars())(&url_str);
            let mime_ns = NSString::from_str(&response.mime_type);
            // Build an NSURLResponse for the consumer. Use UTF-8 as
            // the implicit text encoding — works for HTML / JS / CSS
            // / JSON; binary payloads are unaffected.
            let utf8 = NSString::from_str("utf-8");
            let length = response.body.len() as isize;
            let url_for_response = match url {
                Some(u) => u,
                // Pathological — WebKit always supplies a URL. Drop
                // silently rather than over-engineer the error path
                // (NSError construction with userInfo would force
                // extra Foundation types into our dependency
                // surface).
                None => return,
            };
            let ns_response =
                NSURLResponse::initWithURL_MIMEType_expectedContentLength_textEncodingName(
                    NSURLResponse::alloc(),
                    &url_for_response,
                    Some(&mime_ns),
                    length,
                    Some(&utf8),
                );
            // SAFETY: `bytes` outlives the `dataWithBytes_length`
            // call; NSData copies the bytes into its own buffer.
            let data = unsafe {
                NSData::dataWithBytes_length(
                    response.body.as_ptr() as *mut std::ffi::c_void,
                    response.body.len(),
                )
            };
            unsafe {
                url_scheme_task.didReceiveResponse(&ns_response);
                url_scheme_task.didReceiveData(&data);
                url_scheme_task.didFinish();
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop_task(
            &self,
            _webview: &WKWebView,
            _url_scheme_task: &ProtocolObject<dyn WKURLSchemeTask>,
        ) {
            // We complete tasks synchronously inside `start_task`,
            // so by the time `stopURLSchemeTask:` arrives there's
            // nothing to cancel. No-op.
        }
    }
);

impl SchemeHandler {
    pub(super) fn new(
        mtm: MainThreadMarker,
        handler: UrlSchemeHandlerFn,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(handler);
        unsafe { msg_send![super(this), init] }
    }
}
