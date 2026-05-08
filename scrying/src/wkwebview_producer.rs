//! macOS WKWebView capture producer (planning skeleton).
//!
//! This is the macOS counterpart to
//! [`crate::webview2_composition_producer::WebView2CompositionProducer`].
//! The shape mirrors the Windows producer so consumers can program
//! against a single trait surface (`WryWebSurfaceProducer`); the
//! *internals* are entirely different because macOS has no public
//! composition-capture API directly analogous to
//! `Windows.Graphics.Capture::CreateFromVisual`.
//!
//! ## Capture options on macOS
//!
//! 1. **`WKWebView.takeSnapshot(...)` → CPU pixels.**
//!    Public, simple, returns an `NSImage`. One-shot per call: each
//!    invocation schedules a fresh render pass. Latency is high
//!    (typically >50ms) and rate is well below display refresh, so this
//!    is a `CpuSnapshot`-tier capability — useful for thumbnails and
//!    offscreen layout inspection, not for an interactive composited
//!    surface.
//!
//! 2. **`ScreenCaptureKit` (macOS 12.3+) → `IOSurfaceRef` →
//!    `MTLTexture`.** The closest analog to `Windows.Graphics.Capture`.
//!    Bind an `SCContentFilter` to either the `NSWindow` hosting the
//!    `WKWebView` or directly to the WKWebView's underlying `CALayer`,
//!    configure an `SCStreamConfiguration` for `BGRA8Unorm`, and stream
//!    frames via `SCStreamOutput`. Each `CMSampleBuffer` carries a
//!    `CVPixelBuffer` whose backing `IOSurfaceRef` maps to a Metal
//!    texture via `MTLDevice::newTextureWithDescriptor:iosurface:plane:`.
//!    This is the intended `ImportedTexture` path. Requires the
//!    "Screen Recording" privacy permission to be granted to the host
//!    binary on first use.
//!
//! 3. **Direct `CALayer` contents observation (private SPI).** WKWebView
//!    is layer-backed; the web-content compositing layer ultimately
//!    holds an `IOSurface`. Reaching it requires SPI / undocumented
//!    interfaces (`-_swapChain`, `-WKLayerHostView`, etc.), is fragile
//!    across macOS versions, and would not be acceptable in App Store
//!    builds. Worth knowing as an emergency hatch but not the canonical
//!    path.
//!
//! ## Sync model
//!
//! Friendlier than the D3D11/D3D12 case on Windows:
//!
//! - `IOSurface` is shared memory with cache-coherence guarantees the
//!   OS manages, *as long as* the producer/consumer pattern is honored
//!   (one writer, multiple readers). ScreenCaptureKit is the writer.
//! - For explicit GPU↔GPU sync, `MTLSharedEvent` is the Metal analog of
//!   a D3D12 fence. ScreenCaptureKit owns its own GPU queue; the
//!   consumer (wgpu's Metal queue) needs a wait point only if
//!   cross-queue ordering matters. For "render the most recent frame
//!   each present" semantics, implicit IOSurface coherence is enough.
//! - The Windows producer's "transition-barrier cache flush" trick
//!   has no Metal analog — IOSurface-backed Metal textures are
//!   `Storage::Shared` and don't need a per-frame barrier.
//!
//! ## Producer lifecycle
//!
//! Mirrors `WebView2CompositionProducer`:
//!
//! - `new(parent_view, config)` builds a `WKWebView` configured for
//!   composition capture and adds it as a subview of `parent_view`.
//! - `navigate_to_string(html, timeout)` loads inline HTML and waits
//!   for `WKNavigationDelegate.didFinishNavigation:`.
//! - `start_capture()` (lazy, triggered by first acquire) constructs
//!   an `SCContentFilter` over the WebView's window, builds an
//!   `SCStream` + `SCStreamConfiguration`, and calls `startCaptureWithCompletionHandler:`.
//! - `try_acquire_frame()` pulls the most recent `CMSampleBuffer`'s
//!   `IOSurfaceRef`, wraps it as `MTLTexture`, and returns it via
//!   `WryWebSurfaceFrame::Native(NativeFrame::Metal(MetalTextureRef))`.
//! - `resize(size)` updates the WKWebView's `frame.size`, the
//!   `SCStreamConfiguration.width/height`, and reapplies the filter.
//! - `Drop` stops the stream and tears down the WKWebView.
//!
//! ## Status
//!
//! Slice A: `new` stands up a real `WKWebView` as a subview of the
//! supplied parent `NSView`, `navigate_to_string` waits on
//! `WKNavigationDelegate.didFinishNavigation:` while pumping the main
//! run loop, and `resize` / `set_offset` reshape the live view.
//!
//! Slice B: [`WkWebViewProducer::start_capture`] stands up the
//! ScreenCaptureKit pipeline — resolves the WKWebView's host
//! `NSWindow` against `SCShareableContent.windows`, builds an
//! `SCContentFilter` over that window, configures an `SCStream` for
//! 32-bit BGRA, registers a custom `SCStreamOutput` delegate on a
//! dedicated dispatch queue, and blocks on
//! `startCaptureWithCompletionHandler:`. After it returns, each
//! `try_acquire_frame` / `acquire_frame` call extracts the most recent
//! `CMSampleBuffer`'s `IOSurfaceRef`, wraps it as an `MTLTexture` on
//! the **host wgpu device** via `MTLDevice::newTextureWithDescriptor:iosurface:plane:`,
//! and emits a [`WryWebSurfaceFrame::Native`] carrying a
//! [`crate::native_frame::MetalTextureRef`]. Capture is opt-in — until
//! `start_capture` is called the producer behaves exactly like slice A
//! (overlay-only output).
//!
//! Slice C: navigation parity. `navigate_to_url` loads a regular URL
//! through `loadRequest:`, the navigation delegate surfaces
//! `Starting` / `SourceChanged` / `Completed` events into a FIFO that
//! `poll_navigation_event` drains, and `move_focus` sends the
//! WKWebView to first-responder via the host `NSWindow`.
//!
//! Slice D: mouse forwarding. `send_mouse_input` synthesizes an
//! `NSEvent` (in window-coordinates, points, bottom-left origin) and
//! dispatches it directly through the WKWebView's NSResponder slots
//! (`mouseDown:` / `mouseUp:` / `mouseDragged:` / `mouseMoved:` /
//! `rightMouse*` / `otherMouse*` / `mouseExited:`). Scroll wheel
//! requires the `CGEvent` path (no `NSEvent` factory) and is
//! deferred; X-button distinction requires the same and is similarly
//! deferred (X-buttons currently arrive at WKWebView as Other-mouse
//! with the default button index).
//!
//! Slice E: bidirectional JS messaging. `WKUserContentController` is
//! pre-loaded with a `WKScriptMessageHandler` named
//! `scryingHostBridge` and a user script (injected at document start,
//! all frames) that builds a `window.chrome.webview` shim around it.
//! From JS: `window.chrome.webview.postMessage(s)` lands in the
//! producer's web-message FIFO drained by `poll_web_message`. From
//! the host: `post_web_message(s)` runs an `evaluateJavaScript:` that
//! dispatches a `{data: s}` event to listeners registered via
//! `window.chrome.webview.addEventListener('message', ...)` — the
//! same JS API consumers see on Windows / WebView2.
//!
//! Slice F: CPU snapshots. `capture_cpu_snapshot` calls
//! `takeSnapshotWithConfiguration:completionHandler:`, blocks the main
//! run loop until the (main-thread) callback fires or
//! `config.frame_timeout` elapses, then decodes the resulting NSImage
//! via its `TIFFRepresentation` through the `image` crate's TIFF
//! decoder into an `RgbaImage`. Independent of `start_capture` — does
//! not require Screen Recording permission.
//!
//! Slice G: scroll wheel via CGEvent. `MouseEventKind::Wheel` /
//! `HorizontalWheel` build a `CGEventCreateScrollWheelEvent2`,
//! convert to NSEvent via `eventWithCGEvent:`, and dispatch through
//! `webview.scrollWheel:`. Pixel-unit deltas, sign convention matches
//! AppKit (positive = up / right).
//!
//! Slice H: TitleChanged via KVO. A custom `TitleObserver` NSObject
//! subclass is registered as a `title`-keyPath KVO observer on the
//! WKWebView at construction time; when the page mutates
//! `document.title` (which happens after `didFinishNavigation:` for
//! many sites), the observer pushes
//! `NavigationEvent::TitleChanged { title }` into the same FIFO that
//! `NavDelegate` writes to. `Drop` calls `removeObserver:` before
//! tearing down so the observed object outlives its observer
//! registration.
//!
//! Slice I: keyboard forwarding. `send_keyboard_input` synthesizes an
//! `NSEvent` via `keyEventWithType:...:characters:...:keyCode:` and
//! dispatches through the WKWebView's `keyDown:` / `keyUp:` /
//! `flagsChanged:` slots. `characters` and
//! `characters_ignoring_modifiers` ride into WebKit's
//! `NSTextInputClient` impl so IME composition for non-Latin input
//! works for free — the host doesn't need to know whether a key is a
//! plain character, a dead key, or part of a composing IME session;
//! it just forwards what the windowing system reports.
//!
//! Slice J: cursor-change reporting. After each forwarded pointer
//! event, `observe_cursor_change` reads `NSCursor.currentSystemCursor`
//! and compares it against a small set of singletons
//! (`arrowCursor`, `IBeamCursor`, `pointingHandCursor`, etc.) to
//! translate to a [`CursorShape`]. Only changes are queued, so
//! `poll_cursor_shape` doesn't flood the consumer with duplicate
//! `Default` entries.
//!
//! Slice K: drag-and-drop forwarding. Documented limitation —
//! `NSDraggingDestination` callbacks require an `NSDraggingInfo`
//! instance only AppKit's drag manager can construct, so synthesizing
//! drag events for capture mode requires SPI. `send_drag_input`
//! returns [`WryWebSurfaceError::Unsupported`] with a message that
//! explains the constraint. Overlay mode handles drag automatically
//! through the responder chain — no producer involvement needed.
//!
//! Slice L: per-profile `WKWebsiteDataStore`. When `config.data_dir`
//! is non-empty, the producer derives a deterministic version-8 UUID
//! from the path's bytes via FNV-1a 128 and resolves a per-profile
//! `WKWebsiteDataStore` through `dataStoreForIdentifier:` (macOS 14+).
//! Empty `data_dir` falls back to the shared default store. macOS
//! doesn't take an arbitrary path for data stores; the UUID is the
//! native analog.
//!
//! Slice M: `MTLSharedEvent` synchronizer scaffolding. A new
//! [`SyncMechanism::ExplicitMetalEvent`] variant and the
//! [`crate::native_frame::MetalSharedEventSynchronizer`] type land
//! the consumer-side wait infrastructure. Currently a no-op (accepts
//! both `None` and `ExplicitMetalEvent` without waiting/signalling)
//! because `ScreenCaptureKit` doesn't expose its render queue, so
//! there's no producer-side hook to drive a signal from. Implicit
//! IOSurface coherence remains the contract today; this slice is
//! infrastructure for future SCK API additions or downstream
//! signal-driver consumers.
//!
//! Slice N: `SCStreamConfiguration` auto-update on resize. `resize`
//! now calls `stream.updateConfiguration:` with a fresh
//! [`SCStreamConfiguration`] (built via `make_stream_configuration`
//! so non-size params stay consistent with `start_capture`) whenever
//! a capture is live, so SCK samples come back at the new resolution
//! without requiring stream restart.

#![cfg(target_os = "macos")]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use dpi::PhysicalSize;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSCursor, NSEvent, NSEventModifierFlags, NSEventType, NSImage, NSView};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface,
    CVPixelBufferGetWidth,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSDate, NSDefaultRunLoopMode, NSDictionary, NSError,
    NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSPoint, NSRect, NSRunLoop, NSSize,
    NSString, NSURL, NSURLRequest, NSUUID,
};
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationDelegate, WKScriptMessage, WKScriptMessageHandler,
    WKSnapshotConfiguration, WKUserContentController, WKUserScript, WKUserScriptInjectionTime,
    WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
};

use crate::native_frame::MetalTextureRef as NativeMetalTextureRef;
use crate::{
    CursorShape, DragInput, FocusReason, HostWgpuContext, InteropBackend, KeyEventKind,
    KeyboardInput, MouseEventKind, MouseInput, MouseVirtualKeys, NativeFrame, NavigationEvent,
    SyncMechanism, SystemWebviewBackend, WebSurfaceMode, WryWebSurfaceCapabilities,
    WryWebSurfaceError, WryWebSurfaceFrame, WryWebSurfaceProducer,
};

/// Configuration for `WkWebViewProducer::new`. Mirrors the shape of
/// [`crate::webview2_composition_producer::WebView2CompositionConfig`].
#[derive(Clone, Debug)]
pub struct WkWebViewProducerConfig {
    /// Initial size of the WKWebView frame and the capture region, in
    /// physical pixels.
    pub size: PhysicalSize<u32>,
    /// Offset of the WKWebView relative to the parent NSView, in
    /// device-independent points (matches AppKit's coordinate system).
    pub offset: (f32, f32),
    /// Directory used as `WKWebsiteDataStore`'s persistent storage.
    /// Currently unused (slice A uses the default data store); reserved
    /// for the slice that wires `WKWebsiteDataStore` profiles.
    pub data_dir: PathBuf,
    /// Timeout for `navigate_to_string`, mirroring the Windows
    /// producer's navigation completion wait.
    pub navigation_timeout: std::time::Duration,
    /// Timeout for the initial frame after `start_capture`. Mirrors the
    /// Windows producer's first-frame block.
    pub frame_timeout: std::time::Duration,
}

impl WkWebViewProducerConfig {
    pub fn new(size: PhysicalSize<u32>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            size,
            offset: (0.0, 0.0),
            data_dir: data_dir.into(),
            navigation_timeout: std::time::Duration::from_secs(5),
            frame_timeout: std::time::Duration::from_secs(2),
        }
    }

    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset = (x, y);
        self
    }
}

#[derive(Default)]
struct NavState {
    /// `Some(Ok(()))` on `didFinishNavigation:`, `Some(Err(message))`
    /// on `didFailNavigation:` / `didFailProvisionalNavigation:`,
    /// `None` while a navigation is in flight or before any has been
    /// started. Reset to `None` at the start of each
    /// `navigate_to_string` / `navigate_to_url` call.
    result: Option<Result<(), String>>,
    /// FIFO of [`NavigationEvent`]s observed by [`NavDelegate`] but
    /// not yet drained by `poll_navigation_event`.
    events: VecDeque<NavigationEvent>,
}

/// Read the WKWebView's current committed URL as a String, falling
/// back to the empty string if WebKit hasn't populated `URL` yet
/// (e.g. inline-HTML loads with no `baseURL`).
fn webview_url_string(web_view: &WKWebView) -> String {
    unsafe { web_view.URL() }
        .and_then(|url| url.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `NavDelegate` does not implement `Drop` (its only state is an
    //   `Arc<Mutex<NavState>>` which cleans up via the standard Rust
    //   drop glue).
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Arc<Mutex<NavState>>]
    struct NavDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for NavDelegate {}

    // SAFETY: `WKNavigationDelegate` requires only that the method
    // signatures match Apple's protocol. All callbacks land on the
    // main thread.
    unsafe impl WKNavigationDelegate for NavDelegate {
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn did_start(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let url = webview_url_string(web_view);
            if let Ok(mut state) = self.ivars().lock() {
                state.events.push_back(NavigationEvent::Starting { url });
            }
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn did_commit(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let url = webview_url_string(web_view);
            if let Ok(mut state) = self.ivars().lock() {
                state.events.push_back(NavigationEvent::SourceChanged { url });
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            let url = webview_url_string(web_view);
            if let Ok(mut state) = self.ivars().lock() {
                state.events.push_back(NavigationEvent::Completed { url, success: true });
                state.result = Some(Ok(()));
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let url = webview_url_string(web_view);
            let message = error.localizedDescription().to_string();
            if let Ok(mut state) = self.ivars().lock() {
                state
                    .events
                    .push_back(NavigationEvent::Completed { url, success: false });
                state.result = Some(Err(message));
            }
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            let url = webview_url_string(web_view);
            let message = error.localizedDescription().to_string();
            if let Ok(mut state) = self.ivars().lock() {
                state
                    .events
                    .push_back(NavigationEvent::Completed { url, success: false });
                state.result = Some(Err(message));
            }
        }
    }
);

impl NavDelegate {
    fn new(mtm: MainThreadMarker, state: Arc<Mutex<NavState>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(state);
        // SAFETY: NSObject's `init` returns a valid initialized instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// JS bridge handler name. JS-side code reaches the host via
/// `window.webkit.messageHandlers.<NAME>.postMessage(...)`. The shim
/// installed in [`HOST_BRIDGE_USER_SCRIPT`] wraps that under a
/// WebView2-compatible `window.chrome.webview` API so consumers can
/// write portable code.
const HOST_BRIDGE_HANDLER_NAME: &str = "scryingHostBridge";

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
///   `evaluateJavaScript:` (see [`WkWebViewProducer::post_web_message`]).
///
/// Idempotent: re-runs (e.g. after a same-document navigation) skip
/// if the shim is already present.
const HOST_BRIDGE_USER_SCRIPT: &str = r#"(function() {
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
    struct ScriptMessageHandler;

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
    fn new(mtm: MainThreadMarker, queue: Arc<Mutex<VecDeque<String>>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(queue);
        // SAFETY: NSObject's `init` returns a valid initialized instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// State the [`TitleObserver`] needs to look up a fresh title and
/// publish a `NavigationEvent`.
///
/// The observer holds a strong [`Retained<WKWebView>`] so the KVO
/// callback can read `webview.title()` directly without rebinding the
/// `object` parameter through `AnyObject` downcasts. The retain cycle
/// (WkWebViewProducer → TitleObserver → WKWebView) is broken in
/// `WkWebViewProducer::Drop` by calling `removeObserver:` before any
/// reference cascades.
struct TitleObserverIvars {
    nav_state: Arc<Mutex<NavState>>,
    webview: Retained<WKWebView>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `TitleObserver` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TitleObserverIvars]
    struct TitleObserver;

    unsafe impl NSObjectProtocol for TitleObserver {}

    // SAFETY: KVO is invoked on the main thread because the WKWebView
    // is registered on the main thread (`addObserver:` is called
    // there) and AppKit / WebKit only mutate observable properties on
    // the main thread. Both the ivar `Arc<Mutex<...>>` and the
    // `Retained<WKWebView>` are therefore accessed from a single
    // thread.
    impl TitleObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut std::ffi::c_void,
        ) {
            // Defensive in case the observer is ever registered for
            // multiple key paths. KVO fires rarely (once per title
            // change), so the small allocation here is fine.
            if key_path.map(|k| k.to_string()).as_deref() != Some("title") {
                return;
            }
            let ivars = self.ivars();
            let title = unsafe { ivars.webview.title() }
                .map(|s| s.to_string())
                .unwrap_or_default();
            if let Ok(mut state) = ivars.nav_state.lock() {
                state
                    .events
                    .push_back(NavigationEvent::TitleChanged { title });
            }
        }
    }
);

impl TitleObserver {
    fn new(
        mtm: MainThreadMarker,
        nav_state: Arc<Mutex<NavState>>,
        webview: Retained<WKWebView>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TitleObserverIvars { nav_state, webview });
        unsafe { msg_send![super(this), init] }
    }
}

/// Newtype that asserts a CF-typed retained reference is safe to send
/// across threads.
///
/// `CMSampleBuffer` and the CF types it transitively references
/// (`CVImageBuffer`, `IOSurfaceRef`) are documented thread-safe by
/// Apple — retain/release is atomic and the underlying data is
/// immutable from the consumer's perspective. The objc2-core-foundation
/// crate is conservative and doesn't auto-derive `Send` for
/// `CFRetained<T>`, so we wrap explicitly at the dispatch-queue
/// boundary.
struct SendCFRetained<T>(CFRetained<T>);
// SAFETY: see `SendCFRetained` doc.
unsafe impl<T> Send for SendCFRetained<T> {}

/// Latest screen-capture sample handed off from the
/// `SCStreamOutput::stream:didOutputSampleBuffer:ofType:` callback
/// (which fires on a background dispatch queue) to `try_acquire_frame`
/// on the main thread. Only the most recent sample is kept; older
/// samples are dropped on overwrite.
type LatestSample = Mutex<Option<SendCFRetained<CMSampleBuffer>>>;

#[derive(Default)]
struct CaptureSignal {
    /// `Some(Ok(()))` once `startCaptureWithCompletionHandler:` /
    /// `stopCaptureWithCompletionHandler:` resolves, `Some(Err(msg))`
    /// on error, `None` while pending.
    result: Option<Result<(), String>>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `StreamOutputDelegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[ivars = Arc<LatestSample>]
    struct StreamOutputDelegate;

    unsafe impl NSObjectProtocol for StreamOutputDelegate {}

    // SAFETY: signature matches Apple's `SCStreamOutput` protocol.
    unsafe impl SCStreamOutput for StreamOutputDelegate {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            r#type: SCStreamOutputType,
        ) {
            if r#type != SCStreamOutputType::Screen {
                return;
            }
            // Retain the sample; the protocol contract is that the
            // callee must retain if it wants to outlive this call.
            let retained = unsafe { CFRetained::retain(NonNull::from(sample_buffer)) };
            if let Ok(mut slot) = self.ivars().lock() {
                *slot = Some(SendCFRetained(retained));
            }
        }
    }
);

impl StreamOutputDelegate {
    fn new(latest: Arc<LatestSample>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(latest);
        // SAFETY: NSObject's `init` returns a valid initialized instance.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `StreamErrorDelegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[ivars = Arc<Mutex<Option<String>>>]
    struct StreamErrorDelegate;

    unsafe impl NSObjectProtocol for StreamErrorDelegate {}

    unsafe impl SCStreamDelegate for StreamErrorDelegate {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            if let Ok(mut slot) = self.ivars().lock() {
                *slot = Some(error.localizedDescription().to_string());
            }
        }
    }
);

impl StreamErrorDelegate {
    fn new(error_slot: Arc<Mutex<Option<String>>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(error_slot);
        unsafe { msg_send![super(this), init] }
    }
}

/// Cross-thread observable status of the ScreenCaptureKit pipeline,
/// reported by [`WkWebViewProducer::capture_status`] so non-blocking
/// consumers (e.g. winit hosts) can poll instead of blocking on the
/// main run loop.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum CaptureStatus {
    /// `start_capture_async` has not been called yet (or `stop_capture`
    /// reset the state machine).
    Idle,
    /// `start_capture_async` was called but neither
    /// `SCShareableContent` nor `startCaptureWithCompletionHandler:`
    /// have resolved yet.
    Starting,
    /// Capture is live; `try_acquire_frame` / `acquire_frame` will
    /// emit `Native` frames.
    Live,
    /// The async start failed at some stage. The consumer can call
    /// `start_capture_async` again to retry.
    Failed(String),
}

/// Internal state machine slot for the async start-capture flow.
/// Held behind `Arc<Mutex<...>>` so the SCK completion blocks
/// (which fire on a private background queue) can advance it without
/// touching the producer's `&mut self`.
enum PendingCaptureSlot {
    Idle,
    Starting,
    Ready(SendOnly<CaptureState>),
    Failed(String),
}

/// Generic Send wrapper for non-Send objc2 `Retained` items that need
/// to traverse a dispatch-queue boundary.
///
/// Justification: SCK / Metal / dispatch types we ferry across the
/// SCShareableContent → main-thread handoff are CF / NSObject types
/// whose retain/release is atomic and whose data is immutable from
/// our consumption perspective. The objc2 crates are conservative
/// and don't auto-derive `Send` for all `Retained<T>`; we wrap
/// explicitly at the queue boundary.
struct SendOnly<T>(T);
// SAFETY: see `SendOnly` doc.
unsafe impl<T> Send for SendOnly<T> {}

/// Captured-by-block bag of all the SCK pieces the inner
/// `startCaptureWithCompletionHandler:` block needs to assemble a
/// [`CaptureState`] when the stream goes live.
struct InProgressCaptureState {
    metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
    stream: Retained<SCStream>,
    output: Retained<StreamOutputDelegate>,
    error_delegate: Retained<StreamErrorDelegate>,
    sample_queue: DispatchRetained<DispatchQueue>,
    latest: Arc<LatestSample>,
    stream_error: Arc<Mutex<Option<String>>>,
}

/// Helper used by SCK completion blocks to update the shared
/// [`PendingCaptureSlot`]. Lock-poisoning failures are silently
/// dropped because there's no useful recovery path from a callback —
/// the next [`WkWebViewProducer::capture_status`] poll will surface
/// the prior state (or `Failed` if a poisoned lock makes things
/// inconsistent).
fn write_pending(
    pending: &Arc<Mutex<PendingCaptureSlot>>,
    state: PendingCaptureSlot,
) {
    if let Ok(mut s) = pending.lock() {
        *s = state;
    }
}

/// State held while ScreenCaptureKit is actively streaming.
struct CaptureState {
    /// Strong reference to the host wgpu device's `MTLDevice`. Used to
    /// allocate IOSurface-backed `MTLTexture`s on the same device the
    /// consumer renders against (no cross-device migration).
    metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
    stream: Retained<SCStream>,
    output: Retained<StreamOutputDelegate>,
    _error_delegate: Retained<StreamErrorDelegate>,
    _sample_queue: DispatchRetained<DispatchQueue>,
    latest: Arc<LatestSample>,
    /// Surfaced via [`StreamErrorDelegate`] when the stream stops
    /// unexpectedly (e.g., capture target window closed). Inspected
    /// from `try_acquire_frame` so the consumer learns the stream is
    /// dead.
    stream_error: Arc<Mutex<Option<String>>>,
    /// Most-recently-emitted MTLTexture. The producer keeps it alive
    /// here because [`NativeMetalTextureRef::raw_metal_texture`] is a
    /// raw pointer; the consumer's [`crate::native_frame`] importer
    /// re-retains the object during import. Replaced on each
    /// successful `try_acquire_frame`.
    last_emitted: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    generation: AtomicU64,
}

/// macOS WKWebView capture producer.
///
/// Slice A: real WKWebView lifecycle, no GPU capture (output is
/// `OverlayOnly`). Slice B will wire ScreenCaptureKit → IOSurface →
/// `MetalTextureRef` and flip `acquire_frame` to
/// `WryWebSurfaceFrame::Native(...)`.
pub struct WkWebViewProducer {
    capabilities: WryWebSurfaceCapabilities,
    webview: Retained<WKWebView>,
    /// The host's parent `NSView`. Retained so the WKWebView's
    /// superview cannot vanish from under us; the caller is also
    /// expected to outlive the producer per the `new` safety contract,
    /// but the extra retain is cheap insurance against early teardown
    /// during `Drop`.
    parent_view: Retained<NSView>,
    /// Shared with the navigation delegate.
    nav_state: Arc<Mutex<NavState>>,
    /// `WKWebView` only holds a weak reference to its navigation
    /// delegate, so the producer owns the strong reference.
    _nav_delegate: Retained<NavDelegate>,
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
    web_messages: Arc<Mutex<VecDeque<String>>>,
    /// Last [`CursorShape`] we observed via `NSCursor.currentSystemCursor`
    /// after a forwarded pointer event. The producer pushes to
    /// [`Self::cursor_shapes`] only when the new shape differs from
    /// this value, so consumers don't get a flood of duplicate
    /// `Default` events.
    last_cursor_shape: Option<CursorShape>,
    /// Cursor-shape changes the host should apply, drained by
    /// [`Self::poll_cursor_shape`]. Populated after each forwarded
    /// pointer event observes a new `NSCursor.currentSystemCursor`.
    cursor_shapes: VecDeque<CursorShape>,
    config: WkWebViewProducerConfig,
    mtm: MainThreadMarker,
    /// `Some` once `start_capture` has succeeded; `None` while the
    /// producer is still in slice-A overlay-only mode.
    capture: Option<CaptureState>,
    /// Counter incremented per [`Self::capture_cpu_snapshot`] call so
    /// consumers can disambiguate snapshot frames. Independent of
    /// [`CaptureState::generation`] which counts SCK samples.
    snapshot_generation: u64,
    /// Most-recent completion of [`Self::request_snapshot`]. Drained
    /// by [`Self::poll_snapshot`]. Older completions are overwritten
    /// before the consumer polls.
    pending_snapshot: Arc<Mutex<Option<PendingSnapshot>>>,
    /// Cross-thread state machine for [`Self::start_capture_async`].
    /// Advanced by SCK completion blocks running on background
    /// dispatch queues; promoted into `self.capture` by the consumer
    /// via [`Self::capture_status`].
    pending_capture: Arc<Mutex<PendingCaptureSlot>>,
}

/// Newtype that asserts a `Retained<NSImage>` is safe to send between
/// threads — the producer's snapshot completion handler fires on the
/// main thread and the producer's `poll_snapshot` reads from the same
/// thread, so the cross-thread `Send` is satisfied trivially. The
/// wrapper exists to satisfy the conservative compiler bound on
/// `Mutex<Option<T>>` where T isn't `Send` by default.
struct SendRetainedNSImage(Retained<NSImage>);
// SAFETY: see `SendRetainedNSImage` doc.
unsafe impl Send for SendRetainedNSImage {}

enum PendingSnapshot {
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
        let mtm = MainThreadMarker::new().ok_or(WryWebSurfaceError::Platform(
            "WkWebViewProducer::new must be called on the main thread".into(),
        ))?;

        let parent_view: Retained<NSView> = {
            let ptr = NonNull::new(parent_view as *mut NSView).ok_or(
                WryWebSurfaceError::Platform("parent_view pointer was null".into()),
            )?;
            // SAFETY: caller-asserted: parent_view is a valid NSView*
            // that outlives this call.
            unsafe { Retained::retain(ptr.as_ptr()) }.ok_or(WryWebSurfaceError::Platform(
                "failed to retain parent NSView".into(),
            ))?
        };

        let backing_scale = backing_scale_for(&parent_view);
        let frame = ns_rect_from_pixels(config.offset, config.size, backing_scale);

        let webview_config = unsafe { WKWebViewConfiguration::new(mtm) };

        // Per-profile cookies + storage isolation. macOS doesn't take an
        // arbitrary path for `WKWebsiteDataStore`; the closest analog
        // is `dataStoreForIdentifier:` (macOS 14+) which keys a
        // persistent store inside the app container by UUID. We derive
        // a stable UUID from `config.data_dir` so the same path always
        // resolves to the same profile across runs. An empty path
        // means "use the shared default store" (slice A behavior).
        if !config.data_dir.as_os_str().is_empty() {
            let identifier = profile_uuid_for_path(&config.data_dir, mtm);
            let data_store = unsafe {
                WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm)
            };
            unsafe {
                webview_config.setWebsiteDataStore(&data_store);
            }
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
        let nav_delegate = NavDelegate::new(mtm, Arc::clone(&nav_state));
        let title_observer =
            TitleObserver::new(mtm, Arc::clone(&nav_state), webview.clone());
        unsafe {
            webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav_delegate)));
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

        Ok(Self {
            capabilities: WryWebSurfaceCapabilities {
                backend: SystemWebviewBackend::WkWebView,
                // The capture pipeline isn't wired yet, so we still
                // advertise NativeChildOverlay as the preferred mode.
                // Slice B flips this to ImportedTexture once
                // ScreenCaptureKit emits frames.
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: crate::native_frame::CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::PlatformNotImplemented,
                ),
                native_child_overlay: crate::native_frame::CapabilityStatus::Supported,
                cpu_snapshot: crate::native_frame::CapabilityStatus::Supported,
                supported_frames: vec![
                    crate::native_frame::NativeFrameKind::MetalTextureRef,
                ],
                reason: "WkWebViewProducer slice A: WKWebView lifecycle (navigate / resize / set_offset) over an overlay surface; ScreenCaptureKit → IOSurface → MetalTextureRef capture pipeline is the next slice.",
            },
            webview,
            parent_view,
            nav_state,
            _nav_delegate: nav_delegate,
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
        })
    }

    /// Non-blocking variant of `navigate_to_url`. Invokes
    /// `WKWebView::loadRequest:` and returns immediately — the load
    /// completes asynchronously and surfaces through
    /// [`Self::poll_navigation_event`].
    ///
    /// **Use this** instead of [`navigate_to_url`](WryWebSurfaceProducer::navigate_to_url)
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
        unsafe { self.webview.loadRequest(&request) };
        Ok(())
    }

    /// Non-blocking variant of `navigate_to_string`. Invokes
    /// `WKWebView::loadHTMLString:` and returns immediately.
    /// Completion arrives through [`Self::poll_navigation_event`].
    ///
    /// See [`Self::load_url`] for when to prefer this over the
    /// blocking trait method.
    pub fn load_html(&self, html: &str) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "load_html must be called on the main thread".into(),
            ));
        }
        let html_ns = NSString::from_str(html);
        unsafe { self.webview.loadHTMLString_baseURL(&html_ns, None) };
        Ok(())
    }

    /// Stand up the ScreenCaptureKit pipeline against the WKWebView's
    /// host window and start streaming `MTLTexture` frames into the
    /// shared sample slot. Idempotent.
    ///
    /// On success, [`Self::acquire_frame`] / [`Self::try_acquire_frame`]
    /// flip from `OverlayOnly` to
    /// `WryWebSurfaceFrame::Native(NativeFrame::MetalTextureRef(...))`,
    /// and `capabilities().preferred_mode` flips to
    /// [`WebSurfaceMode::ImportedTexture`].
    ///
    /// `host` must be a wgpu device on the Metal backend; the textures
    /// are allocated on `host.device`'s underlying `MTLDevice` so they
    /// can be imported into wgpu without cross-device migration.
    ///
    /// `timeout` bounds both the `SCShareableContent` enumeration and
    /// the `startCaptureWithCompletionHandler:` callback wait.
    ///
    /// Requires the user-facing **Screen Recording** privacy
    /// permission. The first call triggers the system prompt; if
    /// denied, this method returns `Platform(...)`.
    ///
    /// # ⚠️ Blocking — host-event-loop hazard
    ///
    /// Pumps the main `NSRunLoop` twice (once for
    /// `SCShareableContent`, once for
    /// `startCaptureWithCompletionHandler:`). **Do not call from
    /// inside a host event-loop callback** (winit `resumed` /
    /// `window_event`) — the pump re-enters the host's dispatch
    /// and panics. Use [`Self::start_capture_async`] +
    /// [`Self::capture_status`] from event-loop contexts.
    pub fn start_capture(
        &mut self,
        host: HostWgpuContext,
        timeout: Duration,
    ) -> Result<(), WryWebSurfaceError> {
        if self.capture.is_some() {
            return Ok(());
        }

        let mtm = MainThreadMarker::new().ok_or_else(|| {
            WryWebSurfaceError::Platform("start_capture must be called on the main thread".into())
        })?;

        if host.backend != InteropBackend::Metal {
            return Err(WryWebSurfaceError::Platform(format!(
                "start_capture requires a Metal wgpu backend, got {:?}",
                host.backend
            )));
        }

        // Acquire the host's MTLDevice via the wgpu-hal escape hatch so
        // textures we allocate land on the same device the consumer
        // renders against.
        let metal_device: Retained<ProtocolObject<dyn MTLDevice>> = unsafe {
            host.device
                .as_hal::<wgpu::wgc::api::Metal>()
                .ok_or_else(|| {
                    WryWebSurfaceError::Platform(
                        "host wgpu device is not on the Metal backend".into(),
                    )
                })?
                .raw_device()
                .clone()
        };

        let target_window = self.resolve_target_window(timeout)?;
        let target_size = self.config.size;

        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(
                SCContentFilter::alloc(),
                &target_window,
            )
        };

        let stream_config = make_stream_configuration(target_size);

        let stream_error = Arc::new(Mutex::new(None::<String>));
        let error_delegate = StreamErrorDelegate::new(Arc::clone(&stream_error));
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &stream_config,
                Some(ProtocolObject::from_ref(&*error_delegate)),
            )
        };

        let latest: Arc<LatestSample> = Arc::new(Mutex::new(None));
        let output_delegate = StreamOutputDelegate::new(Arc::clone(&latest));
        let sample_queue = DispatchQueue::new("scrying.wkwebview.sck-sample", None);

        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output_delegate),
                    SCStreamOutputType::Screen,
                    Some(&sample_queue),
                )
                .map_err(|e| {
                    WryWebSurfaceError::Platform(format!(
                        "addStreamOutput failed: {}",
                        e.localizedDescription()
                    ))
                })?;
        }

        let signal = Arc::new(Mutex::new(CaptureSignal::default()));
        {
            let signal = Arc::clone(&signal);
            let block = RcBlock::new(move |err: *mut NSError| {
                let result = if err.is_null() {
                    Ok(())
                } else {
                    let msg = unsafe { (*err).localizedDescription().to_string() };
                    Err(msg)
                };
                if let Ok(mut s) = signal.lock() {
                    s.result = Some(result);
                }
            });
            unsafe {
                stream.startCaptureWithCompletionHandler(Some(&block));
            }
        }

        // Pump the main run loop until the start completion handler
        // fires (it runs on a background queue but the signal is
        // observed on the main thread via the Mutex).
        match pump_until(timeout, || signal.lock().ok().and_then(|s| s.result.clone())) {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                return Err(WryWebSurfaceError::Platform(format!(
                    "startCapture failed: {}",
                    msg
                )));
            }
            Err(()) => {
                return Err(WryWebSurfaceError::Platform(format!(
                    "startCapture did not resolve within {:?}",
                    timeout
                )));
            }
        }

        self.capture = Some(CaptureState {
            metal_device,
            stream,
            output: output_delegate,
            _error_delegate: error_delegate,
            _sample_queue: sample_queue,
            latest,
            stream_error,
            last_emitted: None,
            generation: AtomicU64::new(0),
        });

        // Capture is live — flip the advertised capability so consumers
        // know the GPU-handoff path is now preferred over overlay.
        self.capabilities.preferred_mode = WebSurfaceMode::ImportedTexture;
        self.capabilities.imported_texture = crate::native_frame::CapabilityStatus::Supported;
        self.capabilities.reason =
            "WkWebViewProducer slice B: ScreenCaptureKit → IOSurface → MetalTextureRef capture is live; consumer should render the imported texture each frame.";
        let _ = mtm;
        Ok(())
    }

    /// Non-blocking variant of [`Self::start_capture`]. Kicks off the
    /// SCShareableContent → SCContentFilter → SCStream chain via
    /// completion blocks and returns immediately. The consumer polls
    /// [`Self::capture_status`] (typically each frame from a host
    /// event-loop callback) to observe progression and to install
    /// the `CaptureState` into the producer once the stream is live.
    ///
    /// Use this in preference to the blocking variant whenever
    /// `start_capture` would be called from a host event-loop
    /// callback (e.g. winit's `resumed` / `window_event`) — pumping
    /// the main `NSRunLoop` from inside such a callback re-enters
    /// the host's dispatch and panics.
    ///
    /// Idempotent: returns `Ok(())` if a capture is already live or
    /// in progress.
    pub fn start_capture_async(
        &mut self,
        host: HostWgpuContext,
    ) -> Result<(), WryWebSurfaceError> {
        if self.capture.is_some() {
            return Ok(());
        }
        // Reset / advance the state machine. If we're already in
        // Starting, return without restarting.
        {
            let p = self.pending_capture.lock().map_err(|_| {
                WryWebSurfaceError::Platform("pending_capture lock poisoned".into())
            })?;
            if matches!(*p, PendingCaptureSlot::Starting) {
                return Ok(());
            }
        }

        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "start_capture_async must be called on the main thread".into(),
            ));
        }
        if host.backend != InteropBackend::Metal {
            return Err(WryWebSurfaceError::Platform(format!(
                "start_capture_async requires a Metal wgpu backend, got {:?}",
                host.backend
            )));
        }

        let metal_device: Retained<ProtocolObject<dyn MTLDevice>> = unsafe {
            host.device
                .as_hal::<wgpu::wgc::api::Metal>()
                .ok_or_else(|| {
                    WryWebSurfaceError::Platform(
                        "host wgpu device is not on the Metal backend".into(),
                    )
                })?
                .raw_device()
                .clone()
        };

        let host_window = self.webview.window().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "WKWebView is not in a window — start_capture_async requires the parent NSView to be embedded in an NSWindow".into(),
            )
        })?;
        let target_window_number = host_window.windowNumber();
        if target_window_number <= 0 {
            return Err(WryWebSurfaceError::Platform(
                "host NSWindow has no valid windowNumber".into(),
            ));
        }
        let target_id = target_window_number as u32;
        let target_size = self.config.size;

        *self
            .pending_capture
            .lock()
            .map_err(|_| {
                WryWebSurfaceError::Platform("pending_capture lock poisoned".into())
            })? = PendingCaptureSlot::Starting;

        let pending = Arc::clone(&self.pending_capture);
        let metal_device_for_block = SendOnly(metal_device);

        let outer_block = RcBlock::new(
            move |content: *mut SCShareableContent, err: *mut NSError| {
                if !err.is_null() {
                    let msg = unsafe { (*err).localizedDescription().to_string() };
                    write_pending(
                        &pending,
                        PendingCaptureSlot::Failed(format!(
                            "SCShareableContent failed: {msg}"
                        )),
                    );
                    return;
                }
                let Some(non_null) = NonNull::new(content) else {
                    write_pending(
                        &pending,
                        PendingCaptureSlot::Failed(
                            "SCShareableContent returned null".into(),
                        ),
                    );
                    return;
                };
                // SAFETY: SCK hands us a +0 borrow; retain to keep
                // it alive across the rest of the block.
                let content: Retained<SCShareableContent> =
                    match unsafe { Retained::retain(non_null.as_ptr()) } {
                        Some(c) => c,
                        None => {
                            write_pending(
                                &pending,
                                PendingCaptureSlot::Failed(
                                    "Retained::retain on SCShareableContent returned None"
                                        .into(),
                                ),
                            );
                            return;
                        }
                    };

                let windows: Retained<NSArray<SCWindow>> = unsafe { content.windows() };
                let mut matched: Option<Retained<SCWindow>> = None;
                for i in 0..windows.count() {
                    let window = windows.objectAtIndex(i);
                    if unsafe { window.windowID() } == target_id {
                        matched = Some(window);
                        break;
                    }
                }
                let target_window = match matched {
                    Some(w) => w,
                    None => {
                        write_pending(
                            &pending,
                            PendingCaptureSlot::Failed(format!(
                                "no SCWindow matched windowNumber {target_window_number}"
                            )),
                        );
                        return;
                    }
                };

                // Build the SCK pipeline. None of these classes are
                // MainThreadOnly so this is fine on the SCK private
                // queue.
                let filter = unsafe {
                    SCContentFilter::initWithDesktopIndependentWindow(
                        SCContentFilter::alloc(),
                        &target_window,
                    )
                };
                let stream_config = make_stream_configuration(target_size);
                let stream_error = Arc::new(Mutex::new(None::<String>));
                let error_delegate = StreamErrorDelegate::new(Arc::clone(&stream_error));
                let stream = unsafe {
                    SCStream::initWithFilter_configuration_delegate(
                        SCStream::alloc(),
                        &filter,
                        &stream_config,
                        Some(ProtocolObject::from_ref(&*error_delegate)),
                    )
                };
                let latest: Arc<LatestSample> = Arc::new(Mutex::new(None));
                let output_delegate = StreamOutputDelegate::new(Arc::clone(&latest));
                let sample_queue =
                    DispatchQueue::new("scrying.wkwebview.sck-sample", None);
                if let Err(e) = unsafe {
                    stream.addStreamOutput_type_sampleHandlerQueue_error(
                        ProtocolObject::from_ref(&*output_delegate),
                        SCStreamOutputType::Screen,
                        Some(&sample_queue),
                    )
                } {
                    write_pending(
                        &pending,
                        PendingCaptureSlot::Failed(format!(
                            "addStreamOutput failed: {}",
                            e.localizedDescription()
                        )),
                    );
                    return;
                }

                // Capture the assembled state into the inner block.
                let metal_device = metal_device_for_block.0.clone();
                let pending_inner = Arc::clone(&pending);
                let in_progress = SendOnly(InProgressCaptureState {
                    metal_device,
                    stream: stream.clone(),
                    output: output_delegate.clone(),
                    error_delegate: error_delegate.clone(),
                    sample_queue: sample_queue.clone(),
                    latest: Arc::clone(&latest),
                    stream_error: Arc::clone(&stream_error),
                });

                let inner_block = RcBlock::new(move |err: *mut NSError| {
                    if !err.is_null() {
                        let msg =
                            unsafe { (*err).localizedDescription().to_string() };
                        write_pending(
                            &pending_inner,
                            PendingCaptureSlot::Failed(format!(
                                "startCapture failed: {msg}"
                            )),
                        );
                        return;
                    }
                    let parts = &in_progress.0;
                    let cap = CaptureState {
                        metal_device: parts.metal_device.clone(),
                        stream: parts.stream.clone(),
                        output: parts.output.clone(),
                        _error_delegate: parts.error_delegate.clone(),
                        _sample_queue: parts.sample_queue.clone(),
                        latest: Arc::clone(&parts.latest),
                        stream_error: Arc::clone(&parts.stream_error),
                        last_emitted: None,
                        generation: AtomicU64::new(0),
                    };
                    write_pending(
                        &pending_inner,
                        PendingCaptureSlot::Ready(SendOnly(cap)),
                    );
                });
                unsafe {
                    stream.startCaptureWithCompletionHandler(Some(&inner_block));
                }
            },
        );

        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&outer_block);
        }
        Ok(())
    }

    /// Poll the async capture state machine. Returns the current
    /// [`CaptureStatus`]. When status is `Live`, the producer's
    /// `self.capture` slot is filled and `try_acquire_frame` /
    /// `acquire_frame` start emitting `Native` frames.
    ///
    /// Call this from a host event-loop callback after
    /// [`Self::start_capture_async`]. Idempotent — once `Live` it
    /// keeps returning `Live`.
    pub fn capture_status(&mut self) -> CaptureStatus {
        if self.capture.is_some() {
            return CaptureStatus::Live;
        }
        let mut slot = match self.pending_capture.lock() {
            Ok(g) => g,
            Err(_) => return CaptureStatus::Failed("pending_capture poisoned".into()),
        };
        match std::mem::replace(&mut *slot, PendingCaptureSlot::Idle) {
            PendingCaptureSlot::Idle => {
                *slot = PendingCaptureSlot::Idle;
                CaptureStatus::Idle
            }
            PendingCaptureSlot::Starting => {
                *slot = PendingCaptureSlot::Starting;
                CaptureStatus::Starting
            }
            PendingCaptureSlot::Failed(msg) => {
                let report = msg.clone();
                *slot = PendingCaptureSlot::Failed(msg);
                CaptureStatus::Failed(report)
            }
            PendingCaptureSlot::Ready(SendOnly(state)) => {
                drop(slot);
                self.install_capture_state(state);
                CaptureStatus::Live
            }
        }
    }

    fn install_capture_state(&mut self, state: CaptureState) {
        self.capture = Some(state);
        self.capabilities.preferred_mode = WebSurfaceMode::ImportedTexture;
        self.capabilities.imported_texture =
            crate::native_frame::CapabilityStatus::Supported;
        self.capabilities.reason =
            "WkWebViewProducer slice B: ScreenCaptureKit → IOSurface → MetalTextureRef capture is live; consumer should render the imported texture each frame.";
        // Advance the slot to "consumed" so subsequent polls don't
        // re-promote the same state.
        if let Ok(mut p) = self.pending_capture.lock() {
            *p = PendingCaptureSlot::Idle;
        }
    }

    /// Stop the capture stream and tear down ScreenCaptureKit state.
    /// Idempotent. Safe to call from `Drop`.
    pub fn stop_capture(&mut self) {
        let Some(capture) = self.capture.take() else {
            return;
        };

        // Synchronously stop on the SCK background thread, but don't
        // block the main thread waiting — completion errors are
        // surfaced via `stream_error` if useful.
        unsafe {
            capture.stream.stopCaptureWithCompletionHandler(None);
            let _ = capture.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*capture.output),
                SCStreamOutputType::Screen,
            );
        }

        // Walk back the advertised capability so a future
        // `start_capture` correctly re-flips it.
        self.capabilities.preferred_mode = WebSurfaceMode::NativeChildOverlay;
        self.capabilities.imported_texture =
            crate::native_frame::CapabilityStatus::Unsupported(
                crate::native_frame::UnsupportedReason::PlatformNotImplemented,
            );
        self.capabilities.reason =
            "WkWebViewProducer slice B: capture stopped; reverting to overlay surface.";
        if let Ok(mut p) = self.pending_capture.lock() {
            *p = PendingCaptureSlot::Idle;
        }
    }

    /// Walk `SCShareableContent.windows` for the entry whose
    /// `windowID` matches the WKWebView's host window's
    /// `windowNumber`. The first call triggers the **Screen
    /// Recording** privacy prompt.
    fn resolve_target_window(
        &self,
        timeout: Duration,
    ) -> Result<Retained<SCWindow>, WryWebSurfaceError> {
        let host_window =
            self.webview
                .window()
                .ok_or_else(|| WryWebSurfaceError::Platform(
                    "WKWebView is not in a window — start_capture requires the producer's parent NSView to be embedded in an NSWindow".into(),
                ))?;
        let target_window_number = host_window.windowNumber();
        if target_window_number <= 0 {
            return Err(WryWebSurfaceError::Platform(
                "host NSWindow has no valid windowNumber".into(),
            ));
        }

        /// Window-resolution result handed off from the async
        /// completion block (which runs on a dispatch queue chosen by
        /// SCK) to the main thread.
        ///
        /// `Retained<SCWindow>` is not `Send` by default, but the
        /// `SCShareableContent` API hands us a fully-formed object
        /// that the system documents as safe to hold across thread
        /// boundaries — same reasoning as `SendCFRetained` above.
        struct WindowQueryResult {
            matched: Option<Retained<SCWindow>>,
            error: Option<String>,
        }
        // SAFETY: SCWindow is a thread-safe descriptor; the SC framework
        // hands these out via an async API explicitly intended to be
        // consumed off the dispatch queue.
        unsafe impl Send for WindowQueryResult {}

        let signal: Arc<Mutex<Option<WindowQueryResult>>> = Arc::new(Mutex::new(None));
        let target_id = target_window_number as u32;
        {
            let signal = Arc::clone(&signal);
            let block = RcBlock::new(
                move |content: *mut SCShareableContent, err: *mut NSError| {
                    let result = if !err.is_null() {
                        WindowQueryResult {
                            matched: None,
                            error: Some(unsafe {
                                (*err).localizedDescription().to_string()
                            }),
                        }
                    } else if let Some(non_null) = NonNull::new(content) {
                        // SAFETY: SCShareableContent hands us a +0
                        // borrow; retain to extend lifetime past the
                        // callback long enough to walk the windows
                        // array.
                        let content: Option<Retained<SCShareableContent>> =
                            unsafe { Retained::retain(non_null.as_ptr()) };
                        match content {
                            Some(content) => {
                                let windows: Retained<NSArray<SCWindow>> =
                                    unsafe { content.windows() };
                                let mut matched: Option<Retained<SCWindow>> = None;
                                for i in 0..windows.count() {
                                    let window = windows.objectAtIndex(i);
                                    if unsafe { window.windowID() } == target_id {
                                        matched = Some(window);
                                        break;
                                    }
                                }
                                WindowQueryResult { matched, error: None }
                            }
                            None => WindowQueryResult {
                                matched: None,
                                error: Some("Retained<SCShareableContent> failed".into()),
                            },
                        }
                    } else {
                        WindowQueryResult {
                            matched: None,
                            error: Some("SCShareableContent was null".into()),
                        }
                    };
                    if let Ok(mut s) = signal.lock() {
                        *s = Some(result);
                    }
                },
            );
            unsafe {
                SCShareableContent::getShareableContentWithCompletionHandler(&block);
            }
        }

        let result = pump_until(timeout, || {
            signal.lock().ok().and_then(|mut s| s.take())
        })
        .map_err(|()| {
            WryWebSurfaceError::Platform(format!(
                "SCShareableContent did not resolve within {:?} (Screen Recording permission may be denied)",
                timeout
            ))
        })?;

        if let Some(msg) = result.error {
            return Err(WryWebSurfaceError::Platform(format!(
                "SCShareableContent failed: {}",
                msg
            )));
        }
        result.matched.ok_or_else(|| {
            WryWebSurfaceError::Platform(format!(
                "no SCWindow matched the WKWebView's host windowNumber {}",
                target_window_number
            ))
        })
    }

    /// Non-blocking variant of [`Self::capture_cpu_snapshot`].
    ///
    /// Kicks off `takeSnapshotWithConfiguration:completionHandler:`
    /// and returns immediately. The result is buffered in a
    /// most-recent slot drained via [`Self::poll_snapshot`]. Pair
    /// this with [`Self::poll_snapshot`] from a host event-loop
    /// callback (e.g. winit's `window_event` / `about_to_wait`) so
    /// the host can drive snapshot capture without blocking on the
    /// main run loop.
    ///
    /// Multiple calls in flight are allowed; only the most recent
    /// completion is preserved (older completions overwrite each
    /// other before the consumer polls).
    pub fn request_snapshot(&mut self) -> Result<(), WryWebSurfaceError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "request_snapshot must be called on the main thread".into(),
            )
        })?;

        let slot = Arc::clone(&self.pending_snapshot);
        let block = RcBlock::new(move |image: *mut NSImage, err: *mut NSError| {
            let result = if !err.is_null() {
                PendingSnapshot::Failed(unsafe { (*err).localizedDescription().to_string() })
            } else if let Some(non_null) = NonNull::new(image) {
                let retained = unsafe { Retained::retain(non_null.as_ptr()) };
                match retained {
                    Some(image) => PendingSnapshot::Image(SendRetainedNSImage(image)),
                    None => PendingSnapshot::Failed(
                        "Retained::retain returned None for the snapshot NSImage".into(),
                    ),
                }
            } else {
                PendingSnapshot::Failed("WKWebView.takeSnapshot returned nil image".into())
            };
            if let Ok(mut s) = slot.lock() {
                *s = Some(result);
            }
        });

        let snapshot_config = unsafe { WKSnapshotConfiguration::new(mtm) };
        unsafe {
            self.webview.takeSnapshotWithConfiguration_completionHandler(
                Some(&snapshot_config),
                &block,
            );
        }
        Ok(())
    }

    /// Drain the most recently completed [`Self::request_snapshot`]
    /// result. Returns `None` until a snapshot is ready,
    /// `Some(Ok(WryWebSurfaceFrame::CpuRgba {..}))` once decoded, or
    /// `Some(Err(...))` on snapshot or decode failure.
    ///
    /// Decoding is performed lazily here (not in the completion
    /// handler) so the main-thread completion callback finishes fast.
    pub fn poll_snapshot(&mut self) -> Option<Result<WryWebSurfaceFrame, WryWebSurfaceError>> {
        let pending = self.pending_snapshot.lock().ok().and_then(|mut s| s.take())?;
        match pending {
            PendingSnapshot::Failed(msg) => Some(Err(WryWebSurfaceError::Platform(format!(
                "snapshot failed: {msg}"
            )))),
            PendingSnapshot::Image(SendRetainedNSImage(ns_image)) => {
                Some(self.decode_ns_image(&ns_image))
            }
        }
    }

    fn decode_ns_image(
        &mut self,
        ns_image: &NSImage,
    ) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        let tiff_data = ns_image.TIFFRepresentation().ok_or_else(|| {
            WryWebSurfaceError::Platform("NSImage has no TIFF representation".into())
        })?;
        let tiff_bytes = tiff_data.to_vec();
        let rgba = image::load_from_memory_with_format(&tiff_bytes, image::ImageFormat::Tiff)
            .map_err(|e| {
                WryWebSurfaceError::Platform(format!("failed to decode TIFF snapshot: {e}"))
            })?
            .to_rgba8();
        let size = PhysicalSize::new(rgba.width(), rgba.height());
        let generation = self.snapshot_generation;
        self.snapshot_generation = self.snapshot_generation.wrapping_add(1);
        Ok(WryWebSurfaceFrame::CpuRgba {
            size,
            pixels: rgba,
            generation,
        })
    }

    /// Acquire a content-pixel snapshot via
    /// `WKWebView.takeSnapshotWithConfiguration:completionHandler:` and
    /// decode it into an `image::RgbaImage`.
    ///
    /// Independent of [`Self::start_capture`] — works whether or not
    /// the ScreenCaptureKit pipeline has been started, and does not
    /// require the Screen Recording privacy permission. Useful for
    /// thumbnails, layout-debug captures, and verifying the WebView is
    /// actually rendering before standing up the SCK path.
    ///
    /// # ⚠️ Blocking — host-event-loop hazard
    ///
    /// Pumps the main `NSRunLoop` until the snapshot's completion
    /// handler fires or `config.frame_timeout` elapses. **Do not
    /// call from inside a host event-loop callback** (winit
    /// `resumed` / `window_event`) — the pump re-enters the host's
    /// dispatch and panics. Prefer the non-blocking
    /// [`Self::request_snapshot`] / [`Self::poll_snapshot`] pair
    /// from event-loop contexts.
    pub fn capture_cpu_snapshot(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "capture_cpu_snapshot must be called on the main thread".into(),
            )
        })?;

        struct SnapshotResult {
            image: Option<Retained<NSImage>>,
            error: Option<String>,
        }
        // SAFETY: `Retained<NSImage>` is not auto-`Send`, but
        // `WKWebView.takeSnapshot` documents that the completion
        // handler runs on the main thread — same thread that
        // observes the slot — so the cross-thread `Send` is satisfied
        // trivially. The `unsafe impl` is for clippy's benefit.
        unsafe impl Send for SnapshotResult {}

        let timeout = self.config.frame_timeout;
        let signal: Arc<Mutex<Option<SnapshotResult>>> = Arc::new(Mutex::new(None));
        {
            let signal = Arc::clone(&signal);
            let block = RcBlock::new(move |image: *mut NSImage, err: *mut NSError| {
                let result = if !err.is_null() {
                    SnapshotResult {
                        image: None,
                        error: Some(unsafe { (*err).localizedDescription().to_string() }),
                    }
                } else if let Some(non_null) = NonNull::new(image) {
                    // SAFETY: `takeSnapshotWithConfiguration:` returns
                    // a +0 borrow that's valid for the duration of
                    // this callback. Retain to keep the NSImage
                    // alive until we read its TIFF representation.
                    let retained = unsafe { Retained::retain(non_null.as_ptr()) };
                    SnapshotResult {
                        image: retained,
                        error: None,
                    }
                } else {
                    SnapshotResult {
                        image: None,
                        error: Some("WKWebView.takeSnapshot returned nil image".into()),
                    }
                };
                if let Ok(mut s) = signal.lock() {
                    *s = Some(result);
                }
            });
            let snapshot_config = unsafe { WKSnapshotConfiguration::new(mtm) };
            unsafe {
                self.webview.takeSnapshotWithConfiguration_completionHandler(
                    Some(&snapshot_config),
                    &block,
                );
            }
        }

        let result = pump_until(timeout, || signal.lock().ok().and_then(|mut s| s.take()))
            .map_err(|()| {
                WryWebSurfaceError::Platform(format!(
                    "takeSnapshot did not resolve within {:?}",
                    timeout
                ))
            })?;

        if let Some(msg) = result.error {
            return Err(WryWebSurfaceError::Platform(format!(
                "takeSnapshot failed: {msg}"
            )));
        }
        let ns_image = result
            .image
            .ok_or_else(|| WryWebSurfaceError::Platform("snapshot returned no image".into()))?;
        let tiff_data = ns_image.TIFFRepresentation().ok_or_else(|| {
            WryWebSurfaceError::Platform("NSImage has no TIFF representation".into())
        })?;
        let tiff_bytes = tiff_data.to_vec();
        let rgba = image::load_from_memory_with_format(&tiff_bytes, image::ImageFormat::Tiff)
            .map_err(|e| {
                WryWebSurfaceError::Platform(format!("failed to decode TIFF snapshot: {e}"))
            })?
            .to_rgba8();

        let size = PhysicalSize::new(rgba.width(), rgba.height());
        let generation = self.snapshot_generation;
        self.snapshot_generation = self.snapshot_generation.wrapping_add(1);
        Ok(WryWebSurfaceFrame::CpuRgba {
            size,
            pixels: rgba,
            generation,
        })
    }

    /// Non-blocking acquire. Returns:
    /// - `Ok(Some(WryWebSurfaceFrame::Native(...)))` if the
    ///   ScreenCaptureKit pipeline has produced a new sample since the
    ///   last call.
    /// - `Ok(None)` if no sample is currently waiting (or capture has
    ///   not been started — in that case the caller should fall back
    ///   to overlay-only rendering via [`Self::acquire_frame`]).
    /// - `Err(...)` if the stream has reported a fatal error since the
    ///   last call.
    pub fn try_acquire_frame(
        &mut self,
    ) -> Result<Option<WryWebSurfaceFrame>, WryWebSurfaceError> {
        let Some(capture) = self.capture.as_mut() else {
            return Ok(None);
        };

        if let Ok(mut slot) = capture.stream_error.lock()
            && let Some(msg) = slot.take()
        {
            return Err(WryWebSurfaceError::Platform(format!(
                "SCStream stopped with error: {}",
                msg
            )));
        }

        let sample = match capture.latest.lock() {
            Ok(mut slot) => match slot.take() {
                Some(SendCFRetained(buffer)) => buffer,
                None => return Ok(None),
            },
            Err(_) => {
                return Err(WryWebSurfaceError::Platform(
                    "latest-sample lock poisoned".into(),
                ))
            }
        };

        let image_buffer = unsafe { sample.image_buffer() }.ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "CMSampleBuffer carried no CVImageBuffer (was the SCStream pixel format set?)"
                    .into(),
            )
        })?;
        // CVPixelBuffer is a type alias for CVImageBuffer in objc2-core-video,
        // so the SCK screen sample buffer can be used directly here without a
        // downcast.
        let pixel_buffer: &CVPixelBuffer = &image_buffer;

        let iosurface = CVPixelBufferGetIOSurface(Some(pixel_buffer)).ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "CVPixelBuffer was not IOSurface-backed (configure SCStreamConfiguration to BGRA)"
                    .into(),
            )
        })?;

        let width = CVPixelBufferGetWidth(pixel_buffer);
        let height = CVPixelBufferGetHeight(pixel_buffer);

        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width,
                height,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        descriptor.setStorageMode(MTLStorageMode::Shared);

        let texture: Retained<ProtocolObject<dyn MTLTexture>> = capture
            .metal_device
            .newTextureWithDescriptor_iosurface_plane(&descriptor, &iosurface, 0)
            .ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "MTLDevice::newTextureWithDescriptor:iosurface:plane: returned nil".into(),
            )
        })?;

        let raw_metal_texture =
            Retained::as_ptr(&texture) as *mut std::ffi::c_void;
        let frame = NativeMetalTextureRef {
            size: PhysicalSize::new(width as u32, height as u32),
            format: wgpu::TextureFormat::Bgra8Unorm,
            generation: capture.generation.fetch_add(1, Ordering::Relaxed),
            // IOSurface coherence is implicit on Apple silicon; the
            // explicit `MTLSharedEvent` upgrade path is documented in
            // `design_docs/2026-05-07_platform_ceilings.md` and not
            // wired in slice B.
            producer_sync: SyncMechanism::None,
            raw_metal_texture,
        };

        // Keep the texture alive past this function. The consumer's
        // importer (`scrying::native_frame::metal::import`) will retain
        // its own reference before our `last_emitted` is overwritten on
        // the next `try_acquire_frame`, so consumers must consume each
        // frame before requesting the next.
        capture.last_emitted = Some(texture);

        Ok(Some(WryWebSurfaceFrame::Native(NativeFrame::MetalTextureRef(frame))))
    }

    fn current_backing_scale(&self) -> objc2_core_foundation::CGFloat {
        backing_scale_for(&self.parent_view)
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
    fn observe_cursor_change(&mut self) {
        let _mtm = self.mtm;
        let shape = current_cursor_shape();
        if self.last_cursor_shape.as_ref() != Some(&shape) {
            self.cursor_shapes.push_back(shape.clone());
            self.last_cursor_shape = Some(shape);
        }
    }

    /// Clear the navigation-result slot before kicking off a new load.
    /// The `events` queue is *not* cleared — consumers may still want
    /// to drain pending events from a prior navigation.
    fn reset_nav_result(&self) -> Result<(), WryWebSurfaceError> {
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
    fn wait_for_nav_completion(
        &self,
        timeout: std::time::Duration,
        op_name: &'static str,
    ) -> Result<(), WryWebSurfaceError> {
        pump_until(timeout, || {
            let state = self.nav_state.lock().ok()?;
            state.result.clone()
        })
        .map_err(|_| {
            WryWebSurfaceError::Platform(format!("{op_name} timed out after {timeout:?}"))
        })?
        .map_err(WryWebSurfaceError::Platform)
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
        // `webview`, `parent_view`, and `_nav_delegate` are released
        // by their own `Retained` Drop impls.
        let _ = self.mtm;
    }
}

impl WryWebSurfaceProducer for WkWebViewProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        if self.capture.is_none() {
            // Slice-A behavior: capture has not been started, so the
            // WKWebView remains a platform overlay child and the
            // consumer composites it itself.
            return Ok(WryWebSurfaceFrame::OverlayOnly);
        }

        // Capture is live: pump the run loop up to `frame_timeout`
        // waiting for the next sample buffer.
        let timeout = self.config.frame_timeout;
        let start = Instant::now();
        let run_loop = NSRunLoop::currentRunLoop();
        loop {
            if let Some(frame) = self.try_acquire_frame()? {
                return Ok(frame);
            }
            if start.elapsed() >= timeout {
                return Err(WryWebSurfaceError::NotReady(
                    "no SCStream sample arrived within frame_timeout",
                ));
            }
            let until = NSDate::dateWithTimeIntervalSinceNow(0.008);
            let _ = run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
        }
    }

    fn navigate_to_string(
        &mut self,
        html: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "navigate_to_string must be called on the main thread".into(),
            ));
        }

        self.reset_nav_result()?;

        let html_ns = NSString::from_str(html);
        unsafe {
            self.webview.loadHTMLString_baseURL(&html_ns, None);
        }

        self.wait_for_nav_completion(timeout, "navigate_to_string")
    }

    fn navigate_to_url(
        &mut self,
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "navigate_to_url must be called on the main thread".into(),
            ));
        }

        let url_ns = NSString::from_str(url);
        let ns_url = NSURL::URLWithString(&url_ns).ok_or_else(|| {
            WryWebSurfaceError::Platform(format!("could not parse URL: {url}"))
        })?;
        let request = NSURLRequest::requestWithURL(&ns_url);

        self.reset_nav_result()?;
        unsafe {
            self.webview.loadRequest(&request);
        }
        self.wait_for_nav_completion(timeout, "navigate_to_url")
    }

    fn move_focus(&mut self, _reason: FocusReason) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "move_focus must be called on the main thread".into(),
            ));
        }
        let window = self.webview.window().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "WKWebView is not in a window — move_focus requires the parent NSView to be embedded in an NSWindow".into(),
            )
        })?;
        // WKWebView ↘ NSView ↘ NSResponder. AppKit's `FocusReason`
        // distinctions (Programmatic / Next / Previous) don't have a
        // direct analog on macOS — `selectKeyViewFollowingView:` /
        // `selectPreviousKeyView:` exist but Cocoa's keyloop is wired
        // in the responder chain, not via WKWebView. Slice C treats
        // every `FocusReason` as a programmatic focus move and lets
        // the host handle keyloop separately.
        let made_first = window.makeFirstResponder(Some(&self.webview));
        if !made_first {
            return Err(WryWebSurfaceError::Platform(
                "NSWindow rejected makeFirstResponder for the WKWebView".into(),
            ));
        }
        Ok(())
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        self.nav_state
            .lock()
            .ok()
            .and_then(|mut state| state.events.pop_front())
    }

    fn post_web_message(&mut self, message: &str) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "post_web_message must be called on the main thread".into(),
            ));
        }
        // The host script invokes the bridge's private dispatcher,
        // matching what JS-side `window.chrome.webview.addEventListener`
        // listeners observe.
        let js_payload = js_string_literal(message);
        let js_source = format!(
            "if (window.chrome && window.chrome.webview && window.chrome.webview._dispatchHostMessage) {{ window.chrome.webview._dispatchHostMessage({js_payload}); }}"
        );
        let js_ns = NSString::from_str(&js_source);
        unsafe {
            self.webview
                .evaluateJavaScript_completionHandler(&js_ns, None);
        }
        Ok(())
    }

    fn poll_web_message(&mut self) -> Option<String> {
        self.web_messages
            .lock()
            .ok()
            .and_then(|mut q| q.pop_front())
    }

    fn send_keyboard_input(&mut self, event: KeyboardInput) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "send_keyboard_input must be called on the main thread".into(),
            ));
        }
        let window = self.webview.window().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "WKWebView is not in a window — send_keyboard_input requires the parent NSView to be embedded in an NSWindow".into(),
            )
        })?;
        let window_number = window.windowNumber();

        let event_type = match event.kind {
            KeyEventKind::Down => NSEventType::KeyDown,
            KeyEventKind::Up => NSEventType::KeyUp,
            KeyEventKind::ModifiersChanged => NSEventType::FlagsChanged,
        };
        let modifier_flags = key_modifier_flags(event.modifiers);
        let characters = NSString::from_str(&event.characters);
        let characters_ignoring =
            NSString::from_str(&event.characters_ignoring_modifiers);

        // For `flagsChanged:` AppKit ignores the characters fields and
        // relies on `keyCode` + `modifierFlags` — match that behavior.
        // For key events, characters drive WebKit's text input
        // pipeline (and therefore IME composition for non-Latin
        // input — WebKit's `NSTextInputClient` impl reads these
        // fields).
        let ns_event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            event_type,
            NSPoint::new(0.0, 0.0),
            modifier_flags,
            0.0,
            window_number,
            None,
            &characters,
            &characters_ignoring,
            event.is_repeat,
            event.virtual_key_code as u16,
        )
        .ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "NSEvent::keyEventWithType returned nil for the synthesized key event".into(),
            )
        })?;

        match event.kind {
            KeyEventKind::Down => self.webview.keyDown(&ns_event),
            KeyEventKind::Up => self.webview.keyUp(&ns_event),
            KeyEventKind::ModifiersChanged => self.webview.flagsChanged(&ns_event),
        }
        Ok(())
    }

    fn send_mouse_input(&mut self, event: MouseInput) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "send_mouse_input must be called on the main thread".into(),
            ));
        }
        let window = self.webview.window().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "WKWebView is not in a window — send_mouse_input requires the parent NSView to be embedded in an NSWindow".into(),
            )
        })?;

        let dispatch = synthesize_mouse_event(&self.webview, &window, event)?;
        let ns_event = dispatch.event;
        match dispatch.target {
            MouseTarget::MouseDown => self.webview.mouseDown(&ns_event),
            MouseTarget::MouseUp => self.webview.mouseUp(&ns_event),
            MouseTarget::MouseDragged => self.webview.mouseDragged(&ns_event),
            MouseTarget::MouseMoved => self.webview.mouseMoved(&ns_event),
            MouseTarget::RightMouseDown => self.webview.rightMouseDown(&ns_event),
            MouseTarget::RightMouseUp => self.webview.rightMouseUp(&ns_event),
            MouseTarget::RightMouseDragged => self.webview.rightMouseDragged(&ns_event),
            MouseTarget::OtherMouseDown => self.webview.otherMouseDown(&ns_event),
            MouseTarget::OtherMouseUp => self.webview.otherMouseUp(&ns_event),
            MouseTarget::OtherMouseDragged => self.webview.otherMouseDragged(&ns_event),
            MouseTarget::MouseExited => self.webview.mouseExited(&ns_event),
            MouseTarget::ScrollWheel => self.webview.scrollWheel(&ns_event),
        }
        self.observe_cursor_change();
        Ok(())
    }

    fn poll_cursor_shape(&mut self) -> Option<CursorShape> {
        self.cursor_shapes.pop_front()
    }

    /// Drag-and-drop forwarding on macOS.
    ///
    /// **Capture mode (post-`start_capture`)**: not feasible without
    /// platform SPI. `WKWebView` receives drags via the
    /// `NSDraggingDestination` protocol — `draggingEntered:`,
    /// `draggingUpdated:`, `performDragOperation:`, etc. — all of
    /// which require an `NSDraggingInfo` instance, an opaque object
    /// only AppKit's drag manager constructs. There is no public API
    /// to synthesize one. Slice K territory if a downstream consumer
    /// builds an SPI bridge; out of scope for the public surface.
    ///
    /// **Overlay mode (pre-`start_capture`)**: AppKit's drag manager
    /// targets the WKWebView directly through the responder chain —
    /// the host doesn't need to forward anything; WebKit gets the
    /// drag through standard `NSDraggingDestination` handling.
    /// `send_drag_input` is therefore unnecessary in this mode and
    /// returns the same `Unsupported` to make the no-op explicit.
    fn send_drag_input(&mut self, _event: DragInput) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WkWebViewProducer::send_drag_input — capture-mode drag forwarding requires \
             NSDraggingInfo synthesis (SPI); overlay-mode drag works automatically through \
             AppKit's responder chain without producer involvement",
        ))
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "resize must be called on the main thread".into(),
            ));
        }
        let scale = self.current_backing_scale();
        let ns_size = NSSize::new(
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        self.webview.setFrameSize(ns_size);
        self.config.size = size;

        // If a capture is live, push the new pixel dimensions through
        // to the SCStream so the next sample arrives at the requested
        // resolution. `updateConfiguration:completionHandler:` is the
        // documented path; failures are surfaced through
        // `stream_error` from the `SCStreamDelegate` rather than this
        // call's return.
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

    fn set_offset(&mut self, x: f32, y: f32) -> Result<(), WryWebSurfaceError> {
        if MainThreadMarker::new().is_none() {
            return Err(WryWebSurfaceError::Platform(
                "set_offset must be called on the main thread".into(),
            ));
        }
        let scale = self.current_backing_scale();
        let ns_origin = NSPoint::new(f64::from(x) / scale, f64::from(y) / scale);
        self.webview.setFrameOrigin(ns_origin);
        self.config.offset = (x, y);
        Ok(())
    }
}

/// Derive a stable [`NSUUID`] from a profile-storage path so that the
/// same `config.data_dir` always resolves to the same
/// [`WKWebsiteDataStore`] across runs.
///
/// Uses FNV-1a 128 over the path's encoded bytes, then formats as a
/// version-8 UUID string (variant bits `10` to satisfy
/// `NSUUID::initWithUUIDString:` parser strictness). Version-8 is the
/// "custom-content" variant — RFC 9562 — which is the right marker for
/// "these bits are not derived from a recognised algorithm" (we're
/// using FNV-1a, not the SHA-1 / SHA-256 the named UUID versions
/// require).
fn profile_uuid_for_path(path: &Path, _mtm: MainThreadMarker) -> Retained<NSUUID> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut h = profile_uuid_helpers::FNV1A_128_OFFSET;
    for &b in bytes {
        h ^= b as u128;
        h = h.wrapping_mul(profile_uuid_helpers::FNV1A_128_PRIME);
    }
    let mut out = h.to_be_bytes();
    // Set version bits to 8 (custom).
    out[6] = (out[6] & 0x0F) | 0x80;
    // Set variant bits to RFC 9562 ("10xxxxxx").
    out[8] = (out[8] & 0x3F) | 0x80;
    let formatted = format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        out[0], out[1], out[2], out[3],
        out[4], out[5],
        out[6], out[7],
        out[8], out[9],
        out[10], out[11], out[12], out[13], out[14], out[15],
    );
    let ns_string = NSString::from_str(&formatted);
    NSUUID::initWithUUIDString(NSUUID::alloc(), &ns_string)
        .expect("FNV-1a UUID string should always parse as a valid NSUUID")
}

mod profile_uuid_helpers {
    pub(super) const FNV1A_128_OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    pub(super) const FNV1A_128_PRIME: u128 = 0x0000000001000000000000000000013b;
}

/// Build the [`SCStreamConfiguration`] used by both
/// [`WkWebViewProducer::start_capture`] and live resizes.
/// Single source of truth for pixel format / cursor / queue depth so
/// `updateConfiguration:` keeps the non-size parameters consistent
/// with the original `start_capture`.
fn make_stream_configuration(size: PhysicalSize<u32>) -> Retained<SCStreamConfiguration> {
    unsafe {
        let cfg = SCStreamConfiguration::new();
        cfg.setWidth(size.width as usize);
        cfg.setHeight(size.height as usize);
        // 32-bit BGRA — matches `MTLPixelFormat::BGRA8Unorm` and
        // `wgpu::TextureFormat::Bgra8Unorm` so the consumer renders
        // without a format swizzle pass.
        cfg.setPixelFormat(kCVPixelFormatType_32BGRA);
        cfg.setShowsCursor(false);
        // Keep the most recent frame; older frames in flight are
        // OK to drop.
        cfg.setQueueDepth(3);
        cfg
    }
}

/// Backing scale of the screen the parent view is on, falling back to
/// the parent window's `backingScaleFactor`, then to 1.0 for views that
/// haven't been placed in a window yet.
fn backing_scale_for(parent_view: &NSView) -> objc2_core_foundation::CGFloat {
    if let Some(window) = parent_view.window() {
        return window.backingScaleFactor();
    }
    1.0
}

fn ns_rect_from_pixels(
    offset_points: (f32, f32),
    size_pixels: PhysicalSize<u32>,
    backing_scale: objc2_core_foundation::CGFloat,
) -> NSRect {
    let origin = NSPoint::new(f64::from(offset_points.0), f64::from(offset_points.1));
    let size = NSSize::new(
        f64::from(size_pixels.width) / backing_scale,
        f64::from(size_pixels.height) / backing_scale,
    );
    NSRect::new(origin, size)
}

/// Which `NSResponder` method should receive the synthesized event.
#[derive(Clone, Copy)]
enum MouseTarget {
    MouseDown,
    MouseUp,
    MouseDragged,
    MouseMoved,
    RightMouseDown,
    RightMouseUp,
    RightMouseDragged,
    OtherMouseDown,
    OtherMouseUp,
    OtherMouseDragged,
    MouseExited,
    ScrollWheel,
}

struct MouseDispatch {
    event: Retained<NSEvent>,
    target: MouseTarget,
}

/// Translate a `MouseInput` into an `NSEvent` to fire at the WKWebView.
///
/// Coordinates: `event.point` is "physical pixels relative to the
/// WebView's top-left." AppKit needs window-coordinates in points with
/// a bottom-left origin. The conversion is:
///
/// 1. Divide by the parent window's `backingScaleFactor` to get points.
/// 2. Flip Y around the WebView's local height to bottom-left origin.
/// 3. `convertPoint_toView(None)` to lift into window space.
fn synthesize_mouse_event(
    webview: &WKWebView,
    window: &objc2_app_kit::NSWindow,
    event: MouseInput,
) -> Result<MouseDispatch, WryWebSurfaceError> {
    let scale = window.backingScaleFactor().max(1.0);
    let bounds = webview.bounds();
    let x_local = f64::from(event.point.0) / scale;
    let y_local_top_left = f64::from(event.point.1) / scale;
    let y_local_bottom_left = bounds.size.height - y_local_top_left;
    let local_pt = NSPoint::new(x_local, y_local_bottom_left);
    let window_pt = webview.convertPoint_toView(local_pt, None);

    let modifier_flags = modifier_flags_from_virtual_keys(event.virtual_keys);
    let window_number = window.windowNumber();

    let (event_type, target, click_count, button_number, pressure) = match event.kind {
        MouseEventKind::LeftButtonDown => {
            (NSEventType::LeftMouseDown, MouseTarget::MouseDown, 1, 0, 1.0)
        }
        MouseEventKind::LeftButtonUp => {
            (NSEventType::LeftMouseUp, MouseTarget::MouseUp, 1, 0, 0.0)
        }
        MouseEventKind::LeftButtonDoubleClick => {
            (NSEventType::LeftMouseDown, MouseTarget::MouseDown, 2, 0, 1.0)
        }
        MouseEventKind::RightButtonDown => (
            NSEventType::RightMouseDown,
            MouseTarget::RightMouseDown,
            1,
            0,
            1.0,
        ),
        MouseEventKind::RightButtonUp => (
            NSEventType::RightMouseUp,
            MouseTarget::RightMouseUp,
            1,
            0,
            0.0,
        ),
        MouseEventKind::RightButtonDoubleClick => (
            NSEventType::RightMouseDown,
            MouseTarget::RightMouseDown,
            2,
            0,
            1.0,
        ),
        MouseEventKind::MiddleButtonDown => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            1,
            2,
            1.0,
        ),
        MouseEventKind::MiddleButtonUp => (
            NSEventType::OtherMouseUp,
            MouseTarget::OtherMouseUp,
            1,
            2,
            0.0,
        ),
        MouseEventKind::MiddleButtonDoubleClick => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            2,
            2,
            1.0,
        ),
        MouseEventKind::XButtonDown => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            1,
            event.mouse_data.max(3),
            1.0,
        ),
        MouseEventKind::XButtonUp => (
            NSEventType::OtherMouseUp,
            MouseTarget::OtherMouseUp,
            1,
            event.mouse_data.max(3),
            0.0,
        ),
        MouseEventKind::XButtonDoubleClick => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            2,
            event.mouse_data.max(3),
            1.0,
        ),
        MouseEventKind::Move => {
            // If a button is held, AppKit reports a `*MouseDragged`
            // event instead of `MouseMoved`. Match that — WKWebView
            // gates pointer-move handling on this distinction.
            if event.virtual_keys.left_button {
                (
                    NSEventType::LeftMouseDragged,
                    MouseTarget::MouseDragged,
                    0,
                    0,
                    0.0,
                )
            } else if event.virtual_keys.right_button {
                (
                    NSEventType::RightMouseDragged,
                    MouseTarget::RightMouseDragged,
                    0,
                    0,
                    0.0,
                )
            } else if event.virtual_keys.middle_button {
                (
                    NSEventType::OtherMouseDragged,
                    MouseTarget::OtherMouseDragged,
                    0,
                    2,
                    0.0,
                )
            } else {
                (NSEventType::MouseMoved, MouseTarget::MouseMoved, 0, 0, 0.0)
            }
        }
        MouseEventKind::Leave => {
            (NSEventType::MouseExited, MouseTarget::MouseExited, 0, 0, 0.0)
        }
        MouseEventKind::Wheel | MouseEventKind::HorizontalWheel => {
            // Scroll wheel events have no `NSEvent` factory — build a
            // CGEvent and round-trip through `eventWithCGEvent:`.
            return synthesize_scroll_wheel_event(event);
        }
    };

    // `NSEvent::mouseEventWithType:` does not expose `buttonNumber`
    // directly — Apple infers the button from the event type. So
    // X-button slots and middle-vs-other-mouse distinctions ride on
    // the kind of event we synthesize, not on `mouse_data`. Setting
    // a synthetic per-button `buttonNumber` (so JS observes
    // `event.button == 3/4` for X-buttons) requires the CGEvent path
    // and is deferred along with scroll-wheel.
    let _ = button_number;

    let ns_event = if matches!(event_type, NSEventType::MouseExited) {
        // SAFETY: `userData` is allowed to be null when no tracking
        // area is associated with the synthesized event.
        unsafe {
            NSEvent::enterExitEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_trackingNumber_userData(
                event_type,
                window_pt,
                modifier_flags,
                0.0,
                window_number,
                None,
                0,
                0,
                std::ptr::null_mut(),
            )
        }
    } else {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            event_type,
            window_pt,
            modifier_flags,
            0.0,
            window_number,
            None,
            0,
            click_count,
            pressure,
        )
    };

    let ns_event = ns_event.ok_or_else(|| {
        WryWebSurfaceError::Platform(
            "NSEvent factory returned nil for the synthesized mouse event".into(),
        )
    })?;

    Ok(MouseDispatch {
        event: ns_event,
        target,
    })
}

/// Encode a Rust string as a JSON-style JavaScript string literal,
/// including the surrounding quotes. The output is safe to splice into
/// a JS expression — control characters, quotes, backslashes, and the
/// problematic `U+2028`/`U+2029` line separators are all escaped.
fn js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            // U+2028 / U+2029 are valid in JSON strings but break JS
            // because they're treated as line terminators outside
            // string context. Escape both so the literal works in
            // either parser.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build a synthetic ScrollWheel `NSEvent` via `CGEventCreateScrollWheelEvent2`.
///
/// `event.mouse_data` carries the wheel delta. Sign convention matches
/// AppKit: positive = up / right (content scrolls toward larger
/// scrollOffset). Pixel units (not lines) so the consumer's host-side
/// scroll-amount accounting maps directly to pixel deltas.
///
/// The event's location does *not* drive WebKit's hit-testing in this
/// dispatch path — we deliver synthesized events straight to
/// `webview.scrollWheel:`, which applies the scroll to the WebView
/// regardless of where the cursor would have been. Future slice could
/// thread `event.point` through `CGEventSetLocation` if a downstream
/// consumer needs DOM-level hover-target precision.
fn synthesize_scroll_wheel_event(
    event: MouseInput,
) -> Result<MouseDispatch, WryWebSurfaceError> {
    let (wheel_count, wheel1, wheel2) = match event.kind {
        MouseEventKind::Wheel => (1u32, event.mouse_data, 0i32),
        MouseEventKind::HorizontalWheel => (2u32, 0i32, event.mouse_data),
        _ => unreachable!("synthesize_scroll_wheel_event called with non-wheel kind"),
    };
    let cg_event = CGEvent::new_scroll_wheel_event2(
        None,
        CGScrollEventUnit::Pixel,
        wheel_count,
        wheel1,
        wheel2,
        0,
    )
    .ok_or_else(|| {
        WryWebSurfaceError::Platform(
            "CGEventCreateScrollWheelEvent2 returned nil".into(),
        )
    })?;
    let ns_event = NSEvent::eventWithCGEvent(&cg_event).ok_or_else(|| {
        WryWebSurfaceError::Platform("NSEvent::eventWithCGEvent returned nil".into())
    })?;
    Ok(MouseDispatch {
        event: ns_event,
        target: MouseTarget::ScrollWheel,
    })
}

fn modifier_flags_from_virtual_keys(keys: MouseVirtualKeys) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    if keys.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if keys.control {
        flags |= NSEventModifierFlags::Control;
    }
    flags
}

/// Translate `NSCursor.currentSystemCursor` to a [`CursorShape`].
///
/// macOS exposes built-in cursors as singleton instances —
/// `NSCursor.iBeamCursor()` returns the same object on every call —
/// so we compare retained pointers via `Eq`. Any cursor we don't
/// recognize falls through to [`CursorShape::Default`]; future slices
/// could plumb `image()` + `name` to surface custom cursors via the
/// [`CursorShape::Custom`] variant.
///
/// `currentSystemCursor` and the `resizeUpDown` / `resizeLeftRight`
/// singletons are deprecated in macOS 15+ in favor of
/// per-direction frame-resize variants. The deprecated forms still
/// return the canonical singletons we need for pointer-comparison
/// fingerprinting; switching to the new APIs (which take direction
/// vectors) would force us to enumerate every direction-tuple
/// permutation. Allow the deprecation here.
#[allow(deprecated)]
fn current_cursor_shape() -> CursorShape {
    let Some(current) = NSCursor::currentSystemCursor() else {
        return CursorShape::Default;
    };
    let candidates: [(CursorShape, Retained<NSCursor>); 13] = [
        (CursorShape::Default, NSCursor::arrowCursor()),
        (CursorShape::Text, NSCursor::IBeamCursor()),
        (CursorShape::Pointer, NSCursor::pointingHandCursor()),
        (CursorShape::Crosshair, NSCursor::crosshairCursor()),
        (CursorShape::Grab, NSCursor::openHandCursor()),
        (CursorShape::Grabbing, NSCursor::closedHandCursor()),
        (CursorShape::NotAllowed, NSCursor::operationNotAllowedCursor()),
        (CursorShape::Help, NSCursor::contextualMenuCursor()),
        (CursorShape::ResizeNs, NSCursor::resizeUpDownCursor()),
        (CursorShape::ResizeEw, NSCursor::resizeLeftRightCursor()),
        (CursorShape::Move, NSCursor::dragCopyCursor()),
        (CursorShape::Pointer, NSCursor::dragLinkCursor()),
        (CursorShape::Wait, NSCursor::disappearingItemCursor()),
    ];
    for (shape, candidate) in &candidates {
        if &*current == candidate.as_ref() {
            return shape.clone();
        }
    }
    CursorShape::Default
}

fn key_modifier_flags(keys: crate::KeyModifierFlags) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    if keys.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if keys.control {
        flags |= NSEventModifierFlags::Control;
    }
    if keys.alt {
        flags |= NSEventModifierFlags::Option;
    }
    if keys.meta {
        flags |= NSEventModifierFlags::Command;
    }
    if keys.caps_lock {
        flags |= NSEventModifierFlags::CapsLock;
    }
    flags
}

/// Pump the main run loop in 16ms slices until `predicate` returns
/// `Some(value)` or `timeout` elapses.
///
/// Returns `Ok(value)` on resolution, `Err(())` on timeout.
fn pump_until<T>(
    timeout: std::time::Duration,
    mut predicate: impl FnMut() -> Option<T>,
) -> Result<T, ()> {
    let start = Instant::now();
    let run_loop = NSRunLoop::currentRunLoop();
    loop {
        if let Some(value) = predicate() {
            return Ok(value);
        }
        if start.elapsed() >= timeout {
            return Err(());
        }
        let until = NSDate::dateWithTimeIntervalSinceNow(0.016);
        // Returning `false` means no input source fired in this
        // slice; that's fine — we'll loop again until the predicate
        // resolves or timeout elapses.
        let _ = run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
    }
}

#[cfg(test)]
mod tests {
    use super::js_string_literal;

    #[test]
    fn js_string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(js_string_literal("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn js_string_literal_escapes_short_form_control_chars() {
        assert_eq!(
            js_string_literal("a\nb\tc\rd"),
            "\"a\\nb\\tc\\rd\""
        );
    }

    #[test]
    fn js_string_literal_escapes_generic_control_chars() {
        // BEL (U+0007) has no short escape; falls through to the
        // generic \\uXXXX form.
        assert_eq!(js_string_literal("\x07"), "\"\\u0007\"");
    }

    #[test]
    fn js_string_literal_escapes_line_separators() {
        // U+2028 is a JS-killer if left unescaped — JS treats it as a
        // line terminator outside string context, which causes
        // eval() and new Function() to bail.
        assert_eq!(js_string_literal("\u{2028}"), "\"\\u2028\"");
        assert_eq!(js_string_literal("\u{2029}"), "\"\\u2029\"");
    }

    #[test]
    fn js_string_literal_passes_unicode() {
        // Non-control non-line-separator Unicode flows through
        // verbatim — JS source files are UTF-8 and arbitrary BMP /
        // SMP code points are valid in string literals.
        assert_eq!(js_string_literal("héllo 🦀"), "\"héllo 🦀\"");
    }
}
