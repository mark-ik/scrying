//! Non-blocking start variant: kicks off the
//! `SCShareableContent` → `SCContentFilter` → `SCStream` chain via
//! completion blocks and returns immediately. The consumer polls
//! [`super::super::WkWebViewProducer::capture_status`] (typically
//! each frame from a host event-loop callback) to observe progression
//! and to install the `CaptureState` into the producer once the
//! stream is live.

use std::ptr::NonNull;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::AnyThread;
use objc2_foundation::{MainThreadMarker, NSArray, NSError};
use objc2_metal::MTLDevice;
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamOutputType, SCWindow,
};

use crate::{HostWgpuContext, InteropBackend, WebSurfaceMode, WryWebSurfaceError};

use super::super::producer::WkWebViewProducer;
use super::{
    host_window_pixel_size, make_stream_configuration, write_pending, CaptureMetrics,
    CaptureState, CaptureStatus, InProgressCaptureState, LatestSample, PendingCaptureSlot,
    SendOnly, StreamErrorDelegate, StreamOutputDelegate,
};

impl WkWebViewProducer {
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
        // Capture at native window pixel resolution; per-frame
        // crop happens in `try_acquire_frame`. WKWebView /
        // NSWindow are MainThreadOnly so we compute this on the
        // main thread now, before the SCK completion block
        // (which fires on a background queue).
        let window_pixel_size = host_window_pixel_size(&host_window);

        // Allocate the command queue for the per-frame blit on
        // the main thread; we ferry it across to the SCK
        // completion via `SendOnly`.
        let command_queue = metal_device.newCommandQueue().ok_or_else(|| {
            WryWebSurfaceError::Platform(
                "MTLDevice::newCommandQueue returned nil".into(),
            )
        })?;

        *self
            .pending_capture
            .lock()
            .map_err(|_| {
                WryWebSurfaceError::Platform("pending_capture lock poisoned".into())
            })? = PendingCaptureSlot::Starting;

        let pending = Arc::clone(&self.pending_capture);
        let metal_device_for_block = SendOnly(metal_device);
        let command_queue_for_block = SendOnly(command_queue);

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
                let stream_config = make_stream_configuration(window_pixel_size);
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
                let samples_received = Arc::new(AtomicU64::new(0));
                let samples_consumed = Arc::new(AtomicU64::new(0));
                let output_delegate = StreamOutputDelegate::new(
                    Arc::clone(&latest),
                    Arc::clone(&samples_received),
                );
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
                let command_queue = command_queue_for_block.0.clone();
                let pending_inner = Arc::clone(&pending);
                let in_progress = SendOnly(InProgressCaptureState {
                    metal_device,
                    command_queue,
                    stream: stream.clone(),
                    output: output_delegate.clone(),
                    error_delegate: error_delegate.clone(),
                    sample_queue: sample_queue.clone(),
                    latest: Arc::clone(&latest),
                    stream_error: Arc::clone(&stream_error),
                    samples_received: Arc::clone(&samples_received),
                    samples_consumed: Arc::clone(&samples_consumed),
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
                        command_queue: parts.command_queue.clone(),
                        stream: parts.stream.clone(),
                        output: parts.output.clone(),
                        _error_delegate: parts.error_delegate.clone(),
                        _sample_queue: parts.sample_queue.clone(),
                        latest: Arc::clone(&parts.latest),
                        stream_error: Arc::clone(&parts.stream_error),
                        samples_received: Arc::clone(&parts.samples_received),
                        samples_consumed: Arc::clone(&parts.samples_consumed),
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

    /// Snapshot the live ScreenCaptureKit pipeline counters. Returns
    /// [`CaptureMetrics::default`] (all zeros) if no capture is
    /// active. The two counters are atomic and safe to call from any
    /// thread, but in practice consumers poll from the same main
    /// thread that drives [`Self::try_acquire_frame`].
    ///
    /// Useful for diagnosing the SCK push cadence: SCK delivers when
    /// the captured window recomposites and aggressively throttles
    /// on a static page, so a low `samples_received` rate is not a
    /// scrying bug but an Apple-side energy optimisation.
    pub fn capture_metrics(&self) -> CaptureMetrics {
        let Some(capture) = self.capture.as_ref() else {
            return CaptureMetrics::default();
        };
        CaptureMetrics {
            samples_received: capture
                .samples_received
                .load(std::sync::atomic::Ordering::Relaxed),
            samples_consumed: capture
                .samples_consumed
                .load(std::sync::atomic::Ordering::Relaxed),
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
}
