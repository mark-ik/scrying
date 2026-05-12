use super::*;

pub(crate) fn composition_contains(pos: winit::dpi::PhysicalPosition<f64>) -> bool {
    let x = pos.x as f32;
    let y = pos.y as f32;
    x >= COMPOSITION_PROBE_X
        && x < COMPOSITION_PROBE_X + COMPOSITION_PROBE_WIDTH
        && y >= COMPOSITION_PROBE_Y
        && y < COMPOSITION_PROBE_Y + COMPOSITION_PROBE_HEIGHT
}

pub(crate) fn forward_mouse_to_composition(
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

pub(crate) fn drain_composition_events(renderer: &mut WebViewRenderer) {
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

fn composition_probe_html(scripted: bool) -> &'static str {
    if scripted {
        COMPOSITION_WEBVIEW_SCRIPTED_HTML
    } else {
        COMPOSITION_WEBVIEW_PROBE_HTML
    }
}

pub(crate) fn run_windows_shared_texture_probe(
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

fn hwnd_from_window(window: &Window) -> Result<*mut std::ffi::c_void, Box<dyn std::error::Error>> {
    let handle = window.window_handle()?.as_raw();
    match handle {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as *mut std::ffi::c_void),
        other => Err(format!("expected Win32 raw window handle, got {other:?}").into()),
    }
}

pub(crate) fn validate_platform_multi_view(
    event_loop: &ActiveEventLoop,
    primary_window: &Window,
) -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::WinRT::{
        CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
        DispatcherQueueOptions,
    };

    let _dispatcher_queue = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })
    }
    .ok();

    let secondary_window = event_loop.create_window(
        Window::default_attributes()
            .with_title("demo-win secondary")
            .with_inner_size(winit::dpi::PhysicalSize::new(640, 420)),
    )?;

    let primary_config = scrying::PlatformWebSurfaceConfig::new(
        winit::dpi::PhysicalSize::new(360, 260),
        std::env::temp_dir().join("demo-win-multi-view-primary"),
    )
    .with_offset(24.0, 24.0)
    .with_diagnostic_backdrop((50, 70, 92));
    let secondary_config = scrying::PlatformWebSurfaceConfig::new(
        winit::dpi::PhysicalSize::new(360, 260),
        std::env::temp_dir().join("demo-win-multi-view-secondary"),
    )
    .with_offset(24.0, 24.0)
    .with_diagnostic_backdrop((80, 64, 72));

    let primary_hwnd = hwnd_from_window(primary_window)?;
    let secondary_hwnd = hwnd_from_window(&secondary_window)?;
    let mut primary =
        unsafe { scrying::PlatformWebSurfaceProducer::new(primary_hwnd, primary_config)? };
    let mut secondary =
        unsafe { scrying::PlatformWebSurfaceProducer::new(secondary_hwnd, secondary_config)? };

    primary.navigate_to_string(
        &multi_view_html("primary"),
        std::time::Duration::from_secs(5),
    )?;
    secondary.navigate_to_string(
        &multi_view_html("secondary"),
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(
        &mut primary,
        "multi-view:primary:ready",
        std::time::Duration::from_secs(2),
    )?;
    wait_for_web_message(
        &mut secondary,
        "multi-view:secondary:ready",
        std::time::Duration::from_secs(2),
    )?;

    drop(secondary);
    drop(primary);
    drop(secondary_window);
    println!(
        "demo-win: multi-view-test: PASS - two simultaneous WebView2 composition producers ran on separate HWNDs"
    );
    Ok(())
}

fn multi_view_html(label: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><title>{label}</title></head>
<body style="margin:0;display:grid;place-items:center;height:100vh;background:#17202a;color:#f8f1d8;font:16px system-ui,sans-serif;">
<main>{label}</main>
<script>window.chrome.webview.postMessage("multi-view:{label}:ready");</script>
</body>
</html>"#
    )
}

pub(crate) struct CapturedComposition {
    /// The most recently imported WebView texture; `None` until the renderer's
    /// first `try_acquire_frame` lands a frame. The probe path no longer
    /// blocks on an initial acquire (it was an intermittent pump-hang).
    pub(crate) imported: Option<scrying::ImportedTexture>,
    pub(crate) producer: scrying::PlatformWebSurfaceProducer,
    #[allow(dead_code)]
    pub(crate) dispatcher_queue: Option<windows::System::DispatcherQueueController>,
}

pub(crate) fn run_platform_composition_visual_probe(
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
    if cli.download_test {
        validate_platform_downloads(&mut producer)?;
    }
    if cli.auth_test {
        validate_platform_basic_auth(&mut producer)?;
    }
    if cli.permission_test {
        validate_platform_permissions(&mut producer)?;
    }
    if cli.visibility_test {
        validate_platform_visibility(&mut producer)?;
    }
    if cli.find_test {
        validate_platform_find(&mut producer)?;
    }
    if cli.pdf_test {
        validate_platform_pdf(&mut producer)?;
    }
    if cli.context_test {
        validate_platform_context_menu(&mut producer)?;
    }
    if cli.media_test {
        validate_platform_media_capture_observability(&mut producer)?;
    }
    if cli.capture_test {
        validate_platform_capture(&mut producer, host)?;
        return Ok(None);
    }
    if cli.profile_test {
        validate_platform_profile_store(producer, parent_hwnd, user_data_dir)?;
        return Ok(None);
    }
    if cli.incognito_test {
        validate_platform_incognito_store(producer, parent_hwnd, user_data_dir)?;
        return Ok(None);
    }
    if cli.keyboard_test || keyboard_validate_enabled() {
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

fn validate_platform_capture(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    host: &HostWgpuContext,
) -> Result<(), Box<dyn std::error::Error>> {
    use scrying::windows_capture::close_shared_handle;

    let captured = producer.acquire_full_frame()?;
    let importer = WgpuTextureImporter::new(host.clone());
    let WebSurfaceFrame::Native(ref native_frame) = captured.frame else {
        return Err("WebView2 composition producer did not emit a native frame".into());
    };
    let imported = importer.import_frame(native_frame, &ImportOptions::default())?;
    let metrics = producer.capture_metrics();
    println!(
        "demo-win: capture-test: captured {}x{}, imported {:?} {}x{} generation {}, received={}, consumed={}, stale_dropped={}",
        captured.content_size.width,
        captured.content_size.height,
        imported.format,
        imported.size.width,
        imported.size.height,
        imported.generation,
        metrics.samples_received,
        metrics.samples_consumed,
        metrics.stale_frames_dropped,
    );
    unsafe {
        close_shared_handle(captured.shared_handle)?;
    }
    println!("demo-win: capture-test: PASS - WebView2 WGC frame acquired and imported");
    Ok(())
}
