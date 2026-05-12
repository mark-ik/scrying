//! Minimal winit + wgpu host probe for scrying system-webview texture interop.

#[cfg(target_os = "windows")]
mod gpu;
#[cfg(target_os = "windows")]
mod probe;
#[cfg(target_os = "windows")]
mod renderer;
#[cfg(target_os = "windows")]
mod smokes;
#[cfg(target_os = "windows")]
mod waits;

use std::sync::Arc;

#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(target_os = "windows")]
use scrying::Dx12FenceSynchronizer;
use scrying::{
    HostWgpuContext, ImportOptions, NavigationEvent, TextureImporter, WebSurfaceCapabilities,
    WebSurfaceFrame, WebSurfaceSettings, WgpuTextureImporter,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::Window;

#[cfg(target_os = "windows")]
use gpu::{create_host_device, validate_imported_pixels};
#[cfg(target_os = "windows")]
use probe::{
    composition_contains, drain_composition_events, forward_mouse_to_composition,
    run_platform_composition_visual_probe, run_windows_shared_texture_probe,
    validate_platform_multi_view,
};
#[cfg(target_os = "windows")]
use renderer::WebViewRenderer;
#[cfg(target_os = "windows")]
use smokes::browser::{
    validate_platform_browser_controls, validate_platform_context_menu,
    validate_platform_drop_observability, validate_platform_find,
    validate_platform_media_capture_observability, validate_platform_pdf,
    validate_platform_popup_routing,
};
#[cfg(target_os = "windows")]
use smokes::capture::{validate_platform_capture, validate_platform_scale_resize};
#[cfg(target_os = "windows")]
use smokes::input::{
    keyboard_validate_enabled, validate_platform_keyboard_smoke, validate_platform_scripted,
};
#[cfg(target_os = "windows")]
use smokes::network::{
    validate_platform_basic_auth, validate_platform_downloads, validate_platform_permissions,
    validate_platform_process_failure_recovery, validate_platform_virtual_host_routing,
    validate_platform_visibility,
};
#[cfg(target_os = "windows")]
use smokes::profile::{
    validate_platform_cookie_store, validate_platform_incognito_store,
    validate_platform_profile_store,
};
#[cfg(target_os = "windows")]
pub(crate) use waits::*;

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
    download_test: bool,
    auth_test: bool,
    permission_test: bool,
    visibility_test: bool,
    keyboard_test: bool,
    multi_view_test: bool,
    find_test: bool,
    pdf_test: bool,
    context_test: bool,
    drop_test: bool,
    media_test: bool,
    capture_test: bool,
    scale_test: bool,
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
                "--download-test" => cli.download_test = true,
                "--auth-test" => cli.auth_test = true,
                "--permission-test" => cli.permission_test = true,
                "--visibility-test" => cli.visibility_test = true,
                "--keyboard-test" => cli.keyboard_test = true,
                "--multi-view-test" => cli.multi_view_test = true,
                "--find-test" => cli.find_test = true,
                "--pdf-test" => cli.pdf_test = true,
                "--context-test" => cli.context_test = true,
                "--drop-test" => cli.drop_test = true,
                "--media-test" => cli.media_test = true,
                "--capture-test" => cli.capture_test = true,
                "--scale-test" => cli.scale_test = true,
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
            || self.download_test
            || self.auth_test
            || self.permission_test
            || self.visibility_test
            || self.keyboard_test
            || self.multi_view_test
            || self.find_test
            || self.pdf_test
            || self.context_test
            || self.drop_test
            || self.media_test
            || self.capture_test
            || self.scale_test
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(state) = self.state.as_mut() {
                    let new_size = state.window.inner_size();
                    println!(
                        "scale-factor changed: scale={} window={}x{}",
                        scale_factor, new_size.width, new_size.height
                    );
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
        let captured = if cli.multi_view_test {
            validate_platform_multi_view(event_loop, &window)?;
            None
        } else {
            run_platform_composition_visual_probe(&window, &host, fence_handle, cli)?
        };

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
fn capture_offset_for_window(window_size: winit::dpi::PhysicalSize<u32>) -> (f32, f32) {
    let capture = capture_size_for_window(window_size);
    let x = window_size.width.saturating_sub(capture.width) as f32;
    (x, 0.0)
}
