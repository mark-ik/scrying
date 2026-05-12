//! Minimal winit + wgpu host probe for scrying system-webview texture interop.

use std::sync::Arc;

#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(target_os = "windows")]
use scrying::Dx12FenceSynchronizer;
use scrying::{
    Cookie, HostWgpuContext, ImportOptions, NavigationEvent, TextureImporter, UrlSchemeResponse,
    WebSurfaceCapabilities, WebSurfaceFrame, WebSurfaceSettings, WgpuTextureImporter,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let cli = Cli::parse();
    let mut app = App { cli, state: None };
    Ok(event_loop.run_app(&mut app)?)
}

#[derive(Clone, Default)]
struct Cli {
    probe_only: bool,
    scripted: bool,
    cookie_test: bool,
    browser_test: bool,
    profile_test: bool,
    incognito_test: bool,
    popup_test: bool,
    process_test: bool,
    routing_test: bool,
}

impl Cli {
    fn parse() -> Self {
        let mut cli = Self::default();
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--probe-only" => cli.probe_only = true,
                "--scripted" => cli.scripted = true,
                "--cookie-test" => cli.cookie_test = true,
                "--browser-test" => cli.browser_test = true,
                "--profile-test" => cli.profile_test = true,
                "--incognito-test" => cli.incognito_test = true,
                "--popup-test" => cli.popup_test = true,
                "--process-test" => cli.process_test = true,
                "--routing-test" => cli.routing_test = true,
                _ => eprintln!("demo-win: unknown arg: {arg}"),
            }
        }
        cli
    }

    fn one_shot(&self) -> bool {
        self.probe_only
            || self.scripted
            || self.cookie_test
            || self.browser_test
            || self.profile_test
            || self.incognito_test
            || self.popup_test
            || self.process_test
            || self.routing_test
    }
}

#[derive(Default)]
struct App {
    cli: Cli,
    state: Option<AppState>,
}

struct AppState {
    window: Arc<Window>,
    _device: wgpu::Device,
    _queue: wgpu::Queue,
    #[cfg(target_os = "windows")]
    renderer: Option<WebViewRenderer>,
    /// Last reported cursor position in physical pixels (host window
    /// coordinates). Used to map mouse input into composition-WebView
    /// space.
    cursor: Option<winit::dpi::PhysicalPosition<f64>>,
    /// Bitmask of currently-held mouse buttons, in the layout
    /// `MouseVirtualKeys` expects.
    mouse_buttons: scrying::MouseVirtualKeys,
    /// Bitmask of currently-held modifier keys (Ctrl / Shift).
    modifiers: scrying::MouseVirtualKeys,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let one_shot = self.cli.one_shot();
        match AppState::new(event_loop, &self.cli) {
            Ok(state) => {
                if one_shot {
                    drop(state);
                    event_loop.exit();
                    std::process::exit(0);
                }
                self.state = Some(state);
            }
            Err(error) => {
                eprintln!("demo-win: initialization failed: {error}");
                event_loop.exit();
                if one_shot {
                    std::process::exit(1);
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            #[cfg(target_os = "windows")]
            WindowEvent::Resized(new_size) => {
                if let Some(state) = self.state.as_mut() {
                    if let Some(renderer) = state.renderer.as_mut() {
                        renderer.resize(new_size);
                    }
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::RedrawRequested => {
                if let Some(state) = self.state.as_mut() {
                    if let Some(renderer) = state.renderer.as_mut() {
                        if let Err(error) = renderer.render() {
                            eprintln!("demo-win: render failed: {error}");
                        }
                        drain_composition_events(renderer);
                    }
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(state) = self.state.as_mut() {
                    state.cursor = Some(position);
                    forward_mouse_to_composition(state, scrying::MouseEventKind::Move, 0);
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                if let Some(state) = self.state.as_mut() {
                    use winit::event::{ElementState, MouseButton};
                    let pressed = matches!(btn_state, ElementState::Pressed);
                    let kind = match (button, pressed) {
                        (MouseButton::Left, true) => {
                            state.mouse_buttons.left_button = true;
                            scrying::MouseEventKind::LeftButtonDown
                        }
                        (MouseButton::Left, false) => {
                            state.mouse_buttons.left_button = false;
                            scrying::MouseEventKind::LeftButtonUp
                        }
                        (MouseButton::Right, true) => {
                            state.mouse_buttons.right_button = true;
                            scrying::MouseEventKind::RightButtonDown
                        }
                        (MouseButton::Right, false) => {
                            state.mouse_buttons.right_button = false;
                            scrying::MouseEventKind::RightButtonUp
                        }
                        (MouseButton::Middle, true) => {
                            state.mouse_buttons.middle_button = true;
                            scrying::MouseEventKind::MiddleButtonDown
                        }
                        (MouseButton::Middle, false) => {
                            state.mouse_buttons.middle_button = false;
                            scrying::MouseEventKind::MiddleButtonUp
                        }
                        _ => return,
                    };
                    if pressed {
                        // Click into the composition WebView region also
                        // hands keyboard focus to it.
                        if let (Some(pos), Some(renderer)) = (state.cursor, state.renderer.as_mut())
                            && composition_contains(pos)
                        {
                            let _ = renderer
                                .captured
                                .producer
                                .move_focus(scrying::FocusReason::Programmatic);
                        }
                    }
                    forward_mouse_to_composition(state, kind, 0);
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::MouseWheel { delta, .. } => {
                use winit::event::MouseScrollDelta;
                if let Some(state) = self.state.as_mut() {
                    let (kind, mouse_data) = match delta {
                        MouseScrollDelta::LineDelta(x, y) => {
                            // 120 units == one wheel notch, per the Win32 convention.
                            if y.abs() >= x.abs() {
                                (scrying::MouseEventKind::Wheel, (y * 120.0) as i32)
                            } else {
                                (scrying::MouseEventKind::HorizontalWheel, (x * 120.0) as i32)
                            }
                        }
                        MouseScrollDelta::PixelDelta(p) => {
                            if p.y.abs() >= p.x.abs() {
                                (scrying::MouseEventKind::Wheel, p.y as i32)
                            } else {
                                (scrying::MouseEventKind::HorizontalWheel, p.x as i32)
                            }
                        }
                    };
                    forward_mouse_to_composition(state, kind, mouse_data);
                }
            }
            #[cfg(target_os = "windows")]
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(state) = self.state.as_mut() {
                    let mods = modifiers.state();
                    state.modifiers.control = mods.control_key();
                    state.modifiers.shift = mods.shift_key();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

impl AppState {
    fn new(event_loop: &ActiveEventLoop, cli: &Cli) -> Result<Self, Box<dyn std::error::Error>> {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("demo-win")
                    .with_inner_size(winit::dpi::PhysicalSize::new(900, 600)),
            )?,
        );

        let (instance, device, queue, adapter_info) = pollster::block_on(create_host_device())?;
        let host = HostWgpuContext::new(device.clone(), queue.clone());
        let capabilities = WebSurfaceCapabilities::probe(Some(&host));

        println!("wgpu adapter: {}", adapter_info.name);
        println!("wgpu backend: {:?}", host.backend);
        println!("system webview backend: {:?}", capabilities.backend);
        println!("preferred surface mode: {:?}", capabilities.preferred_mode);
        println!(
            "imported texture support: {:?}",
            capabilities.imported_texture
        );
        println!(
            "native overlay support: {:?}",
            capabilities.native_child_overlay
        );
        println!("CPU snapshot support: {:?}", capabilities.cpu_snapshot);
        println!("reason: {}", capabilities.reason);

        #[cfg(target_os = "windows")]
        run_windows_shared_texture_probe(&window, &host)?;

        // Opt into the explicit-fence cross-API sync path
        // (wgpu D3D12 `Wait` on a `D3D12_FENCE_FLAG_SHARED` fence the WebView2
        // producer signals after `CopyResource`). Disabled by default so the
        // existing keyed-mutex + CPU-spin path stays the verified default.
        // Setting `WEBVIEW_FENCE_SYNC=1` enables it. Mutually exclusive with
        // `WEBVIEW_READBACK_VALIDATE` because that path uses a separate
        // importer that doesn't carry the synchronizer.
        #[cfg(target_os = "windows")]
        let fence_synchronizer: Option<Dx12FenceSynchronizer> = if std::env::var(
            "WEBVIEW_FENCE_SYNC",
        )
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
        {
            if std::env::var("WEBVIEW_READBACK_VALIDATE")
                .ok()
                .filter(|v| !v.is_empty() && v != "0")
                .is_some()
            {
                println!(
                    "WEBVIEW_FENCE_SYNC ignored: incompatible with WEBVIEW_READBACK_VALIDATE (separate importer)"
                );
                None
            } else {
                let sync = Dx12FenceSynchronizer::new(&host)?;
                println!(
                    "WEBVIEW_FENCE_SYNC enabled: shared fence handle {:p}",
                    sync.shared_handle().0
                );
                Some(sync)
            }
        } else {
            None
        };

        #[cfg(target_os = "windows")]
        let fence_handle = fence_synchronizer
            .as_ref()
            .map(|s| s.shared_handle().0 as *mut std::ffi::c_void);

        #[cfg(target_os = "windows")]
        let captured = run_platform_composition_visual_probe(&window, &host, fence_handle, cli)?;

        #[cfg(target_os = "windows")]
        let renderer = if cli.one_shot() {
            None
        } else {
            match captured {
                Some(captured) => Some(WebViewRenderer::new(
                    instance,
                    window.clone(),
                    host.clone(),
                    captured,
                    fence_synchronizer,
                )?),
                None => {
                    drop(instance);
                    None
                }
            }
        };

        Ok(Self {
            window,
            _device: device,
            _queue: queue,
            #[cfg(target_os = "windows")]
            renderer,
            cursor: None,
            mouse_buttons: scrying::MouseVirtualKeys::default(),
            modifiers: scrying::MouseVirtualKeys::default(),
        })
    }
}

#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_X: f32 = 450.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_Y: f32 = 48.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_WIDTH: f32 = 420.0;
#[cfg(target_os = "windows")]
const COMPOSITION_PROBE_HEIGHT: f32 = 260.0;

#[cfg(target_os = "windows")]
fn composition_contains(pos: winit::dpi::PhysicalPosition<f64>) -> bool {
    let x = pos.x as f32;
    let y = pos.y as f32;
    x >= COMPOSITION_PROBE_X
        && x < COMPOSITION_PROBE_X + COMPOSITION_PROBE_WIDTH
        && y >= COMPOSITION_PROBE_Y
        && y < COMPOSITION_PROBE_Y + COMPOSITION_PROBE_HEIGHT
}

#[cfg(target_os = "windows")]
fn forward_mouse_to_composition(
    state: &mut AppState,
    kind: scrying::MouseEventKind,
    mouse_data: i32,
) {
    let Some(pos) = state.cursor else {
        return;
    };
    let Some(renderer) = state.renderer.as_mut() else {
        return;
    };
    if !composition_contains(pos) {
        return;
    }
    let local_x = (pos.x as f32 - COMPOSITION_PROBE_X) as i32;
    let local_y = (pos.y as f32 - COMPOSITION_PROBE_Y) as i32;
    let mut virtual_keys = state.modifiers;
    virtual_keys.left_button = state.mouse_buttons.left_button;
    virtual_keys.right_button = state.mouse_buttons.right_button;
    virtual_keys.middle_button = state.mouse_buttons.middle_button;
    let event = scrying::MouseInput {
        kind,
        virtual_keys,
        mouse_data,
        point: (local_x, local_y),
    };
    if let Err(error) = renderer.captured.producer.send_mouse_input(event) {
        eprintln!("demo-win: send_mouse_input failed: {error}");
    }
}

#[cfg(target_os = "windows")]
fn drain_composition_events(renderer: &mut WebViewRenderer) {
    while let Some(event) = renderer.captured.producer.poll_navigation_event() {
        match event {
            scrying::NavigationEvent::Starting { url } => {
                println!("[nav] starting -> {url}");
            }
            scrying::NavigationEvent::SourceChanged { url } => {
                println!("[nav] source changed -> {url}");
            }
            scrying::NavigationEvent::Completed { url, success } => {
                println!("[nav] completed (success={success}) -> {url}");
            }
            scrying::NavigationEvent::TitleChanged { title } => {
                println!("[nav] title -> {title}");
            }
            scrying::NavigationEvent::NewWindowRequested { url } => {
                println!("[nav] new window requested -> {url}");
            }
            scrying::NavigationEvent::ContentProcessTerminated => {
                println!("[nav] content process terminated");
            }
            _ => {}
        }
    }
    while let Some(message) = renderer.captured.producer.poll_web_message() {
        println!("[web message] {message}");
    }
    while let Some(shape) = renderer.captured.producer.poll_cursor_shape() {
        println!("[cursor] {shape:?}");
    }
}

#[cfg(target_os = "windows")]
const COMPOSITION_WEBVIEW_PROBE_HTML: &str = r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        html, body {
            margin: 0;
            width: 100%;
            height: 100%;
            overflow: hidden;
            background: #17202a;
            color: #f8f1d8;
            font-family: system-ui, sans-serif;
        }
        body {
            display: grid;
            place-items: center;
            position: relative;
        }
        main {
            display: grid;
            gap: 10px;
            text-align: center;
            z-index: 1;
        }
        h1 {
            margin: 0;
            font-size: 26px;
            font-weight: 650;
            letter-spacing: 0;
        }
        p {
            margin: 0;
            color: #ffbe70;
            font-size: 14px;
        }
        input {
            width: 220px;
            justify-self: center;
            box-sizing: border-box;
            border: 1px solid #8fd2c7;
            border-radius: 4px;
            background: #0f1720;
            color: #f8f1d8;
            padding: 6px 8px;
            font: 15px system-ui, sans-serif;
            letter-spacing: 0;
        }
        #tick {
            position: absolute;
            top: 8px;
            left: 8px;
            font-size: 13px;
            color: #8fd2c7;
            font-variant-numeric: tabular-nums;
        }
        @keyframes sweep {
            0%   { transform: translateX(0); background: #ff6b6b; }
            50%  { background: #56cfe1; }
            100% { transform: translateX(calc(100% - 24px)); background: #ff6b6b; }
        }
        #bar {
            position: absolute;
            bottom: 12px;
            left: 12px;
            right: 12px;
            height: 6px;
            border-radius: 3px;
            background: #2c3e50;
            overflow: hidden;
        }
        #bar::after {
            content: "";
            display: block;
            width: 24px;
            height: 100%;
            background: #ff6b6b;
            animation: sweep 2.4s linear infinite;
        }
    </style>
</head>
<body>
    <div id="tick">frame 0</div>
    <main>
        <h1>Scrying Composition Probe</h1>
        <p>Rendered through the selected system-webview producer.</p>
        <input id="keyboard-smoke" autofocus autocomplete="off" spellcheck="false" aria-label="keyboard smoke input">
    </main>
    <div id="bar"></div>
    <script>
        let n = 0;
        const tick = document.getElementById("tick");
        const input = document.getElementById("keyboard-smoke");
        input.focus();
        input.addEventListener("input", () => {
            window.chrome.webview.postMessage("keyboard-smoke:" + input.value);
        });
        function loop() {
            n++;
            tick.textContent = "frame " + n;
            requestAnimationFrame(loop);
        }
        requestAnimationFrame(loop);
    </script>
</body>
</html>"#;

#[cfg(target_os = "windows")]
const COMPOSITION_WEBVIEW_SCRIPTED_HTML: &str = r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <style>
        html, body {
            margin: 0;
            width: 100%;
            min-height: 100%;
            background: #17202a;
            color: #f8f1d8;
            font-family: system-ui, sans-serif;
        }
        body {
            box-sizing: border-box;
            padding: 18px;
        }
        input {
            box-sizing: border-box;
            width: 220px;
            border: 1px solid #8fd2c7;
            border-radius: 4px;
            background: #0f1720;
            color: #f8f1d8;
            padding: 6px 8px;
            font: 15px system-ui, sans-serif;
            letter-spacing: 0;
        }
        #scroll-target {
            margin-top: 12px;
            height: 520px;
            border-top: 1px solid #8fd2c7;
            color: #ffbe70;
        }
    </style>
</head>
<body>
    <h1>Scrying Windows Scripted Probe</h1>
    <input id="scripted-input" autofocus autocomplete="off" spellcheck="false" aria-label="scripted input">
    <div id="scroll-target">scroll target</div>
    <script>
        const post = value => window.chrome.webview.postMessage(value);
        const input = document.getElementById("scripted-input");
        input.focus();
        window.chrome.webview.addEventListener("message", event => {
            post("scripted:host-echo:" + event.data);
        });
        window.addEventListener("wheel", () => post("scripted:wheel"));
        input.addEventListener("input", () => post("scripted:input:" + input.value));
        post("scripted:ready");
    </script>
</body>
</html>"#;

#[cfg(target_os = "windows")]
fn composition_probe_html(scripted: bool) -> &'static str {
    if scripted {
        COMPOSITION_WEBVIEW_SCRIPTED_HTML
    } else {
        COMPOSITION_WEBVIEW_PROBE_HTML
    }
}

#[cfg(target_os = "windows")]
fn run_windows_shared_texture_probe(
    window: &Window,
    host: &HostWgpuContext,
) -> Result<(), Box<dyn std::error::Error>> {
    use scrying::windows_capture::{
        D3D11SharedTextureFactory, DxgiSharedHandleBridge, capture_window_frame_once,
        close_shared_handle, probe_graphics_capture_prerequisites,
    };

    let graphics_capture = probe_graphics_capture_prerequisites()?;
    println!(
        "GraphicsCapture probe: session_supported={} winrt_d3d_device={} free_threaded_frame_pool={}",
        graphics_capture.session_supported,
        graphics_capture.winrt_d3d_device_created,
        graphics_capture.free_threaded_frame_pool_created
    );

    let factory = D3D11SharedTextureFactory::new_hardware()?;
    let shared = factory.create_shared_texture_frame(
        winit::dpi::PhysicalSize::new(64, 64),
        wgpu::TextureFormat::Bgra8Unorm,
        1,
    )?;
    let handle = shared.shared_handle;
    let dx12_frame = DxgiSharedHandleBridge.bridge_shared_handle(shared)?;
    println!("D3D11 shared texture probe: exported NT handle {handle:p}");

    let surface_frame = dx12_frame.into_surface_frame();
    let WebSurfaceFrame::Native(native_frame) = surface_frame else {
        return Err("D3D11 shared texture bridge did not produce a native frame".into());
    };
    let importer = WgpuTextureImporter::new(host.clone());
    let imported = importer.import_frame(&native_frame, &ImportOptions::default())?;
    println!(
        "D3D11 shared texture probe: imported {:?} {}x{} generation {}",
        imported.format, imported.size.width, imported.size.height, imported.generation
    );

    unsafe {
        close_shared_handle(handle)?;
    }

    let hwnd = hwnd_from_window(window)?;
    let captured = unsafe { capture_window_frame_once(hwnd, std::time::Duration::from_secs(2)) }?;
    let captured_handle = captured.shared_frame.shared_handle;
    let captured_dx12 = DxgiSharedHandleBridge.bridge_shared_handle(captured.shared_frame)?;
    let captured_surface_frame = captured_dx12.into_surface_frame();
    let WebSurfaceFrame::Native(captured_native_frame) = captured_surface_frame else {
        return Err("captured window bridge did not produce a native frame".into());
    };
    let captured_imported =
        importer.import_frame(&captured_native_frame, &ImportOptions::default())?;
    println!(
        "GraphicsCapture window probe: captured {}x{}, imported {:?} {}x{} generation {}",
        captured.content_size.width,
        captured.content_size.height,
        captured_imported.format,
        captured_imported.size.width,
        captured_imported.size.height,
        captured_imported.generation
    );
    unsafe {
        close_shared_handle(captured_handle)?;
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn hwnd_from_window(window: &Window) -> Result<*mut std::ffi::c_void, Box<dyn std::error::Error>> {
    let handle = window.window_handle()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as *mut std::ffi::c_void),
        other => Err(format!("expected Win32 raw window handle, got {other:?}").into()),
    }
}

#[cfg(target_os = "windows")]
struct CapturedComposition {
    /// The most recently imported WebView texture; `None` until the renderer's
    /// first `try_acquire_frame` lands a frame. The probe path no longer
    /// blocks on an initial acquire (it was an intermittent pump-hang).
    imported: Option<scrying::ImportedTexture>,
    producer: scrying::PlatformWebSurfaceProducer,
    #[allow(dead_code)]
    dispatcher_queue: Option<windows::System::DispatcherQueueController>,
}

#[cfg(target_os = "windows")]
fn run_platform_composition_visual_probe(
    window: &Window,
    host: &HostWgpuContext,
    fence_shared_handle: Option<*mut std::ffi::c_void>,
    cli: &Cli,
) -> Result<Option<CapturedComposition>, Box<dyn std::error::Error>> {
    use scrying::windows_capture::close_shared_handle;
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
        DispatcherQueueOptions,
    };

    let parent_hwnd = hwnd_from_window(window)?;
    let dispatcher_queue = match unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
    } {
        Ok(controller) => Some(controller),
        Err(error) => {
            println!(
                "CompositionController visual probe: dispatcher queue setup returned {error}; continuing"
            );
            None
        }
    };

    let user_data_dir = std::env::temp_dir().join("demo-win-composition-webview2");
    let mut config = scrying::PlatformWebSurfaceConfig::new(
        winit::dpi::PhysicalSize::new(
            COMPOSITION_PROBE_WIDTH as u32,
            COMPOSITION_PROBE_HEIGHT as u32,
        ),
        user_data_dir.clone(),
    )
    .with_offset(COMPOSITION_PROBE_X, COMPOSITION_PROBE_Y)
    .with_diagnostic_backdrop((27, 86, 96));
    if let Some(handle) = fence_shared_handle {
        config = config.with_fence_shared_handle(handle);
    }
    if cli.incognito_test {
        config = config.non_persistent();
    }

    let producer = unsafe { scrying::PlatformWebSurfaceProducer::new(parent_hwnd, config)? };
    producer.navigate_to_string(
        composition_probe_html(cli.scripted),
        std::time::Duration::from_secs(5),
    )?;
    println!("CompositionController visual probe: navigation completed");

    let mut producer = producer;
    if cli.scripted {
        validate_platform_scripted(&mut producer)?;
    }
    if cli.cookie_test {
        validate_platform_cookie_store(&mut producer)?;
    }
    if cli.browser_test {
        validate_platform_browser_controls(&mut producer)?;
    }
    if cli.popup_test {
        validate_platform_popup_routing(&mut producer)?;
    }
    if cli.routing_test {
        validate_platform_virtual_host_routing(&mut producer)?;
    }
    if cli.process_test {
        validate_platform_process_failure_recovery(&mut producer)?;
    }
    if cli.profile_test {
        validate_platform_profile_store(producer, parent_hwnd, user_data_dir)?;
        return Ok(None);
    }
    if cli.incognito_test {
        validate_platform_incognito_store(producer, parent_hwnd, user_data_dir)?;
        return Ok(None);
    }
    if keyboard_validate_enabled() {
        validate_platform_keyboard_smoke(&mut producer)?;
    }

    let imported = if std::env::var("WEBVIEW_READBACK_VALIDATE")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
    {
        let mut producer_for_readback = producer;
        let captured = producer_for_readback.acquire_full_frame()?;
        let importer = WgpuTextureImporter::new(host.clone());
        let WebSurfaceFrame::Native(ref native_frame) = captured.frame else {
            return Err("WebView2 composition producer did not emit a native frame".into());
        };
        let imported = importer.import_frame(native_frame, &ImportOptions::default())?;
        println!(
            "GraphicsCapture WebView2 CompositionController WebView target visual: captured {}x{}, imported {:?} {}x{} generation {}",
            captured.content_size.width,
            captured.content_size.height,
            imported.format,
            imported.size.width,
            imported.size.height,
            imported.generation
        );
        let html_background_rgb = (0x17u8, 0x20u8, 0x2au8);
        validate_imported_pixels(&imported, &host.device, &host.queue, html_background_rgb)?;
        unsafe {
            close_shared_handle(captured.shared_handle)?;
        }
        return Ok(Some(CapturedComposition {
            imported: Some(imported),
            producer: producer_for_readback,
            dispatcher_queue,
        }));
    } else {
        eprintln!(
            "WebView readback: skipped (set WEBVIEW_READBACK_VALIDATE=1 to enable startup pixel check + initial acquire). Renderer will perform first acquire on its own."
        );
        None
    };

    Ok(Some(CapturedComposition {
        imported,
        producer,
        dispatcher_queue,
    }))
}

#[cfg(target_os = "windows")]
fn validate_platform_virtual_host_routing(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const HOST: &str = "scrying-test.local";
    producer.register_virtual_host_handler(
        HOST,
        Arc::new(|url: &str| -> UrlSchemeResponse {
            let body = format!(
                r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Scrying Routing Test</title></head>
<body>
    <h1>routing-test</h1>
    <script>
        window.chrome.webview.postMessage("routing-test:ready:{url}");
    </script>
</body>
</html>"#
            );
            UrlSchemeResponse {
                mime_type: "text/html".into(),
                body: body.into_bytes(),
                headers: Vec::new(),
            }
        }),
    )?;

    drain_navigation_events(producer);
    drain_web_messages(producer);
    let url = format!("https://{HOST}/app-shell");
    producer.navigate_to_url(&url, std::time::Duration::from_secs(5))?;
    wait_for_web_message(
        producer,
        &format!("routing-test:ready:{url}"),
        std::time::Duration::from_secs(3),
    )?;
    println!(
        "demo-win: routing-test: PASS - WebResourceRequested virtual host served app-owned content"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn validate_platform_process_failure_recovery(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    drain_navigation_events(producer);
    drain_web_messages(producer);
    producer.call_devtools_protocol_method("Page.crash", "{}")?;
    wait_for_content_process_terminated(producer, std::time::Duration::from_secs(5))?;
    producer.navigate_to_string(process_recovery_html(), std::time::Duration::from_secs(5))?;
    wait_for_web_message(
        producer,
        "process-test:recovered",
        std::time::Duration::from_secs(3),
    )?;
    println!(
        "demo-win: process-test: PASS - ProcessFailed surfaced and producer recovered with a fresh navigation"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn process_recovery_html() -> &'static str {
    r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>Scrying Process Recovery Test</title></head>
<body>
    <h1>process recovery</h1>
    <script>window.chrome.webview.postMessage("process-test:recovered");</script>
</body>
</html>"#
}

#[cfg(target_os = "windows")]
fn validate_platform_popup_routing(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const POPUP_URL: &str = "https://example.com/scrying-popup-target";
    drain_navigation_events(producer);
    drain_web_messages(producer);
    let html = popup_test_html(POPUP_URL);
    producer.navigate_to_string(&html, std::time::Duration::from_secs(5))?;
    wait_for_web_message(
        producer,
        "popup-test:ready",
        std::time::Duration::from_secs(3),
    )?;
    producer.post_web_message("popup-test:open")?;
    wait_for_web_message(
        producer,
        "popup-test:opened",
        std::time::Duration::from_secs(3),
    )?;

    let observed_url = wait_for_new_window_request(producer, std::time::Duration::from_secs(3))?;
    if observed_url != POPUP_URL {
        return Err(format!(
            "popup-test: expected new-window URL {POPUP_URL:?}, got {observed_url:?}"
        )
        .into());
    }

    println!(
        "demo-win: popup-test: PASS - NewWindowRequested routed to the host and default popup was suppressed"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn popup_test_html(popup_url: &str) -> String {
    r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <title>Scrying Popup Test</title>
    <style>
        html, body { margin: 0; width: 100%; height: 100%; }
        body { background: #17202a; color: #f6ead0; font-family: system-ui, sans-serif; padding: 18px; }
        button { font: inherit; padding: 8px 12px; }
    </style>
</head>
<body>
    <button id="open-popup">Open popup</button>
    <script>
        const post = value => window.chrome.webview.postMessage(value);
        document.getElementById("open-popup").addEventListener("click", () => {
            window.open("__POPUP_URL__", "_blank");
            post("popup-test:clicked");
        });
        window.chrome.webview.addEventListener("message", event => {
            if (event.data === "popup-test:open") {
                window.open("__POPUP_URL__", "_blank");
                post("popup-test:opened");
            }
        });
        window.addEventListener("pageshow", () => post("popup-test:ready"));
    </script>
</body>
</html>"#
    .replace("__POPUP_URL__", popup_url)
}

#[cfg(target_os = "windows")]
fn validate_platform_profile_store(
    mut producer: scrying::PlatformWebSurfaceProducer,
    parent_hwnd: *mut std::ffi::c_void,
    user_data_dir: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let cookie_name = format!("demo_win_profile_cookie_{}", std::process::id());
    let cookie = Cookie {
        name: cookie_name.clone(),
        value: "shared-profile".into(),
        domain: "example.com".into(),
        path: "/".into(),
        expires_at: Some(4_102_444_800.0),
        is_secure: false,
        is_http_only: false,
    };

    let _ = producer.delete_cookie(&cookie.name, &cookie.domain, &cookie.path);
    producer.set_cookie(&cookie)?;
    pump_windows_messages_for(std::time::Duration::from_millis(250));

    let primary_cookies = request_cookies(&mut producer, std::time::Duration::from_secs(3))?;
    require_cookie(
        &primary_cookies,
        &cookie,
        "profile-test: primary producer did not report the cookie after set_cookie",
    )?;
    drop(producer);
    pump_windows_messages_for(std::time::Duration::from_millis(250));

    let secondary_config = scrying::PlatformWebSurfaceConfig::new(
        winit::dpi::PhysicalSize::new(
            COMPOSITION_PROBE_WIDTH as u32,
            COMPOSITION_PROBE_HEIGHT as u32,
        ),
        user_data_dir,
    )
    .with_offset(
        COMPOSITION_PROBE_X + COMPOSITION_PROBE_WIDTH + 24.0,
        COMPOSITION_PROBE_Y,
    )
    .with_diagnostic_backdrop((67, 61, 89));
    let mut secondary =
        unsafe { scrying::PlatformWebSurfaceProducer::new(parent_hwnd, secondary_config)? };
    secondary.navigate_to_string(
        &browser_test_html("profile-secondary"),
        std::time::Duration::from_secs(5),
    )?;

    let secondary_cookies = request_cookies(&mut secondary, std::time::Duration::from_secs(3))?;
    require_cookie(
        &secondary_cookies,
        &cookie,
        "profile-test: secondary producer with the same user_data_dir did not see the cookie",
    )?;

    secondary.delete_cookie(&cookie.name, &cookie.domain, &cookie.path)?;
    pump_windows_messages_for(std::time::Duration::from_millis(250));
    let secondary_after_delete =
        request_cookies(&mut secondary, std::time::Duration::from_secs(3))?;
    if contains_cookie(&secondary_after_delete, &cookie) {
        return Err(format!(
            "profile-test: secondary producer still saw {:?} after delete_cookie",
            cookie.name
        )
        .into());
    }

    println!(
        "demo-win: profile-test: PASS - persistent cookie store survived producer recreation with the same user_data_dir"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn validate_platform_incognito_store(
    mut incognito: scrying::PlatformWebSurfaceProducer,
    parent_hwnd: *mut std::ffi::c_void,
    user_data_dir: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let cookie_name = format!("demo_win_incognito_cookie_{}", std::process::id());
    let cookie = Cookie {
        name: cookie_name.clone(),
        value: "incognito-only".into(),
        domain: "example.com".into(),
        path: "/".into(),
        expires_at: Some(4_102_444_800.0),
        is_secure: false,
        is_http_only: false,
    };

    let _ = incognito.delete_cookie(&cookie.name, &cookie.domain, &cookie.path);
    incognito.set_cookie(&cookie)?;
    pump_windows_messages_for(std::time::Duration::from_millis(250));

    let incognito_cookies = request_cookies(&mut incognito, std::time::Duration::from_secs(3))?;
    require_cookie(
        &incognito_cookies,
        &cookie,
        "incognito-test: InPrivate producer did not report the cookie after set_cookie",
    )?;
    drop(incognito);
    pump_windows_messages_for(std::time::Duration::from_millis(250));

    let persistent_config = scrying::PlatformWebSurfaceConfig::new(
        winit::dpi::PhysicalSize::new(
            COMPOSITION_PROBE_WIDTH as u32,
            COMPOSITION_PROBE_HEIGHT as u32,
        ),
        user_data_dir,
    )
    .with_offset(
        COMPOSITION_PROBE_X + COMPOSITION_PROBE_WIDTH + 24.0,
        COMPOSITION_PROBE_Y,
    )
    .with_diagnostic_backdrop((59, 92, 72));
    let mut persistent =
        unsafe { scrying::PlatformWebSurfaceProducer::new(parent_hwnd, persistent_config)? };
    persistent.navigate_to_string(
        &browser_test_html("incognito-persistent"),
        std::time::Duration::from_secs(5),
    )?;

    let persistent_cookies = request_cookies(&mut persistent, std::time::Duration::from_secs(3))?;
    if contains_cookie(&persistent_cookies, &cookie) {
        return Err(format!(
            "incognito-test: persistent producer saw InPrivate cookie {:?}",
            cookie.name
        )
        .into());
    }

    println!(
        "demo-win: incognito-test: PASS - InPrivate cookie stayed isolated from the persistent user_data_dir profile"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn require_cookie(
    cookies: &[Cookie],
    expected: &Cookie,
    context: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let observed = cookies
        .iter()
        .find(|candidate| cookie_identity_matches(candidate, expected))
        .ok_or_else(|| format!("{context} ({} cookies observed)", cookies.len()))?;
    if observed.value != expected.value {
        return Err(format!(
            "{context}: cookie value mismatch: expected {:?}, got {:?}",
            expected.value, observed.value
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn contains_cookie(cookies: &[Cookie], expected: &Cookie) -> bool {
    cookies
        .iter()
        .any(|candidate| cookie_identity_matches(candidate, expected))
}

#[cfg(target_os = "windows")]
fn cookie_identity_matches(candidate: &Cookie, expected: &Cookie) -> bool {
    candidate.name == expected.name
        && candidate.domain.trim_start_matches('.') == expected.domain
        && candidate.path == expected.path
}

#[cfg(target_os = "windows")]
fn validate_platform_browser_controls(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    let first_ready = "browser-test:ready:first";
    let second_ready = "browser-test:ready:second";

    drain_web_messages(producer);
    producer.navigate_to_string(
        &browser_test_html("first"),
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(producer, first_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    producer.navigate_to_string(
        &browser_test_html("second"),
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    wait_for_title(
        producer,
        "Scrying Browser Test second",
        std::time::Duration::from_secs(2),
    )?;
    drain_web_messages(producer);

    if !producer.can_go_back() {
        return Err("browser-test: WebView2 did not report a back-history entry".into());
    }
    if producer.can_go_forward() {
        return Err(
            "browser-test: WebView2 unexpectedly reported forward history before back".into(),
        );
    }

    if !producer.go_back()? {
        return Err("browser-test: go_back returned false despite can_go_back".into());
    }
    wait_for_web_message(producer, first_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    if !producer.can_go_forward() {
        return Err("browser-test: WebView2 did not report a forward-history entry".into());
    }
    if !producer.go_forward()? {
        return Err("browser-test: go_forward returned false despite can_go_forward".into());
    }
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    producer.reload()?;
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    producer.stop()?;

    producer.apply_settings(&WebSurfaceSettings {
        zoom_factor: Some(1.05),
        user_agent: Some("scrying-demo-win-browser-test/1.0".into()),
        devtools_enabled: Some(false),
        javascript_enabled: Some(true),
        default_context_menus_enabled: Some(false),
        builtin_accelerator_keys_enabled: Some(false),
        inactive_scheduling_policy: None,
    })?;
    producer.set_visible(false)?;
    pump_windows_messages_for(std::time::Duration::from_millis(100));
    producer.set_visible(true)?;
    producer.apply_settings(&WebSurfaceSettings {
        zoom_factor: Some(1.0),
        user_agent: None,
        devtools_enabled: Some(true),
        javascript_enabled: Some(true),
        default_context_menus_enabled: Some(true),
        builtin_accelerator_keys_enabled: Some(true),
        inactive_scheduling_policy: None,
    })?;

    println!(
        "demo-win: browser-test: PASS - history, reload/stop, title, settings, and visibility controls verified"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn browser_test_html(label: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <title>Scrying Browser Test {label}</title>
    <style>
        html, body {{
            margin: 0;
            width: 100%;
            min-height: 100%;
            background: #11242a;
            color: #f6ead0;
            font-family: system-ui, sans-serif;
        }}
        body {{
            box-sizing: border-box;
            padding: 18px;
        }}
        main {{
            display: grid;
            gap: 8px;
        }}
    </style>
</head>
<body>
    <main>
        <h1>Browser Test {label}</h1>
        <p>WebView2 history and lifecycle probe.</p>
    </main>
    <script>
        const post = value => window.chrome.webview.postMessage(value);
        window.addEventListener("pageshow", () => post("browser-test:ready:{label}"));
    </script>
</body>
</html>"#
    )
}

#[cfg(target_os = "windows")]
fn drain_web_messages(producer: &mut scrying::PlatformWebSurfaceProducer) {
    while producer.poll_web_message().is_some() {}
}

#[cfg(target_os = "windows")]
fn drain_navigation_events(producer: &mut scrying::PlatformWebSurfaceProducer) {
    while producer.poll_navigation_event().is_some() {}
}

#[cfg(target_os = "windows")]
fn wait_for_new_window_request(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::NewWindowRequested { url } => return Ok(url),
                other => last_event = format!("{other:?}"),
            }
        }
    }

    Err(
        format!("timed out waiting for NewWindowRequested; last navigation event {last_event:?}")
            .into(),
    )
}

#[cfg(target_os = "windows")]
fn wait_for_content_process_terminated(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::ContentProcessTerminated => return Ok(()),
                other => last_event = format!("{other:?}"),
            }
        }
    }

    Err(format!(
        "timed out waiting for ContentProcessTerminated; last navigation event {last_event:?}"
    )
    .into())
}

#[cfg(target_os = "windows")]
fn wait_for_title(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    expected: &str,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::TitleChanged { title } if title == expected => return Ok(()),
                other => last_event = format!("{other:?}"),
            }
        }
    }

    Err(
        format!("timed out waiting for title {expected:?}; last navigation event {last_event:?}")
            .into(),
    )
}

#[cfg(target_os = "windows")]
fn validate_platform_cookie_store(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    let cookie_name = format!("demo_win_cookie_{}", std::process::id());
    let cookie = Cookie {
        name: cookie_name.clone(),
        value: "cookie-test".into(),
        domain: "example.com".into(),
        path: "/".into(),
        expires_at: None,
        is_secure: false,
        is_http_only: true,
    };

    let _ = producer.delete_cookie(&cookie.name, &cookie.domain, &cookie.path);
    producer.set_cookie(&cookie)?;
    pump_windows_messages_for(std::time::Duration::from_millis(250));

    let cookies = request_cookies(producer, std::time::Duration::from_secs(3))?;
    let observed = cookies
        .iter()
        .find(|candidate| cookie_identity_matches(candidate, &cookie))
        .ok_or_else(|| {
            format!(
                "cookie-test: cookie {:?} was not visible after set_cookie ({} cookies observed)",
                cookie.name,
                cookies.len()
            )
        })?;

    if observed.value != cookie.value {
        return Err(format!(
            "cookie-test: cookie value mismatch: expected {:?}, got {:?}",
            cookie.value, observed.value
        )
        .into());
    }
    if !observed.is_http_only {
        return Err("cookie-test: HttpOnly flag did not round-trip".into());
    }

    producer.delete_cookie(&cookie.name, &cookie.domain, &cookie.path)?;
    pump_windows_messages_for(std::time::Duration::from_millis(250));
    let cookies_after_delete = request_cookies(producer, std::time::Duration::from_secs(3))?;
    let still_present = contains_cookie(&cookies_after_delete, &cookie);
    if still_present {
        return Err(format!(
            "cookie-test: cookie {:?} was still visible after delete_cookie",
            cookie.name
        )
        .into());
    }

    println!(
        "demo-win: cookie-test: PASS - set/read/delete round-trip verified for {}",
        cookie.name
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn request_cookies(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<Vec<Cookie>, Box<dyn std::error::Error>> {
    while producer.poll_cookies().is_some() {}
    producer.request_all_cookies()?;

    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        if let Some(cookies) = producer.poll_cookies() {
            return Ok(cookies);
        }
    }

    Err(format!("timed out waiting {timeout:?} for cookie query result").into())
}

#[cfg(target_os = "windows")]
fn keyboard_validate_enabled() -> bool {
    std::env::var("WEBVIEW_KEYBOARD_VALIDATE")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
}

#[cfg(target_os = "windows")]
fn validate_platform_keyboard_smoke(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const EXPECTED: &str = "scry42";

    producer.move_focus(scrying::FocusReason::Programmatic)?;
    pump_windows_messages_for(std::time::Duration::from_millis(150));

    for ch in EXPECTED.chars() {
        let virtual_key_code = match ch {
            'a'..='z' => ch.to_ascii_uppercase() as u32,
            'A'..='Z' => ch as u32,
            '0'..='9' => ch as u32,
            _ => return Err(format!("unsupported keyboard smoke character: {ch:?}").into()),
        };
        let characters = ch.to_string();
        producer.send_keyboard_input(scrying::KeyboardInput {
            kind: scrying::KeyEventKind::Down,
            virtual_key_code,
            characters: characters.clone(),
            characters_ignoring_modifiers: characters,
            modifiers: scrying::KeyModifierFlags::default(),
            is_repeat: false,
        })?;
        producer.send_keyboard_input(scrying::KeyboardInput {
            kind: scrying::KeyEventKind::Up,
            virtual_key_code,
            characters: String::new(),
            characters_ignoring_modifiers: String::new(),
            modifiers: scrying::KeyModifierFlags::default(),
            is_repeat: false,
        })?;
        pump_windows_messages_for(std::time::Duration::from_millis(30));
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut last_value = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(message) = producer.poll_web_message() {
            if let Some(value) = message.strip_prefix("keyboard-smoke:") {
                last_value = value.to_string();
                if value == EXPECTED {
                    println!("WebView2 keyboard smoke: typed {value:?} via send_keyboard_input");
                    return Ok(());
                }
            }
        }
    }

    Err(format!(
        "WebView2 keyboard smoke timed out: expected {EXPECTED:?}, last observed {last_value:?}"
    )
    .into())
}

#[cfg(target_os = "windows")]
fn validate_platform_scripted(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const MESSAGE: &str = "ping-from-demo-win";
    const EXPECTED_ECHO: &str = "scripted:host-echo:ping-from-demo-win";

    wait_for_web_message(
        producer,
        "scripted:ready",
        std::time::Duration::from_secs(2),
    )?;
    producer.post_web_message(MESSAGE)?;
    wait_for_web_message(producer, EXPECTED_ECHO, std::time::Duration::from_secs(2))?;

    producer.send_mouse_input(scrying::MouseInput {
        kind: scrying::MouseEventKind::Move,
        virtual_keys: scrying::MouseVirtualKeys::default(),
        mouse_data: 0,
        point: (32, 32),
    })?;
    producer.send_mouse_input(scrying::MouseInput {
        kind: scrying::MouseEventKind::Wheel,
        virtual_keys: scrying::MouseVirtualKeys::default(),
        mouse_data: 120,
        point: (32, 32),
    })?;

    producer.move_focus(scrying::FocusReason::Programmatic)?;
    for ch in ['a', 'b', 'c'] {
        send_scripted_key_pair(producer, ch)?;
    }

    println!(
        "demo-win: scripted: PASS - JS message round-trip plus mouse/keyboard API dispatch verified"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn wait_for_web_message(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    expected: &str,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_message = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(message) = producer.poll_web_message() {
            if message == expected {
                return Ok(());
            }
            last_message = message;
        }
    }

    Err(
        format!("timed out waiting for web message {expected:?}; last observed {last_message:?}")
            .into(),
    )
}

#[cfg(target_os = "windows")]
fn send_scripted_key_pair(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    ch: char,
) -> Result<(), Box<dyn std::error::Error>> {
    let virtual_key_code = match ch {
        'a'..='z' => ch.to_ascii_uppercase() as u32,
        'A'..='Z' => ch as u32,
        '0'..='9' => ch as u32,
        _ => return Err(format!("unsupported scripted keyboard character: {ch:?}").into()),
    };
    let characters = ch.to_string();
    producer.send_keyboard_input(scrying::KeyboardInput {
        kind: scrying::KeyEventKind::Down,
        virtual_key_code,
        characters: characters.clone(),
        characters_ignoring_modifiers: characters,
        modifiers: scrying::KeyModifierFlags::default(),
        is_repeat: false,
    })?;
    producer.send_keyboard_input(scrying::KeyboardInput {
        kind: scrying::KeyEventKind::Up,
        virtual_key_code,
        characters: String::new(),
        characters_ignoring_modifiers: String::new(),
        modifiers: scrying::KeyModifierFlags::default(),
        is_repeat: false,
    })?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn pump_windows_messages_for(duration: std::time::Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        unsafe {
            let mut message = MSG::default();
            let mut drained = 0u32;
            while std::time::Instant::now() < deadline
                && PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool()
            {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                drained += 1;
                if drained >= 256 {
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

#[cfg(target_os = "windows")]
fn validate_imported_pixels(
    imported: &scrying::ImportedTexture,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    expected_rgb: (u8, u8, u8),
) -> Result<(), Box<dyn std::error::Error>> {
    if imported.format != wgpu::TextureFormat::Bgra8Unorm {
        return Err(format!(
            "WebView readback: expected Bgra8Unorm imported texture, got {:?}",
            imported.format
        )
        .into());
    }

    let width = imported.size.width;
    let height = imported.size.height;
    let bytes_per_pixel = 4u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
    let buffer_size = (padded_bytes_per_row as u64) * (height as u64);

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("webview-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("webview-readback-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &imported.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()
        .map_err(|error| format!("readback channel closed: {error}"))?
        .map_err(|error| format!("buffer map failed: {error}"))?;
    let data = slice.get_mapped_range();

    let row_stride = padded_bytes_per_row as usize;
    let sample = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y as usize) * row_stride + (x as usize) * 4;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };

    let inset = 4u32
        .min(width.saturating_sub(1))
        .min(height.saturating_sub(1));
    let tl = sample(inset, inset);
    let tr = sample(width.saturating_sub(1 + inset), inset);
    let bl = sample(inset, height.saturating_sub(1 + inset));
    let br = sample(
        width.saturating_sub(1 + inset),
        height.saturating_sub(1 + inset),
    );
    let center = sample(width / 2, height / 2);

    drop(data);
    buffer.unmap();

    let (er, eg, eb) = expected_rgb;
    println!(
        "WebView readback: expected background BGRA=({},{},{},255) from CSS rgb({},{},{})",
        eb, eg, er, er, eg, eb
    );
    println!(
        "WebView readback: tl=BGRA{:?} tr=BGRA{:?} bl=BGRA{:?} br=BGRA{:?} center=BGRA{:?}",
        tl, tr, bl, br, center
    );

    let tolerance: i32 = 6;
    let close_to_background = |bgra: [u8; 4]| -> bool {
        let [b, g, r, _a] = bgra;
        (b as i32 - eb as i32).abs() <= tolerance
            && (g as i32 - eg as i32).abs() <= tolerance
            && (r as i32 - er as i32).abs() <= tolerance
    };
    let corners_match = close_to_background(tl)
        && close_to_background(tr)
        && close_to_background(bl)
        && close_to_background(br);
    println!(
        "WebView readback: corner pixels match background within ±{tolerance}: {corners_match}"
    );

    if !corners_match {
        return Err(
            "WebView readback: corner pixels do not match the HTML background; \
             capture content is likely wrong (zeros, swapped channels, or empty)."
                .into(),
        );
    }

    let nonzero_alpha = tl[3] > 0 || tr[3] > 0 || bl[3] > 0 || br[3] > 0 || center[3] > 0;
    if !nonzero_alpha {
        return Err(
            "WebView readback: every sampled alpha is zero; capture is likely uninitialized."
                .into(),
        );
    }

    Ok(())
}

async fn create_host_device() -> Result<
    (wgpu::Instance, wgpu::Device, wgpu::Queue, wgpu::AdapterInfo),
    Box<dyn std::error::Error>,
> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: preferred_backends(),
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .map_err(|error| format!("adapter request failed: {error}"))?;

    let adapter_info = adapter.get_info();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("demo-win"),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("device request failed: {error}"))?;

    Ok((instance, device, queue, adapter_info))
}

fn preferred_backends() -> wgpu::Backends {
    if cfg!(target_os = "windows") {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::PRIMARY
    }
}

#[cfg(target_os = "windows")]
const WEBVIEW_BLIT_SHADER: &str = r#"
struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(uv.x, 1.0 - uv.y);
    return out;
}

@group(0) @binding(0) var captured: texture_2d<f32>;
@group(0) @binding(1) var captured_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(captured, captured_sampler, in.uv);
}
"#;

#[cfg(target_os = "windows")]
struct WebViewRenderer {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    importer: WgpuTextureImporter,
    captured: CapturedComposition,
    /// Tiny destination buffer for a 1×1 `copy_texture_to_buffer` issued
    /// each render. The copy itself is throwaway — the point is to force
    /// wgpu to emit a `SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE`
    /// transition barrier on the imported texture, which on D3D12 flushes
    /// shader caches that would otherwise hold the producer's first
    /// captured frame indefinitely. Without this the wgpu render goes
    /// stale even while the producer continuously `CopyResource`s into
    /// the same shared D3D11 texture.
    cache_flush_buffer: wgpu::Buffer,
    frames_imported: u64,
    frames_polled: u64,
    frames_acquired: u64,
    frames_resource_swapped: u64,
    consecutive_empty_polls: u32,
    capture_restarts: u64,
    last_metric_log: std::time::Instant,
    /// Pending producer resize, deferred until the user stops dragging.
    /// Stores the most recent target window size and the time of the last
    /// `Resized` event. We apply the producer rebuild only after a quiet
    /// period — `producer.resize` + `start_capture` together cost ~300ms,
    /// which would wedge the Win32 modal resize loop if run synchronously
    /// per-event during a drag.
    pending_producer_resize: Option<(winit::dpi::PhysicalSize<u32>, std::time::Instant)>,
    last_committed_capture_size: winit::dpi::PhysicalSize<u32>,
    /// Last (width, height) actually passed to `surface.configure`.
    /// Tracked separately from `surface_config.width/height` so we can
    /// notice when the resize handler updated the desired size and lazily
    /// reconfigure on the next render.
    configured_surface_size: (u32, u32),
}

#[cfg(target_os = "windows")]
impl WebViewRenderer {
    fn new(
        instance: wgpu::Instance,
        window: Arc<Window>,
        host: HostWgpuContext,
        captured: CapturedComposition,
        fence_synchronizer: Option<Dx12FenceSynchronizer>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let device = host.device.clone();
        let queue = host.queue.clone();
        let importer = match fence_synchronizer {
            Some(sync) => WgpuTextureImporter::with_synchronizer(host.clone(), Box::new(sync)),
            None => WgpuTextureImporter::new(host.clone()),
        };
        let surface = instance.create_surface(window.clone())?;
        let size = window.inner_size();

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|error| format!("renderer adapter request failed: {error}"))?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm))
            .unwrap_or_else(|| caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("webview-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(WEBVIEW_BLIT_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("webview-blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("webview-blit-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("webview-blit-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("webview-blit-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        // 1×1 placeholder so the bind group has something to point at until
        // the first real WebView frame lands via try_acquire_frame.
        let placeholder_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("webview-blit-placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bind_group = match captured.imported.as_ref() {
            Some(imported) => build_bind_group(&device, &bind_group_layout, &sampler, imported),
            None => build_bind_group_for_texture(
                &device,
                &bind_group_layout,
                &sampler,
                &placeholder_texture,
            ),
        };

        // 256 bytes is `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`; minimum size
        // that satisfies `copy_texture_to_buffer` row-stride rules even
        // for a 1×1 copy. We never read it.
        let cache_flush_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("webview-cache-flush-buffer"),
            size: wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            window,
            device,
            queue,
            surface,
            surface_config,
            pipeline,
            bind_group_layout,
            sampler,
            bind_group,
            importer,
            captured,
            cache_flush_buffer,
            frames_imported: 1,
            frames_polled: 0,
            frames_acquired: 0,
            frames_resource_swapped: 0,
            consecutive_empty_polls: 0,
            capture_restarts: 0,
            last_metric_log: std::time::Instant::now(),
            pending_producer_resize: None,
            last_committed_capture_size: capture_size_for_window(size),
            configured_surface_size: (size.width.max(1), size.height.max(1)),
        })
    }

    /// Poll the producer for a fresh capture frame.
    ///
    /// In steady state the producer reuses a single shared D3D11 destination
    /// texture, so most polls return `resource_is_new = false` — the bind
    /// group's `wgpu::Texture` already references the same memory and just
    /// needs to be re-rendered. Only when the producer (re-)allocates (first
    /// frame, post-resize) do we re-import and rebuild the bind group.
    fn refresh_captured_texture(&mut self) -> Result<bool, Box<dyn std::error::Error>> {
        use scrying::windows_capture::close_shared_handle;

        // Diagnostic: if FORCE_REIMPORT_EVERY_FRAME=1 in the env, drop the
        // producer's persistent dest before each acquire so every frame goes
        // through the full re-import path (fresh NT handle + new wgpu::Texture
        // + new bind group). This isolates whether the visible-frozen-wgpu
        // bug is a D3D11/D3D12 shared-texture cache coherence issue.
        if force_reimport_every_frame() {
            self.captured.producer.invalidate_persistent_dest();
        }

        self.frames_polled = self.frames_polled.saturating_add(1);

        let new_frame = match self.captured.producer.try_acquire_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                self.consecutive_empty_polls = self.consecutive_empty_polls.saturating_add(1);
                // ~120 polls / ~60Hz redraw ≈ 1s of no frames; assume the WGC
                // session is wedged after a resize and restart it.
                if self.consecutive_empty_polls >= 120 {
                    self.captured.producer.force_restart_capture();
                    self.capture_restarts = self.capture_restarts.saturating_add(1);
                    self.consecutive_empty_polls = 0;
                    eprintln!(
                        "demo-win: capture stalled after >=1s of empty polls; restarting capture session (restart #{})",
                        self.capture_restarts
                    );
                }
                self.maybe_log_metrics();
                return Ok(false);
            }
            Err(error) => {
                eprintln!("demo-win: try_acquire_frame failed: {error}");
                return Ok(false);
            }
        };

        self.frames_acquired = self.frames_acquired.saturating_add(1);
        self.consecutive_empty_polls = 0;

        if new_frame.resource_is_new {
            let WebSurfaceFrame::Native(ref native_frame) = new_frame.frame else {
                return Err("WebView2 producer did not emit a native frame".into());
            };
            let imported = self
                .importer
                .import_frame(native_frame, &ImportOptions::default())?;
            unsafe {
                close_shared_handle(new_frame.shared_handle)?;
            }

            self.bind_group = build_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.sampler,
                &imported,
            );
            self.captured.imported = Some(imported);
            self.frames_imported = self.frames_imported.saturating_add(1);
            self.frames_resource_swapped = self.frames_resource_swapped.saturating_add(1);
        }

        self.maybe_log_metrics();
        Ok(true)
    }

    fn maybe_log_metrics(&mut self) {
        let elapsed = self.last_metric_log.elapsed();
        if elapsed < std::time::Duration::from_secs(2) {
            return;
        }
        let secs = elapsed.as_secs_f64().max(0.001);
        println!(
            "renderer metrics ({:.1}s): polled={}, acquired={} ({:.1}/s), resource_swaps={}, total_imports={}, capture_restarts={}",
            secs,
            self.frames_polled,
            self.frames_acquired,
            (self.frames_acquired as f64) / secs,
            self.frames_resource_swapped,
            self.frames_imported,
            self.capture_restarts,
        );
        self.frames_polled = 0;
        self.frames_acquired = 0;
        self.frames_resource_swapped = 0;
        self.last_metric_log = std::time::Instant::now();
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        // Truly O(1): only stash state. Even `surface.configure` is too
        // expensive to call from inside the Win32 modal resize loop —
        // winit fires WindowEvent::Resized at very high cadence during a
        // drag and a few-ms-per-call configure starves the modal loop's
        // input processing, locking the cursor in resize-arrow mode.
        // Both surface reconfigure and producer rebuild happen lazily
        // from `render()`.
        self.surface_config.width = new_size.width;
        self.surface_config.height = new_size.height;
        self.pending_producer_resize = Some((new_size, std::time::Instant::now()));
    }

    /// Apply a deferred producer resize once the user has stopped dragging.
    /// Returns true if a producer rebuild actually ran.
    fn apply_pending_resize(&mut self) -> bool {
        const SETTLE_MS: u128 = 120;
        let (target_size, last_event) = match self.pending_producer_resize {
            Some(p) => p,
            None => return false,
        };
        let elapsed = last_event.elapsed().as_millis();
        if elapsed < SETTLE_MS {
            return false;
        }
        let capture_size = capture_size_for_window(target_size);
        if capture_size == self.last_committed_capture_size {
            self.pending_producer_resize = None;
            return false;
        }
        let (offset_x, offset_y) = capture_offset_for_window(target_size);
        println!(
            "resize (settled): window={}x{} -> capture={}x{} offset=({}, {})",
            target_size.width,
            target_size.height,
            capture_size.width,
            capture_size.height,
            offset_x,
            offset_y
        );
        let _ = elapsed;
        if let Err(error) = self.captured.producer.set_offset(offset_x, offset_y) {
            eprintln!("demo-win: producer.set_offset({offset_x}, {offset_y}) failed: {error}");
        }
        if let Err(error) = self.captured.producer.resize(capture_size) {
            eprintln!(
                "demo-win: producer.resize({}x{}) failed: {error}",
                capture_size.width, capture_size.height
            );
        }
        self.last_committed_capture_size = capture_size;
        self.pending_producer_resize = None;
        true
    }

    fn render(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Lazy surface reconfigure, kept out of the resize event handler so
        // it can't run inside the Win32 modal resize loop.
        let desired = (self.surface_config.width, self.surface_config.height);
        if desired != self.configured_surface_size {
            self.surface.configure(&self.device, &self.surface_config);
            self.configured_surface_size = desired;
        }
        self.apply_pending_resize();
        let _ = self.refresh_captured_texture()?;
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("webview-blit-encoder"),
            });

        // Force wgpu to insert a SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE
        // transition on the imported texture by issuing a throwaway 1×1 copy
        // before the render pass samples it. On D3D12 the transition flushes
        // the shader caches that would otherwise hold a stale view of the
        // externally-written shared NT-handle texture.
        if let Some(imported) = self.captured.imported.as_ref() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &imported.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.cache_flush_buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("webview-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.07,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.window.pre_present_notify();
        frame.present();
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn build_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    imported: &scrying::ImportedTexture,
) -> wgpu::BindGroup {
    build_bind_group_for_texture(device, layout, sampler, &imported.texture)
}

#[cfg(target_os = "windows")]
fn build_bind_group_for_texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    texture: &wgpu::Texture,
) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("webview-blit-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Half the window's width and full height for the WebView capture target.
/// Pairs with the demo's right-half overlay layout: the live composition
/// visual sits on the right, and the wgpu surface fills the whole window
/// behind it with the captured texture stretched, so resize keeps the two
/// in sync.
#[cfg(target_os = "windows")]
fn capture_size_for_window(
    window_size: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    let w = (window_size.width / 2).max(120);
    let h = window_size.height.max(120);
    winit::dpi::PhysicalSize::new(w, h)
}

/// Top-left of the WinComp overlay relative to the parent window. Pairs with
/// `capture_size_for_window`: pin the right-half overlay flush against the
/// window's right edge so it tracks the intended layout as the window resizes.
#[cfg(target_os = "windows")]
fn force_reimport_every_frame() -> bool {
    std::env::var("FORCE_REIMPORT_EVERY_FRAME")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
}

#[cfg(target_os = "windows")]
fn capture_offset_for_window(window_size: winit::dpi::PhysicalSize<u32>) -> (f32, f32) {
    let capture = capture_size_for_window(window_size);
    let x = window_size.width.saturating_sub(capture.width) as f32;
    (x, 0.0)
}
