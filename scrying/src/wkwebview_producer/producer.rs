//! [`WkWebViewProducer`] struct, the
//! [`WkWebViewProducer::new`] / [`WkWebViewProducer::new_with_url_schemes`]
//! constructors, the `Drop` teardown, and a handful of small inherent
//! helpers (cursor observation, nav-result reset, completion wait,
//! DPI flush, internal resize) shared across the public API surface.

use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dpi::PhysicalSize;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::MainThreadOnly;
use objc2_app_kit::{NSImage, NSView};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSNotification, NSNotificationCenter,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSSize, NSString,
};
use objc2_web_kit::{
    WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore,
};
use objc2_foundation::NSKeyValueObservingOptions;

use crate::native_frame;
use crate::{
    CursorShape, SystemWebviewBackend, WebSurfaceMode, WryWebSurfaceCapabilities,
    WryWebSurfaceError,
};

use super::capture::{CaptureState, PendingCaptureSlot};
use super::config::WkWebViewProducerConfig;
use super::download_handler::DownloadHandler;
use super::helpers::{backing_scale_for, ns_rect_from_pixels, profile_uuid_for_path};
use super::nav_delegate::{AuthHandlerFn, NavDelegate, NavState};
use super::scheme_handler::{SchemeHandler, UrlSchemeHandlerFn};
use super::script_message::{ScriptMessageHandler, HOST_BRIDGE_HANDLER_NAME, HOST_BRIDGE_USER_SCRIPT};
use super::title_observer::TitleObserver;
use super::ui_delegate::{PermissionHandlerFn, UiDelegate};

/// macOS WKWebView capture producer.
///
/// Slice A: real WKWebView lifecycle, no GPU capture (output is
/// `OverlayOnly`). Slice B will wire ScreenCaptureKit → IOSurface →
/// `MetalTextureRef` and flip `acquire_frame` to
/// `WryWebSurfaceFrame::Native(...)`.
pub struct WkWebViewProducer {
    pub(super) capabilities: WryWebSurfaceCapabilities,
    pub(super) webview: Retained<WKWebView>,
    /// The host's parent `NSView`. Retained so the WKWebView's
    /// superview cannot vanish from under us; the caller is also
    /// expected to outlive the producer per the `new` safety contract,
    /// but the extra retain is cheap insurance against early teardown
    /// during `Drop`.
    pub(super) parent_view: Retained<NSView>,
    /// Shared with the navigation delegate.
    pub(super) nav_state: Arc<Mutex<NavState>>,
    /// `WKWebView` only holds a weak reference to its navigation
    /// delegate, so the producer owns the strong reference.
    _nav_delegate: Retained<NavDelegate>,
    /// `WKWebView` also holds a weak reference to its UI delegate.
    /// Currently used for new-window-request interception (slice 2
    /// of the browser-class roadmap).
    _ui_delegate: Retained<UiDelegate>,
    /// `WKUserContentController` retains its script-message handlers,
    /// but we keep our own strong reference so we can issue
    /// `removeScriptMessageHandlerForName:` cleanly in `Drop`.
    _script_message_handler: Retained<ScriptMessageHandler>,
    /// KVO observer registered against the WKWebView's `title` key
    /// path so we can synthesize [`NavigationEvent::TitleChanged`].
    /// Removed via `removeObserver:` in `Drop` before the WKWebView
    /// drops.
    title_observer: Retained<TitleObserver>,
    /// FIFO of messages posted by JS via
    /// `window.chrome.webview.postMessage(...)`, drained by
    /// [`Self::poll_web_message`].
    pub(super) web_messages: Arc<Mutex<VecDeque<String>>>,
    /// Last [`CursorShape`] we observed via `NSCursor.currentSystemCursor`
    /// after a forwarded pointer event. The producer pushes to
    /// [`Self::cursor_shapes`] only when the new shape differs from
    /// this value, so consumers don't get a flood of duplicate
    /// `Default` events.
    pub(super) last_cursor_shape: Option<CursorShape>,
    /// Cursor-shape changes the host should apply, drained by
    /// [`Self::poll_cursor_shape`]. Populated after each forwarded
    /// pointer event observes a new `NSCursor.currentSystemCursor`.
    pub(super) cursor_shapes: VecDeque<CursorShape>,
    pub(super) config: WkWebViewProducerConfig,
    pub(super) mtm: MainThreadMarker,
    /// `Some` once `start_capture` has succeeded; `None` while the
    /// producer is still in slice-A overlay-only mode.
    pub(super) capture: Option<CaptureState>,
    /// Counter incremented per [`Self::capture_cpu_snapshot`] call so
    /// consumers can disambiguate snapshot frames. Independent of
    /// [`CaptureState::generation`] which counts SCK samples.
    pub(super) snapshot_generation: u64,
    /// Most-recent completion of [`Self::request_snapshot`]. Drained
    /// by [`Self::poll_snapshot`]. Older completions are overwritten
    /// before the consumer polls.
    pub(super) pending_snapshot: Arc<Mutex<Option<PendingSnapshot>>>,
    /// Cross-thread state machine for [`Self::start_capture_async`].
    /// Advanced by SCK completion blocks running on background
    /// dispatch queues; promoted into `self.capture` by the consumer
    /// via [`Self::capture_status`].
    pub(super) pending_capture: Arc<Mutex<PendingCaptureSlot>>,
    /// Custom-URL-scheme handlers registered on the
    /// `WKWebViewConfiguration` at construction time. WebKit only
    /// holds weak references to scheme handlers, so the producer
    /// keeps the strong refs.
    _scheme_handlers: Vec<Retained<SchemeHandler>>,
    /// Strong reference to the shared `WKDownloadDelegate`. Each
    /// `WKDownload` we receive gets this as its delegate (Apple's
    /// `setDelegate:` is weak), so the strong ref has to live here.
    _download_handler: Retained<DownloadHandler>,
    /// Most-recent completion of [`Self::find_in_page`] — `true` if
    /// any match was found, `false` if not. Drained by
    /// [`Self::poll_find_match`].
    pub(super) pending_find: Arc<Mutex<Option<bool>>>,
    /// Most-recent completion of [`Self::request_pdf`]. `Vec<u8>` is
    /// the encoded PDF bytes on success; `String` is the localized
    /// description on error.
    pub(super) pending_pdf: PendingPdfSlot,
    /// Optional host-driven auth-challenge handler (item 6, option
    /// B). Shared with `NavDelegate` via its ivars so the delegate
    /// can call into it from the auth callback. Behind a `Mutex`
    /// so [`Self::set_auth_handler`] can mutate while the producer
    /// is still alive.
    pub(super) auth_handler: Arc<Mutex<Option<AuthHandlerFn>>>,
    /// Most-recent completion of [`Self::request_all_cookies`].
    /// Drained by [`Self::poll_cookies`].
    pub(super) pending_cookies: Arc<Mutex<Option<Vec<crate::Cookie>>>>,
    /// Optional host-driven permission handler (camera / mic /
    /// orientation). Shared with `UiDelegate` via its ivars. Behind
    /// `Mutex` so [`Self::set_permission_handler`] can mutate live.
    pub(super) permission_handler: Arc<Mutex<Option<PermissionHandlerFn>>>,
    /// Set by the `NSWindowDidChangeBackingPropertiesNotification`
    /// observer when the host window moves between screens with
    /// different backing-scale factors. Read + cleared by
    /// [`Self::flush_pending_dpi_change`] (called by `resize` and
    /// `try_acquire_frame`), which then re-applies the producer's
    /// `config.size` so points/pixels stay coherent.
    pub(super) dpi_pending: Arc<AtomicBool>,
    /// Token returned by `NSNotificationCenter::addObserverForName:object:queue:usingBlock:`.
    /// Holding it keeps the observer registered; dropped on producer
    /// `Drop` (after explicit `removeObserver:`).
    pub(super) dpi_observer: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

pub(super) type PendingPdfSlot = Arc<Mutex<Option<Result<Vec<u8>, String>>>>;

/// Newtype that asserts a `Retained<NSImage>` is safe to send between
/// threads — the producer's snapshot completion handler fires on the
/// main thread and the producer's `poll_snapshot` reads from the same
/// thread, so the cross-thread `Send` is satisfied trivially. The
/// wrapper exists to satisfy the conservative compiler bound on
/// `Mutex<Option<T>>` where T isn't `Send` by default.
pub(super) struct SendRetainedNSImage(pub(super) Retained<NSImage>);
// SAFETY: see `SendRetainedNSImage` doc.
unsafe impl Send for SendRetainedNSImage {}

pub(super) enum PendingSnapshot {
    Image(SendRetainedNSImage),
    Failed(String),
}

impl WkWebViewProducer {
    /// Construct the producer.
    ///
    /// # Safety
    ///
    /// - Must be called on the main thread (AppKit / WebKit are main-
    ///   thread-only). Returns [`WryWebSurfaceError::Platform`] if not.
    /// - `parent_view` must be a valid `NSView *` that outlives the
    ///   producer.
    pub unsafe fn new(
        parent_view: *mut std::ffi::c_void,
        config: WkWebViewProducerConfig,
    ) -> Result<Self, WryWebSurfaceError> {
        // SAFETY: parent_view contract delegated to the public
        // `new_with_url_schemes` form below.
        unsafe { Self::new_with_url_schemes(parent_view, config, Vec::new()) }
    }

    /// Like [`Self::new`] but registers custom URL-scheme handlers
    /// on the `WKWebViewConfiguration` before the WKWebView is
    /// initialized. Each `(scheme, handler)` pair routes
    /// `<scheme>://...` requests through the handler closure (which
    /// runs on the main thread, synchronously inside WebKit's load
    /// pipeline). Useful for browser-shape consumers serving their
    /// own chrome pages (e.g. `mere://settings`,
    /// `mere://newtab`).
    ///
    /// `WKURLSchemeHandler` registration must happen before the
    /// WKWebView is constructed — Apple's API doesn't allow
    /// post-init registration.
    ///
    /// # Safety
    ///
    /// Same contract as [`Self::new`].
    pub unsafe fn new_with_url_schemes(
        parent_view: *mut std::ffi::c_void,
        config: WkWebViewProducerConfig,
        schemes: Vec<(String, UrlSchemeHandlerFn)>,
    ) -> Result<Self, WryWebSurfaceError> {
        let mtm = MainThreadMarker::new().ok_or(WryWebSurfaceError::Platform(
            "WkWebViewProducer::new must be called on the main thread".into(),
        ))?;

        let parent_view: Retained<NSView> = {
            let ptr = NonNull::new(parent_view as *mut NSView).ok_or(
                WryWebSurfaceError::Platform("parent_view pointer was null".into()),
            )?;
            // SAFETY: caller-asserted: parent_view is a valid NSView*
            // that outlives this call.
            unsafe { Retained::retain(ptr.as_ptr()) }.ok_or(
                WryWebSurfaceError::Platform("failed to retain parent NSView".into()),
            )?
        };

        let backing_scale = backing_scale_for(&parent_view);
        let frame = ns_rect_from_pixels(config.offset, config.size, backing_scale);

        let webview_config = unsafe { WKWebViewConfiguration::new(mtm) };

        // Data-store selection:
        //   1. `non_persistent` overrides everything → ephemeral
        //      `WKWebsiteDataStore::nonPersistentDataStore` (incognito
        //      mode). Wiped on Drop.
        //   2. Non-empty `data_dir` → per-profile UUID-keyed store
        //      via `dataStoreForIdentifier:` (macOS 14+).
        //   3. Empty `data_dir` → shared default store.
        if config.non_persistent {
            let data_store = unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
            unsafe {
                webview_config.setWebsiteDataStore(&data_store);
            }
        } else if !config.data_dir.as_os_str().is_empty() {
            let identifier = profile_uuid_for_path(&config.data_dir, mtm);
            let data_store = unsafe {
                WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm)
            };
            unsafe {
                webview_config.setWebsiteDataStore(&data_store);
            }
        }

        // Register custom URL scheme handlers BEFORE constructing
        // the WKWebView — Apple's API only honors handlers attached
        // to the configuration at init time. Each scheme gets its
        // own SchemeHandler instance; we keep strong refs so the
        // weakly-held WebKit reference stays valid.
        let mut scheme_handler_retained: Vec<Retained<SchemeHandler>> =
            Vec::with_capacity(schemes.len());
        for (scheme, handler) in schemes {
            let scheme_ns = NSString::from_str(&scheme);
            let delegate = SchemeHandler::new(mtm, handler);
            unsafe {
                webview_config.setURLSchemeHandler_forURLScheme(
                    Some(ProtocolObject::from_ref(&*delegate)),
                    &scheme_ns,
                );
            }
            scheme_handler_retained.push(delegate);
        }

        // Install the `window.chrome.webview` bridge before any frame
        // loads — both the user script and the `WKScriptMessageHandler`
        // need to be on the configuration's `WKUserContentController`
        // when the WKWebView is initialized.
        let web_messages: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let script_message_handler =
            ScriptMessageHandler::new(mtm, Arc::clone(&web_messages));
        let bridge_handler_name = NSString::from_str(HOST_BRIDGE_HANDLER_NAME);
        let bridge_user_script_source = NSString::from_str(HOST_BRIDGE_USER_SCRIPT);
        let user_content_controller = unsafe { webview_config.userContentController() };
        unsafe {
            user_content_controller.addScriptMessageHandler_name(
                ProtocolObject::from_ref(&*script_message_handler),
                &bridge_handler_name,
            );
            let user_script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &bridge_user_script_source,
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
            );
            user_content_controller.addUserScript(&user_script);
        }

        let webview: Retained<WKWebView> = unsafe {
            WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &webview_config)
        };

        let nav_state = Arc::new(Mutex::new(NavState::default()));
        let download_handler = DownloadHandler::new(
            mtm,
            Arc::clone(&nav_state),
            config.download_dir.clone(),
        );
        let auth_handler: Arc<Mutex<Option<AuthHandlerFn>>> = Arc::new(Mutex::new(None));
        let nav_delegate = NavDelegate::new(
            mtm,
            Arc::clone(&nav_state),
            download_handler.clone(),
            Arc::clone(&auth_handler),
        );
        let title_observer =
            TitleObserver::new(mtm, Arc::clone(&nav_state), webview.clone());
        let permission_handler: Arc<Mutex<Option<PermissionHandlerFn>>> =
            Arc::new(Mutex::new(None));
        let ui_delegate = UiDelegate::new(
            mtm,
            Arc::clone(&nav_state),
            Arc::clone(&permission_handler),
        );
        unsafe {
            webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav_delegate)));
            webview.setUIDelegate(Some(ProtocolObject::from_ref(&*ui_delegate)));
            // KVO on `title` lets us synthesize `TitleChanged` events
            // even when the page mutates `document.title` after the
            // initial load (the navigation delegate's
            // `didFinishNavigation:` only fires once per top-level
            // load).
            webview.addObserver_forKeyPath_options_context(
                &title_observer,
                ns_string!("title"),
                NSKeyValueObservingOptions::New,
                std::ptr::null_mut(),
            );
            parent_view.addSubview(&webview);
        }

        // DPI observer: fires when the host window moves between
        // displays with different backing-scale factors. The block
        // sets a flag the producer polls before its next resize /
        // capture access; we re-apply `config.size` to keep
        // points/pixels coherent.
        let dpi_pending = Arc::new(AtomicBool::new(false));
        let dpi_observer = if let Some(host_window) = parent_view.window() {
            let flag = Arc::clone(&dpi_pending);
            let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                flag.store(true, Ordering::Release);
            });
            let center = NSNotificationCenter::defaultCenter();
            let token = unsafe {
                let host_window_obj: &AnyObject = (&*host_window).as_ref();
                center.addObserverForName_object_queue_usingBlock(
                    Some(objc2_app_kit::NSWindowDidChangeBackingPropertiesNotification),
                    Some(host_window_obj),
                    None,
                    &block,
                )
            };
            Some(token)
        } else {
            None
        };

        Ok(Self {
            capabilities: WryWebSurfaceCapabilities {
                backend: SystemWebviewBackend::WkWebView,
                // The capture pipeline isn't wired yet, so we still
                // advertise NativeChildOverlay as the preferred mode.
                // Slice B flips this to ImportedTexture once
                // ScreenCaptureKit emits frames.
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: native_frame::CapabilityStatus::Unsupported(
                    native_frame::UnsupportedReason::PlatformNotImplemented,
                ),
                native_child_overlay: native_frame::CapabilityStatus::Supported,
                cpu_snapshot: native_frame::CapabilityStatus::Supported,
                supported_frames: vec![native_frame::NativeFrameKind::MetalTextureRef],
                reason: "WkWebViewProducer slice A: WKWebView lifecycle (navigate / resize / set_offset) over an overlay surface; ScreenCaptureKit → IOSurface → MetalTextureRef capture pipeline is the next slice.",
            },
            webview,
            parent_view,
            nav_state,
            _nav_delegate: nav_delegate,
            _ui_delegate: ui_delegate,
            _script_message_handler: script_message_handler,
            title_observer,
            web_messages,
            last_cursor_shape: None,
            cursor_shapes: VecDeque::new(),
            config,
            mtm,
            capture: None,
            snapshot_generation: 0,
            pending_snapshot: Arc::new(Mutex::new(None)),
            pending_capture: Arc::new(Mutex::new(PendingCaptureSlot::Idle)),
            _scheme_handlers: scheme_handler_retained,
            _download_handler: download_handler,
            pending_find: Arc::new(Mutex::new(None)),
            pending_pdf: Arc::new(Mutex::new(None)),
            auth_handler,
            pending_cookies: Arc::new(Mutex::new(None)),
            permission_handler,
            dpi_pending,
            dpi_observer,
        })
    }

    pub(super) fn current_backing_scale(&self) -> objc2_core_foundation::CGFloat {
        backing_scale_for(&self.parent_view)
    }

    /// If the host window has moved between displays with different
    /// backing scales since the last call, re-apply the WKWebView's
    /// frame size for the new scale and re-issue
    /// `SCStream::updateConfiguration:` if a capture is live. Idempotent
    /// when no change is pending. Called automatically from `resize`
    /// and from `try_acquire_frame` so consumers don't have to wire
    /// the DPI observer themselves.
    pub(super) fn flush_pending_dpi_change(&mut self) {
        if !self.dpi_pending.swap(false, Ordering::AcqRel) {
            return;
        }
        // Re-apply the requested physical-pixel size; the WKWebView
        // frame is in points, derived inside `resize` via the parent
        // window's current backingScaleFactor. Calling resize with
        // the same `config.size` recomputes points-from-pixels under
        // the new scale and pushes the result through to
        // `setFrameSize` and `stream.updateConfiguration:`.
        let size = self.config.size;
        if let Err(error) = self.resize_internal(size) {
            eprintln!(
                "scrying: flush_pending_dpi_change: resize failed: {error}"
            );
        }
    }

    /// Internal resize that bypasses the trait's main-thread check
    /// (we're already on the main thread when called from internal
    /// helpers like `flush_pending_dpi_change`).
    pub(super) fn resize_internal(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<(), WryWebSurfaceError> {
        use super::capture::make_stream_configuration;
        let scale = self.current_backing_scale();
        let ns_size = NSSize::new(
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        self.webview.setFrameSize(ns_size);
        self.config.size = size;
        if let Some(capture) = self.capture.as_ref() {
            let new_cfg = make_stream_configuration(size);
            unsafe {
                capture
                    .stream
                    .updateConfiguration_completionHandler(&new_cfg, None);
            }
        }
        Ok(())
    }

    /// Read `NSCursor.currentSystemCursor` and, if the shape differs
    /// from the last reported one, push a [`CursorShape`] event onto
    /// the queue [`Self::poll_cursor_shape`] drains.
    ///
    /// Called after each forwarded pointer event — WebKit reacts to
    /// the synthesized `mouseDown:` / `mouseMoved:` etc. by setting
    /// the cursor on the system, and we read it back. There is no
    /// public callback for "cursor changed"; polling after events is
    /// the canonical pattern.
    pub(super) fn observe_cursor_change(&mut self) {
        let _mtm = self.mtm;
        let shape = super::helpers::current_cursor_shape();
        if self.last_cursor_shape.as_ref() != Some(&shape) {
            self.cursor_shapes.push_back(shape.clone());
            self.last_cursor_shape = Some(shape);
        }
    }

    /// Clear the navigation-result slot before kicking off a new load.
    /// The `events` queue is *not* cleared — consumers may still want
    /// to drain pending events from a prior navigation.
    pub(super) fn reset_nav_result(&self) -> Result<(), WryWebSurfaceError> {
        let mut state = self
            .nav_state
            .lock()
            .map_err(|_| WryWebSurfaceError::Platform("nav_state lock poisoned".into()))?;
        state.result = None;
        Ok(())
    }

    /// Pump the main run loop until the navigation completes or
    /// `timeout` elapses. Shared by `navigate_to_string` and
    /// `navigate_to_url`. `op_name` is woven into the error messages.
    pub(super) fn wait_for_nav_completion(
        &self,
        timeout: std::time::Duration,
        op_name: &'static str,
    ) -> Result<(), WryWebSurfaceError> {
        super::helpers::pump_until(timeout, || {
            let state = self.nav_state.lock().ok()?;
            state.result.clone()
        })
        .map_err(|_| {
            WryWebSurfaceError::Platform(format!("{op_name} timed out after {timeout:?}"))
        })?
        .map_err(WryWebSurfaceError::Platform)
    }

    /// Used by the `navigate_to_string` trait method to refer to a
    /// `WKWebView` regardless of crate-internal field privacy. Inline
    /// access; no allocation.
    pub(super) fn webview(&self) -> &WKWebView {
        &self.webview
    }
}

impl Drop for WkWebViewProducer {
    fn drop(&mut self) {
        // Tear down the SCK pipeline before the WKWebView so the
        // stream's content filter (which holds an SCWindow reference
        // pointing at the WebView's host window) is released first.
        self.stop_capture();

        // Detach the navigation delegate (the WKWebView holds a weak
        // reference, but explicit clear is harmless and keeps the
        // teardown order obvious), remove the title KVO observer
        // (must happen before any retained references cascade —
        // observed objects must outlive their observer registration),
        // remove the script-message handler from the user content
        // controller (which holds a strong ref), and remove the
        // WKWebView from its superview before our retained references
        // drop.
        unsafe {
            self.webview.setNavigationDelegate(None);
            self.webview.setUIDelegate(None);
            self.webview.removeObserver_forKeyPath_context(
                &self.title_observer,
                ns_string!("title"),
                std::ptr::null_mut(),
            );
            let config = self.webview.configuration();
            let ucc = config.userContentController();
            let bridge_name = NSString::from_str(HOST_BRIDGE_HANDLER_NAME);
            ucc.removeScriptMessageHandlerForName(&bridge_name);
            self.webview.removeFromSuperview();
        }

        // Drop the DPI-change observer registration. The token is the
        // opaque observer handle returned by `addObserverForName:...`;
        // `removeObserver:` un-registers the block-owning observer and
        // breaks the strong cycle holding our `Arc<AtomicBool>` flag.
        if let Some(token) = self.dpi_observer.take() {
            let center = NSNotificationCenter::defaultCenter();
            let observer_obj: &AnyObject = (&*token).as_ref();
            unsafe {
                center.removeObserver(observer_obj);
            }
        }
        // `webview`, `parent_view`, and `_nav_delegate` are released
        // by their own `Retained` Drop impls.
        let _ = self.mtm;
    }
}

