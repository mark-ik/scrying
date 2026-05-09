//! ScreenCaptureKit pipeline state, delegates, and the lazy
//! `stop_capture` teardown method. Lifecycle entry points
//! ([`super::WkWebViewProducer::start_capture`] and
//! [`super::WkWebViewProducer::start_capture_async`]) live in the
//! `blocking` / `async_start` siblings.

use std::ptr::NonNull;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use dispatch2::{DispatchQueue, DispatchRetained};
use dpi::PhysicalSize;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_core_foundation::CFRetained;
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::kCVPixelFormatType_32BGRA;
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_metal::MTLDevice;
use objc2_screen_capture_kit::{
    SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};

use crate::WebSurfaceMode;

use super::producer::WkWebViewProducer;

mod async_start;
mod blocking;

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
pub(super) struct SendCFRetained<T>(pub(super) CFRetained<T>);
// SAFETY: see `SendCFRetained` doc.
unsafe impl<T> Send for SendCFRetained<T> {}

/// Latest screen-capture sample handed off from the
/// `SCStreamOutput::stream:didOutputSampleBuffer:ofType:` callback
/// (which fires on a background dispatch queue) to `try_acquire_frame`
/// on the main thread. Only the most recent sample is kept; older
/// samples are dropped on overwrite.
pub(super) type LatestSample = Mutex<Option<SendCFRetained<CMSampleBuffer>>>;

#[derive(Default)]
pub(super) struct CaptureSignal {
    /// `Some(Ok(()))` once `startCaptureWithCompletionHandler:` /
    /// `stopCaptureWithCompletionHandler:` resolves, `Some(Err(msg))`
    /// on error, `None` while pending.
    pub(super) result: Option<Result<(), String>>,
}

define_class!(
    // SAFETY:
    // - The superclass NSObject has no subclassing requirements.
    // - `StreamOutputDelegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[ivars = Arc<LatestSample>]
    pub(super) struct StreamOutputDelegate;

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
    pub(super) fn new(latest: Arc<LatestSample>) -> Retained<Self> {
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
    pub(super) struct StreamErrorDelegate;

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
    pub(super) fn new(error_slot: Arc<Mutex<Option<String>>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(error_slot);
        unsafe { msg_send![super(this), init] }
    }
}

/// Cross-thread observable status of the ScreenCaptureKit pipeline,
/// reported by [`super::WkWebViewProducer::capture_status`] so
/// non-blocking consumers (e.g. winit hosts) can poll instead of
/// blocking on the main run loop.
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
pub(super) enum PendingCaptureSlot {
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
pub(super) struct SendOnly<T>(pub(super) T);
// SAFETY: see `SendOnly` doc.
unsafe impl<T> Send for SendOnly<T> {}

/// Captured-by-block bag of all the SCK pieces the inner
/// `startCaptureWithCompletionHandler:` block needs to assemble a
/// [`CaptureState`] when the stream goes live.
pub(super) struct InProgressCaptureState {
    pub(super) metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub(super) stream: Retained<SCStream>,
    pub(super) output: Retained<StreamOutputDelegate>,
    pub(super) error_delegate: Retained<StreamErrorDelegate>,
    pub(super) sample_queue: DispatchRetained<DispatchQueue>,
    pub(super) latest: Arc<LatestSample>,
    pub(super) stream_error: Arc<Mutex<Option<String>>>,
}

/// Helper used by SCK completion blocks to update the shared
/// [`PendingCaptureSlot`]. Lock-poisoning failures are silently
/// dropped because there's no useful recovery path from a callback —
/// the next [`super::WkWebViewProducer::capture_status`] poll will
/// surface the prior state (or `Failed` if a poisoned lock makes
/// things inconsistent).
pub(super) fn write_pending(
    pending: &Arc<Mutex<PendingCaptureSlot>>,
    state: PendingCaptureSlot,
) {
    if let Ok(mut s) = pending.lock() {
        *s = state;
    }
}

/// State held while ScreenCaptureKit is actively streaming.
pub(super) struct CaptureState {
    /// Strong reference to the host wgpu device's `MTLDevice`. Used to
    /// allocate IOSurface-backed `MTLTexture`s on the same device the
    /// consumer renders against (no cross-device migration).
    pub(super) metal_device: Retained<ProtocolObject<dyn MTLDevice>>,
    pub(super) stream: Retained<SCStream>,
    pub(super) output: Retained<StreamOutputDelegate>,
    pub(super) _error_delegate: Retained<StreamErrorDelegate>,
    pub(super) _sample_queue: DispatchRetained<DispatchQueue>,
    pub(super) latest: Arc<LatestSample>,
    /// Surfaced via [`StreamErrorDelegate`] when the stream stops
    /// unexpectedly (e.g., capture target window closed). Inspected
    /// from `try_acquire_frame` so the consumer learns the stream is
    /// dead.
    pub(super) stream_error: Arc<Mutex<Option<String>>>,
    /// Most-recently-emitted MTLTexture. The producer keeps it alive
    /// here because [`crate::native_frame::MetalTextureRef::raw_metal_texture`]
    /// is a raw pointer; the consumer's [`crate::native_frame`]
    /// importer re-retains the object during import. Replaced on
    /// each successful `try_acquire_frame`.
    pub(super) last_emitted: Option<Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>>>,
    pub(super) generation: AtomicU64,
}

/// Build the [`SCStreamConfiguration`] used by both
/// [`super::WkWebViewProducer::start_capture`] and live resizes.
/// Single source of truth for pixel format / cursor / queue depth /
/// source-rect-cropping so `updateConfiguration:` keeps the non-size
/// parameters consistent with the original `start_capture`.
///
/// `source_rect`, when `Some`, is the rect within the captured
/// window — in points, top-left origin — that SCK should sample
/// from. Without this, an `initWithDesktopIndependentWindow`
/// filter captures the *entire* host window: every pixel of host
/// chrome around the WKWebView, plus (for a host that re-renders
/// the captured texture into the same window) recursively
/// captured frames.
///
/// Compute via [`webview_window_rect`] and pass through
/// `start_capture` / `start_capture_async` / `resize_internal`.
pub(super) fn make_stream_configuration(
    size: PhysicalSize<u32>,
    source_rect: Option<objc2_core_foundation::CGRect>,
) -> Retained<SCStreamConfiguration> {
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
        if let Some(rect) = source_rect {
            cfg.setSourceRect(rect);
        }
        cfg
    }
}

/// Compute the WKWebView's rect within its host window, in
/// **points** with **top-left origin** — the coordinate system
/// SCK's `setSourceRect:` expects for window-bound streams.
///
/// AppKit's `convertRect_toView(.., None)` lifts the webview's
/// `bounds` into window coords (bottom-left origin). We then flip
/// Y against the window's content-view height so the rect aligns
/// with SCK's top-left convention.
pub(super) fn webview_window_rect(
    webview: &objc2_web_kit::WKWebView,
    window: &objc2_app_kit::NSWindow,
) -> objc2_core_foundation::CGRect {
    let local_bounds = webview.bounds();
    let window_pt_rect =
        webview.convertRect_toView(local_bounds, None);
    let content_height = window
        .contentView()
        .map(|cv| cv.frame().size.height)
        .unwrap_or_else(|| window.frame().size.height);
    objc2_core_foundation::CGRect {
        origin: objc2_core_foundation::CGPoint {
            x: window_pt_rect.origin.x,
            y: content_height - window_pt_rect.origin.y - window_pt_rect.size.height,
        },
        size: objc2_core_foundation::CGSize {
            width: window_pt_rect.size.width,
            height: window_pt_rect.size.height,
        },
    }
}

impl WkWebViewProducer {
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
}
