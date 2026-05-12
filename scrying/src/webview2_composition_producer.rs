//! Windows-only WebView2 composition-controller capture producer.
//!
//! This owns the moving parts the demo previously inlined:
//! - WebView2 environment / composition controller / controller / webview
//! - Windows.UI.Composition compositor, desktop window target, root + webview
//!   visuals
//! - Windows.Graphics.Capture item / frame pool / session lifecycle
//! - Post-StartCapture content invalidation nudge for the first frame
//! - Shared-handle export for the host's `wgpu-native-texture-interop` importer
//!
//! The proven flow this encapsulates was validated as:
//! 1. Create a real WebView2 composition-controller WebView.
//! 2. Attach it to a WinComp container visual.
//! 3. Feed the visual to `GraphicsCaptureItem::CreateFromVisual`.
//! 4. Start WGC capture.
//! 5. Nudge WebView content after `StartCapture`.
//! 6. Receive a `Bgra8Unorm` frame.
//! 7. Bridge D3D11 capture output into a DX12-importable native frame.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, COREWEBVIEW2_MOUSE_EVENT_KIND,
    COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS, COREWEBVIEW2_MOVE_FOCUS_REASON, ICoreWebView2,
    ICoreWebView2_2, ICoreWebView2CompositionController, ICoreWebView2Controller,
    ICoreWebView2Cookie, ICoreWebView2CookieManager, ICoreWebView2Environment,
    ICoreWebView2Environment3, ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    AddScriptToExecuteOnDocumentCreatedCompletedHandler, CoTaskMemPWSTR,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, DocumentTitleChangedEventHandler,
    ExecuteScriptCompletedHandler, GetCookiesCompletedHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, SourceChangedEventHandler, WebMessageReceivedEventHandler,
};
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat};
use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{E_POINTER, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::{CoTaskMemFree, IStream};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, HCURSOR, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND, IDC_HELP,
    IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT,
    LoadCursorW, MSG, PM_REMOVE, PeekMessageW, PostMessageW, TranslateMessage, WM_CHAR,
    WM_DEADCHAR, WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_COMPOSITIONFULL, WM_IME_CONTROL,
    WM_IME_ENDCOMPOSITION, WM_IME_KEYDOWN, WM_IME_KEYUP, WM_IME_NOTIFY, WM_IME_REQUEST,
    WM_IME_SELECT, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP, WM_SYSCHAR,
    WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows_numerics::{Vector2, Vector3};

use crate::{
    Cookie, CursorShape, FocusReason, KeyEventKind, KeyboardInput, MouseEventKind, MouseInput,
    NavigationEvent,
};

use crate::windows_capture::{
    D3D11SharedTexture, D3D11SharedTextureFactory, WebView2D3D11CaptureFrame,
    WebView2DxgiSharedHandleFrame,
};
use crate::{
    SystemWebviewBackend, WebSurfaceMode, WebSurfaceCapabilities, WebSurfaceError,
    WebSurfaceFrame,
};

const FIRST_FRAME_NUDGE_LABEL: &str = "WebView2CompositionProducer.first-frame";
const COOKIE_CHANGE_BRIDGE_MESSAGE: &str = "\0scrying:cookie-change";

pub type WebView2CookieChangeHandlerFn = Box<dyn Fn() + Send + Sync + 'static>;

/// Configuration for `WebView2CompositionProducer::new`.
#[derive(Clone, Debug)]
pub struct WebView2CompositionConfig {
    /// Initial size of the WebView visual and capture region.
    pub size: PhysicalSize<u32>,
    /// Offset of the root visual relative to the parent window.
    pub offset: (f32, f32),
    /// User-data directory for the WebView2 environment. Created if missing.
    pub user_data_dir: PathBuf,
    /// Optional CSS color used for a sprite visual placed under the WebView
    /// visual. Mostly useful as a diagnostic backstop while the WebView paints.
    pub diagnostic_backdrop: Option<(u8, u8, u8)>,
    /// Timeout for the navigation-completed wait inside `navigate_to_string`.
    pub navigation_timeout: Duration,
    /// Timeout for `acquire_frame` to wait on `TryGetNextFrame`.
    pub frame_timeout: Duration,
    /// Optional NT shared handle to a `D3D12_FENCE_FLAG_SHARED` fence
    /// (typically from
    /// `crate::native_frame::Dx12FenceSynchronizer::shared_handle`).
    /// When `Some`, the producer opens the fence on its D3D11 device and
    /// signals it after each `CopyResource` instead of using a keyed mutex
    /// + CPU spin. Frames are then emitted with
    /// `producer_sync = SyncMechanism::ExplicitFence` and a per-frame
    /// `fence_value`. The consumer-side `Dx12FenceSynchronizer` owns the
    /// canonical handle; the producer never closes it.
    pub fence_shared_handle: Option<*mut std::ffi::c_void>,
}

impl WebView2CompositionConfig {
    pub fn new(size: PhysicalSize<u32>, user_data_dir: impl Into<PathBuf>) -> Self {
        Self {
            size,
            offset: (0.0, 0.0),
            user_data_dir: user_data_dir.into(),
            diagnostic_backdrop: None,
            navigation_timeout: Duration::from_secs(5),
            frame_timeout: Duration::from_secs(2),
            fence_shared_handle: None,
        }
    }

    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset = (x, y);
        self
    }

    pub fn with_diagnostic_backdrop(mut self, rgb: (u8, u8, u8)) -> Self {
        self.diagnostic_backdrop = Some(rgb);
        self
    }

    /// Enable explicit-fence sync using the shared NT handle from the
    /// consumer's `Dx12FenceSynchronizer`. See
    /// [`fence_shared_handle`](Self::fence_shared_handle) for semantics.
    pub fn with_fence_shared_handle(mut self, handle: *mut std::ffi::c_void) -> Self {
        self.fence_shared_handle = Some(handle);
        self
    }
}

/// Captured WebView frame ready to be imported via `wgpu-native-texture-interop`.
///
/// When `resource_is_new` is `true`, this frame points at a freshly allocated
/// shared D3D11 texture that the consumer must (re-)import; the consumer owns
/// the NT handle and is responsible for calling
/// `crate::windows_capture::close_shared_handle` after import.
///
/// When `resource_is_new` is `false`, the producer reused the previous
/// allocation: the consumer should keep its previously-imported `wgpu::Texture`
/// (whose underlying memory was just overwritten by the producer's
/// `CopyResource`) and ignore `shared_handle`.
pub struct WebView2CompositionFrame {
    pub frame: WebSurfaceFrame,
    pub content_size: PhysicalSize<u32>,
    pub generation: u64,
    pub shared_handle: *mut std::ffi::c_void,
    pub resource_is_new: bool,
}

/// WebView2 + WinComp + WGC capture producer.
///
/// Construction sets up the composition tree and the WebView2 environment.
/// Capture is started lazily on the first `acquire_frame` call so the caller
/// can navigate and prepare content first.
pub struct WebView2CompositionProducer {
    #[allow(dead_code)]
    parent_hwnd: HWND,
    size: PhysicalSize<u32>,
    generation: u64,

    #[allow(dead_code)]
    compositor: Compositor,
    #[allow(dead_code)]
    desktop_target: windows::UI::Composition::Desktop::DesktopWindowTarget,
    root_visual: ContainerVisual,
    webview_visual: ContainerVisual,

    #[allow(dead_code)]
    environment: ICoreWebView2Environment,
    #[allow(dead_code)]
    composition_controller: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,

    capture_factory: D3D11SharedTextureFactory,
    capture_device: IDirect3DDevice,
    capture_state: Option<CaptureState>,
    persistent_dest: Option<PersistentDest>,

    // Persistent event queues drained by `poll_navigation_event` and
    // `poll_web_message`. Handler closures own clones of these `Arc`s and
    // push from the COM thread; consumer code drains from any thread.
    nav_event_queue: Arc<Mutex<VecDeque<NavigationEvent>>>,
    web_message_queue: Arc<Mutex<VecDeque<String>>>,
    cursor_queue: Arc<Mutex<VecDeque<CursorShape>>>,
    pending_cookies: Arc<Mutex<Option<Vec<Cookie>>>>,
    cookie_change_handler: Arc<Mutex<Option<WebView2CookieChangeHandlerFn>>>,
    nav_starting_token: i64,
    nav_completed_token: i64,
    source_changed_token: i64,
    title_changed_token: i64,
    web_message_token: i64,
    cursor_changed_token: i64,
}

/// A reusable shared D3D11 destination texture and its NT handle. The handle
/// is exposed exactly once via `WebView2CompositionFrame::shared_handle` (with
/// `resource_is_new = true`); subsequent frames reuse the same texture and
/// signal `resource_is_new = false`.
struct PersistentDest {
    texture: D3D11SharedTexture,
    size: PhysicalSize<u32>,
    handle_handed_off: bool,
}

struct CaptureState {
    #[allow(dead_code)]
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    first_frame_emitted: bool,
}

impl WebView2CompositionProducer {
    /// Build the composition tree, the WebView2 controller, and prepare for
    /// capture. Capture is not started until the first `acquire_frame` call.
    ///
    /// # Safety
    ///
    /// `parent_hwnd` must be a live top-level HWND for the lifetime of the
    /// returned producer.
    pub unsafe fn new(
        parent_hwnd: *mut std::ffi::c_void,
        config: WebView2CompositionConfig,
    ) -> Result<Self, WebSurfaceError> {
        if parent_hwnd.is_null() {
            return Err(WebSurfaceError::Platform(
                "parent HWND was null".to_string(),
            ));
        }
        if config.size.width == 0 || config.size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WebView2 producer size must be non-zero, got {}x{}",
                config.size.width, config.size.height
            )));
        }

        let parent_hwnd = HWND(parent_hwnd);

        let compositor = Compositor::new().map_err(platform("Compositor::new"))?;
        let desktop_interop: ICompositorDesktopInterop = compositor
            .cast()
            .map_err(platform("Compositor cast to ICompositorDesktopInterop"))?;
        let desktop_target =
            unsafe { desktop_interop.CreateDesktopWindowTarget(parent_hwnd, false) }
                .map_err(platform("CreateDesktopWindowTarget"))?;

        let root_visual = compositor
            .CreateContainerVisual()
            .map_err(platform("CreateContainerVisual (root)"))?;
        root_visual
            .SetOffset(Vector3 {
                X: config.offset.0,
                Y: config.offset.1,
                Z: 0.0,
            })
            .map_err(platform("ContainerVisual::SetOffset"))?;
        let visual_size = Vector2 {
            X: config.size.width as f32,
            Y: config.size.height as f32,
        };
        root_visual
            .SetSize(visual_size)
            .map_err(platform("ContainerVisual::SetSize (root)"))?;

        if let Some((r, g, b)) = config.diagnostic_backdrop {
            let sprite = compositor
                .CreateSpriteVisual()
                .map_err(platform("CreateSpriteVisual (diagnostic)"))?;
            sprite
                .SetSize(visual_size)
                .map_err(platform("SpriteVisual::SetSize"))?;
            let brush = compositor
                .CreateColorBrushWithColor(windows::UI::Color {
                    A: 255,
                    R: r,
                    G: g,
                    B: b,
                })
                .map_err(platform("CreateColorBrushWithColor"))?;
            sprite
                .SetBrush(&brush)
                .map_err(platform("SpriteVisual::SetBrush"))?;
            root_visual
                .Children()
                .map_err(platform("root.Children()"))?
                .InsertAtBottom(&sprite)
                .map_err(platform("Children::InsertAtBottom"))?;
        }

        let webview_visual = compositor
            .CreateContainerVisual()
            .map_err(platform("CreateContainerVisual (webview)"))?;
        webview_visual
            .SetSize(visual_size)
            .map_err(platform("ContainerVisual::SetSize (webview)"))?;
        root_visual
            .Children()
            .map_err(platform("root.Children() (webview)"))?
            .InsertAtTop(&webview_visual)
            .map_err(platform("Children::InsertAtTop (webview)"))?;
        desktop_target
            .SetRoot(&root_visual)
            .map_err(platform("DesktopWindowTarget::SetRoot"))?;

        let environment = create_environment(&config.user_data_dir)?;
        let composition_controller = create_composition_controller(&environment, parent_hwnd)?;
        unsafe {
            composition_controller
                .SetRootVisualTarget(&webview_visual)
                .map_err(platform("SetRootVisualTarget"))?;
        }

        let controller: ICoreWebView2Controller = composition_controller
            .cast()
            .map_err(platform("composition controller cast"))?;
        unsafe {
            controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: config.size.width as i32,
                    bottom: config.size.height as i32,
                })
                .map_err(platform("controller.SetBounds"))?;
            controller
                .SetIsVisible(true)
                .map_err(platform("controller.SetIsVisible"))?;
        }
        let webview =
            unsafe { controller.CoreWebView2() }.map_err(platform("controller.CoreWebView2"))?;

        let capture_factory = match config.fence_shared_handle {
            Some(handle) => D3D11SharedTextureFactory::new_hardware_with_fence(handle)?,
            None => D3D11SharedTextureFactory::new_hardware()?,
        };
        let capture_device = capture_factory.create_winrt_direct3d_device()?;

        let nav_event_queue: Arc<Mutex<VecDeque<NavigationEvent>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let web_message_queue: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let cursor_queue: Arc<Mutex<VecDeque<CursorShape>>> = Arc::new(Mutex::new(VecDeque::new()));
        let pending_cookies = Arc::new(Mutex::new(None));
        let cookie_change_handler = Arc::new(Mutex::new(None));

        install_cookie_change_bridge(&webview)?;

        let (
            nav_starting_token,
            nav_completed_token,
            source_changed_token,
            title_changed_token,
            web_message_token,
        ) = register_persistent_handlers(
            &webview,
            nav_event_queue.clone(),
            web_message_queue.clone(),
            cookie_change_handler.clone(),
        )?;
        let cursor_changed_token =
            register_cursor_changed_handler(&composition_controller, cursor_queue.clone())?;

        Ok(Self {
            parent_hwnd,
            size: config.size,
            generation: 0,
            compositor,
            desktop_target,
            root_visual,
            webview_visual,
            environment,
            composition_controller,
            controller,
            webview,
            capture_factory,
            capture_device,
            capture_state: None,
            persistent_dest: None,
            nav_event_queue,
            web_message_queue,
            cursor_queue,
            pending_cookies,
            cookie_change_handler,
            nav_starting_token,
            nav_completed_token,
            source_changed_token,
            title_changed_token,
            web_message_token,
            cursor_changed_token,
        })
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    /// Navigate the underlying WebView2 to an inline HTML document and block
    /// until `NavigationCompleted` fires (or the configured timeout elapses).
    pub fn navigate_to_string(
        &self,
        html: &str,
        timeout: Duration,
    ) -> Result<(), WebSurfaceError> {
        let (tx, rx) = mpsc::channel::<()>();
        let mut navigation_token = 0;
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, _args| {
            let _ = tx.send(());
            Ok(())
        }));

        unsafe {
            self.webview
                .add_NavigationCompleted(&handler, &mut navigation_token)
                .map_err(platform("add_NavigationCompleted"))?;
            let html = CoTaskMemPWSTR::from(html);
            self.webview
                .NavigateToString(*html.as_ref().as_pcwstr())
                .map_err(platform("NavigateToString"))?;
        }

        let result = pump_until(timeout, &rx);

        unsafe {
            let _ = self
                .webview
                .remove_NavigationCompleted(navigation_token)
                .map_err(webview2_com::Error::WindowsError);
        }

        result.map_err(|()| {
            WebSurfaceError::Platform(format!(
                "WebView2 navigation did not complete within {timeout:?}"
            ))
        })?;

        // Make sure at least one render tick has happened so the visual has
        // content before capture starts.
        self.wait_for_render_tick()
    }

    fn wait_for_render_tick(&self) -> Result<(), WebSurfaceError> {
        let script = r#"(() => new Promise(resolve => {
            requestAnimationFrame(() => requestAnimationFrame(() => resolve("present")));
        }))()"#
            .to_string();
        execute_script_blocking(&self.webview, script)
    }

    /// Navigate the underlying WebView2 to a URL and block until
    /// `NavigationCompleted` fires (or the timeout elapses). The
    /// completion signal is delivered via the persistent
    /// `NavigationCompleted` handler — drain
    /// [`Self::poll_navigation_event`] separately if the consumer also
    /// wants the structured event for UI state.
    pub fn navigate_to_url(&self, url: &str, timeout: Duration) -> Result<(), WebSurfaceError> {
        let (tx, rx) = mpsc::channel::<()>();
        let mut navigation_token = 0;
        let handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, _args| {
            let _ = tx.send(());
            Ok(())
        }));

        unsafe {
            self.webview
                .add_NavigationCompleted(&handler, &mut navigation_token)
                .map_err(platform("add_NavigationCompleted (navigate_to_url)"))?;
            let url = CoTaskMemPWSTR::from(url);
            self.webview
                .Navigate(*url.as_ref().as_pcwstr())
                .map_err(platform("Navigate"))?;
        }

        let result = pump_until(timeout, &rx);

        unsafe {
            let _ = self
                .webview
                .remove_NavigationCompleted(navigation_token)
                .map_err(webview2_com::Error::WindowsError);
        }

        result.map_err(|()| {
            WebSurfaceError::Platform(format!(
                "WebView2 navigation did not complete within {timeout:?}"
            ))
        })?;

        // Same render-tick wait as navigate_to_string so callers don't
        // see a blank visual immediately after navigation.
        self.wait_for_render_tick()
    }

    /// Forward a mouse / scroll event to the composition WebView2.
    ///
    /// `event.point` is in physical pixels relative to the webview's
    /// top-left corner (the same coordinate space the controller's
    /// `Bounds` uses).
    pub fn send_mouse_input(&self, event: MouseInput) -> Result<(), WebSurfaceError> {
        let kind = mouse_event_kind(event.kind);
        let virtual_keys =
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(virtual_keys_bits(event.virtual_keys) as i32);
        let point = POINT {
            x: event.point.0,
            y: event.point.1,
        };
        unsafe {
            self.composition_controller
                .SendMouseInput(kind, virtual_keys, event.mouse_data as u32, point)
                .map_err(platform("SendMouseInput"))
        }
    }

    /// Forward a touch / pen pointer event to the composition WebView2.
    ///
    /// Builds an `ICoreWebView2PointerInfo` from `event` and dispatches via
    /// `ICoreWebView2CompositionController::SendPointerInput`. Pen tilt is
    /// in radians on the public API; converted to degrees for WebView2.
    pub fn send_pointer_input(&self, event: crate::PointerInput) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment3;
        let env3: ICoreWebView2Environment3 = self
            .environment
            .cast()
            .map_err(platform("environment cast to ICoreWebView2Environment3"))?;
        let info = unsafe { env3.CreateCoreWebView2PointerInfo() }
            .map_err(platform("CreateCoreWebView2PointerInfo"))?;

        let pointer_kind: u32 = match event.device {
            crate::PointerDevice::Touch => 2,
            crate::PointerDevice::Pen => 3,
            crate::PointerDevice::Mouse => 4,
        };
        let pointer_flags = pointer_flags_for(event.kind);
        let point = POINT {
            x: event.point.0,
            y: event.point.1,
        };
        let mut perf_count: i64 = 0;
        unsafe {
            windows::Win32::System::Performance::QueryPerformanceCounter(&mut perf_count)
                .map_err(platform("QueryPerformanceCounter"))?;
        }

        unsafe {
            info.SetPointerKind(pointer_kind)
                .map_err(platform("SetPointerKind"))?;
            info.SetPointerId(event.pointer_id)
                .map_err(platform("SetPointerId"))?;
            info.SetFrameId(0).map_err(platform("SetFrameId"))?;
            info.SetPointerFlags(pointer_flags)
                .map_err(platform("SetPointerFlags"))?;
            info.SetPixelLocation(point)
                .map_err(platform("SetPixelLocation"))?;
            info.SetPixelLocationRaw(point)
                .map_err(platform("SetPixelLocationRaw"))?;
            info.SetHimetricLocation(POINT { x: 0, y: 0 })
                .map_err(platform("SetHimetricLocation"))?;
            info.SetHimetricLocationRaw(POINT { x: 0, y: 0 })
                .map_err(platform("SetHimetricLocationRaw"))?;
            info.SetPerformanceCount(perf_count as u64)
                .map_err(platform("SetPerformanceCount"))?;
            info.SetHistoryCount(1)
                .map_err(platform("SetHistoryCount"))?;
            info.SetButtonChangeKind(0)
                .map_err(platform("SetButtonChangeKind"))?;

            match event.device {
                crate::PointerDevice::Touch => {
                    // TOUCH_MASK_PRESSURE = 0x4
                    info.SetTouchMask(0x4).map_err(platform("SetTouchMask"))?;
                    // Pressure is 0..1024 in the WebView2 API.
                    let pressure = (event.pressure.clamp(0.0, 1.0) * 1024.0) as u32;
                    info.SetTouchPressure(pressure)
                        .map_err(platform("SetTouchPressure"))?;
                    let contact = RECT {
                        left: point.x - 1,
                        top: point.y - 1,
                        right: point.x + 1,
                        bottom: point.y + 1,
                    };
                    info.SetTouchContact(contact)
                        .map_err(platform("SetTouchContact"))?;
                    info.SetTouchContactRaw(contact)
                        .map_err(platform("SetTouchContactRaw"))?;
                }
                crate::PointerDevice::Pen => {
                    // PEN_MASK_PRESSURE = 0x1, PEN_MASK_TILT_X = 0x4, PEN_MASK_TILT_Y = 0x8
                    info.SetPenMask(0x1 | 0x4 | 0x8)
                        .map_err(platform("SetPenMask"))?;
                    let pressure = (event.pressure.clamp(0.0, 1.0) * 1024.0) as u32;
                    info.SetPenPressure(pressure)
                        .map_err(platform("SetPenPressure"))?;
                    // Tilt in the public API is radians; WebView2 wants
                    // degrees in -90..90.
                    let tilt_x_deg = event.tilt.0.to_degrees().clamp(-90.0, 90.0) as i32;
                    let tilt_y_deg = event.tilt.1.to_degrees().clamp(-90.0, 90.0) as i32;
                    info.SetPenTiltX(tilt_x_deg)
                        .map_err(platform("SetPenTiltX"))?;
                    info.SetPenTiltY(tilt_y_deg)
                        .map_err(platform("SetPenTiltY"))?;
                }
                crate::PointerDevice::Mouse => {
                    // No extra fields needed; WebView2 ignores touch/pen
                    // masks for mouse pointers.
                }
            }
        }

        let event_kind = pointer_event_kind(event.kind);
        unsafe {
            self.composition_controller
                .SendPointerInput(event_kind, &info)
                .map_err(platform("SendPointerInput"))
        }
    }

    /// Forward a drag-enter event to the composition WebView2 with an
    /// `IDataObject` carrying the dragged content. Hosts get the
    /// `IDataObject` from their `IDropTarget::DragEnter` callback (the OLE
    /// drag-and-drop pattern); scrying doesn't construct it.
    ///
    /// `key_state` is the Win32 `MK_*` modifier-key bitmask.
    /// `effects` is mutated in place: caller passes the allowed `DROPEFFECT_*`
    /// bits, WebView2 returns the chosen effect.
    pub fn drag_enter(
        &self,
        data_object: &windows::Win32::System::Com::IDataObject,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.DragEnter(data_object, key_state, p, effects as *mut u32)
                .map_err(platform("DragEnter"))
        }
    }

    /// Forward a drag-over event. Hosts call this on every
    /// `IDropTarget::DragOver` callback while the drag is over the webview.
    pub fn drag_over(
        &self,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.DragOver(key_state, p, effects as *mut u32)
                .map_err(platform("DragOver"))
        }
    }

    /// Forward a drag-leave event.
    pub fn drag_leave(&self) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        unsafe { cc3.DragLeave() }.map_err(platform("DragLeave"))
    }

    /// Forward a drop event. Same `IDataObject` shape as
    /// [`drag_enter`](Self::drag_enter).
    pub fn drop_data(
        &self,
        data_object: &windows::Win32::System::Com::IDataObject,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.Drop(data_object, key_state, p, effects as *mut u32)
                .map_err(platform("Drop"))
        }
    }

    /// Move keyboard focus into the WebView2.
    pub fn move_focus(&self, reason: FocusReason) -> Result<(), WebSurfaceError> {
        let reason = focus_reason(reason);
        unsafe {
            self.controller
                .MoveFocus(reason)
                .map_err(platform("MoveFocus"))
        }
    }

    /// Forward one raw Win32 keyboard / IME message to the WebView2 parent
    /// HWND. Hosts with access to their window procedure or message filter
    /// should call this for `WM_KEY*`, `WM_CHAR`, `WM_DEADCHAR`, and `WM_IME*`
    /// traffic while the WebView owns focus; it preserves native IME payloads
    /// that cannot be represented by [`KeyboardInput`].
    pub fn forward_keyboard_message(
        &self,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> Result<(), WebSurfaceError> {
        if !is_webview2_keyboard_message(message) {
            return Err(WebSurfaceError::Unsupported(
                "WebView2CompositionProducer::forward_keyboard_message only accepts WM_KEY*, WM_CHAR, WM_DEADCHAR, and WM_IME* messages",
            ));
        }
        self.post_keyboard_message(message, wparam, lparam)
    }

    /// Best-effort portable keyboard bridge. For full IME fidelity on Windows,
    /// prefer [`Self::forward_keyboard_message`] with the host's native Win32
    /// message stream.
    pub fn send_keyboard_input(&self, event: KeyboardInput) -> Result<(), WebSurfaceError> {
        let message = keyboard_message_for(&event);
        let lparam = keyboard_lparam(&event);
        self.post_keyboard_message(message, event.virtual_key_code as usize, lparam)?;

        if event.kind == KeyEventKind::Down && !event.characters.is_empty() {
            let char_message = if event.modifiers.alt {
                WM_SYSCHAR
            } else {
                WM_CHAR
            };
            for unit in event.characters.encode_utf16() {
                self.post_keyboard_message(char_message, unit as usize, lparam)?;
            }
        }
        Ok(())
    }

    fn post_keyboard_message(
        &self,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> Result<(), WebSurfaceError> {
        unsafe {
            PostMessageW(
                Some(self.parent_hwnd),
                message,
                WPARAM(wparam),
                LPARAM(lparam),
            )
        }
        .map_err(platform("PostMessageW keyboard message"))
    }

    /// Drain the next pending [`NavigationEvent`] from the producer's
    /// queue. Returns `None` when no event is available. Events are
    /// pushed FIFO from the COM thread by handlers registered in
    /// `new`.
    pub fn poll_navigation_event(&self) -> Option<NavigationEvent> {
        self.nav_event_queue.lock().ok()?.pop_front()
    }

    /// Post a string message into `window.chrome.webview` for the page's
    /// `addEventListener("message", ...)` handlers to consume.
    pub fn post_web_message(&self, message: &str) -> Result<(), WebSurfaceError> {
        let message = CoTaskMemPWSTR::from(message);
        unsafe {
            self.webview
                .PostWebMessageAsString(*message.as_ref().as_pcwstr())
                .map_err(platform("PostWebMessageAsString"))
        }
    }

    /// Drain the next pending message posted from JS via
    /// `window.chrome.webview.postMessage(...)`. Returns `None` when no
    /// message is queued.
    pub fn poll_web_message(&self) -> Option<String> {
        self.web_message_queue.lock().ok()?.pop_front()
    }

    /// Drain the next cursor-change request from the webview. Producers
    /// push a fresh [`CursorShape`] each time the engine's hovered
    /// element changes (e.g. anchor → pointer, text input → text).
    pub fn poll_cursor_shape(&self) -> Option<CursorShape> {
        self.cursor_queue.lock().ok()?.pop_front()
    }

    /// Reload the current page.
    pub fn reload(&self) -> Result<(), WebSurfaceError> {
        unsafe { self.webview.Reload() }.map_err(platform("Reload"))
    }

    /// Stop the current navigation, if any.
    pub fn stop(&self) -> Result<(), WebSurfaceError> {
        unsafe { self.webview.Stop() }.map_err(platform("Stop"))
    }

    /// Navigate one entry back in session history. Returns `Ok(false)`
    /// if the back stack is empty.
    pub fn go_back(&self) -> Result<bool, WebSurfaceError> {
        if !self.can_go_back() {
            return Ok(false);
        }
        unsafe { self.webview.GoBack() }.map_err(platform("GoBack"))?;
        Ok(true)
    }

    /// Navigate one entry forward in session history. Returns
    /// `Ok(false)` if the forward stack is empty.
    pub fn go_forward(&self) -> Result<bool, WebSurfaceError> {
        if !self.can_go_forward() {
            return Ok(false);
        }
        unsafe { self.webview.GoForward() }.map_err(platform("GoForward"))?;
        Ok(true)
    }

    /// Whether the back stack currently has at least one entry.
    pub fn can_go_back(&self) -> bool {
        let mut value = windows::core::BOOL::default();
        unsafe { self.webview.CanGoBack(&mut value) }
            .ok()
            .map(|()| value.as_bool())
            .unwrap_or(false)
    }

    /// Whether the forward stack currently has at least one entry.
    pub fn can_go_forward(&self) -> bool {
        let mut value = windows::core::BOOL::default();
        unsafe { self.webview.CanGoForward(&mut value) }
            .ok()
            .map(|()| value.as_bool())
            .unwrap_or(false)
    }

    /// Open the WebView2 DevTools window.
    pub fn open_devtools_window(&self) -> Result<(), WebSurfaceError> {
        unsafe { self.webview.OpenDevToolsWindow() }.map_err(platform("OpenDevToolsWindow"))
    }

    /// Toggle WebView2's page visibility / occlusion state. Browser-shape
    /// consumers call this as tabs become active or inactive.
    pub fn set_visible(&self, visible: bool) -> Result<(), WebSurfaceError> {
        unsafe { self.controller.SetIsVisible(visible) }
            .map_err(platform("controller.SetIsVisible"))
    }

    /// Apply a partial settings update. `None` fields are left at their
    /// current value.
    pub fn apply_settings(
        &self,
        settings: &crate::WebSurfaceSettings,
    ) -> Result<(), WebSurfaceError> {
        if let Some(zoom) = settings.zoom_factor {
            unsafe { self.controller.SetZoomFactor(zoom) }
                .map_err(platform("controller.SetZoomFactor"))?;
        }
        let webview_settings: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings =
            unsafe { self.webview.Settings() }.map_err(platform("webview.Settings"))?;
        if let Some(enabled) = settings.javascript_enabled {
            unsafe { webview_settings.SetIsScriptEnabled(enabled) }
                .map_err(platform("Settings.SetIsScriptEnabled"))?;
        }
        if let Some(enabled) = settings.devtools_enabled {
            unsafe { webview_settings.SetAreDevToolsEnabled(enabled) }
                .map_err(platform("Settings.SetAreDevToolsEnabled"))?;
        }
        if let Some(enabled) = settings.default_context_menus_enabled {
            unsafe { webview_settings.SetAreDefaultContextMenusEnabled(enabled) }
                .map_err(platform("Settings.SetAreDefaultContextMenusEnabled"))?;
        }
        if let Some(enabled) = settings.builtin_accelerator_keys_enabled {
            let settings3: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3 =
                webview_settings
                    .cast()
                    .map_err(platform("Settings cast to ICoreWebView2Settings3"))?;
            unsafe { settings3.SetAreBrowserAcceleratorKeysEnabled(enabled) }
                .map_err(platform("Settings3.SetAreBrowserAcceleratorKeysEnabled"))?;
        }
        if let Some(ref ua) = settings.user_agent {
            let settings2: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings2 =
                webview_settings
                    .cast()
                    .map_err(platform("Settings cast to ICoreWebView2Settings2"))?;
            let ua = CoTaskMemPWSTR::from(ua.as_str());
            unsafe { settings2.SetUserAgent(*ua.as_ref().as_pcwstr()) }
                .map_err(platform("Settings2.SetUserAgent"))?;
        }
        Ok(())
    }

    /// Kick off an async fetch of every cookie in the WebView2 profile's
    /// cookie manager. Drain via [`Self::poll_cookies`].
    pub fn request_all_cookies(&mut self) -> Result<(), WebSurfaceError> {
        let manager = self.cookie_manager()?;
        let slot = self.pending_cookies.clone();
        let handler = GetCookiesCompletedHandler::create(Box::new(move |result, cookie_list| {
            result?;
            if let Some(cookie_list) = cookie_list {
                match unsafe { cookies_from_webview2_list(&cookie_list) } {
                    Ok(cookies) => {
                        if let Ok(mut pending) = slot.lock() {
                            *pending = Some(cookies);
                        }
                    }
                    Err(error) => {
                        eprintln!("scrying: WebView2 cookie conversion failed: {error}");
                    }
                }
            }
            Ok(())
        }));
        unsafe { manager.GetCookies(PCWSTR::null(), &handler) }
            .map_err(platform("CookieManager.GetCookies"))
    }

    /// Drain the most recent [`Self::request_all_cookies`] result.
    pub fn poll_cookies(&mut self) -> Option<Vec<Cookie>> {
        self.pending_cookies.lock().ok().and_then(|mut s| s.take())
    }

    /// Set / overwrite a cookie in the WebView2 profile's cookie manager.
    pub fn set_cookie(&mut self, cookie: &Cookie) -> Result<(), WebSurfaceError> {
        let manager = self.cookie_manager()?;
        let webview_cookie = unsafe { webview2_cookie_from(&manager, cookie)? };
        unsafe { manager.AddOrUpdateCookie(&webview_cookie) }
            .map_err(platform("CookieManager.AddOrUpdateCookie"))?;
        self.notify_cookie_changed();
        Ok(())
    }

    /// Delete a cookie by name + domain + path.
    pub fn delete_cookie(
        &mut self,
        name: &str,
        domain: &str,
        path: &str,
    ) -> Result<(), WebSurfaceError> {
        let manager = self.cookie_manager()?;
        let name = CoTaskMemPWSTR::from(name);
        let domain = CoTaskMemPWSTR::from(domain);
        let path = CoTaskMemPWSTR::from(path);
        unsafe {
            manager.DeleteCookiesWithDomainAndPath(
                *name.as_ref().as_pcwstr(),
                *domain.as_ref().as_pcwstr(),
                *path.as_ref().as_pcwstr(),
            )
        }
        .map_err(platform("CookieManager.DeleteCookiesWithDomainAndPath"))?;
        self.notify_cookie_changed();
        Ok(())
    }

    /// Register a best-effort cookie-change callback. This fires for host
    /// `set_cookie` / `delete_cookie` calls and for page-side `document.cookie`
    /// writes observed by scrying's document-start script. The WebView2
    /// bindings used here do not expose a native `Set-Cookie` response-header
    /// observer, so consumers that need network-cookie deltas should pair this
    /// with periodic [`Self::request_all_cookies`] calls.
    pub fn set_cookie_change_handler(
        &mut self,
        handler: WebView2CookieChangeHandlerFn,
    ) -> Result<(), WebSurfaceError> {
        let mut slot = self.cookie_change_handler.lock().map_err(|_| {
            WebSurfaceError::Platform("cookie_change_handler lock poisoned".into())
        })?;
        *slot = Some(handler);
        Ok(())
    }

    pub fn clear_cookie_change_handler(&mut self) -> Result<(), WebSurfaceError> {
        let mut slot = self.cookie_change_handler.lock().map_err(|_| {
            WebSurfaceError::Platform("cookie_change_handler lock poisoned".into())
        })?;
        *slot = None;
        Ok(())
    }

    fn cookie_manager(&self) -> Result<ICoreWebView2CookieManager, WebSurfaceError> {
        let webview2: ICoreWebView2_2 = self
            .webview
            .cast()
            .map_err(platform("webview cast to ICoreWebView2_2"))?;
        unsafe { webview2.CookieManager() }.map_err(platform("webview.CookieManager"))
    }

    fn notify_cookie_changed(&self) {
        if let Ok(slot) = self.cookie_change_handler.lock()
            && let Some(handler) = slot.as_ref()
        {
            handler();
        }
    }

    /// Take a one-shot PNG snapshot of the current document via
    /// `ICoreWebView2::CapturePreview`. Returns the encoded PNG bytes.
    /// The webview must have completed at least one navigation; calling
    /// this against a newly-constructed producer that has not navigated
    /// yields an empty / failed snapshot.
    pub fn capture_snapshot_png(&self) -> Result<Vec<u8>, WebSurfaceError> {
        let stream: IStream =
            unsafe { CreateStreamOnHGlobal(windows::Win32::Foundation::HGLOBAL::default(), true) }
                .map_err(platform("CreateStreamOnHGlobal"))?;
        let (tx, rx) = mpsc::channel::<windows::core::Result<()>>();
        let handler = webview2_com::CapturePreviewCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>| {
                let _ = tx.send(result);
                Ok(())
            },
        ));
        unsafe {
            self.webview
                .CapturePreview(
                    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                    &stream,
                    &handler,
                )
                .map_err(platform("CapturePreview"))?;
        }

        // Pump messages until the async completion handler fires. We
        // don't accept a timeout here because PNG snapshot is a
        // one-shot diagnostic; consumers that need bounded latency
        // should wrap this in their own timeout / thread.
        loop {
            pump_messages_for(Duration::from_millis(16));
            match rx.try_recv() {
                Ok(result) => {
                    result.map_err(platform("CapturePreview completion"))?;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(WebSurfaceError::Platform(
                        "CapturePreview completion channel closed unexpectedly".into(),
                    ));
                }
            }
        }

        // Read the stream's HGLOBAL contents into a Vec<u8>.
        unsafe {
            let hglobal =
                GetHGlobalFromStream(&stream).map_err(platform("GetHGlobalFromStream"))?;
            let size = GlobalSize(hglobal);
            if size == 0 {
                return Ok(Vec::new());
            }
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return Err(WebSurfaceError::Platform(
                    "GlobalLock returned null".into(),
                ));
            }
            let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
            let _ = GlobalUnlock(hglobal);
            Ok(bytes)
        }
    }

    /// Tear down the capture session + frame pool. The next call to
    /// `try_acquire_frame` will run `start_capture` against the current visual
    /// state, allocating a fresh `GraphicsCaptureItem`.
    ///
    /// Use this when the consumer detects that frame emission has stalled
    /// (e.g. enough consecutive `Ok(None)` polls to suggest WGC has lost track
    /// of the visual after rapid resize cycling). Persistent destination state
    /// is intentionally preserved — the consumer keeps its imported texture
    /// and only re-imports if `ContentSize` changes.
    pub fn force_restart_capture(&mut self) {
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
        }
    }

    /// Drop the cached shared D3D11 destination texture so the next
    /// `try_acquire_frame` allocates a fresh one and signals
    /// `resource_is_new = true`.
    ///
    /// This is the consumer-driven escape hatch for D3D12-side cache
    /// staleness on the externally-written shared texture: by forcing a
    /// re-import (new `ID3D12Resource` from a fresh NT handle, new
    /// `wgpu::Texture` and bind group), the consumer flushes whatever
    /// driver-level caching was holding the previous frame's pixels.
    pub fn invalidate_persistent_dest(&mut self) {
        self.persistent_dest = None;
    }

    /// Reposition the root visual relative to the parent window, in physical
    /// pixels. The capture region follows the visual.
    pub fn set_offset(&self, x: f32, y: f32) -> Result<(), WebSurfaceError> {
        self.root_visual
            .SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })
            .map_err(platform("root.SetOffset"))
    }

    /// Resize the WebView visual, controller bounds, and capture frame pool.
    pub fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WebSurfaceError> {
        if size.width == 0 || size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WebView2 producer resize must be non-zero, got {}x{}",
                size.width, size.height
            )));
        }
        if size == self.size {
            return Ok(());
        }
        eprintln!(
            "[producer] resize: {}x{} -> {}x{}",
            self.size.width, self.size.height, size.width, size.height
        );

        let visual_size = Vector2 {
            X: size.width as f32,
            Y: size.height as f32,
        };
        self.root_visual
            .SetSize(visual_size)
            .map_err(platform("root.SetSize"))?;
        self.webview_visual
            .SetSize(visual_size)
            .map_err(platform("webview_visual.SetSize"))?;
        unsafe {
            self.controller
                .SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: size.width as i32,
                    bottom: size.height as i32,
                })
                .map_err(platform("controller.SetBounds"))?;
        }

        // `Direct3D11CaptureFramePool::Recreate` does not reliably resume frame
        // emission against a resized WinComp visual: in practice it produces
        // exactly one frame at the new size and then goes silent. Tear the
        // session + pool down here so the next `try_acquire_frame` calls
        // `start_capture()` against a fresh `GraphicsCaptureItem` derived from
        // the resized visual, with a fresh frame budget and a re-armed nudge.
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
        }

        // Drop the persistent destination so the next capture allocates a
        // freshly-sized texture and re-issues a shared NT handle. The consumer
        // sees `resource_is_new = true` and can re-import on its side.
        self.persistent_dest = None;

        self.size = size;
        Ok(())
    }

    /// Acquire the next capture frame, returning the full producer-side
    /// frame (including the platform-specific shared NT handle and the
    /// `resource_is_new` reuse hint).
    ///
    /// The first call lazily starts the capture session and runs a
    /// one-shot content nudge so WebView2 issues a fresh paint that WGC
    /// will observe.
    ///
    /// Generic consumers can use [`Self::acquire_frame`] (the
    /// `WebSurfaceProducer` trait method) for the platform-agnostic
    /// view of the same frame.
    pub fn acquire_full_frame(&mut self) -> Result<WebView2CompositionFrame, WebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }
        let timeout = Duration::from_secs(2);
        self.acquire_frame_with_timeout(timeout)
    }

    /// Non-blocking variant of `acquire_frame`: poll the frame pool exactly
    /// once. Returns `Ok(None)` when no new frame is ready, leaving the
    /// capture session running for the next call.
    ///
    /// This is the per-render-frame entry point in steady-state: call it
    /// every redraw, swap the consumer's bound texture only when `Some` is
    /// returned, and otherwise reuse the previous frame.
    ///
    /// On the first call after `start_capture()` (initial capture or
    /// post-`resize`) the WGC pool can take several compositor ticks to begin
    /// emitting frames against the freshly-bound visual; observed in practice,
    /// a non-blocking poll right after `nudge_content` returns can race ahead
    /// and miss the first emission, leaving the consumer stuck on stale
    /// content. Block briefly here on the first attempt so the post-resize
    /// re-import reliably lands.
    pub fn try_acquire_frame(
        &mut self,
    ) -> Result<Option<WebView2CompositionFrame>, WebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }

        let needs_nudge = self
            .capture_state
            .as_ref()
            .map(|state| !state.first_frame_emitted)
            .unwrap_or(true);
        if needs_nudge {
            let _ = self.nudge_content(FIRST_FRAME_NUDGE_LABEL);
        }

        // Intentionally do NOT pump messages here in steady state. winit's
        // run-app loop is already pumping on this thread, and during a Win32
        // modal resize loop, peeking with `PM_REMOVE` from a render call
        // steals drag-tracking messages from the modal loop and causes
        // re-entrant `DispatchMessage` hangs. The first-frame block below
        // reinstates pumping for the post-`start_capture` warmup.

        let first_frame_deadline = if needs_nudge {
            Some(Instant::now() + Duration::from_millis(500))
        } else {
            None
        };

        let block_started = Instant::now();
        loop {
            let state = self
                .capture_state
                .as_mut()
                .expect("capture state populated above");
            match state.pool.TryGetNextFrame() {
                Ok(frame) => {
                    let captured = self.capture_frame_to_shared(frame)?;
                    return Ok(Some(captured));
                }
                Err(_) => match first_frame_deadline {
                    Some(deadline) if Instant::now() < deadline => {
                        // Pump messages so WebView2's composition commits
                        // propagate into the WGC pool.
                        pump_messages_for(Duration::from_millis(16));
                        continue;
                    }
                    Some(_) => {
                        eprintln!(
                            "[producer] first-frame block: TIMED OUT after {}ms",
                            block_started.elapsed().as_millis()
                        );
                        return Ok(None);
                    }
                    None => return Ok(None),
                },
            }
        }
    }

    /// Acquire the next capture frame with a caller-controlled timeout.
    pub fn acquire_frame_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<WebView2CompositionFrame, WebSurfaceError> {
        if self.capture_state.is_none() {
            self.start_capture()?;
        }
        let needs_nudge = self
            .capture_state
            .as_ref()
            .map(|state| !state.first_frame_emitted)
            .unwrap_or(true);
        if needs_nudge {
            // Best-effort: a nudge failure should not abort the capture, since
            // WebView2 may still emit a frame on its own.
            let _ = self.nudge_content(FIRST_FRAME_NUDGE_LABEL);
        }

        let state = self
            .capture_state
            .as_mut()
            .expect("capture state populated above");

        let deadline = Instant::now() + timeout;
        let frame = loop {
            match state.pool.TryGetNextFrame() {
                Ok(frame) => break frame,
                Err(_) if Instant::now() < deadline => {
                    // Pump messages: WebView2's composition commits drive the
                    // WGC pool, and those commits propagate via Windows
                    // messages on this thread. (`start_capture` uses a plain
                    // sleep instead because dispatch-during-init has been
                    // observed to hang re-entrantly there.)
                    pump_messages_for(Duration::from_millis(16));
                }
                Err(error) => {
                    return Err(WebSurfaceError::Platform(format!(
                        "TryGetNextFrame timed out after {timeout:?} for {}x{}: {error}",
                        self.size.width, self.size.height
                    )));
                }
            }
        };

        self.capture_frame_to_shared(frame)
    }

    fn capture_frame_to_shared(
        &mut self,
        frame: windows::Graphics::Capture::Direct3D11CaptureFrame,
    ) -> Result<WebView2CompositionFrame, WebSurfaceError> {
        let content_size = frame
            .ContentSize()
            .map_err(platform("Direct3D11CaptureFrame::ContentSize"))?;
        let surface = frame
            .Surface()
            .map_err(platform("Direct3D11CaptureFrame::Surface"))?;
        let access = surface
            .cast::<IDirect3DDxgiInterfaceAccess>()
            .map_err(platform(
                "IDirect3DSurface cast to IDirect3DDxgiInterfaceAccess",
            ))?;
        let texture = unsafe { access.GetInterface::<ID3D11Texture2D>() }
            .map_err(platform("GetInterface<ID3D11Texture2D>"))?;
        let raw_texture = Interface::as_raw(&texture);

        self.generation = self.generation.saturating_add(1);
        let captured_size =
            PhysicalSize::new(content_size.Width as u32, content_size.Height as u32);

        let allocated_now = self.ensure_persistent_dest(captured_size)?;
        let dest = self
            .persistent_dest
            .as_mut()
            .expect("persistent_dest populated above");

        let fence_value = self.capture_factory.copy_capture_into_existing_target(
            &dest.texture.texture,
            WebView2D3D11CaptureFrame {
                size: captured_size,
                format: wgpu::TextureFormat::Bgra8Unorm,
                generation: self.generation,
                raw_d3d11_texture: raw_texture,
            },
        )?;

        let _ = frame.Close();

        if let Some(state) = self.capture_state.as_mut() {
            state.first_frame_emitted = true;
        }

        // The shared handle is only meaningful when the consumer has not yet
        // imported the current allocation. Hand it off exactly once, then null
        // it on every later frame so the consumer reliably reuses its
        // previously-imported `wgpu::Texture`.
        let resource_is_new = allocated_now || !dest.handle_handed_off;
        let shared_handle = if resource_is_new {
            dest.handle_handed_off = true;
            dest.texture.shared_frame.shared_handle
        } else {
            std::ptr::null_mut()
        };

        let surface_frame = WebView2DxgiSharedHandleFrame {
            size: captured_size,
            format: wgpu::TextureFormat::Bgra8Unorm,
            generation: self.generation,
            shared_handle,
            producer_sync: self.capture_factory.sync_mechanism(),
            fence_value,
        }
        .into_surface_frame();

        Ok(WebView2CompositionFrame {
            frame: surface_frame,
            content_size: captured_size,
            generation: self.generation,
            shared_handle,
            resource_is_new,
        })
    }

    fn ensure_persistent_dest(
        &mut self,
        size: PhysicalSize<u32>,
    ) -> Result<bool, WebSurfaceError> {
        if self
            .persistent_dest
            .as_ref()
            .map(|dest| dest.size == size)
            .unwrap_or(false)
        {
            return Ok(false);
        }

        // Re-allocating; drop the old D3D11 texture (the consumer's wgpu
        // texture, opened from the old NT handle, keeps that allocation alive
        // until the consumer drops it).
        self.persistent_dest = None;

        let texture = self.capture_factory.create_shared_texture(
            size,
            wgpu::TextureFormat::Bgra8Unorm,
            self.generation,
        )?;
        self.persistent_dest = Some(PersistentDest {
            texture,
            size,
            handle_handed_off: false,
        });
        Ok(true)
    }

    fn start_capture(&mut self) -> Result<(), WebSurfaceError> {
        let started = Instant::now();
        if !GraphicsCaptureSession::IsSupported()
            .map_err(platform("GraphicsCaptureSession::IsSupported"))?
        {
            return Err(WebSurfaceError::Unsupported(
                "Windows.Graphics.Capture is not supported in this session",
            ));
        }

        // Give the WebView2 compositor time to commit *content* into the
        // visual before we bind a `GraphicsCaptureItem` to it. With a too-
        // short wait, the first WGC frame is the initial fully-transparent
        // buffer (BGRA all zeros) and any content-pixel validation fails.
        //
        // We deliberately do *not* pump Windows messages here: dispatching
        // mid-call has been observed to occasionally hang on a re-entrant
        // WebView2/WGC handler. Compositor commits run on a separate
        // thread, so a plain sleep is enough — we just need to wait long
        // enough for at least one WebView2 paint to land in the visual.
        std::thread::sleep(Duration::from_millis(500));

        let visual: Visual = self
            .webview_visual
            .cast()
            .map_err(platform("webview_visual cast to Visual"))?;
        let item = GraphicsCaptureItem::CreateFromVisual(&visual)
            .map_err(platform("GraphicsCaptureItem::CreateFromVisual"))?;
        let item_size = item.Size().map_err(platform("GraphicsCaptureItem::Size"))?;
        if item_size.Width <= 0 || item_size.Height <= 0 {
            return Err(WebSurfaceError::Platform(format!(
                "GraphicsCaptureItem returned invalid size {}x{}",
                item_size.Width, item_size.Height
            )));
        }

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &self.capture_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            item_size,
        )
        .map_err(platform("Direct3D11CaptureFramePool::CreateFreeThreaded"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(platform("CreateCaptureSession"))?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture().map_err(platform("StartCapture"))?;

        self.capture_state = Some(CaptureState {
            item,
            pool,
            session,
            first_frame_emitted: false,
        });
        eprintln!(
            "[producer] start_capture: {}x{} ready in {}ms",
            item_size.Width,
            item_size.Height,
            started.elapsed().as_millis()
        );
        Ok(())
    }

    /// Inject a small JavaScript repaint hint after a capture-state change
    /// (e.g. just after `StartCapture`). Composition-controller WebView2s do
    /// not always issue a fresh paint until something invalidates layout.
    pub fn nudge_content(&self, label: &str) -> Result<(), WebSurfaceError> {
        let safe_label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
            .collect();
        let script = format!(
            r#"(() => new Promise(resolve => {{
                document.body.dataset.captureNudge = "{safe_label}";
                document.body.style.boxShadow = `inset 0 0 0 4px rgb(${{Math.floor(Math.random() * 255)}}, 190, 112)`;
                requestAnimationFrame(() => requestAnimationFrame(() => resolve("nudged")));
            }}))()"#
        );
        execute_script_blocking(&self.webview, script)
    }

    /// Direct access to the underlying `ICoreWebView2` for callers that need
    /// to attach event handlers, post Web messages, or invoke JS.
    pub fn webview(&self) -> &ICoreWebView2 {
        &self.webview
    }

    /// Direct access to the underlying `ICoreWebView2Controller`.
    pub fn controller(&self) -> &ICoreWebView2Controller {
        &self.controller
    }
}

impl Drop for WebView2CompositionProducer {
    fn drop(&mut self) {
        if let Some(state) = self.capture_state.take() {
            let _ = state.session.Close();
            let _ = state.pool.Close();
            let _ = state;
        }
        unsafe {
            let _ = self
                .webview
                .remove_NavigationStarting(self.nav_starting_token);
            let _ = self
                .webview
                .remove_NavigationCompleted(self.nav_completed_token);
            let _ = self.webview.remove_SourceChanged(self.source_changed_token);
            let _ = self
                .webview
                .remove_DocumentTitleChanged(self.title_changed_token);
            let _ = self
                .webview
                .remove_WebMessageReceived(self.web_message_token);
            let _ = self
                .composition_controller
                .remove_CursorChanged(self.cursor_changed_token);
            let _ = self.controller.Close();
        }
    }
}

impl crate::WebSurfaceProducer for WebView2CompositionProducer {
    fn capabilities(&self) -> WebSurfaceCapabilities {
        // Windows can produce a `Dx12SharedTexture` whenever the host's
        // wgpu device is on the DX12 backend; the host context isn't
        // visible from inside the producer, so we report the shape we
        // actually emit (`Dx12SharedTexture` frames) and leave the
        // host-backend match-up to the consumer's import call.
        WebSurfaceCapabilities {
            backend: SystemWebviewBackend::WebView2,
            preferred_mode: WebSurfaceMode::ImportedTexture,
            imported_texture: crate::native_frame::CapabilityStatus::Supported,
            native_child_overlay: crate::native_frame::CapabilityStatus::Supported,
            cpu_snapshot: crate::native_frame::CapabilityStatus::Supported,
            supported_frames: vec![crate::native_frame::NativeFrameKind::Dx12SharedTexture],
            reason: "WebView2 CompositionController visual + Windows.Graphics.Capture + shared D3D11 NT-handle texture imported as Dx12SharedTexture.",
        }
    }

    fn acquire_frame(&mut self) -> Result<WebSurfaceFrame, WebSurfaceError> {
        let full = self.acquire_full_frame()?;
        Ok(full.frame)
    }

    fn navigate_to_string(
        &mut self,
        html: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::navigate_to_string(self, html, timeout)
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::resize(self, size)
    }

    fn set_offset(&mut self, x: f32, y: f32) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::set_offset(self, x, y)
    }

    fn navigate_to_url(
        &mut self,
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::navigate_to_url(self, url, timeout)
    }

    fn send_mouse_input(&mut self, event: MouseInput) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::send_mouse_input(self, event)
    }

    fn move_focus(&mut self, reason: FocusReason) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::move_focus(self, reason)
    }

    fn send_keyboard_input(&mut self, event: KeyboardInput) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::send_keyboard_input(self, event)
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        WebView2CompositionProducer::poll_navigation_event(self)
    }

    fn post_web_message(&mut self, message: &str) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::post_web_message(self, message)
    }

    fn poll_web_message(&mut self) -> Option<String> {
        WebView2CompositionProducer::poll_web_message(self)
    }

    fn capture_snapshot_png(&mut self) -> Result<Vec<u8>, WebSurfaceError> {
        WebView2CompositionProducer::capture_snapshot_png(self)
    }

    fn reload(&mut self) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::reload(self)
    }

    fn stop(&mut self) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::stop(self)
    }

    fn go_back(&mut self) -> Result<bool, WebSurfaceError> {
        WebView2CompositionProducer::go_back(self)
    }

    fn go_forward(&mut self) -> Result<bool, WebSurfaceError> {
        WebView2CompositionProducer::go_forward(self)
    }

    fn can_go_back(&self) -> bool {
        WebView2CompositionProducer::can_go_back(self)
    }

    fn can_go_forward(&self) -> bool {
        WebView2CompositionProducer::can_go_forward(self)
    }

    fn open_devtools_window(&mut self) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::open_devtools_window(self)
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::set_visible(self, visible)
    }

    fn apply_settings(
        &mut self,
        settings: &crate::WebSurfaceSettings,
    ) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::apply_settings(self, settings)
    }

    fn poll_cursor_shape(&mut self) -> Option<CursorShape> {
        WebView2CompositionProducer::poll_cursor_shape(self)
    }

    fn send_pointer_input(&mut self, event: crate::PointerInput) -> Result<(), WebSurfaceError> {
        WebView2CompositionProducer::send_pointer_input(self, event)
    }
}

fn create_environment(
    user_data_dir: &Path,
) -> Result<ICoreWebView2Environment, WebSurfaceError> {
    if let Err(error) = std::fs::create_dir_all(user_data_dir) {
        return Err(WebSurfaceError::Platform(format!(
            "create user_data_dir {}: {error}",
            user_data_dir.display()
        )));
    }
    let user_data_dir = user_data_dir.to_string_lossy().into_owned();

    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| {
            let user_data_dir = CoTaskMemPWSTR::from(user_data_dir.as_str());
            let options = CoreWebView2EnvironmentOptions::default();
            unsafe {
                webview2_com::Microsoft::Web::WebView2::Win32::CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    *user_data_dir.as_ref().as_pcwstr(),
                    &ICoreWebView2EnvironmentOptions::from(options),
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }
        }),
        Box::new(move |error_code, environment| {
            error_code?;
            tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send over mpsc channel");
            Ok(())
        }),
    )
    .map_err(|error| WebSurfaceError::Platform(format!("CreateCoreWebView2Environment: {error}")))?;

    rx.recv()
        .map_err(|_| {
            WebSurfaceError::Platform(
                "CreateCoreWebView2Environment completion channel closed".to_string(),
            )
        })?
        .map_err(platform("CreateCoreWebView2Environment result"))
}

fn create_composition_controller(
    environment: &ICoreWebView2Environment,
    parent_hwnd: HWND,
) -> Result<ICoreWebView2CompositionController, WebSurfaceError> {
    let environment3: ICoreWebView2Environment3 = environment
        .cast()
        .map_err(platform("environment cast to ICoreWebView2Environment3"))?;
    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            environment3
                .CreateCoreWebView2CompositionController(parent_hwnd, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error_code, controller| {
            error_code?;
            tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)))
                .expect("send over mpsc channel");
            Ok(())
        }),
    )
    .map_err(|error| {
        WebSurfaceError::Platform(format!("CreateCoreWebView2CompositionController: {error}"))
    })?;

    rx.recv()
        .map_err(|_| {
            WebSurfaceError::Platform(
                "CreateCoreWebView2CompositionController completion channel closed".to_string(),
            )
        })?
        .map_err(platform("CreateCoreWebView2CompositionController result"))
}

fn execute_script_blocking(
    webview: &ICoreWebView2,
    script: String,
) -> Result<(), WebSurfaceError> {
    let webview = webview.clone();
    ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            let script = CoTaskMemPWSTR::from(script.as_str());
            webview
                .ExecuteScript(*script.as_ref().as_pcwstr(), &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(|error_code, _result| error_code),
    )
    .map_err(|error| WebSurfaceError::Platform(format!("ExecuteScript: {error}")))
}

fn add_script_to_execute_on_document_created_blocking(
    webview: &ICoreWebView2,
    script: String,
) -> Result<(), WebSurfaceError> {
    let webview = webview.clone();
    AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            let script = CoTaskMemPWSTR::from(script.as_str());
            webview
                .AddScriptToExecuteOnDocumentCreated(*script.as_ref().as_pcwstr(), &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(|error_code, _script_id| error_code),
    )
    .map_err(|error| {
        WebSurfaceError::Platform(format!("AddScriptToExecuteOnDocumentCreated: {error}"))
    })
}

fn install_cookie_change_bridge(webview: &ICoreWebView2) -> Result<(), WebSurfaceError> {
    let script = format!(
        r#"(() => {{
            if (window.__scryingCookieBridgeInstalled) return;
            Object.defineProperty(window, "__scryingCookieBridgeInstalled", {{ value: true }});
            const notify = () => {{
                try {{ window.chrome.webview.postMessage({message:?}); }} catch (_) {{}}
            }};
            let proto = Document.prototype;
            let descriptor = Object.getOwnPropertyDescriptor(proto, "cookie");
            if (!descriptor || !descriptor.configurable || !descriptor.get || !descriptor.set) return;
            Object.defineProperty(proto, "cookie", {{
                configurable: true,
                enumerable: descriptor.enumerable,
                get() {{ return descriptor.get.call(this); }},
                set(value) {{
                    const result = descriptor.set.call(this, value);
                    notify();
                    return result;
                }},
            }});
        }})()"#,
        message = COOKIE_CHANGE_BRIDGE_MESSAGE,
    );
    add_script_to_execute_on_document_created_blocking(webview, script)
}

fn pump_messages_for(duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn pump_until(timeout: Duration, rx: &mpsc::Receiver<()>) -> Result<(), ()> {
    let deadline = Instant::now() + timeout;
    loop {
        if rx.try_recv().is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(());
        }
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                if rx.try_recv().is_ok() {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn platform<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> WebSurfaceError {
    move |error| WebSurfaceError::Platform(format!("{context} failed: {error}"))
}

unsafe fn consume_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

unsafe fn webview2_cookie_string(
    cookie: &ICoreWebView2Cookie,
    read: unsafe fn(&ICoreWebView2Cookie, *mut PWSTR) -> windows::core::Result<()>,
) -> Result<String, WebSurfaceError> {
    let mut value = PWSTR::null();
    unsafe { read(cookie, &mut value) }.map_err(platform("ICoreWebView2Cookie string field"))?;
    Ok(unsafe { consume_pwstr(value) })
}

unsafe fn cookie_from_webview2(cookie: &ICoreWebView2Cookie) -> Result<Cookie, WebSurfaceError> {
    let name = unsafe { webview2_cookie_string(cookie, ICoreWebView2Cookie::Name)? };
    let value = unsafe { webview2_cookie_string(cookie, ICoreWebView2Cookie::Value)? };
    let domain = unsafe { webview2_cookie_string(cookie, ICoreWebView2Cookie::Domain)? };
    let path = unsafe { webview2_cookie_string(cookie, ICoreWebView2Cookie::Path)? };
    let mut expires = 0.0;
    unsafe { cookie.Expires(&mut expires) }.map_err(platform("ICoreWebView2Cookie.Expires"))?;
    let mut is_session = windows::core::BOOL::default();
    unsafe { cookie.IsSession(&mut is_session) }
        .map_err(platform("ICoreWebView2Cookie.IsSession"))?;
    let mut is_secure = windows::core::BOOL::default();
    unsafe { cookie.IsSecure(&mut is_secure) }.map_err(platform("ICoreWebView2Cookie.IsSecure"))?;
    let mut is_http_only = windows::core::BOOL::default();
    unsafe { cookie.IsHttpOnly(&mut is_http_only) }
        .map_err(platform("ICoreWebView2Cookie.IsHttpOnly"))?;
    Ok(Cookie {
        name,
        value,
        domain,
        path,
        expires_at: if is_session.as_bool() {
            None
        } else {
            Some(expires)
        },
        is_secure: is_secure.as_bool(),
        is_http_only: is_http_only.as_bool(),
    })
}

unsafe fn cookies_from_webview2_list(
    list: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CookieList,
) -> Result<Vec<Cookie>, WebSurfaceError> {
    let mut count = 0;
    unsafe { list.Count(&mut count) }.map_err(platform("ICoreWebView2CookieList.Count"))?;
    let mut cookies = Vec::with_capacity(count as usize);
    for index in 0..count {
        let cookie = unsafe { list.GetValueAtIndex(index) }
            .map_err(platform("ICoreWebView2CookieList.GetValueAtIndex"))?;
        cookies.push(unsafe { cookie_from_webview2(&cookie)? });
    }
    Ok(cookies)
}

unsafe fn webview2_cookie_from(
    manager: &ICoreWebView2CookieManager,
    cookie: &Cookie,
) -> Result<ICoreWebView2Cookie, WebSurfaceError> {
    let name = CoTaskMemPWSTR::from(cookie.name.as_str());
    let value = CoTaskMemPWSTR::from(cookie.value.as_str());
    let domain = CoTaskMemPWSTR::from(cookie.domain.as_str());
    let path = CoTaskMemPWSTR::from(cookie.path.as_str());
    let webview_cookie = unsafe {
        manager.CreateCookie(
            *name.as_ref().as_pcwstr(),
            *value.as_ref().as_pcwstr(),
            *domain.as_ref().as_pcwstr(),
            *path.as_ref().as_pcwstr(),
        )
    }
    .map_err(platform("CookieManager.CreateCookie"))?;
    unsafe { webview_cookie.SetIsSecure(cookie.is_secure) }
        .map_err(platform("ICoreWebView2Cookie.SetIsSecure"))?;
    unsafe { webview_cookie.SetIsHttpOnly(cookie.is_http_only) }
        .map_err(platform("ICoreWebView2Cookie.SetIsHttpOnly"))?;
    if let Some(expires_at) = cookie.expires_at {
        unsafe { webview_cookie.SetExpires(expires_at) }
            .map_err(platform("ICoreWebView2Cookie.SetExpires"))?;
    }
    Ok(webview_cookie)
}

fn mouse_event_kind(kind: MouseEventKind) -> COREWEBVIEW2_MOUSE_EVENT_KIND {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL, COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
        COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN, COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
    };
    match kind {
        MouseEventKind::LeftButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
        MouseEventKind::LeftButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
        MouseEventKind::LeftButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::MiddleButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
        MouseEventKind::MiddleButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
        MouseEventKind::MiddleButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::RightButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
        MouseEventKind::RightButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
        MouseEventKind::RightButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::XButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN,
        MouseEventKind::XButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
        MouseEventKind::XButtonDoubleClick => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOUBLE_CLICK,
        MouseEventKind::Move => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
        MouseEventKind::Wheel => COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
        MouseEventKind::HorizontalWheel => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
        MouseEventKind::Leave => COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
    }
}

fn virtual_keys_bits(keys: crate::MouseVirtualKeys) -> u32 {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON1,
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON2,
    };
    let mut bits = 0u32;
    if keys.control {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL.0 as u32;
    }
    if keys.shift {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT.0 as u32;
    }
    if keys.left_button {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON.0 as u32;
    }
    if keys.middle_button {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON.0 as u32;
    }
    if keys.right_button {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON.0 as u32;
    }
    if keys.x_button1 {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON1.0 as u32;
    }
    if keys.x_button2 {
        bits |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_X_BUTTON2.0 as u32;
    }
    bits
}

fn pointer_event_kind(
    kind: crate::PointerEventKind,
) -> webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_POINTER_EVENT_KIND {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_POINTER_EVENT_KIND_ACTIVATE, COREWEBVIEW2_POINTER_EVENT_KIND_DOWN,
        COREWEBVIEW2_POINTER_EVENT_KIND_ENTER, COREWEBVIEW2_POINTER_EVENT_KIND_LEAVE,
        COREWEBVIEW2_POINTER_EVENT_KIND_UP, COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
    };
    match kind {
        crate::PointerEventKind::Activate => COREWEBVIEW2_POINTER_EVENT_KIND_ACTIVATE,
        crate::PointerEventKind::Down => COREWEBVIEW2_POINTER_EVENT_KIND_DOWN,
        crate::PointerEventKind::Enter => COREWEBVIEW2_POINTER_EVENT_KIND_ENTER,
        crate::PointerEventKind::Leave => COREWEBVIEW2_POINTER_EVENT_KIND_LEAVE,
        crate::PointerEventKind::Up => COREWEBVIEW2_POINTER_EVENT_KIND_UP,
        crate::PointerEventKind::Update => COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
        // CaptureChanged falls back to Update; WebView2 doesn't have a
        // distinct capture-change pointer kind.
        crate::PointerEventKind::CaptureChanged => COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
    }
}

/// Derive POINTER_FLAGS bits from a high-level [`PointerEventKind`].
///
/// Constants reproduced inline to avoid pulling another `windows` feature
/// just for these values; they are stable across windows-rs versions.
fn pointer_flags_for(kind: crate::PointerEventKind) -> u32 {
    const POINTER_FLAG_INRANGE: u32 = 0x00000002;
    const POINTER_FLAG_INCONTACT: u32 = 0x00000004;
    const POINTER_FLAG_PRIMARY: u32 = 0x00002000;
    const POINTER_FLAG_DOWN: u32 = 0x00010000;
    const POINTER_FLAG_UPDATE: u32 = 0x00020000;
    const POINTER_FLAG_UP: u32 = 0x00040000;
    const POINTER_FLAG_CAPTURECHANGED: u32 = 0x00200000;
    match kind {
        crate::PointerEventKind::Down => {
            POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT | POINTER_FLAG_INRANGE | POINTER_FLAG_PRIMARY
        }
        crate::PointerEventKind::Up => POINTER_FLAG_UP | POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Update => {
            POINTER_FLAG_UPDATE
                | POINTER_FLAG_INCONTACT
                | POINTER_FLAG_INRANGE
                | POINTER_FLAG_PRIMARY
        }
        crate::PointerEventKind::Enter => POINTER_FLAG_INRANGE | POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Leave => POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Activate => POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::CaptureChanged => POINTER_FLAG_CAPTURECHANGED,
    }
}

fn focus_reason(reason: FocusReason) -> COREWEBVIEW2_MOVE_FOCUS_REASON {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MOVE_FOCUS_REASON_NEXT, COREWEBVIEW2_MOVE_FOCUS_REASON_PREVIOUS,
        COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
    };
    match reason {
        FocusReason::Programmatic => COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
        FocusReason::Next => COREWEBVIEW2_MOVE_FOCUS_REASON_NEXT,
        FocusReason::Previous => COREWEBVIEW2_MOVE_FOCUS_REASON_PREVIOUS,
    }
}

fn keyboard_message_for(event: &KeyboardInput) -> u32 {
    match event.kind {
        KeyEventKind::Down => {
            if event.modifiers.alt {
                WM_SYSKEYDOWN
            } else {
                WM_KEYDOWN
            }
        }
        KeyEventKind::Up => {
            if event.modifiers.alt {
                WM_SYSKEYUP
            } else {
                WM_KEYUP
            }
        }
        KeyEventKind::ModifiersChanged => {
            if modifier_is_down(event.virtual_key_code, event.modifiers) {
                WM_KEYDOWN
            } else {
                WM_KEYUP
            }
        }
    }
}

fn keyboard_lparam(event: &KeyboardInput) -> isize {
    let repeat_count = 1isize;
    let previous_down = match event.kind {
        KeyEventKind::Down => event.is_repeat,
        KeyEventKind::Up => true,
        KeyEventKind::ModifiersChanged => {
            !modifier_is_down(event.virtual_key_code, event.modifiers)
        }
    };
    let transition_up = matches!(keyboard_message_for(event), WM_KEYUP | WM_SYSKEYUP);
    repeat_count | ((previous_down as isize) << 30) | ((transition_up as isize) << 31)
}

fn modifier_is_down(virtual_key_code: u32, modifiers: crate::KeyModifierFlags) -> bool {
    match virtual_key_code {
        0x10 | 0xA0 | 0xA1 => modifiers.shift,
        0x11 | 0xA2 | 0xA3 => modifiers.control,
        0x12 | 0xA4 | 0xA5 => modifiers.alt,
        0x5B | 0x5C => modifiers.meta,
        0x14 => modifiers.caps_lock,
        _ => false,
    }
}

fn is_webview2_keyboard_message(message: u32) -> bool {
    matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_DEADCHAR
            | WM_SYSCHAR
            | WM_SYSDEADCHAR
            | WM_IME_STARTCOMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_IME_COMPOSITION
            | WM_IME_COMPOSITIONFULL
            | WM_IME_CONTROL
            | WM_IME_NOTIFY
            | WM_IME_SELECT
            | WM_IME_SETCONTEXT
            | WM_IME_CHAR
            | WM_IME_REQUEST
            | WM_IME_KEYDOWN
            | WM_IME_KEYUP
    )
}

fn register_persistent_handlers(
    webview: &ICoreWebView2,
    nav_queue: Arc<Mutex<VecDeque<NavigationEvent>>>,
    web_message_queue: Arc<Mutex<VecDeque<String>>>,
    cookie_change_handler: Arc<Mutex<Option<WebView2CookieChangeHandlerFn>>>,
) -> Result<(i64, i64, i64, i64, i64), WebSurfaceError> {
    // NavigationStarting -> NavigationEvent::Starting { url }
    let queue = nav_queue.clone();
    let nav_starting_handler = NavigationStartingEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            let mut uri = PWSTR::null();
            if unsafe { args.Uri(&mut uri) }.is_ok() {
                let url = unsafe { consume_pwstr(uri) };
                if let Ok(mut q) = queue.lock() {
                    q.push_back(NavigationEvent::Starting { url });
                }
            }
        }
        Ok(())
    }));
    let mut nav_starting_token = 0i64;
    unsafe {
        webview
            .add_NavigationStarting(&nav_starting_handler, &mut nav_starting_token)
            .map_err(platform("add_NavigationStarting"))?;
    }

    // NavigationCompleted -> NavigationEvent::Completed { url, success }
    let queue = nav_queue.clone();
    let webview_for_handler = webview.clone();
    let nav_completed_handler =
        NavigationCompletedEventHandler::create(Box::new(move |_, args| {
            let success = args
                .as_ref()
                .and_then(|a| {
                    let mut b = windows::core::BOOL::default();
                    unsafe { a.IsSuccess(&mut b) }.ok().map(|()| b.as_bool())
                })
                .unwrap_or(false);
            let mut source = PWSTR::null();
            let url = if unsafe { webview_for_handler.Source(&mut source) }.is_ok() {
                unsafe { consume_pwstr(source) }
            } else {
                String::new()
            };
            if let Ok(mut q) = queue.lock() {
                q.push_back(NavigationEvent::Completed { url, success });
            }
            Ok(())
        }));
    let mut nav_completed_token = 0i64;
    unsafe {
        webview
            .add_NavigationCompleted(&nav_completed_handler, &mut nav_completed_token)
            .map_err(platform("add_NavigationCompleted (persistent)"))?;
    }

    // SourceChanged -> NavigationEvent::SourceChanged { url }
    let queue = nav_queue.clone();
    let webview_for_handler = webview.clone();
    let source_changed_handler = SourceChangedEventHandler::create(Box::new(move |_, _args| {
        let mut source = PWSTR::null();
        let url = if unsafe { webview_for_handler.Source(&mut source) }.is_ok() {
            unsafe { consume_pwstr(source) }
        } else {
            String::new()
        };
        if let Ok(mut q) = queue.lock() {
            q.push_back(NavigationEvent::SourceChanged { url });
        }
        Ok(())
    }));
    let mut source_changed_token = 0i64;
    unsafe {
        webview
            .add_SourceChanged(&source_changed_handler, &mut source_changed_token)
            .map_err(platform("add_SourceChanged"))?;
    }

    // DocumentTitleChanged -> NavigationEvent::TitleChanged { title }
    let queue = nav_queue;
    let webview_for_handler = webview.clone();
    let title_changed_handler =
        DocumentTitleChangedEventHandler::create(Box::new(move |_, _args| {
            let mut title_pw = PWSTR::null();
            let title = if unsafe { webview_for_handler.DocumentTitle(&mut title_pw) }.is_ok() {
                unsafe { consume_pwstr(title_pw) }
            } else {
                String::new()
            };
            if let Ok(mut q) = queue.lock() {
                q.push_back(NavigationEvent::TitleChanged { title });
            }
            Ok(())
        }));
    let mut title_changed_token = 0i64;
    unsafe {
        webview
            .add_DocumentTitleChanged(&title_changed_handler, &mut title_changed_token)
            .map_err(platform("add_DocumentTitleChanged"))?;
    }

    // WebMessageReceived -> string queue
    let queue = web_message_queue;
    let cookie_handler = cookie_change_handler;
    let web_message_handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            let mut message = PWSTR::null();
            if unsafe { args.TryGetWebMessageAsString(&mut message) }.is_ok() {
                let s = unsafe { consume_pwstr(message) };
                if s == COOKIE_CHANGE_BRIDGE_MESSAGE {
                    if let Ok(slot) = cookie_handler.lock()
                        && let Some(handler) = slot.as_ref()
                    {
                        handler();
                    }
                    return Ok(());
                }
                if let Ok(mut q) = queue.lock() {
                    q.push_back(s);
                }
            }
        }
        Ok(())
    }));
    let mut web_message_token = 0i64;
    unsafe {
        webview
            .add_WebMessageReceived(&web_message_handler, &mut web_message_token)
            .map_err(platform("add_WebMessageReceived"))?;
    }

    Ok((
        nav_starting_token,
        nav_completed_token,
        source_changed_token,
        title_changed_token,
        web_message_token,
    ))
}

fn register_cursor_changed_handler(
    composition_controller: &ICoreWebView2CompositionController,
    cursor_queue: Arc<Mutex<VecDeque<CursorShape>>>,
) -> Result<i64, WebSurfaceError> {
    use webview2_com::CursorChangedEventHandler;
    let cc = composition_controller.clone();
    let handler = CursorChangedEventHandler::create(Box::new(move |_, _| {
        let mut hcursor: HCURSOR = HCURSOR::default();
        if unsafe { cc.Cursor(&mut hcursor) }.is_ok() {
            let shape = hcursor_to_shape(hcursor);
            if let Ok(mut q) = cursor_queue.lock() {
                q.push_back(shape);
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        composition_controller
            .add_CursorChanged(&handler, &mut token)
            .map_err(platform("add_CursorChanged"))?;
    }
    Ok(token)
}

fn hcursor_to_shape(cursor: HCURSOR) -> CursorShape {
    // Compare the incoming HCURSOR against the standard system cursors.
    // Win32 returns the same HCURSOR pointer for repeated `LoadCursorW`
    // calls, so equality is a reliable identity check for the named
    // standard cursors.
    let pairs: [(windows::core::PCWSTR, CursorShape); 13] = [
        (IDC_ARROW, CursorShape::Default),
        (IDC_HAND, CursorShape::Pointer),
        (IDC_IBEAM, CursorShape::Text),
        (IDC_WAIT, CursorShape::Wait),
        (IDC_CROSS, CursorShape::Crosshair),
        (IDC_SIZEALL, CursorShape::ResizeAll),
        (IDC_SIZENS, CursorShape::ResizeNs),
        (IDC_SIZEWE, CursorShape::ResizeEw),
        (IDC_SIZENESW, CursorShape::ResizeNeSw),
        (IDC_SIZENWSE, CursorShape::ResizeNwSe),
        (IDC_NO, CursorShape::NotAllowed),
        (IDC_HELP, CursorShape::Help),
        (IDC_APPSTARTING, CursorShape::Progress),
    ];
    for (id, shape) in pairs {
        let h = unsafe { LoadCursorW(None, id) };
        if let Ok(h) = h
            && h.0 == cursor.0
        {
            return shape;
        }
    }
    CursorShape::Custom(format!("hcursor:{:?}", cursor.0))
}
