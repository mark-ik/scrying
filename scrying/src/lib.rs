#![doc = include_str!("../README.md")]

pub mod native_frame;

use dpi::PhysicalSize;
use thiserror::Error;

pub use native_frame::{
    CapabilityStatus, Dx12SharedTexture, HostWgpuContext, ImportOptions, ImportedTexture,
    InteropBackend, InteropError, MetalTextureRef, NativeFrame, NativeFrameKind,
    ProducerCapabilities, SyncMechanism, TextureImporter, UnsupportedReason, WgpuTextureImporter,
};
#[cfg(target_os = "windows")]
pub use native_frame::Dx12FenceSynchronizer;

#[cfg(target_os = "windows")]
pub mod windows_capture;

#[cfg(target_os = "windows")]
pub mod webview2_composition_producer;

#[cfg(target_os = "macos")]
pub mod wkwebview_producer;

#[cfg(target_os = "macos")]
pub use wkwebview_producer::{
    CaptureMetrics, CaptureStatus, WkWebViewProducer, WkWebViewProducerConfig,
};

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
            SystemWebviewBackend::WkWebView => {
                let imported_texture = match host.map(|h| h.backend) {
                    Some(InteropBackend::Metal) => CapabilityStatus::Supported,
                    Some(_) => CapabilityStatus::Unsupported(
                        crate::native_frame::UnsupportedReason::HostBackendMismatch,
                    ),
                    None => CapabilityStatus::Unsupported(
                        crate::native_frame::UnsupportedReason::HostBackendUnavailable,
                    ),
                };
                Self {
                    backend: SystemWebviewBackend::WkWebView,
                    // ImportedTexture only when the host's wgpu
                    // device is Metal — that's the only case
                    // ScreenCaptureKit's IOSurface→MTLTexture path
                    // can hand us a wgpu-importable handle.
                    preferred_mode: match imported_texture {
                        CapabilityStatus::Supported => WebSurfaceMode::ImportedTexture,
                        _ => WebSurfaceMode::NativeChildOverlay,
                    },
                    imported_texture,
                    native_child_overlay: CapabilityStatus::Supported,
                    cpu_snapshot: CapabilityStatus::Supported,
                    supported_frames: vec![NativeFrameKind::MetalTextureRef],
                    reason: "WKWebView producer: ScreenCaptureKit → IOSurface → MTLTexture path is wired (requires Screen Recording permission and a Metal-backed host wgpu device); falls back to NativeChildOverlay if the host isn't on Metal, and CpuSnapshot via takeSnapshot: is always available.",
                }
            }
            SystemWebviewBackend::WebKitGtk => Self {
                backend: SystemWebviewBackend::WebKitGtk,
                preferred_mode: WebSurfaceMode::NativeChildOverlay,
                imported_texture: CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                native_child_overlay: CapabilityStatus::Supported,
                cpu_snapshot: CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::NativeImportNotYetImplemented,
                ),
                supported_frames: Vec::new(),
                reason: "WebKitGTK has internal DMABUF presentation paths, but Wry does not expose them as a frame producer.",
            },
            SystemWebviewBackend::Unknown => Self {
                backend: SystemWebviewBackend::Unknown,
                preferred_mode: WebSurfaceMode::Unsupported,
                imported_texture: CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::HostBackendUnavailable,
                ),
                native_child_overlay: CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::HostBackendUnavailable,
                ),
                cpu_snapshot: CapabilityStatus::Unsupported(
                    crate::native_frame::UnsupportedReason::HostBackendUnavailable,
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
            crate::native_frame::UnsupportedReason::HostBackendMismatch,
        ),
        None => CapabilityStatus::Unsupported(
            crate::native_frame::UnsupportedReason::HostBackendUnavailable,
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
    /// The page tried to open a new window (`target="_blank"`,
    /// `window.open(...)`, JS-triggered popup). The producer
    /// suppresses the engine-level popup unconditionally — browser-
    /// shape consumers (multiple tabs per process) want full control
    /// over how popups are routed and should observe this event,
    /// then call `load_url(url)` on a fresh producer instance to
    /// open it as a tab.
    NewWindowRequested { url: String },
    /// The web content process backing this WebView terminated
    /// (typically a content-side crash). The producer's WKWebView
    /// is no longer rendering; the host should reload (and may show
    /// a "tab crashed" UI). Recovery is `producer.reload()` or
    /// `load_url(...)` — the WKWebView itself is reusable.
    ContentProcessTerminated,
    /// The engine received an authentication challenge for the
    /// given URL and protection space. The producer responds with
    /// `PerformDefaultHandling` (system Keychain / interactive UI),
    /// so this event is informational only — useful for browser
    /// chrome that wants to log auth events or show a status
    /// indicator. A future slice may grow a `respond_to_auth`
    /// method for hosts that need to drive the disposition
    /// themselves.
    AuthChallenged {
        url: String,
        /// Host the credential is being requested for.
        host: String,
        /// Authentication method identifier (NSURLAuthenticationMethod*),
        /// e.g. `NSURLAuthenticationMethodHTTPBasic`,
        /// `NSURLAuthenticationMethodServerTrust`,
        /// `NSURLAuthenticationMethodClientCertificate`.
        auth_method: String,
    },
    /// A WebKit-managed download started. The producer chose
    /// `destination_path` automatically (under the configured
    /// download directory); the host is responsible for any UI
    /// (progress bars, "show in Finder", etc.). The `id` correlates
    /// with subsequent `DownloadProgress` / `DownloadFinished` /
    /// `DownloadCancelled` events for this download.
    DownloadStarted {
        id: DownloadId,
        url: String,
        suggested_filename: String,
        destination_path: std::path::PathBuf,
        /// Total bytes the server announced via `Content-Length`.
        /// `None` when the server didn't announce one (chunked
        /// transfer, etc.).
        total_bytes_expected: Option<u64>,
    },
    /// Throttled progress notification. Emitted at most ~10 Hz per
    /// download, plus a final emit at completion. `bytes_written` is
    /// the cumulative count of bytes written to disk.
    DownloadProgress {
        id: DownloadId,
        bytes_written: u64,
        total_bytes_expected: Option<u64>,
    },
    /// A download completed. `error` is `Some` on failure (the file
    /// at `destination_path` may be partial or absent), `None` on
    /// successful completion. Hosts that want to distinguish
    /// host-driven cancellation from a server / network error
    /// should listen for `DownloadCancelled` instead — that variant
    /// only fires when the disposition came from
    /// `set_download_handler` returning `DownloadDecision::Cancel`
    /// or the host calling `cancel_download(id)`.
    DownloadFinished {
        id: DownloadId,
        destination_path: std::path::PathBuf,
        error: Option<String>,
    },
    /// A download was cancelled — either because a
    /// host-registered destination handler returned
    /// `DownloadDecision::Cancel`, or because the host called
    /// `cancel_download(id)` mid-stream.
    ///
    /// `resume_data` is `Some` when WebKit captured enough state
    /// to potentially resume the download via
    /// [`crate::wkwebview_producer::WkWebViewProducer::resume_download`].
    /// `None` when the cancel happened before any bytes
    /// transferred (e.g. host destination handler returned
    /// `Cancel` from `decideDestination`) or when the protocol /
    /// server doesn't support resumption (no `Accept-Ranges`
    /// support, etc.).
    DownloadCancelled {
        id: DownloadId,
        destination_path: std::path::PathBuf,
        resume_data: Option<Vec<u8>>,
    },
    /// The user right-clicked inside the page. The producer
    /// suppresses WebKit's default context menu and emits this
    /// event so a browser-class consumer can show its own — usually
    /// a host-rendered `NSMenu` with items like "Open link in new
    /// tab" or "Save image as...". Apple's
    /// `webView:contextMenuConfigurationForElement:` is iOS-only,
    /// so on macOS the producer goes through a JS user-script
    /// (`contextmenu` capture-phase listener +
    /// `event.preventDefault()`) and a dedicated
    /// `WKScriptMessageHandler` to deliver this event.
    ///
    /// Coordinates are in CSS pixels relative to the WebView's
    /// viewport (matching `MouseEvent.clientX` /
    /// `MouseEvent.clientY`). `link_url` and `image_url` walk the
    /// click target's ancestor chain to recover the closest
    /// enclosing `<a href>` / `<img src>`; both are `None` when no
    /// such ancestor exists (e.g. a right-click on plain body
    /// text).
    ContextMenuRequested {
        page_url: String,
        x: f64,
        y: f64,
        link_url: Option<String>,
        image_url: Option<String>,
    },
}

/// Opaque per-producer identifier for a download. Issued when
/// WebKit promotes a navigation to a download; used by the host to
/// correlate `DownloadStarted` / `DownloadProgress` /
/// `DownloadFinished` / `DownloadCancelled` events and to drive
/// [`crate::wkwebview_producer::WkWebViewProducer::cancel_download`].
///
/// IDs are monotonically increasing per producer and are not
/// reused. They have no meaning across producers.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DownloadId(pub u64);

/// Information passed to a host-registered download-destination
/// handler (see
/// [`crate::wkwebview_producer::WkWebViewProducer::set_download_handler`]).
/// The host returns a [`DownloadDecision`] describing whether to
/// accept the download (and where to write it) or cancel it.
#[derive(Clone, Debug)]
pub struct DownloadDestinationRequest {
    pub id: DownloadId,
    pub url: String,
    pub suggested_filename: String,
    pub mime_type: String,
    pub total_bytes_expected: Option<u64>,
}

/// Disposition the host's destination handler returns to
/// [`DownloadDestinationRequest`].
#[derive(Clone, Debug)]
pub enum DownloadDecision {
    /// Accept the download and write it to this absolute path.
    /// Parent directory is created if it doesn't exist.
    AcceptAt(std::path::PathBuf),
    /// Cancel the download. Triggers a `DownloadCancelled` event;
    /// no bytes are written.
    Cancel,
}

/// Information passed to a host-registered auth-challenge handler
/// (see `WkWebViewProducer::set_auth_handler`). The host returns an
/// [`AuthDisposition`] describing how the challenge should be
/// resolved.
#[derive(Clone, Debug)]
pub struct AuthChallenge {
    pub url: String,
    /// Host the credential is being requested for.
    pub host: String,
    /// Authentication method identifier
    /// (`NSURLAuthenticationMethodHTTPBasic`,
    /// `...HTTPDigest`, `...ServerTrust`,
    /// `...ClientCertificate`, etc.).
    pub auth_method: String,
    /// Realm the server announced (HTTP basic / digest only —
    /// empty for other methods).
    pub realm: String,
}

/// Disposition the host returns from its auth-challenge handler.
/// Maps onto `NSURLSessionAuthChallengeDisposition`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum AuthDisposition {
    /// Fall back to WebKit's default handling (system Keychain /
    /// interactive prompts).
    PerformDefault,
    /// Cancel the auth challenge — the load fails.
    Cancel,
    /// "I can't satisfy this protection space; ask the next one."
    /// Useful for client-cert challenges where the host has no
    /// matching cert.
    RejectProtectionSpace,
    /// Provide a username + password credential (HTTP basic /
    /// digest). Persistence is session-only — no Keychain write.
    UseCredential { username: String, password: String },
}

/// Information passed to a host-registered permission handler
/// (see `WkWebViewProducer::set_permission_handler`). The host
/// returns a [`PermissionDecision`].
#[derive(Clone, Debug)]
pub struct PermissionRequest {
    /// Web origin requesting the permission, e.g.
    /// `"https://example.com"`. Empty for `about:` / `data:` URLs.
    pub origin: String,
    /// URL of the frame that initiated the request — usually the
    /// same as `origin` plus the path; differs from `origin` for
    /// nested iframes.
    pub frame_url: String,
    pub kind: PermissionKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PermissionKind {
    Camera,
    Microphone,
    CameraAndMicrophone,
    /// `DeviceMotionEvent` / `DeviceOrientationEvent`.
    DeviceOrientation,
}

/// Disposition the host returns from its permission handler. Maps
/// onto `WKPermissionDecision`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PermissionDecision {
    /// Allow the requested permission.
    Grant,
    /// Refuse the requested permission.
    Deny,
    /// Fall back to WebKit's default behavior — for media-capture
    /// requests this means the OS shows its standard prompt; for
    /// device-orientation it means the engine prompts according to
    /// its own policy. Use this when the host doesn't have an
    /// opinion (e.g. for an "untrusted page, let WebKit handle it"
    /// fallback).
    Prompt,
}

/// HTTP cookie payload used by the producer's cookie-store API
/// (`request_all_cookies` / `set_cookie` / `delete_cookie` on the
/// macOS producer). Mirrors the subset of `NSHTTPCookie` that
/// browser-shape consumers actually need; `expires_at` is a Unix
/// timestamp (seconds since 1970-01-01 UTC), `None` for session
/// cookies.
#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires_at: Option<f64>,
    pub is_secure: bool,
    pub is_http_only: bool,
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

/// One pointer (touch / pen / mouse-as-pointer) event forwarded to the
/// underlying webview. Coordinates are physical pixels relative to the
/// webview's top-left corner.
#[derive(Clone, Copy, Debug)]
pub struct PointerInput {
    pub kind: PointerEventKind,
    pub device: PointerDevice,
    /// Pointer ID. Two simultaneous touches use distinct IDs; a single pen
    /// stays at ID 1. Zero is reserved for "no ID".
    pub pointer_id: u32,
    /// Position of the pointer in physical pixels relative to the webview.
    pub point: (i32, i32),
    /// Pressure in `0.0..=1.0`. `0.0` for non-pressure-aware devices.
    pub pressure: f32,
    /// Tilt in radians for pen input; zero for touch / mouse.
    pub tilt: (f32, f32),
}

/// Discrete kinds of pointer event. Mirrors `COREWEBVIEW2_POINTER_EVENT_KIND`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PointerEventKind {
    Activate,
    Down,
    Enter,
    Leave,
    Up,
    Update,
    CaptureChanged,
}

/// The kind of input device that produced a [`PointerInput`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PointerDevice {
    Touch,
    Pen,
    Mouse,
}

/// Cursor shape the webview wants the host to display, reported via the
/// `cursor_changed` callback registered with
/// [`WryWebSurfaceProducer::set_cursor_handler`].
///
/// The full Win32 / cocoa / X11 cursor namespace is large and platform-
/// specific. This enum is the subset CSS / WebKit consensus settles
/// on; producers may report `Custom(name)` for shapes not enumerated.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CursorShape {
    Default,
    Pointer,
    Text,
    Wait,
    Crosshair,
    Move,
    NotAllowed,
    Help,
    Progress,
    ResizeNs,
    ResizeEw,
    ResizeNeSw,
    ResizeNwSe,
    ResizeAll,
    Grab,
    Grabbing,
    ZoomIn,
    ZoomOut,
    Custom(String),
}

/// One key / modifier-change event forwarded to the underlying webview.
#[derive(Clone, Debug)]
pub struct KeyboardInput {
    pub kind: KeyEventKind,
    /// Platform-native virtual-key code. Windows: VK_*, Mac: AppKit
    /// `keyCode` (Apple HID usage, e.g. 0x00 = A, 0x24 = Return),
    /// Linux: xkb keycode. Producers map this to whatever the
    /// underlying engine expects.
    pub virtual_key_code: u32,
    /// Text characters this event would produce (after IME / dead-key
    /// composition), if any. Empty for pure modifier-state changes.
    pub characters: String,
    /// Same text but with modifier keys (shift / alt) ignored, for
    /// keyboard-shortcut handling. Empty when not applicable.
    pub characters_ignoring_modifiers: String,
    pub modifiers: KeyModifierFlags,
    /// `true` when the OS reports this event as auto-repeat.
    pub is_repeat: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum KeyEventKind {
    Down,
    Up,
    /// A modifier key (shift, control, alt, meta / cmd) toggled.
    /// `virtual_key_code` identifies which one; `characters` is empty.
    ModifiersChanged,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyModifierFlags {
    pub shift: bool,
    pub control: bool,
    /// `Alt` on Windows, `Option` on macOS, `Mod1` on Linux.
    pub alt: bool,
    /// `Win` on Windows, `Command` on macOS, `Mod4` / `Super` on
    /// Linux.
    pub meta: bool,
    /// Caps-lock toggle state at the moment of the event.
    pub caps_lock: bool,
}

/// Drag-and-drop event forwarded to the webview.
#[derive(Clone, Copy, Debug)]
pub struct DragInput {
    pub kind: DragEventKind,
    pub virtual_keys: MouseVirtualKeys,
    pub point: (i32, i32),
    /// Set of allowed effects bitmask; `0` for default.
    pub allowed_effects: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DragEventKind {
    Enter,
    Over,
    Leave,
    Drop,
}

/// Snapshot of webview-level settings exposed by the producer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebSurfaceSettings {
    /// Zoom factor (`1.0` is normal). `None` leaves the producer's
    /// default in place.
    pub zoom_factor: Option<f64>,
    /// Custom user-agent string. `None` leaves the producer's default
    /// in place.
    pub user_agent: Option<String>,
    /// Whether developer-tools are accessible (Ctrl+Shift+I, F12, the
    /// host's [`WryWebSurfaceProducer::open_devtools_window`] call).
    pub devtools_enabled: Option<bool>,
    /// Whether JavaScript execution is enabled in the webview.
    pub javascript_enabled: Option<bool>,
    /// Whether the engine's default right-click context menu is shown.
    pub default_context_menus_enabled: Option<bool>,
    /// Whether the engine's default browser-acceleration shortcuts (zoom,
    /// reload, F5, etc.) are intercepted.
    pub builtin_accelerator_keys_enabled: Option<bool>,
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
    ///
    /// # ⚠️ Blocking — host-event-loop hazard
    ///
    /// On macOS this method pumps the main `NSRunLoop` to wait for
    /// the navigation delegate. Calling it from inside a host
    /// event-loop callback (e.g. winit's `resumed` / `window_event`
    /// under macOS, where winit guards against re-entrant handler
    /// invocation) will trigger a re-entrancy panic. From event-loop
    /// callbacks, prefer the non-blocking inherent
    /// [`crate::wkwebview_producer::WkWebViewProducer::load_html`]
    /// and observe completion via
    /// [`Self::poll_navigation_event`].
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
    ///
    /// # ⚠️ Blocking — host-event-loop hazard
    ///
    /// Same caveat as [`Self::navigate_to_string`]: pumps the main
    /// `NSRunLoop` on macOS, panics if invoked from a winit event
    /// handler. Use [`crate::wkwebview_producer::WkWebViewProducer::load_url`]
    /// paired with [`Self::poll_navigation_event`] for non-blocking
    /// navigation from event-loop contexts.
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

    /// Forward a touch / pen / pointer event to the webview.
    fn send_pointer_input(&mut self, event: PointerInput) -> Result<(), WryWebSurfaceError> {
        let _ = event;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::send_pointer_input is not implemented for this platform",
        ))
    }

    /// Forward a keyboard / modifier-state event to the webview. The
    /// host typically calls this when the webview is the focus target
    /// (see [`WryWebSurfaceProducer::move_focus`]) and the windowing
    /// system delivers a key event. Producers that don't yet support
    /// keyboard forwarding return [`WryWebSurfaceError::Unsupported`].
    fn send_keyboard_input(&mut self, event: KeyboardInput) -> Result<(), WryWebSurfaceError> {
        let _ = event;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::send_keyboard_input is not implemented for this platform",
        ))
    }

    /// Forward a drag / drop event to the webview. Hosts call this when
    /// the user drags content over the webview region.
    fn send_drag_input(&mut self, event: DragInput) -> Result<(), WryWebSurfaceError> {
        let _ = event;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::send_drag_input is not implemented for this platform",
        ))
    }

    /// Drain the next pending cursor shape requested by the webview.
    /// Producers that support cursor reporting push a fresh
    /// [`CursorShape`] each time the engine's hovered element changes.
    fn poll_cursor_shape(&mut self) -> Option<CursorShape> {
        None
    }

    /// Reload the current page (equivalent to the user pressing F5).
    fn reload(&mut self) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::reload is not implemented for this platform",
        ))
    }

    /// Stop loading the current navigation.
    fn stop(&mut self) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::stop is not implemented for this platform",
        ))
    }

    /// Navigate one entry back in the session history if possible. Returns
    /// `Ok(false)` if the back stack is empty, `Ok(true)` if a navigation
    /// was started.
    fn go_back(&mut self) -> Result<bool, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::go_back is not implemented for this platform",
        ))
    }

    /// Navigate one entry forward in the session history if possible.
    fn go_forward(&mut self) -> Result<bool, WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::go_forward is not implemented for this platform",
        ))
    }

    /// Whether the back stack currently has at least one entry.
    fn can_go_back(&self) -> bool {
        false
    }

    /// Whether the forward stack currently has at least one entry.
    fn can_go_forward(&self) -> bool {
        false
    }

    /// Open the developer-tools UI for this webview.
    ///
    /// - **Windows**: opens the WebView2 DevTools window.
    /// - **macOS**: macOS WebKit doesn't expose a way to *open* the
    ///   inspector programmatically — Apple's public API only flips
    ///   the *attachable* bit (`setInspectable:`, macOS 13.3+, wired
    ///   via [`WebSurfaceSettings::devtools_enabled`]). The host or
    ///   user must then attach Safari's Web Inspector manually:
    ///   `Safari → Develop → <hostname> → <webview-page>` (Safari
    ///   must be running and Develop menu enabled in
    ///   `Safari → Settings → Advanced → Show features for web
    ///   developers`). Calling this on macOS returns `Unsupported`;
    ///   set `devtools_enabled = Some(true)` via `apply_settings`
    ///   first to make the WebView discoverable.
    /// - **Linux**: opens the WebKit Web Inspector.
    fn open_devtools_window(&mut self) -> Result<(), WryWebSurfaceError> {
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::open_devtools_window is not implemented for this platform",
        ))
    }

    /// Toggle the WebView's *page visibility* — what the page sees
    /// via the W3C Page Visibility API (`document.hidden`,
    /// `document.visibilityState`, the `visibilitychange` event).
    ///
    /// Browser-shape consumers call this when a tab becomes / stops
    /// being the active one in their tab UI. `false` causes:
    ///
    /// - `document.hidden = true`, `visibilityState = "hidden"`
    /// - `requestAnimationFrame` callbacks throttle to at most ~1 Hz
    /// - Background-tab autoplay / video-decoding throttles per
    ///   the engine's policy
    /// - Setinterval / setTimeout callbacks may coalesce
    ///
    /// This is the *light* throttle: the page still runs, just
    /// slower. It's distinct from the heavier
    /// `_setSuspended:` SPI-only path which fully pauses execution
    /// and is unsupported here. Most tabs-in-one-process consumers
    /// only need the light path.
    fn set_visible(&mut self, visible: bool) -> Result<(), WryWebSurfaceError> {
        let _ = visible;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::set_visible is not implemented for this platform",
        ))
    }

    /// Apply a partial settings update to the webview. Each `Some` field
    /// is applied; `None` fields are left at their current value.
    /// Producers report unsupported fields by ignoring them silently.
    fn apply_settings(&mut self, settings: &WebSurfaceSettings) -> Result<(), WryWebSurfaceError> {
        let _ = settings;
        Err(WryWebSurfaceError::Unsupported(
            "WryWebSurfaceProducer::apply_settings is not implemented for this platform",
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
                crate::native_frame::UnsupportedReason::HostBackendUnavailable,
            )
        );
    }
}
