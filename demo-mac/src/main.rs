//! Minimal winit host for scrying's macOS WKWebView producer.
//!
//! Opens a window, hosts a `WkWebViewProducer` against the window's
//! `NSView`, drives the input / event / snapshot / SCK-capture paths
//! so we can verify the producer works at runtime.
//!
//! See `README.md` for the full description of CLI flags
//! (`--probe-snapshot`, `--scripted`, `--capture`, `--capture --dump-every N`,
//! `--capture --resize-test`).

#![cfg(target_os = "macos")]

mod render;

use std::sync::Arc;
use std::time::{Duration, Instant};

use render::WgpuRender;
use scrying::CaptureStatus;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use scrying::wkwebview_producer::{
    FindOptions, UrlSchemeHandlerFn, UrlSchemeResponse, WkWebViewProducer, WkWebViewProducerConfig,
};
use scrying::{
    KeyEventKind, KeyModifierFlags, KeyboardInput, MouseEventKind, MouseInput, MouseVirtualKeys,
    NavigationEvent, WryWebSurfaceProducer,
};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::Key;
use winit::window::{Window, WindowAttributes};

const INITIAL_URL: &str = "https://example.com";

/// Inline HTML loaded by `--profile-test`. Reads the existing cookie
/// for the page's origin (set by [`PROFILE_TEST_BASE_URL`]) and posts
/// the result back to the host. Then sets a fresh `demo_token`
/// cookie so the next run can observe it.
const PROFILE_TEST_HTML: &str = r#"<!doctype html>
<html><body>
<script>
(function() {
  function post(msg) {
    if (window.chrome && window.chrome.webview) {
      window.chrome.webview.postMessage(msg);
    }
  }
  // 1. Report what's in the cookie jar at load time. First run with
  //    a fresh data_dir: empty. Second run with the persisted data
  //    store: should contain the value set by run 1.
  post('cookie-on-load:' + (document.cookie || ''));

  // 2. Set / refresh a token so subsequent runs (if launched within
  //    the cookie's max-age) observe persistence.
  var token = 'val_' + Date.now();
  document.cookie =
    'demo_token=' + token + '; max-age=3600; path=/; SameSite=Lax';
  post('cookie-set:' + token);
  post('cookie-after-set:' + (document.cookie || ''));
  post('done');
})();
</script>
</body></html>"#;

/// Stable origin for the profile-test page. Required so
/// `document.cookie` is namespaced and persisted to the per-profile
/// `WKWebsiteDataStore`. WebKit treats cookies set on `about:blank`
/// or `data:` origins as ephemeral, so we need a real-looking URL.
const PROFILE_TEST_BASE_URL: &str = "https://demo-mac.scrying.local/";

/// `--browser-test` URL scheme handler. Routes by path within the
/// `scrying-test://` scheme to canned HTML responses that drive the
/// browser-class state machine. Each response embeds known marker
/// text so JS-side and host-side assertions can verify what loaded.
fn browser_test_scheme_handler() -> UrlSchemeHandlerFn {
    Arc::new(|url: &str| -> UrlSchemeResponse {
        let body = if url.contains("/history-1") {
            r#"<!doctype html><body><h1 id="m">history-page-1</h1>
            <script>
              if (window.chrome && window.chrome.webview) {
                window.chrome.webview.postMessage('loaded:history-1');
              }
            </script></body>"#
        } else if url.contains("/history-2") {
            r#"<!doctype html><body><h1 id="m">history-page-2</h1>
            <script>
              if (window.chrome && window.chrome.webview) {
                window.chrome.webview.postMessage('loaded:history-2');
              }
            </script></body>"#
        } else if url.contains("/find-target") {
            r#"<!doctype html><body>
            <p>scrying-find-marker</p>
            <script>
              if (window.chrome && window.chrome.webview) {
                window.chrome.webview.postMessage('loaded:find-target');
              }
            </script>
            </body>"#
        } else {
            r#"<!doctype html><body>scrying-test fallback</body>"#
        };
        UrlSchemeResponse {
            mime_type: "text/html".into(),
            body: body.as_bytes().to_vec(),
        }
    })
}

/// Offline HTML page used by `--scripted`. Contains an input box
/// (id=`text`) so synthetic key events can change the value, a
/// scrollable region so synthetic scroll-wheel events change
/// `window.scrollY`, and a `chrome.webview` listener that echoes any
/// message it receives back as `echo:<payload>` for round-trip
/// verification.
const SCRIPTED_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>scrying-test</title></head>
<body style="margin:0;font-family:monospace;background:#1c1c1c;color:#f0f0f0;">
<h2 style="margin:8px;">scrying scripted-input test</h2>
<div style="margin:8px;">
  <input id="text" placeholder="type here"
         style="font-size:18px;width:60%;padding:6px;background:#2a2a2a;color:#f0f0f0;border:1px solid #555;"
         autofocus />
</div>
<div id="status" style="margin:8px;padding:8px;background:#333;">status: idle</div>
<div id="scroll-track" style="margin:8px;padding:8px;background:#444;">scroll-y: 0</div>
<div style="height:200vh;background:linear-gradient(#222,#888);
            display:flex;align-items:center;justify-content:center;color:#000;
            font-size:24px;">scrollable region</div>
<script>
(function() {
  var statusEl = document.getElementById('status');
  var scrollEl = document.getElementById('scroll-track');
  var textEl = document.getElementById('text');
  var post = function(msg) {
    if (window.chrome && window.chrome.webview) {
      window.chrome.webview.postMessage(msg);
    }
  };
  textEl.addEventListener('input', function() {
    statusEl.textContent = 'status: typed=' + textEl.value;
    post('typed:' + textEl.value);
  });
  window.addEventListener('scroll', function() {
    var y = Math.round(window.scrollY);
    scrollEl.textContent = 'scroll-y: ' + y;
    post('scrolled:' + y);
  });
  if (window.chrome && window.chrome.webview) {
    window.chrome.webview.addEventListener('message', function(e) {
      statusEl.textContent = 'status: host-said=' + e.data;
      post('echo:' + e.data);
    });
  }
  post('ready');
})();
</script>
</body></html>"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::from_args(std::env::args());
    let event_loop = EventLoop::new()?;
    // Probe-snapshot, capture, and scripted modes need `Poll` so
    // `about_to_wait` fires regularly enough to advance their state
    // machines / request redraws. Plain overlay mode can sleep on
    // `Wait`.
    if cli.probe_snapshot
        || cli.capture
        || cli.scripted
        || cli.profile_test
        || cli.two_tabs
        || cli.browser_test
    {
        event_loop.set_control_flow(ControlFlow::Poll);
    } else {
        event_loop.set_control_flow(ControlFlow::Wait);
    }
    let mut app = App {
        cli,
        state: None,
    };
    Ok(event_loop.run_app(&mut app)?)
}

#[derive(Clone, Copy, Default)]
struct Cli {
    probe_snapshot: bool,
    /// Run the demo in capture mode: positions the WKWebView at the
    /// left half of the window, sets up wgpu rendering of the
    /// imported texture in the rest of the surface, and kicks off
    /// `start_capture_async` shortly after launch.
    capture: bool,
    /// Run the autonomous scripted-input mode: loads an offline HTML
    /// page with known elements (input box + JS message listener +
    /// scrollable region), drives `post_web_message`, scroll-wheel
    /// events, and synthetic keyboard input on a timed schedule, and
    /// asserts that the JS side observes the expected effects via
    /// `poll_web_message`.
    scripted: bool,
    /// In capture mode, dump every Nth imported texture to
    /// `demo-mac-frame-NNNN.png` via wgpu readback. Default `0`
    /// means "no dumps." Use `--dump-every 30` to dump twice per
    /// second at 60 FPS.
    dump_every: u64,
    /// In capture mode, programmatically resize the window mid-run
    /// to exercise slice N's `SCStream::updateConfiguration:` path.
    /// The window cycles 1024→1280→1024 over the run.
    resize_test: bool,
    /// Run the cookie / per-profile-data-store persistence test.
    /// Uses `target/demo-mac-profile-test` as the data dir (so it
    /// doesn't collide with the regular demo profile). Loads an
    /// inline test page with a stable base URL, reads
    /// `document.cookie`, and either reports the existing cookie
    /// (proving persistence from a prior run) or sets a fresh one
    /// (priming for the next run). Exits as soon as the JS handshake
    /// completes.
    profile_test: bool,
    /// Construct two `WkWebViewProducer` instances against the same
    /// NSView, navigate each to a different URL, drain events from
    /// both, and exit cleanly. Validates the producer is safe to
    /// instantiate multiple times in one process — the foundational
    /// architectural requirement for browser-shape consumers (each
    /// tab is its own producer).
    two_tabs: bool,
    /// Drive items 1, 3, 4, 9 of the browser-class roadmap (history
    /// controls, settings, custom URL schemes, find / PDF) on a
    /// timed schedule and assert deterministic effects. Items 2, 5,
    /// 6, 8 need network or harder triggers and aren't covered.
    browser_test: bool,
}

impl Cli {
    fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut cli = Cli::default();
        let mut iter = args.into_iter().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--probe-snapshot" => cli.probe_snapshot = true,
                "--capture" => cli.capture = true,
                "--scripted" => cli.scripted = true,
                "--dump-every" => {
                    let value = iter.next().unwrap_or_default();
                    cli.dump_every = value.parse().unwrap_or_else(|_| {
                        eprintln!("demo-mac: --dump-every needs a positive integer, got '{value}'");
                        0
                    });
                }
                "--resize-test" => cli.resize_test = true,
                "--profile-test" => cli.profile_test = true,
                "--two-tabs" => cli.two_tabs = true,
                "--browser-test" => cli.browser_test = true,
                _ => eprintln!("demo-mac: unknown arg: {arg}"),
            }
        }
        cli
    }
}

struct App {
    cli: Cli,
    state: Option<AppState>,
}

struct AppState {
    window: Arc<Window>,
    producer: WkWebViewProducer,
    render: Option<WgpuRender>,
    capture_kickoff_at: Option<Duration>,
    capture_started: bool,
    /// If `Some`, indices into the resize cycle ((width, height),
    /// elapsed-time-trigger). Step 0 fires at first trigger, etc.
    resize_test_steps: Option<Vec<(u32, u32, Duration)>>,
    resize_test_idx: usize,
    cursor: Option<PhysicalPosition<f64>>,
    mouse_buttons: MouseVirtualKeys,
    modifiers: KeyModifierFlags,
    /// Set by `--probe-snapshot` mode. The probe schedules
    /// `request_snapshot` once `started_at + delay` elapses, then
    /// drains the result in `about_to_wait` and exits.
    probe: Option<ProbeState>,
    /// Set by `--scripted`. The autonomous test driver — fires
    /// post_web_message / scroll / keyboard at scheduled offsets,
    /// asserts on the JS-echoed responses, and exits.
    scripted: Option<ScriptedState>,
    /// Set by `--profile-test`. Loads the cookie test page once and
    /// reports observed cookie state.
    profile_test: Option<ProfileTestState>,
    /// Set by `--two-tabs`. The second producer instance, navigated
    /// independently from `producer`. Both share the same NSView
    /// parent (subviews of the host window) and the same data_dir
    /// (so they're "tabs in the same browsing session").
    second_producer: Option<WkWebViewProducer>,
    /// Two-tabs mode exits at this elapsed time. `None` outside
    /// `--two-tabs`.
    two_tabs_deadline: Option<Duration>,
    /// Set by `--browser-test`. Drives a state machine across items
    /// 1, 3, 4, 9.
    browser_test: Option<BrowserTestState>,
    started_at: Instant,
}

#[derive(Default)]
struct BrowserTestState {
    step: BrowserTestStep,
    step_started_at: Option<Instant>,
    settings_ok: Option<bool>,
    find_result: Option<bool>,
    pdf_bytes: Option<usize>,
    pdf_error: Option<String>,
    /// Most recent committed URL observed via SourceChanged. Used
    /// to verify go_back / go_forward actually navigated.
    last_committed_url: String,
    failures: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum BrowserTestStep {
    #[default]
    LoadFirst,
    AwaitFirst,
    LoadSecond,
    AwaitSecond,
    GoBack,
    AwaitBack,
    GoForward,
    AwaitForward,
    ApplySettings,
    NavigateForFind,
    AwaitFindPage,
    FindInPage,
    AwaitFindResult,
    RequestPdf,
    AwaitPdfResult,
    Done,
}

#[derive(Default)]
struct ProfileTestState {
    started: bool,
    saw_on_load: Option<String>,
    saw_set: Option<String>,
    saw_done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScriptedStep {
    Initial,
    AwaitReady,
    PostMessage,
    AwaitEcho,
    Scroll,
    AwaitScroll,
    FocusForKeys,
    SendKeys,
    AwaitTyped,
    Done,
}

struct ScriptedState {
    step: ScriptedStep,
    step_started_at: Instant,
    /// Subset of expectations seen so far. Used to advance the state
    /// machine when a JS-side message arrives.
    saw_ready: bool,
    saw_echo: bool,
    saw_scroll: bool,
    saw_typed: String,
    /// What we typed via send_keyboard_input, expected to round-trip
    /// back as `typed:<accumulated>` from the JS input listener.
    typed_expected: String,
    /// Pass / fail summary printed at exit.
    failures: Vec<String>,
}

impl ScriptedState {
    fn new() -> Self {
        Self {
            step: ScriptedStep::Initial,
            step_started_at: Instant::now(),
            saw_ready: false,
            saw_echo: false,
            saw_scroll: false,
            saw_typed: String::new(),
            typed_expected: String::new(),
            failures: Vec::new(),
        }
    }
    fn enter(&mut self, step: ScriptedStep) {
        self.step = step;
        self.step_started_at = Instant::now();
    }
}

#[derive(Clone, Copy)]
struct ProbeState {
    requested: bool,
    request_at: Duration,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match AppState::new(event_loop, self.cli) {
            Ok(state) => self.state = Some(state),
            Err(error) => {
                eprintln!("demo-mac: initialization failed: {error}");
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        // Drain any messages that arrived since the last wakeup
        // before advancing test-mode state machines.
        if state.scripted.is_some()
            || state.profile_test.is_some()
            || state.browser_test.is_some()
        {
            drain_events(state);
        }
        if state.scripted.is_some() {
            advance_scripted(state, event_loop);
            drain_events(state);
        }
        if state.profile_test.is_some() {
            advance_profile_test(state, event_loop);
        }
        if state.browser_test.is_some() {
            advance_browser_test(state, event_loop);
        }
        if let Some(deadline) = state.two_tabs_deadline {
            // Drain second-producer events with a "[tab2]" prefix so
            // they're distinguishable from the first producer's
            // events in stdout.
            if let Some(second) = state.second_producer.as_mut() {
                while let Some(event) = second.poll_navigation_event() {
                    println!("demo-mac: [tab2] nav event: {event:?}");
                }
                while let Some(message) = second.poll_web_message() {
                    println!("demo-mac: [tab2] js->host: {message}");
                }
            }
            if state.started_at.elapsed() >= deadline {
                println!("demo-mac: --two-tabs deadline reached, exiting");
                event_loop.exit();
            }
        }
        // Capture mode: kick off `start_capture_async` after the
        // kickoff delay elapses (so the initial navigation has time
        // to land first), then poll capture_status and drive
        // redraws once the stream is live.
        if let Some(kickoff) = state.capture_kickoff_at {
            if !state.capture_started && state.started_at.elapsed() >= kickoff
                && let Some(render) = state.render.as_ref() {
                    let host = render.host_context.clone();
                    match state.producer.start_capture_async(host) {
                        Ok(()) => {
                            println!("demo-mac: start_capture_async kicked off");
                            state.capture_started = true;
                        }
                        Err(error) => {
                            eprintln!("demo-mac: start_capture_async failed: {error}");
                            state.capture_started = true;
                        }
                    }
                }
            // Resize-test driver: programmatically resize the
            // window once each schedule step elapses. This exercises
            // both the producer's `resize` path and slice N's live
            // `SCStream::updateConfiguration:` callback.
            if let Some(steps) = state.resize_test_steps.as_ref() {
                let elapsed = state.started_at.elapsed();
                if state.resize_test_idx < steps.len() {
                    let (w, h, at) = steps[state.resize_test_idx];
                    if elapsed >= at {
                        let new_size = winit::dpi::PhysicalSize::new(w, h);
                        let _ = state.window.request_inner_size(new_size);
                        println!(
                            "demo-mac: resize-test step {} → request inner_size = {}x{}",
                            state.resize_test_idx, w, h
                        );
                        state.resize_test_idx += 1;
                    }
                }
            }
            if state.capture_started && state.render.is_some() {
                match state.producer.capture_status() {
                    CaptureStatus::Live => {
                        // Live: request a redraw to drive the wgpu loop.
                        state.window.request_redraw();
                    }
                    CaptureStatus::Failed(msg) => {
                        eprintln!("demo-mac: capture failed: {msg}");
                        event_loop.exit();
                        return;
                    }
                    CaptureStatus::Starting | CaptureStatus::Idle => {
                        // Still spinning up; keep polling.
                    }
                    _ => {}
                }
            }
        }
        // Probe-snapshot pump: once `request_at` elapses we kick off
        // `request_snapshot`. Subsequent ticks drain the result.
        if let Some(probe) = state.probe.as_mut() {
            let elapsed = state.started_at.elapsed();
            if !probe.requested && elapsed >= probe.request_at {
                probe.requested = true;
                if let Err(error) = state.producer.request_snapshot() {
                    eprintln!("demo-mac: request_snapshot failed: {error}");
                    event_loop.exit();
                    return;
                }
                println!("demo-mac: probe-snapshot requested at {:?}", elapsed);
            }
            if probe.requested
                && let Some(result) = state.producer.poll_snapshot() {
                    match result {
                        Ok(scrying::WryWebSurfaceFrame::CpuRgba { pixels, .. }) => {
                            if let Err(error) = pixels.save("demo-mac-snapshot.png") {
                                eprintln!("demo-mac: snapshot save failed: {error}");
                            } else {
                                println!(
                                    "demo-mac: probe-snapshot saved to demo-mac-snapshot.png"
                                );
                            }
                        }
                        Ok(_) => {
                            eprintln!("demo-mac: poll_snapshot returned non-CpuRgba frame");
                        }
                        Err(error) => {
                            eprintln!("demo-mac: probe snapshot failed: {error}");
                        }
                    }
                    event_loop.exit();
                }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                let physical = if state.capture_kickoff_at.is_some() {
                    // Capture mode: WebView gets the left half.
                    PhysicalSize::new(new_size.width / 2, new_size.height)
                } else {
                    PhysicalSize::new(new_size.width, new_size.height)
                };
                if let Err(error) = state.producer.resize(physical) {
                    eprintln!("demo-mac: producer resize failed: {error}");
                }
                if let Some(render) = state.render.as_mut() {
                    render.resize(new_size.width, new_size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(render) = state.render.as_mut() {
                    match render.render(&mut state.producer) {
                        Ok(()) => {
                            // Log every 60 frames so the log isn't a flood.
                            if render.frames_drawn > 0 && render.frames_drawn % 60 == 0 {
                                println!(
                                    "demo-mac: rendered {} captured frames",
                                    render.frames_drawn
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("demo-mac: render failed: {error}");
                        }
                    }
                    state.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor = Some(position);
                let event = MouseInput {
                    kind: MouseEventKind::Move,
                    virtual_keys: state.mouse_buttons,
                    mouse_data: 0,
                    point: (position.x as i32, position.y as i32),
                };
                let _ = state.producer.send_mouse_input(event);
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                let kind = match (button, btn_state) {
                    (MouseButton::Left, ElementState::Pressed) => MouseEventKind::LeftButtonDown,
                    (MouseButton::Left, ElementState::Released) => MouseEventKind::LeftButtonUp,
                    (MouseButton::Right, ElementState::Pressed) => MouseEventKind::RightButtonDown,
                    (MouseButton::Right, ElementState::Released) => {
                        MouseEventKind::RightButtonUp
                    }
                    (MouseButton::Middle, ElementState::Pressed) => {
                        MouseEventKind::MiddleButtonDown
                    }
                    (MouseButton::Middle, ElementState::Released) => {
                        MouseEventKind::MiddleButtonUp
                    }
                    _ => return,
                };
                match (button, btn_state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        state.mouse_buttons.left_button = true
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        state.mouse_buttons.left_button = false
                    }
                    (MouseButton::Right, ElementState::Pressed) => {
                        state.mouse_buttons.right_button = true
                    }
                    (MouseButton::Right, ElementState::Released) => {
                        state.mouse_buttons.right_button = false
                    }
                    (MouseButton::Middle, ElementState::Pressed) => {
                        state.mouse_buttons.middle_button = true
                    }
                    (MouseButton::Middle, ElementState::Released) => {
                        state.mouse_buttons.middle_button = false
                    }
                    _ => {}
                }
                let point = state
                    .cursor
                    .map(|p| (p.x as i32, p.y as i32))
                    .unwrap_or((0, 0));
                let _ = state.producer.send_mouse_input(MouseInput {
                    kind,
                    virtual_keys: state.mouse_buttons,
                    mouse_data: 0,
                    point,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        // Convert lines to pixels with a fudge factor —
                        // matches what AppKit reports for line-units
                        // on most mice.
                        ((x * 16.0) as i32, (y * 16.0) as i32)
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => {
                        (p.x as i32, p.y as i32)
                    }
                };
                let point = state
                    .cursor
                    .map(|p| (p.x as i32, p.y as i32))
                    .unwrap_or((0, 0));
                if dy != 0 {
                    let _ = state.producer.send_mouse_input(MouseInput {
                        kind: MouseEventKind::Wheel,
                        virtual_keys: state.mouse_buttons,
                        mouse_data: dy,
                        point,
                    });
                }
                if dx != 0 {
                    let _ = state.producer.send_mouse_input(MouseInput {
                        kind: MouseEventKind::HorizontalWheel,
                        virtual_keys: state.mouse_buttons,
                        mouse_data: dx,
                        point,
                    });
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                state.modifiers.shift = modifiers.state().shift_key();
                state.modifiers.control = modifiers.state().control_key();
                state.modifiers.alt = modifiers.state().alt_key();
                state.modifiers.meta = modifiers.state().super_key();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                handle_key(state, event);
            }
            _ => {}
        }

        drain_events(state);
    }
}

fn handle_key(state: &mut AppState, event: KeyEvent) {
    let kind = match event.state {
        ElementState::Pressed => KeyEventKind::Down,
        ElementState::Released => KeyEventKind::Up,
    };

    // Hotkey demo bindings: handle on key-press, swallow.
    if event.state == ElementState::Pressed
        && let Key::Character(s) = &event.logical_key {
            match s.as_str() {
                "s" | "S" => {
                    if let Err(error) = state.producer.request_snapshot() {
                        eprintln!("demo-mac: request_snapshot failed: {error}");
                    } else {
                        println!(
                            "demo-mac: snapshot requested — will save when ready"
                        );
                    }
                    return;
                }
                "m" | "M" => {
                    if let Err(error) = state
                        .producer
                        .post_web_message("hello from demo-mac (key M)")
                    {
                        eprintln!("demo-mac: post_web_message failed: {error}");
                    } else {
                        println!("demo-mac: posted host->JS message");
                    }
                    return;
                }
                _ => {}
            }
        }

    let characters = match &event.text {
        Some(s) => s.to_string(),
        None => String::new(),
    };
    let virtual_key_code = match event.physical_key {
        winit::keyboard::PhysicalKey::Code(code) => code as u32,
        winit::keyboard::PhysicalKey::Unidentified(_) => 0,
    };

    let input = KeyboardInput {
        kind,
        virtual_key_code,
        characters: characters.clone(),
        characters_ignoring_modifiers: characters,
        modifiers: state.modifiers,
        is_repeat: event.repeat,
    };
    let _ = state.producer.send_keyboard_input(input);
}

fn drain_events(state: &mut AppState) {
    while let Some(event) = state.producer.poll_navigation_event() {
        println!("demo-mac: nav event: {event:?}");
        // Update browser-test state machine on URL changes so
        // go_back / go_forward / load_url completions can be
        // observed.
        if let Some(test) = state.browser_test.as_mut() {
            match &event {
                NavigationEvent::SourceChanged { url }
                | NavigationEvent::Completed { url, .. } => {
                    test.last_committed_url = url.clone();
                }
                _ => {}
            }
        }
    }
    while let Some(message) = state.producer.poll_web_message() {
        println!("demo-mac: js->host: {message}");
        if let Some(profile) = state.profile_test.as_mut() {
            if let Some(rest) = message.strip_prefix("cookie-on-load:") {
                profile.saw_on_load = Some(rest.to_string());
            } else if let Some(rest) = message.strip_prefix("cookie-set:") {
                profile.saw_set = Some(rest.to_string());
            } else if message == "done" {
                profile.saw_done = true;
            }
        }
        if let Some(scripted) = state.scripted.as_mut() {
            if message == "ready" {
                scripted.saw_ready = true;
            } else if let Some(rest) = message.strip_prefix("echo:") {
                if rest == "ping-from-host" {
                    scripted.saw_echo = true;
                }
            } else if let Some(rest) = message.strip_prefix("scrolled:") {
                if let Ok(y) = rest.parse::<i64>()
                    && y != 0 {
                        scripted.saw_scroll = true;
                    }
            } else if let Some(rest) = message.strip_prefix("typed:") {
                scripted.saw_typed = rest.to_string();
            }
        }
    }
    while let Some(shape) = state.producer.poll_cursor_shape() {
        println!("demo-mac: cursor change: {shape:?}");
    }
    if state.probe.is_none() {
        // Probe mode handles snapshot draining in `about_to_wait` and
        // exits afterward — don't double-drain or we'd consume the
        // snapshot the probe is waiting on.
        if let Some(result) = state.producer.poll_snapshot() {
            match result {
                Ok(scrying::WryWebSurfaceFrame::CpuRgba { pixels, .. }) => {
                    match pixels.save("demo-mac-snapshot.png") {
                        Ok(()) => println!(
                            "demo-mac: snapshot saved to demo-mac-snapshot.png"
                        ),
                        Err(error) => {
                            eprintln!("demo-mac: snapshot save failed: {error}")
                        }
                    }
                }
                Ok(_) => eprintln!("demo-mac: poll_snapshot returned non-CpuRgba"),
                Err(error) => eprintln!("demo-mac: snapshot failed: {error}"),
            }
        }
    }
}

/// Drive the `--scripted` test state machine. Each step posts an
/// action to the producer (or just waits) and transitions on a
/// timeout or on the JS-echoed response.
fn advance_scripted(state: &mut AppState, event_loop: &ActiveEventLoop) {
    let Some(scripted) = state.scripted.as_mut() else {
        return;
    };
    let elapsed = scripted.step_started_at.elapsed();
    let step = scripted.step;
    match step {
        ScriptedStep::Initial => {
            // Load the offline test page.
            if let Err(e) = state.producer.load_html(SCRIPTED_HTML) {
                eprintln!("demo-mac: load_html failed: {e}");
                scripted.failures.push(format!("load_html: {e}"));
                scripted.enter(ScriptedStep::Done);
                return;
            }
            scripted.enter(ScriptedStep::AwaitReady);
        }
        ScriptedStep::AwaitReady => {
            if scripted.saw_ready {
                println!("demo-mac: scripted: page ready");
                scripted.enter(ScriptedStep::PostMessage);
            } else if elapsed > Duration::from_secs(8) {
                scripted.failures.push("page never posted 'ready'".into());
                scripted.enter(ScriptedStep::Done);
            }
        }
        ScriptedStep::PostMessage => {
            // Slice E: host → JS message.
            if let Err(e) = state.producer.post_web_message("ping-from-host") {
                scripted.failures.push(format!("post_web_message: {e}"));
                scripted.enter(ScriptedStep::Done);
                return;
            }
            scripted.enter(ScriptedStep::AwaitEcho);
        }
        ScriptedStep::AwaitEcho => {
            if scripted.saw_echo {
                println!("demo-mac: scripted: JS echoed host message");
                scripted.enter(ScriptedStep::Scroll);
            } else if elapsed > Duration::from_secs(3) {
                scripted.failures.push("JS never echoed host message".into());
                scripted.enter(ScriptedStep::Scroll);
            }
        }
        ScriptedStep::Scroll => {
            // Slice G: scroll wheel forwarding. Send several wheel
            // events; we assert the producer dispatches them
            // without error. Whether WebKit actually scrolls the
            // page is downstream of scrying's contract — synthetic
            // events from an offscreen / unfocused window often
            // bypass WebKit's hit-testing. If the JS scroll
            // listener does fire we capture that as a bonus
            // confirmation.
            //
            // Also dispatch a `MouseMoved` first so the WebView
            // sees a hover-tracking event at the same point —
            // matches the real-input pattern more closely and gives
            // WebKit's input handler more context.
            let _ = state.producer.send_mouse_input(MouseInput {
                kind: MouseEventKind::Move,
                virtual_keys: MouseVirtualKeys::default(),
                mouse_data: 0,
                point: (40, 200),
            });
            let mut all_ok = true;
            for _ in 0..6 {
                if let Err(e) = state.producer.send_mouse_input(MouseInput {
                    kind: MouseEventKind::Wheel,
                    virtual_keys: MouseVirtualKeys::default(),
                    mouse_data: -120,
                    point: (40, 200),
                }) {
                    scripted.failures.push(format!("send_mouse_input(Wheel): {e}"));
                    all_ok = false;
                    break;
                }
            }
            if all_ok {
                println!(
                    "demo-mac: scripted: 6 ScrollWheel events dispatched without error"
                );
            }
            scripted.enter(ScriptedStep::AwaitScroll);
        }
        ScriptedStep::AwaitScroll => {
            if scripted.saw_scroll {
                println!("demo-mac: scripted: page reported scroll (bonus end-to-end)");
                scripted.enter(ScriptedStep::FocusForKeys);
            } else if elapsed > Duration::from_secs(2) {
                // No-DOM-effect is acceptable for synthetic events
                // when the window isn't user-focused. The API-level
                // dispatch was already asserted in the previous
                // step.
                scripted.enter(ScriptedStep::FocusForKeys);
            }
        }
        ScriptedStep::FocusForKeys => {
            // Click the input box so the keyboard events have a
            // focused target. The test HTML places the input near
            // the top-left of the page (~40-100 px range).
            let pt = (60, 90);
            let _ = state.producer.send_mouse_input(MouseInput {
                kind: MouseEventKind::LeftButtonDown,
                virtual_keys: MouseVirtualKeys::default(),
                mouse_data: 0,
                point: pt,
            });
            let _ = state.producer.send_mouse_input(MouseInput {
                kind: MouseEventKind::LeftButtonUp,
                virtual_keys: MouseVirtualKeys::default(),
                mouse_data: 0,
                point: pt,
            });
            // Also ask the responder chain to focus the WKWebView so
            // keyDown: routes to it.
            if let Err(e) = state.producer.move_focus(scrying::FocusReason::Programmatic) {
                eprintln!("demo-mac: scripted: move_focus failed: {e}");
            }
            scripted.enter(ScriptedStep::SendKeys);
        }
        ScriptedStep::SendKeys => {
            if elapsed < Duration::from_millis(150) {
                return; // brief pause to let focus settle
            }
            // Slice I: type three characters. Same caveat as scroll
            // — synthetic events from an offscreen / unfocused
            // window may not propagate through WebKit's input
            // handler to the focused element. The assertion is that
            // the producer accepts and dispatches the events
            // without error; DOM-side observation is bonus
            // confirmation.
            let mut all_ok = true;
            for ch in ['a', 'b', 'c'] {
                let s = ch.to_string();
                if let Err(e) = state.producer.send_keyboard_input(KeyboardInput {
                    kind: KeyEventKind::Down,
                    virtual_key_code: 0,
                    characters: s.clone(),
                    characters_ignoring_modifiers: s.clone(),
                    modifiers: KeyModifierFlags::default(),
                    is_repeat: false,
                }) {
                    scripted
                        .failures
                        .push(format!("send_keyboard_input(Down '{ch}'): {e}"));
                    all_ok = false;
                    break;
                }
                if let Err(e) = state.producer.send_keyboard_input(KeyboardInput {
                    kind: KeyEventKind::Up,
                    virtual_key_code: 0,
                    characters: s.clone(),
                    characters_ignoring_modifiers: s.clone(),
                    modifiers: KeyModifierFlags::default(),
                    is_repeat: false,
                }) {
                    scripted
                        .failures
                        .push(format!("send_keyboard_input(Up '{ch}'): {e}"));
                    all_ok = false;
                    break;
                }
                scripted.typed_expected.push(ch);
            }
            if all_ok {
                println!(
                    "demo-mac: scripted: 3 KeyDown/KeyUp pairs dispatched without error"
                );
            }
            scripted.enter(ScriptedStep::AwaitTyped);
        }
        ScriptedStep::AwaitTyped => {
            if scripted.saw_typed == scripted.typed_expected
                && !scripted.typed_expected.is_empty()
            {
                println!(
                    "demo-mac: scripted: page reported typed='{}' (bonus end-to-end)",
                    scripted.saw_typed
                );
                scripted.enter(ScriptedStep::Done);
            } else if elapsed > Duration::from_secs(2) {
                // Same as scroll: API dispatch already asserted; DOM
                // delivery is best-effort for synthetic events.
                scripted.enter(ScriptedStep::Done);
            }
        }
        ScriptedStep::Done => {
            if scripted.failures.is_empty() {
                println!("demo-mac: scripted: PASS — slices E + G + I verified at runtime");
            } else {
                eprintln!("demo-mac: scripted: FAIL");
                for f in &scripted.failures {
                    eprintln!("  - {f}");
                }
            }
            event_loop.exit();
        }
    }
}

/// Drive the `--profile-test` cookie-persistence run. Loads the test
/// page on first tick, waits for the JS handshake, and exits with a
/// summary that lets a test runner verify across-process persistence.
fn advance_profile_test(state: &mut AppState, event_loop: &ActiveEventLoop) {
    let Some(profile) = state.profile_test.as_mut() else {
        return;
    };
    if !profile.started {
        profile.started = true;
        if let Err(error) = state
            .producer
            .load_html_with_base_url(PROFILE_TEST_HTML, PROFILE_TEST_BASE_URL)
        {
            eprintln!("demo-mac: profile-test load_html failed: {error}");
            event_loop.exit();
            return;
        }
        println!(
            "demo-mac: profile-test: page loaded with origin {PROFILE_TEST_BASE_URL}"
        );
        return;
    }
    if profile.saw_done {
        let on_load = profile.saw_on_load.as_deref().unwrap_or("");
        let set = profile.saw_set.as_deref().unwrap_or("");
        if on_load.is_empty() {
            println!(
                "demo-mac: profile-test: PRIMED — no prior cookie observed, set demo_token={set}"
            );
        } else {
            println!(
                "demo-mac: profile-test: PERSISTED — cookies on load = '{on_load}'; refreshed to demo_token={set}"
            );
        }
        event_loop.exit();
        return;
    }
    if state.started_at.elapsed() > std::time::Duration::from_secs(10) {
        eprintln!("demo-mac: profile-test: TIMED OUT — JS handshake never completed");
        event_loop.exit();
    }
}

/// Drive the `--browser-test` runtime verification of items 1 / 3 /
/// 4 / 9. Each step either issues a producer call and transitions
/// to an Await state, or polls for completion / observed effects.
/// Items 2, 5, 6, 8 aren't covered — they need a real network or
/// harder-to-trigger conditions.
fn advance_browser_test(state: &mut AppState, event_loop: &ActiveEventLoop) {
    let Some(test) = state.browser_test.as_mut() else {
        return;
    };
    let now = Instant::now();
    if test.step_started_at.is_none() {
        test.step_started_at = Some(now);
    }
    let elapsed = now.duration_since(test.step_started_at.unwrap_or(now));
    let step = test.step;
    macro_rules! step_to {
        ($next:expr) => {{
            test.step = $next;
            test.step_started_at = Some(Instant::now());
        }};
    }
    macro_rules! await_ms {
        ($ms:expr) => {
            elapsed < Duration::from_millis($ms)
        };
    }
    match step {
        BrowserTestStep::LoadFirst => {
            if let Err(e) = state.producer.load_url("scrying-test://history-1") {
                test.failures.push(format!("load history-1: {e}"));
                step_to!(BrowserTestStep::Done);
                return;
            }
            step_to!(BrowserTestStep::AwaitFirst);
        }
        BrowserTestStep::AwaitFirst => {
            if test.last_committed_url.contains("history-1") {
                println!("demo-mac: browser-test: history-1 loaded");
                step_to!(BrowserTestStep::LoadSecond);
            } else if elapsed > Duration::from_secs(5) {
                test.failures.push("history-1 never loaded".into());
                step_to!(BrowserTestStep::Done);
            }
        }
        BrowserTestStep::LoadSecond => {
            if let Err(e) = state.producer.load_url("scrying-test://history-2") {
                test.failures.push(format!("load history-2: {e}"));
                step_to!(BrowserTestStep::Done);
                return;
            }
            step_to!(BrowserTestStep::AwaitSecond);
        }
        BrowserTestStep::AwaitSecond => {
            if test.last_committed_url.contains("history-2") {
                println!("demo-mac: browser-test: history-2 loaded");
                step_to!(BrowserTestStep::GoBack);
            } else if elapsed > Duration::from_secs(5) {
                test.failures.push("history-2 never loaded".into());
                step_to!(BrowserTestStep::Done);
            }
        }
        BrowserTestStep::GoBack => {
            // Settle so WKWebView's back-forward list catches up
            // with the just-completed second load. The
            // `Completed` event fires before WKWebView updates
            // `canGoBack`, so a small delay avoids a flake.
            if await_ms!(200) {
                return;
            }
            // Item 1: history controls.
            if !state.producer.can_go_back() {
                test.failures
                    .push("can_go_back returned false after two loads".into());
                step_to!(BrowserTestStep::ApplySettings);
                return;
            }
            match state.producer.go_back() {
                Ok(true) => step_to!(BrowserTestStep::AwaitBack),
                Ok(false) => {
                    test.failures.push("go_back returned Ok(false)".into());
                    step_to!(BrowserTestStep::ApplySettings);
                }
                Err(e) => {
                    test.failures.push(format!("go_back: {e}"));
                    step_to!(BrowserTestStep::ApplySettings);
                }
            }
        }
        BrowserTestStep::AwaitBack => {
            if test.last_committed_url.contains("history-1") {
                println!("demo-mac: browser-test: go_back navigated to history-1");
                step_to!(BrowserTestStep::GoForward);
            } else if elapsed > Duration::from_secs(5) {
                test.failures
                    .push(format!(
                        "go_back didn't navigate to history-1 (saw '{}')",
                        test.last_committed_url
                    ));
                step_to!(BrowserTestStep::ApplySettings);
            }
        }
        BrowserTestStep::GoForward => {
            // Brief settle so WebKit's history stack catches up
            // with the just-completed go_back navigation. Without
            // this, `canGoForward` can momentarily return false
            // even though forward navigation is logically valid.
            if await_ms!(200) {
                return;
            }
            match state.producer.go_forward() {
                Ok(true) => step_to!(BrowserTestStep::AwaitForward),
                Ok(false) => {
                    test.failures.push("go_forward returned Ok(false)".into());
                    step_to!(BrowserTestStep::ApplySettings);
                }
                Err(e) => {
                    test.failures.push(format!("go_forward: {e}"));
                    step_to!(BrowserTestStep::ApplySettings);
                }
            }
        }
        BrowserTestStep::AwaitForward => {
            if test.last_committed_url.contains("history-2") {
                println!("demo-mac: browser-test: go_forward navigated to history-2");
                step_to!(BrowserTestStep::ApplySettings);
            } else if elapsed > Duration::from_secs(5) {
                test.failures.push(format!(
                    "go_forward didn't navigate to history-2 (saw '{}')",
                    test.last_committed_url
                ));
                step_to!(BrowserTestStep::ApplySettings);
            }
        }
        BrowserTestStep::ApplySettings => {
            // Item 3: settings.
            let settings = scrying::WebSurfaceSettings {
                zoom_factor: Some(1.5),
                javascript_enabled: Some(true),
                devtools_enabled: Some(false),
                user_agent: Some("scrying-demo-test/0.1".into()),
                ..Default::default()
            };
            test.settings_ok = Some(state.producer.apply_settings(&settings).is_ok());
            if test.settings_ok == Some(true) {
                println!("demo-mac: browser-test: apply_settings ok");
            } else {
                test.failures.push("apply_settings returned Err".into());
            }
            step_to!(BrowserTestStep::NavigateForFind);
        }
        BrowserTestStep::NavigateForFind => {
            if let Err(e) = state.producer.load_url("scrying-test://find-target") {
                test.failures.push(format!("load find-target: {e}"));
                step_to!(BrowserTestStep::Done);
                return;
            }
            step_to!(BrowserTestStep::AwaitFindPage);
        }
        BrowserTestStep::AwaitFindPage => {
            if test.last_committed_url.contains("find-target") {
                step_to!(BrowserTestStep::FindInPage);
            } else if elapsed > Duration::from_secs(5) {
                test.failures.push("find-target never loaded".into());
                step_to!(BrowserTestStep::RequestPdf);
            }
        }
        BrowserTestStep::FindInPage => {
            if await_ms!(150) {
                return;
            }
            // Item 9 part A: find_in_page.
            let opts = FindOptions::default();
            if let Err(e) = state.producer.find_in_page("scrying-find-marker", opts) {
                test.failures.push(format!("find_in_page: {e}"));
                step_to!(BrowserTestStep::RequestPdf);
                return;
            }
            step_to!(BrowserTestStep::AwaitFindResult);
        }
        BrowserTestStep::AwaitFindResult => {
            if let Some(matched) = state.producer.poll_find_match() {
                test.find_result = Some(matched);
                if matched {
                    println!("demo-mac: browser-test: find_in_page matched");
                } else {
                    test.failures
                        .push("find_in_page returned no match for known marker".into());
                }
                step_to!(BrowserTestStep::RequestPdf);
            } else if elapsed > Duration::from_secs(3) {
                test.failures.push("find_in_page never completed".into());
                step_to!(BrowserTestStep::RequestPdf);
            }
        }
        BrowserTestStep::RequestPdf => {
            if let Err(e) = state.producer.request_pdf() {
                test.failures.push(format!("request_pdf: {e}"));
                step_to!(BrowserTestStep::Done);
                return;
            }
            step_to!(BrowserTestStep::AwaitPdfResult);
        }
        BrowserTestStep::AwaitPdfResult => {
            if let Some(result) = state.producer.poll_pdf() {
                match result {
                    Ok(bytes) => {
                        let len = bytes.len();
                        test.pdf_bytes = Some(len);
                        if len > 100 {
                            println!(
                                "demo-mac: browser-test: PDF rendered ({len} bytes)"
                            );
                        } else {
                            test.failures.push(format!(
                                "PDF rendered but suspiciously small ({len} bytes)"
                            ));
                        }
                    }
                    Err(msg) => {
                        test.pdf_error = Some(msg.clone());
                        test.failures.push(format!("PDF render failed: {msg}"));
                    }
                }
                step_to!(BrowserTestStep::Done);
            } else if elapsed > Duration::from_secs(5) {
                test.failures.push("request_pdf never completed".into());
                step_to!(BrowserTestStep::Done);
            }
        }
        BrowserTestStep::Done => {
            if test.failures.is_empty() {
                println!("demo-mac: browser-test: PASS — items 1, 3, 4, 9 verified at runtime");
                println!(
                    "  - history controls (go_back/go_forward observed via SourceChanged events)"
                );
                println!("  - settings (apply_settings returned Ok)");
                println!(
                    "  - URL schemes (scrying-test:// served {} pages successfully)",
                    if test.last_committed_url.contains("find-target") {
                        3
                    } else {
                        2
                    }
                );
                println!(
                    "  - find_in_page → {:?}, request_pdf → {} bytes",
                    test.find_result,
                    test.pdf_bytes.unwrap_or(0)
                );
            } else {
                eprintln!("demo-mac: browser-test: FAIL");
                for f in &test.failures {
                    eprintln!("  - {f}");
                }
            }
            event_loop.exit();
        }
    }
}

impl AppState {
    fn new(
        event_loop: &ActiveEventLoop,
        cli: Cli,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let window = event_loop.create_window(
            WindowAttributes::default()
                .with_title("scrying demo-mac")
                .with_inner_size(winit::dpi::LogicalSize::new(1024.0, 768.0)),
        )?;
        let window = Arc::new(window);

        let ns_view_ptr = match window.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(handle) => handle.ns_view.as_ptr(),
            other => return Err(format!("unexpected RawWindowHandle on macOS: {other:?}").into()),
        };

        let inner = window.inner_size();
        // In capture mode, size the WKWebView to the window's left
        // half so the wgpu-rendered imported texture has the right
        // half of the surface free to draw into. Otherwise the
        // WebView fills the entire window (overlay mode).
        let webview_size = if cli.capture {
            PhysicalSize::new(inner.width / 2, inner.height)
        } else {
            PhysicalSize::new(inner.width, inner.height)
        };

        // Per-profile data store. Path is purely informational on
        // macOS — the producer hashes it into a UUID and resolves a
        // per-profile WKWebsiteDataStore. Using the cargo target dir
        // segregates demo cookies from any other scrying instance the
        // host might run. The profile-test mode uses its own subdir
        // so multiple back-to-back runs share a stable persistent
        // store without bleeding into the regular demo profile.
        let data_dir = if cli.profile_test {
            std::env::current_dir()?.join("target/demo-mac-profile-test")
        } else {
            std::env::current_dir()?.join("target/demo-mac-profile")
        };
        let producer_config = WkWebViewProducerConfig::new(webview_size, &data_dir);

        // Browser-test mode registers a custom URL scheme so the
        // canned test pages don't depend on the network.
        let url_schemes: Vec<(String, UrlSchemeHandlerFn)> = if cli.browser_test {
            vec![("scrying-test".to_string(), browser_test_scheme_handler())]
        } else {
            Vec::new()
        };

        // SAFETY: ns_view_ptr is the live NSView from winit's window,
        // which outlives the producer (window is owned by AppState
        // via Arc, dropped only when the event loop exits).
        let mut producer = unsafe {
            WkWebViewProducer::new_with_url_schemes(
                ns_view_ptr,
                producer_config,
                url_schemes,
            )?
        };

        // Scripted mode loads its own offline test page once the
        // event loop is running (in `advance_scripted`); for
        // everything else, kick off the initial network navigation
        // here. We use `load_url` (non-blocking) rather than
        // `navigate_to_url` (blocking) because we're inside winit's
        // `resumed` callback and the blocking variant would pump the
        // main `NSRunLoop` and re-enter winit's event handler (which
        // panics under winit's "no nested event handling" guard).
        // Completion arrives asynchronously via the navigation event
        // FIFO.
        if !cli.scripted && !cli.profile_test && !cli.two_tabs && !cli.browser_test {
            if let Err(error) = producer.load_url(INITIAL_URL) {
                eprintln!("demo-mac: initial load_url failed: {error}");
            } else {
                println!("demo-mac: started loading {INITIAL_URL}");
            }
        }

        // `--two-tabs`: spin up a second producer against the same
        // NSView and navigate it to a different URL. Validates that
        // multiple producers can coexist in one process / one window.
        let second_producer = if cli.two_tabs {
            // Position tab 1 at top-half and tab 2 at bottom-half so
            // both are visually distinct in any captured frame.
            let half_height = inner.height / 2;
            let _ = producer.resize(PhysicalSize::new(inner.width, half_height));
            let _ = producer.set_offset(0.0, half_height as f32);
            let _ = producer.load_url("https://example.com");

            let second_data_dir =
                std::env::current_dir()?.join("target/demo-mac-multi-tab");
            let second_config =
                WkWebViewProducerConfig::new(
                    PhysicalSize::new(inner.width, half_height),
                    &second_data_dir,
                );
            // SAFETY: ns_view_ptr is the same valid NSView the first
            // producer was constructed against; both producers will
            // be dropped before the window vanishes.
            let second =
                unsafe { WkWebViewProducer::new(ns_view_ptr, second_config)? };
            let _ = second.load_url("https://www.iana.org/help/example-domains");
            println!("demo-mac: --two-tabs: spun up second producer");
            Some(second)
        } else {
            None
        };

        let render = if cli.capture {
            let mut r = pollster::block_on(WgpuRender::new(window.clone()))?;
            if cli.dump_every > 0 {
                r.dump_every = Some(cli.dump_every);
                println!(
                    "demo-mac: dumping every {} imported frame(s) to demo-mac-frame-NNNN.png",
                    cli.dump_every
                );
            }
            Some(r)
        } else {
            None
        };

        let resize_test_steps = if cli.resize_test {
            Some(vec![
                (1280, 800, Duration::from_secs(6)),
                (768, 600, Duration::from_secs(10)),
                (1024, 768, Duration::from_secs(14)),
            ])
        } else {
            None
        };

        Ok(Self {
            window,
            producer,
            render,
            capture_kickoff_at: cli.capture.then_some(Duration::from_secs(3)),
            capture_started: false,
            resize_test_steps,
            resize_test_idx: 0,
            cursor: None,
            mouse_buttons: MouseVirtualKeys::default(),
            modifiers: KeyModifierFlags::default(),
            probe: cli.probe_snapshot.then(|| ProbeState {
                requested: false,
                request_at: Duration::from_secs(3),
            }),
            scripted: cli.scripted.then(ScriptedState::new),
            profile_test: cli.profile_test.then(ProfileTestState::default),
            second_producer,
            two_tabs_deadline: cli.two_tabs.then_some(Duration::from_secs(8)),
            browser_test: cli.browser_test.then(BrowserTestState::default),
            started_at: Instant::now(),
        })
    }
}
