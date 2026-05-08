//! Inherent (non-trait, non-capture) public surface for the macOS
//! producer. Loads, async snapshots and a CPU-snapshot fallback,
//! find-in-page, PDF rendering, host-driven auth and permission
//! handler setters, cookie-store API, and `interactionState`
//! round-trip for tab restoration.

use std::ptr::NonNull;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSError, NSHTTPCookie, NSKeyedArchiver, NSKeyedUnarchiver,
    NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKFindConfiguration, WKFindResult, WKHTTPCookieStore, WKPDFConfiguration,
};

use crate::{
    AuthChallenge, AuthDisposition, Cookie, DownloadDecision, DownloadDestinationRequest,
    PermissionDecision, PermissionRequest, WryWebSurfaceError,
};

use super::cookies::{cookie_from_ns, ns_cookie_from};
use super::producer::WkWebViewProducer;

/// Options for [`WkWebViewProducer::find_in_page`]. Mirrors the
/// fields of `WKFindConfiguration`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub backwards: bool,
    pub wraps: bool,
}

impl WkWebViewProducer {
    /// Non-blocking variant of `navigate_to_url`. Invokes
    /// `WKWebView::loadRequest:` and returns immediately — the load
    /// completes asynchronously and surfaces through
    /// [`Self::poll_navigation_event`].
    ///
    /// **Use this** instead of [`navigate_to_url`](crate::WryWebSurfaceProducer::navigate_to_url)
    /// when calling from inside a host event-loop callback (e.g.
    /// winit's `resumed` / `window_event`). The blocking variant
    /// pumps the main `NSRunLoop` to wait for completion, which
    /// re-enters the event loop and panics under winit's
    /// "no nested event handling" guard.
    pub fn load_url(&self, url: &str) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "load_url must be called on the main thread".into(),
            ));
        }
        let url_ns = NSString::from_str(url);
        let ns_url = NSURL::URLWithString(&url_ns).ok_or_else(|| {
            WryWebSurfaceError::Platform(format!("could not parse URL: {url}"))
        })?;
        let request = NSURLRequest::requestWithURL(&ns_url);
        unsafe { self.webview().loadRequest(&request) };
        Ok(())
    }

    /// Non-blocking variant of `navigate_to_string`. Invokes
    /// `WKWebView::loadHTMLString:` and returns immediately.
    /// Completion arrives through [`Self::poll_navigation_event`].
    ///
    /// See [`Self::load_url`] for when to prefer this over the
    /// blocking trait method.
    pub fn load_html(&self, html: &str) -> Result<(), WryWebSurfaceError> {
        self.load_html_inner(html, None)
    }

    /// Like [`Self::load_html`] but takes a base URL string. The
    /// inline HTML loads with that URL as its document origin —
    /// required for `document.cookie` (and any same-origin
    /// JavaScript API) to behave the same way it would for a real
    /// network load. Useful for cookie / storage persistence
    /// testing where the inline HTML wants to interact with the
    /// per-profile [`objc2_web_kit::WKWebsiteDataStore`].
    pub fn load_html_with_base_url(
        &self,
        html: &str,
        base_url: &str,
    ) -> Result<(), WryWebSurfaceError> {
        let url_ns = NSString::from_str(base_url);
        let parsed = NSURL::URLWithString(&url_ns).ok_or_else(|| {
            WryWebSurfaceError::Platform(format!("could not parse base URL: {base_url}"))
        })?;
        self.load_html_inner(html, Some(&parsed))
    }

    fn load_html_inner(
        &self,
        html: &str,
        base_url: Option<&NSURL>,
    ) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "load_html must be called on the main thread".into(),
            ));
        }
        let html_ns = NSString::from_str(html);
        unsafe { self.webview().loadHTMLString_baseURL(&html_ns, base_url) };
        Ok(())
    }

    /// Search the current document for `query` (non-blocking). The
    /// completion arrives as a `bool` (matched / didn't match)
    /// drained via [`Self::poll_find_match`]. WebKit's `findString:`
    /// is a navigation-and-highlight, not a match-counter — only one
    /// bit of information comes back. Browser chrome that wants
    /// `n of m` indicators would have to layer on JS-side counting.
    pub fn find_in_page(
        &mut self,
        query: &str,
        options: FindOptions,
    ) -> Result<(), WryWebSurfaceError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "find_in_page must be called on the main thread".into(),
            )
        })?;
        let query_ns = NSString::from_str(query);
        let config = unsafe {
            let cfg = WKFindConfiguration::new(mtm);
            cfg.setBackwards(options.backwards);
            cfg.setCaseSensitive(options.case_sensitive);
            cfg.setWraps(options.wraps);
            cfg
        };
        let slot = Arc::clone(&self.pending_find);
        let block = RcBlock::new(move |result: NonNull<WKFindResult>| {
            let matched = unsafe { result.as_ref().matchFound() };
            if let Ok(mut s) = slot.lock() {
                *s = Some(matched);
            }
        });
        unsafe {
            self.webview().findString_withConfiguration_completionHandler(
                &query_ns,
                Some(&config),
                &block,
            );
        }
        Ok(())
    }

    /// Drain the most recent [`Self::find_in_page`] result.
    /// `Some(true)` if WebKit found at least one match,
    /// `Some(false)` if it didn't, `None` until the completion fires.
    pub fn poll_find_match(&mut self) -> Option<bool> {
        self.pending_find.lock().ok().and_then(|mut s| s.take())
    }

    /// Render the current document to PDF (non-blocking). The PDF
    /// bytes arrive via [`Self::poll_pdf`] when WebKit's
    /// `createPDFWithConfiguration:` completes. Useful for "save as
    /// PDF" / "export" / mere's print-preview path.
    pub fn request_pdf(&mut self) -> Result<(), WryWebSurfaceError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "request_pdf must be called on the main thread".into(),
            )
        })?;
        let pdf_config = unsafe { WKPDFConfiguration::new(mtm) };
        let slot = Arc::clone(&self.pending_pdf);
        let block = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
            let result = if !err.is_null() {
                Err(unsafe { (*err).localizedDescription().to_string() })
            } else if data.is_null() {
                Err("PDF completion handler returned null data with no error".into())
            } else {
                let data: &NSData = unsafe { &*data };
                Ok(data.to_vec())
            };
            if let Ok(mut s) = slot.lock() {
                *s = Some(result);
            }
        });
        unsafe {
            self.webview()
                .createPDFWithConfiguration_completionHandler(Some(&pdf_config), &block);
        }
        Ok(())
    }

    /// Drain the most recent [`Self::request_pdf`] result. `Ok` is
    /// the encoded PDF bytes; `Err` is the localized error message.
    pub fn poll_pdf(&mut self) -> Option<Result<Vec<u8>, String>> {
        self.pending_pdf.lock().ok().and_then(|mut s| s.take())
    }

    /// Register a host-driven auth-challenge handler (browser-class
    /// item 6, option B). The closure runs synchronously on the
    /// main thread inside the navigation delegate's
    /// `webView:didReceiveAuthenticationChallenge:` callback; its
    /// returned [`AuthDisposition`] is translated into the matching
    /// `NSURLSessionAuthChallengeDisposition` + `NSURLCredential`.
    ///
    /// Replaces any previously-registered handler. While a handler
    /// is registered, [`crate::NavigationEvent::AuthChallenged`] events
    /// are still emitted alongside (so hosts can log every
    /// challenge regardless of how it was resolved).
    ///
    /// The handler must do its work quickly — it blocks WebKit's
    /// load pipeline until the closure returns. UI prompts that
    /// require interactive user input belong on a different
    /// surface (e.g. emit the event, suppress the engine prompt
    /// with `Cancel`, drive a separate UI, then re-trigger via
    /// JS). True async dispositions would require buffering the
    /// completion handler in an `RcBlock<...>` slot and making
    /// the host call back into the producer when the user has
    /// decided — out of scope for this slice.
    pub fn set_auth_handler<F>(&mut self, handler: F)
    where
        F: Fn(AuthChallenge) -> AuthDisposition + Send + Sync + 'static,
    {
        if let Ok(mut h) = self.auth_handler.lock() {
            *h = Some(Box::new(handler));
        }
    }

    /// Drop any registered auth handler, reverting to option-A
    /// default-handling behavior.
    pub fn clear_auth_handler(&mut self) {
        if let Ok(mut h) = self.auth_handler.lock() {
            *h = None;
        }
    }

    /// Register a permission handler for camera / microphone /
    /// device-orientation requests. The closure runs synchronously
    /// on the main thread inside the UI delegate's
    /// `requestMediaCapturePermission*` /
    /// `requestDeviceOrientationAndMotionPermission*` callbacks; its
    /// returned [`PermissionDecision`] is translated into the
    /// matching `WKPermissionDecision`.
    ///
    /// With no handler registered the producer responds with
    /// `Prompt` so WebKit / the OS shows the standard system UI.
    pub fn set_permission_handler<F>(&mut self, handler: F)
    where
        F: Fn(PermissionRequest) -> PermissionDecision + Send + Sync + 'static,
    {
        if let Ok(mut h) = self.permission_handler.lock() {
            *h = Some(Box::new(handler));
        }
    }

    /// Drop the registered permission handler, reverting all
    /// requests to the default `Prompt` disposition.
    pub fn clear_permission_handler(&mut self) {
        if let Ok(mut h) = self.permission_handler.lock() {
            *h = None;
        }
    }

    /// Register a host-driven destination handler for downloads.
    /// The closure runs synchronously on the main thread inside
    /// the WKDownload `decideDestination` callback; its returned
    /// [`DownloadDecision`] either accepts the download to a
    /// specific path or cancels it.
    ///
    /// With no handler registered, downloads land at
    /// `<config.download_dir>/<suggested_filename>` (with `-N`
    /// suffixing on collision).
    ///
    /// The handler must do its work quickly — it blocks WebKit's
    /// download pipeline until the closure returns. UI prompts
    /// (a Save As dialog) belong on a different surface: you'd
    /// register a handler that always returns `Cancel`, observe
    /// the resulting `DownloadCancelled` event, drive the host UI
    /// asynchronously, and re-trigger the download with the
    /// chosen path through whatever your re-fetch mechanism is.
    pub fn set_download_handler<F>(&mut self, handler: F)
    where
        F: Fn(DownloadDestinationRequest) -> DownloadDecision
            + Send
            + Sync
            + 'static,
    {
        if let Ok(mut h) = self.download_host_handler.lock() {
            *h = Some(Box::new(handler));
        }
    }

    /// Drop the registered download destination handler. Future
    /// downloads use the default `<config.download_dir>/<name>`
    /// policy.
    pub fn clear_download_handler(&mut self) {
        if let Ok(mut h) = self.download_host_handler.lock() {
            *h = None;
        }
    }

    /// Register a host-driven cursor-change handler. The closure
    /// runs synchronously on the main thread inside
    /// [`Self::send_mouse_input`] / [`Self::send_pointer_input`]
    /// every time `NSCursor.currentSystemCursor` reports a
    /// different shape from the previous observation.
    ///
    /// The pull-model [`Self::poll_cursor_shape`] keeps working
    /// regardless — both surfaces fire on the same change, so a
    /// host can mix and match. Useful for hosts that prefer
    /// callbacks for cursor changes (e.g., to set the host
    /// window's cursor immediately) but still want to drain other
    /// per-frame state via polling.
    pub fn set_cursor_handler<F>(&mut self, handler: F)
    where
        F: Fn(crate::CursorShape) + Send + Sync + 'static,
    {
        if let Ok(mut h) = self.cursor_handler.lock() {
            *h = Some(Box::new(handler));
        }
    }

    /// Drop the registered cursor handler. Future cursor changes
    /// will only surface through [`Self::poll_cursor_shape`].
    pub fn clear_cursor_handler(&mut self) {
        if let Ok(mut h) = self.cursor_handler.lock() {
            *h = None;
        }
    }

    /// Cancel an in-flight download by [`DownloadId`]. Returns
    /// `Ok(true)` if the ID matched an active download (a
    /// [`crate::NavigationEvent::DownloadCancelled`] event will
    /// follow shortly), `Ok(false)` if the ID is unknown — either
    /// because the download already completed / failed and was
    /// pruned from the registry, or because it was never issued.
    ///
    /// Cancellation drops any partial bytes; resume support is a
    /// future slice (would surface the `resumeData` from
    /// `WKDownload::cancel(_:)` and add a corresponding restart
    /// API).
    pub fn cancel_download(
        &mut self,
        id: crate::DownloadId,
    ) -> Result<bool, WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "cancel_download must be called on the main thread".into(),
            ));
        }
        let download = {
            let mut registry = self.download_registry.lock().map_err(|_| {
                WryWebSurfaceError::Platform(
                    "download registry lock poisoned".into(),
                )
            })?;
            let Some(entry) = registry.by_id.get_mut(&id) else {
                return Ok(false);
            };
            // Mark host-driven so the ensuing
            // `download:didFailWithError:` callback routes to
            // `DownloadCancelled` rather than `DownloadFinished`.
            entry.cancelled_by_host = true;
            entry.wk_download.clone()
        };
        // `cancel:` takes a completion block that receives
        // optional resumeData. We pass `None` (no completion
        // handler) — resume support is deferred.
        unsafe { download.cancel(None) };
        Ok(true)
    }

    /// Kick off an async fetch of every cookie in the
    /// `WKHTTPCookieStore` backing this producer's
    /// `WKWebsiteDataStore`. Drain via [`Self::poll_cookies`].
    /// Useful for browser-shape consumers running an "import from
    /// Safari" / "show all cookies" / "sync cookies to native
    /// password manager" flow.
    pub fn request_all_cookies(&mut self) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "request_all_cookies must be called on the main thread".into(),
            ));
        }
        let store = unsafe { self.cookie_store() };
        let slot = Arc::clone(&self.pending_cookies);
        let block = RcBlock::new(move |array_ptr: NonNull<NSArray<NSHTTPCookie>>| {
            let array = unsafe { array_ptr.as_ref() };
            let mut out = Vec::with_capacity(array.count());
            for i in 0..array.count() {
                let ns_cookie = array.objectAtIndex(i);
                out.push(cookie_from_ns(&ns_cookie));
            }
            if let Ok(mut s) = slot.lock() {
                *s = Some(out);
            }
        });
        unsafe { store.getAllCookies(&block) };
        Ok(())
    }

    /// Drain the most recent [`Self::request_all_cookies`] result.
    pub fn poll_cookies(&mut self) -> Option<Vec<Cookie>> {
        self.pending_cookies.lock().ok().and_then(|mut s| s.take())
    }

    /// Set / overwrite a cookie in the producer's data store.
    /// Fire-and-forget — Apple's API takes a completion handler with
    /// no value; we don't expose a poll for it. The cookie is
    /// visible to subsequent network requests once the data-store
    /// has committed it.
    pub fn set_cookie(&mut self, cookie: &Cookie) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "set_cookie must be called on the main thread".into(),
            ));
        }
        let ns_cookie = ns_cookie_from(cookie).ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "could not construct NSHTTPCookie — required field (name/value/domain/path) was rejected by the cookie parser".into(),
            )
        })?;
        let store = unsafe { self.cookie_store() };
        unsafe { store.setCookie_completionHandler(&ns_cookie, None) };
        Ok(())
    }

    /// Delete a cookie by name + domain + path. Constructs an
    /// `NSHTTPCookie` with the same identity (name/domain/path/value
    /// don't matter for the lookup beyond those three) and hands
    /// it to `WKHTTPCookieStore::deleteCookie:`. Fire-and-forget.
    pub fn delete_cookie(
        &mut self,
        name: &str,
        domain: &str,
        path: &str,
    ) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "delete_cookie must be called on the main thread".into(),
            ));
        }
        let placeholder = Cookie {
            name: name.to_string(),
            value: String::new(),
            domain: domain.to_string(),
            path: path.to_string(),
            expires_at: None,
            is_secure: false,
            is_http_only: false,
        };
        let ns_cookie = ns_cookie_from(&placeholder).ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "could not construct NSHTTPCookie for deletion".into(),
            )
        })?;
        let store = unsafe { self.cookie_store() };
        unsafe { store.deleteCookie_completionHandler(&ns_cookie, None) };
        Ok(())
    }

    /// Serialize the WebView's interaction state — back-forward
    /// list, scroll position, form data, etc. — into an opaque
    /// blob. Round-trip via [`Self::restore_interaction_state`]
    /// to restore. Useful for browser-shape consumers persisting
    /// per-tab session state.
    ///
    /// Returns `None` if the WebView has no state to serialize
    /// (e.g. before any navigation). The blob format is private
    /// to WebKit; treat it as opaque bytes.
    pub fn serialize_interaction_state(&self) -> Option<Vec<u8>> {
        if MainThreadMarker::new().is_none() {
            return None;
        }
        let state = unsafe { self.webview().interactionState() }?;
        // The deprecated archiver pair (no `requiringSecureCoding`)
        // round-trips correctly for the WebKit-internal types in
        // the interaction state. The modern path requires
        // class-allowlist work that's brittle for opaque blobs.
        #[allow(deprecated)]
        let data = unsafe { NSKeyedArchiver::archivedDataWithRootObject(&*state) };
        Some(data.to_vec())
    }

    /// Restore a previously-serialized interaction state. The blob
    /// must come from [`Self::serialize_interaction_state`] called
    /// on a `WkWebViewProducer` running compatible WebKit; cross-
    /// version restore is allowed but not guaranteed by Apple.
    pub fn restore_interaction_state(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "restore_interaction_state must be called on the main thread".into(),
            ));
        }
        let data = unsafe {
            NSData::dataWithBytes_length(bytes.as_ptr() as *mut _, bytes.len())
        };
        #[allow(deprecated)]
        let obj = unsafe { NSKeyedUnarchiver::unarchiveObjectWithData(&data) }
            .ok_or_else(|| {
                WryWebSurfaceError::Platform(
                    "could not unarchive interaction state — blob may be corrupt or from an incompatible WebKit version".into(),
                )
            })?;
        unsafe { self.webview().setInteractionState(Some(&*obj)) };
        Ok(())
    }

    /// Reach the producer's `WKHTTPCookieStore` via the live
    /// `WKWebsiteDataStore` on its configuration. The store is
    /// cheap to fetch each call (returns a wrapper around a shared
    /// underlying store) and we don't cache it on the producer to
    /// avoid lifetime entanglement.
    unsafe fn cookie_store(&self) -> Retained<WKHTTPCookieStore> {
        unsafe {
            let config = self.webview().configuration();
            let data_store = config.websiteDataStore();
            data_store.httpCookieStore()
        }
    }
}
