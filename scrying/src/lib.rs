#![doc = include_str!("../README.md")]

use dpi::PhysicalSize;
use thiserror::Error;
use wgpu_native_texture_interop::{
    CapabilityStatus, HostWgpuContext, InteropBackend, InteropError, NativeFrame, NativeFrameKind,
    ProducerCapabilities,
};

#[cfg(target_os = "windows")]
pub mod windows_capture;

#[cfg(target_os = "windows")]
pub mod webview2_composition_producer;

#[cfg(target_os = "macos")]
pub mod wkwebview_producer;

#[cfg(target_os = "linux")]
pub mod webkitgtk_producer;

/// How a system webview can participate in a host compositor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WebSurfaceMode {
    /// The adapter can emit a native GPU frame importable by `wgpu-native-texture-interop`.
    ImportedTexture,
    /// The webview must remain a platform child window/visual overlay.
    NativeChildOverlay,
    /// The adapter can emit CPU pixels or encoded snapshots.
    CpuSnapshot,
    /// No usable surface path is available.
    Unsupported,
}

/// The system webview backend behind Wry on the current platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SystemWebviewBackend {
    WebView2,
    WkWebView,
    WebKitGtk,
    Unknown,
}

impl SystemWebviewBackend {
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            Self::WebView2
        } else if cfg!(target_os = "macos") {
            Self::WkWebView
        } else if cfg!(target_os = "linux") {
            Self::WebKitGtk
        } else {
            Self::Unknown
        }
    }
}

/// Probe result for a Wry/system-webview surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WryWebSurfaceCapabilities {
    pub backend: SystemWebviewBackend,
    pub preferred_mode: WebSurfaceMode,
    pub imported_texture: CapabilityStatus,
    pub native_child_overlay: CapabilityStatus,
    pub cpu_snapshot: CapabilityStatus,
    pub supported_frames: Vec<NativeFrameKind>,
    pub reason: &'static str,
}

impl WryWebSurfaceCapabilities {
    pub fn probe(host: Option<&HostWgpuContext>) -> Self {
        match SystemWebviewBackend::detect() {
            SystemWebviewBackend::WebView2 => probe_webview2(host),
            SystemWebviewBackend::WkWebView => Self {
                backend: SystemWebviewBackend::WkWebView,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: CapabilityStatus::Supported,
                cpu_snapshot: CapabilityStatus::Supported,
                supported_frames: Vec::new(),
                reason: "WKWebView snapshot capture is useful as a fallback, but no Metal texture producer is wired.",
            },
            SystemWebviewBackend::WebKitGtk => Self {
                backend: SystemWebviewBackend::WebKitGtk,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: CapabilityStatus::Supported,
                cpu_snapshot: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                supported_frames: Vec::new(),
                reason: "WebKitGTK has internal DMABUF presentation paths, but Wry does not expose them as a frame producer.",
            },
            SystemWebviewBackend::Unknown => Self {
                backend: SystemWebviewBackend::Unknown,
                preferred_mode: WebSurfaceMode::Unsupported,
                imported_texture: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                native_child_overlay: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                cpu_snapshot: CapabilityStatus::Unsupported(
                    wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
                ),
                supported_frames: Vec::new(),
                reason: "No Wry/system-webview backend is defined for this platform.",
            },
        }
    }

    pub fn producer_capabilities(&self) -> ProducerCapabilities {
        ProducerCapabilities {
            supported_frames: self.supported_frames.clone(),
        }
    }
}

fn probe_webview2(host: Option<&HostWgpuContext>) -> WryWebSurfaceCapabilities {
    let imported_texture = match host.map(|host| host.backend) {
        Some(InteropBackend::Dx12) => CapabilityStatus::Supported,
        Some(_) => CapabilityStatus::Unsupported(
            wgpu_native_texture_interop::UnsupportedReason::HostBackendMismatch,
        ),
        None => CapabilityStatus::Unsupported(
            wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
        ),
    };

    let preferred_mode = if imported_texture == CapabilityStatus::Supported {
        WebSurfaceMode::ImportedTexture
    } else {
        WebSurfaceMode::NativeChildOverlay
    };

    WryWebSurfaceCapabilities {
        backend: SystemWebviewBackend::WebView2,
        preferred_mode,
        imported_texture,
        native_child_overlay: CapabilityStatus::Supported,
        cpu_snapshot: CapabilityStatus::Supported,
        supported_frames: vec![NativeFrameKind::Dx12SharedTexture],
        reason: "Windows target path is WebView2 CompositionController visual capture into a D3D texture, then Dx12SharedTexture import.",
    }
}

/// A frame emitted by a Wry/system-webview producer.
#[non_exhaustive]
pub enum WryWebSurfaceFrame {
    Native(NativeFrame),
    CpuRgba {
        size: PhysicalSize<u32>,
        pixels: image::RgbaImage,
        generation: u64,
    },
    PngSnapshot {
        size: PhysicalSize<u32>,
        bytes: Vec<u8>,
        generation: u64,
    },
    OverlayOnly,
}

impl WryWebSurfaceFrame {
    pub fn mode(&self) -> WebSurfaceMode {
        match self {
            Self::Native(_) => WebSurfaceMode::ImportedTexture,
            Self::CpuRgba { .. } | Self::PngSnapshot { .. } => WebSurfaceMode::CpuSnapshot,
            Self::OverlayOnly => WebSurfaceMode::NativeChildOverlay,
        }
    }
}

#[derive(Debug, Error)]
pub enum WryWebSurfaceError {
    #[error("web surface mode is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("frame is not ready yet: {0}")]
    NotReady(&'static str),
    #[error(transparent)]
    Interop(#[from] InteropError),
    #[error("platform capture failed: {0}")]
    Platform(String),
}

/// Lifecycle / state event emitted by the underlying webview.
///
/// Drained from a producer via [`WryWebSurfaceProducer::poll_navigation_event`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum NavigationEvent {
    /// Navigation has started toward the given URL. Does not guarantee the
    /// load will succeed.
    Starting { url: String },
    /// The committed source URL has changed (covers same-document
    /// navigations and history pushState/replaceState).
    SourceChanged { url: String },
    /// Navigation finished. `success` reflects whether the load completed
    /// without a top-level error; sub-resource failures do not affect it.
    Completed { url: String, success: bool },
    /// The document title changed.
    TitleChanged { title: String },
}

/// Reason supplied to a focus move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FocusReason {
    /// Programmatic focus (e.g. user clicked a host control that should
    /// hand focus to the webview).
    Programmatic,
    /// User tabbed forward into the webview.
    Next,
    /// User shift-tabbed into the webview.
    Previous,
}

/// One mouse / scroll event forwarded to the underlying webview.
///
/// Coordinates are in physical pixels, relative to the webview's top-left
/// corner (i.e. the origin of the bounds set by the most recent
/// [`WryWebSurfaceProducer::resize`] / `set_offset` pair).
#[derive(Clone, Copy, Debug)]
pub struct MouseInput {
    pub kind: MouseEventKind,
    /// Modifier and button state at the moment of the event.
    pub virtual_keys: MouseVirtualKeys,
    /// Wheel delta (for `Wheel` / `HorizontalWheel`) or X-button index
    /// (for `XButton*`). Zero for other event kinds.
    pub mouse_data: i32,
    pub point: (i32, i32),
}

/// Discrete kinds of mouse / scroll event recognised by the underlying
/// composition controller. Mirrors `COREWEBVIEW2_MOUSE_EVENT_KIND`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MouseEventKind {
    LeftButtonDown,
    LeftButtonUp,
    LeftButtonDoubleClick,
    MiddleButtonDown,
    MiddleButtonUp,
    MiddleButtonDoubleClick,
    RightButtonDown,
    RightButtonUp,
    RightButtonDoubleClick,
    XButtonDown,
    XButtonUp,
    XButtonDoubleClick,
    Move,
    Wheel,
    HorizontalWheel,
    Leave,
}

/// Modifier and button state for a [`MouseInput`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MouseVirtualKeys {
    pub control: bool,
    pub shift: bool,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
    pub x_button1: bool,
    pub x_button2: bool,
}

/// Producer contract implemented by platform-specific Wry/WebView frame sources.
///
/// The trait covers the cross-platform lifecycle (capabilities + navigate +
/// resize + offset + a blocking acquire). Per-frame fast-path acquisition
/// and any platform-specific optimization signals (e.g. the Windows
/// "did the shared destination texture get re-allocated this frame"
/// flag) are exposed on the concrete platform producer types and not
/// on the trait, since they have no portable shape.
pub trait WryWebSurfaceProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities;

    fn mode(&self) -> WebSurfaceMode {
        self.capabilities().preferred_mode
    }

    /// Blocking acquire — returns the next available frame from the
    /// underlying capture path, possibly waiting for the WebView to
    /// produce one.
    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError>;

    /// Navigate the underlying WebView to inline HTML and block until
    /// `NavigationCompleted` (or analog) fires, or the timeout elapses.
    /// Producers that don't yet support navigation return
    /// [`WryWebSurfaceError::Unsupported`].
    fn navigate_to_string(
        &mut self,
        html: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = (html, timeout);
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::navigate_to_string is not implemented for this platform",
        ))
    }

    /// Resize the underlying WebView and capture region.
    fn resize(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = size;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::resize is not implemented for this platform",
        ))
    }

    /// Reposition the underlying WebView overlay relative to the parent
    /// host, in physical pixels.
    fn set_offset(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = (x, y);
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::set_offset is not implemented for this platform",
        ))
    }

    /// Navigate the underlying WebView to a URL and block until
    /// `NavigationCompleted` fires (or the timeout elapses). Producers
    /// that don't yet support URL navigation return
    /// [`WryWebSurfaceError::Unsupported`].
    fn navigate_to_url(
        &mut self,
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WryWebSurfaceError> {
        let _ = (url, timeout);
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::navigate_to_url is not implemented for this platform",
        ))
    }

    /// Forward a mouse / scroll event to the underlying webview.
    /// Coordinates are physical pixels relative to the webview's top-left
    /// corner.
    fn send_mouse_input(&mut self, event: MouseInput) -> Result<(), WryWebSurfaceError> {
        let _ = event;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::send_mouse_input is not implemented for this platform",
        ))
    }

    /// Move keyboard focus into the underlying webview. Hosts typically
    /// call this when the user clicks the webview region or tabs into it
    /// from a host control.
    fn move_focus(&mut self, reason: FocusReason) -> Result<(), WryWebSurfaceError> {
        let _ = reason;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::move_focus is not implemented for this platform",
        ))
    }

    /// Drain the next pending [`NavigationEvent`], if any. Returns `None`
    /// when no event is queued.
    ///
    /// Consumers should poll this each frame (or on demand) to reflect
    /// load progress in their UI. Events are queued FIFO per producer.
    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        None
    }

    /// Post a string message into the webview's `window.chrome.webview`
    /// listener. Producers that don't support JS messaging return
    /// [`WryWebSurfaceError::Unsupported`].
    fn post_web_message(&mut self, message: &str) -> Result<(), WryWebSurfaceError> {
        let _ = message;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::post_web_message is not implemented for this platform",
        ))
    }

    /// Drain the next pending message posted from JS via
    /// `window.chrome.webview.postMessage(...)`, if any. Messages are
    /// queued FIFO per producer.
    fn poll_web_message(&mut self) -> Option<String> {
        None
    }

    /// Take a one-shot PNG snapshot of the current webview document.
    /// Useful for thumbnails / previews / diagnostics; not a substitute
    /// for the live capture path. Producers that don't support snapshot
    /// capture return [`WryWebSurfaceError::Unsupported`].
    fn capture_snapshot_png(&mut self) -> Result<Vec<u8>, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::capture_snapshot_png is not implemented for this platform",
        ))
    }
}

/// Conservative overlay-only producer used when no capture backend is available yet.
#[derive(Clone, Debug)]
pub struct OverlayOnlyProducer {
    capabilities: WryWebSurfaceCapabilities,
}

impl OverlayOnlyProducer {
    pub fn new(capabilities: WryWebSurfaceCapabilities) -> Self {
        Self { capabilities }
    }
}

impl WryWebSurfaceProducer for OverlayOnlyProducer {
    fn capabilities(&self) -> WryWebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn acquire_frame(&mut self) -> Result<WryWebSurfaceFrame, WryWebSurfaceError> {
        Ok(WryWebSurfaceFrame::OverlayOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_frame_reports_overlay_mode() {
        assert_eq!(
            WryWebSurfaceFrame::OverlayOnly.mode(),
            WebSurfaceMode::NativeChildOverlay
        );
    }

    #[test]
    fn unknown_host_on_windows_does_not_promise_imported_texture() {
        let caps = probe_webview2(None);
        assert_eq!(caps.backend, SystemWebviewBackend::WebView2);
        assert_eq!(caps.preferred_mode, WebSurfaceMode::NativeChildOverlay);
        assert_eq!(
            caps.imported_texture,
            CapabilityStatus::Unsupported(
                wgpu_native_texture_interop::UnsupportedReason::HostBackendUnavailable,
            )
        );
    }
}
