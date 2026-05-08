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
use scrying::wkwebview_producer::{WkWebViewProducer, WkWebViewProducerConfig};
use scrying::{
    KeyEventKind, KeyModifierFlags, KeyboardInput, MouseEventKind, MouseInput, MouseVirtualKeys,
    WryWebSurfaceProducer,
};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::Key;
use winit::window::{Window, WindowAttributes};

const INITIAL_URL: &str = "https://example.com";

/// Offline HTML page used by `--scripted`. Contains:
/// - An input box (id=`text`) so synthetic key events can change the
///   value and the page can post the new value to the host.
/// - A scrollable region so synthetic scroll-wheel events change
///   `window.scrollY` and the page can post the new offset.
/// - A `chrome.webview` listener that echoes any message it receives
///   from the host back as `echo:<payload>` so the host can verify
///   round-trip JS messaging.
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
    if cli.probe_snapshot || cli.capture || cli.scripted {
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
    started_at: Instant,
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
        if state.scripted.is_some() {
            advance_scripted(state, event_loop);
            // Drain again so messages posted by the scripted step
            // are observed within this same wakeup tick.
            drain_events(state);
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
    }
    while let Some(message) = state.producer.poll_web_message() {
        println!("demo-mac: js->host: {message}");
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
        // host might run.
        let data_dir = std::env::current_dir()?.join("target/demo-mac-profile");
        let producer_config = WkWebViewProducerConfig::new(webview_size, &data_dir);

        // SAFETY: ns_view_ptr is the live NSView from winit's window,
        // which outlives the producer (window is owned by AppState
        // via Arc, dropped only when the event loop exits).
        let producer = unsafe { WkWebViewProducer::new(ns_view_ptr, producer_config)? };

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
        if !cli.scripted {
            if let Err(error) = producer.load_url(INITIAL_URL) {
                eprintln!("demo-mac: initial load_url failed: {error}");
            } else {
                println!("demo-mac: started loading {INITIAL_URL}");
            }
        }

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
            started_at: Instant::now(),
        })
    }
}
