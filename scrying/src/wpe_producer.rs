//! Linux WPE producer scaffold.
//!
//! This is the primary planned Linux path: WPE renders the webview into
//! DMABUF-backed buffers, then scrying imports those buffers through wgpu's
//! Vulkan backend. The public Rust bindings for the DMABUF view backend are
//! not wired here yet, so this module establishes the API/interop shape and
//! keeps the actual WPE symbol work isolated for a Linux implementation pass.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;

use crate::native_frame::{
    CapabilityStatus, DmaBufImage, NativeFrame, NativeFrameKind, SyncMechanism, UnsupportedReason,
};
use crate::{
    SystemWebviewBackend, WebSurfaceCapabilities, WebSurfaceError, WebSurfaceFrame, WebSurfaceMode,
    WebSurfaceProducer,
};

/// Configuration for [`WpeProducer`].
#[derive(Clone, Debug)]
pub struct WpeProducerConfig {
    /// Initial view size in physical pixels.
    pub size: PhysicalSize<u32>,
    /// Offset of the embedded view relative to the host surface, in
    /// device-independent pixels.
    pub offset: (f32, f32),
    /// Directory used for WebKit website data.
    pub data_dir: PathBuf,
    /// Timeout for blocking navigation helpers.
    pub navigation_timeout: std::time::Duration,
    /// Timeout for blocking first-frame helpers.
    pub frame_timeout: std::time::Duration,
}

impl WpeProducerConfig {
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

/// Linux WPE producer shell.
///
/// `view_backend` in [`Self::new`] is expected to be the host-created
/// `struct wpe_view_backend *` once the WPE FFI bridge lands. Keeping that
/// object host-owned matches the rest of scrying's contract: the host owns
/// native embedding and event-loop integration; scrying owns frame production
/// and import contracts.
pub struct WpeProducer {
    capabilities: WebSurfaceCapabilities,
    size: PhysicalSize<u32>,
    offset: (f32, f32),
    pending_frame: Arc<Mutex<Option<DmaBufImage>>>,
    generation: u64,
}

impl WpeProducer {
    /// Construct the WPE producer shell.
    ///
    /// # Safety
    ///
    /// `view_backend` must be a valid `wpe_view_backend *` that outlives the
    /// producer once WPE creation is wired. The current scaffold only checks
    /// for null and stores the requested configuration.
    pub unsafe fn new(
        view_backend: *mut std::ffi::c_void,
        config: WpeProducerConfig,
    ) -> Result<Self, WebSurfaceError> {
        if view_backend.is_null() {
            return Err(WebSurfaceError::Platform(
                "WPE view backend pointer was null".to_string(),
            ));
        }
        if config.size.width == 0 || config.size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WPE producer size must be non-zero, got {}x{}",
                config.size.width, config.size.height
            )));
        }

        Ok(Self {
            capabilities: linux_wpe_capabilities(),
            size: config.size,
            offset: config.offset,
            pending_frame: Arc::new(Mutex::new(None)),
            generation: 0,
        })
    }

    /// Queue a DMABUF frame from the WPE backend callback.
    ///
    /// This is the seam the Linux FFI bridge should call when
    /// `WPEViewBackendDMABuf` exports a fresh buffer. It is public so a Linux
    /// smoke harness can inject a known frame before the real callback bridge
    /// is complete.
    pub fn enqueue_dmabuf_frame(&mut self, mut frame: DmaBufImage) -> Result<(), WebSurfaceError> {
        if frame.size.width == 0 || frame.size.height == 0 {
            return Err(WebSurfaceError::Platform(
                "WPE DMABUF frame size must be non-zero".to_string(),
            ));
        }
        if frame.planes.is_empty() {
            return Err(WebSurfaceError::Platform(
                "WPE DMABUF frame did not include any planes".to_string(),
            ));
        }
        self.generation = self.generation.saturating_add(1);
        frame.generation = self.generation;
        frame.producer_sync = if frame.semaphore_fd.is_some() {
            SyncMechanism::ExplicitExternalSemaphore
        } else {
            SyncMechanism::None
        };
        *self.pending_frame.lock().map_err(|_| {
            WebSurfaceError::Platform("WPE pending frame mutex was poisoned".to_string())
        })? = Some(frame);
        Ok(())
    }

    /// Non-blocking acquire. Returns the newest queued DMABUF frame, if any.
    pub fn try_acquire_frame(&mut self) -> Result<Option<WebSurfaceFrame>, WebSurfaceError> {
        let Some(frame) = self
            .pending_frame
            .lock()
            .map_err(|_| {
                WebSurfaceError::Platform("WPE pending frame mutex was poisoned".to_string())
            })?
            .take()
        else {
            return Ok(None);
        };
        Ok(Some(WebSurfaceFrame::Native(NativeFrame::DmaBufImage(
            frame,
        ))))
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }
}

impl WebSurfaceProducer for WpeProducer {
    fn capabilities(&self) -> WebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WebSurfaceFrame, WebSurfaceError> {
        self.try_acquire_frame()?
            .ok_or(WebSurfaceError::Unsupported(
                "WpeProducer has no queued DMABUF frame; WPE callback bridge is not wired yet",
            ))
    }

    fn navigate_to_string(
        &mut self,
        _html: &str,
        _timeout: std::time::Duration,
    ) -> Result<(), WebSurfaceError> {
        Err(WebSurfaceError::Unsupported(
            "WpeProducer::navigate_to_string is waiting on the WPE WebKit FFI bridge",
        ))
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WebSurfaceError> {
        if size.width == 0 || size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WPE producer size must be non-zero, got {}x{}",
                size.width, size.height
            )));
        }
        self.size = size;
        Ok(())
    }

    fn set_offset(&mut self, x: f32, y: f32) -> Result<(), WebSurfaceError> {
        self.offset = (x, y);
        Ok(())
    }
}

pub(crate) fn linux_wpe_capabilities() -> WebSurfaceCapabilities {
    WebSurfaceCapabilities {
        backend: SystemWebviewBackend::Wpe,
        preferred_mode: WebSurfaceMode::Unsupported,
        imported_texture: CapabilityStatus::Unsupported(
            UnsupportedReason::NativeImportNotYetImplemented,
        ),
        native_child_overlay: CapabilityStatus::Unsupported(
            UnsupportedReason::PlatformNotImplemented,
        ),
        cpu_snapshot: CapabilityStatus::Unsupported(
            UnsupportedReason::NativeImportNotYetImplemented,
        ),
        supported_frames: vec![NativeFrameKind::DmaBufImage],
        reason: "WPE is the planned Linux primary backend (DMABUF + Vulkan external memory); the producer API and DMABUF frame contract are present, but the WPE FFI callback bridge and Vulkan importer are not wired yet.",
    }
}
