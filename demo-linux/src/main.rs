//! Minimal Linux runtime probe for scrying's WebKitGTK producer.
//!
//! Hosts a [`WebKitGtkProducer`] in an offscreen WebKit page, drives a
//! navigation, takes a CPU-RGBA snapshot, and writes it to disk.
//!
//! ```sh
//! cargo run -p demo-linux                                  # default HTML page → snapshot.png
//! cargo run -p demo-linux -- --url https://example.com
//! cargo run -p demo-linux -- --snapshot-test               # exit-1 on empty / missing frame
//! cargo run -p demo-linux -- --probe-only                  # capability probe + exit
//! ```

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use dpi::PhysicalSize;
use scrying::webkitgtk_producer::{WebKitGtkProducer, WebKitGtkProducerConfig};
use scrying::{
    Cookie, FocusReason, KeyEventKind, KeyModifierFlags, KeyboardInput, MouseEventKind, MouseInput,
    MouseVirtualKeys, NavigationEvent, UrlSchemeHandlerFn, UrlSchemeResponse,
    WebSurfaceCapabilities, WebSurfaceFrame, WebSurfaceProducer,
};

const DEFAULT_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>scrying linux smoke</title></head>
<body style="margin:0;display:flex;align-items:center;justify-content:center;
height:100vh;background:linear-gradient(135deg,#1e293b,#0f172a);color:#facc15;
font:bold 64px system-ui,sans-serif">scrying · linux</body></html>"#;

const SCRIPTED_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>scrying scripted</title></head>
<body><script>
// Echo every host → page message back with an "echo:" prefix.
window.chrome.webview.addEventListener('message', function(e) {
    window.chrome.webview.postMessage('echo:' + e.data);
});
// Tell the host we're loaded.
window.chrome.webview.postMessage('hello from page');
</script></body></html>"#;

const INPUT_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>scrying input</title></head>
<body>
<button id="btn" style="position:absolute;left:100px;top:100px;width:200px;height:60px">click target</button>
<script>
var btn = document.getElementById('btn');
btn.addEventListener('mousedown', function(e) {
    window.chrome.webview.postMessage('mousedown@' + e.clientX + ',' + e.clientY + ' trusted=' + e.isTrusted);
});
btn.addEventListener('mouseup', function(e) {
    window.chrome.webview.postMessage('mouseup@' + e.clientX + ',' + e.clientY + ' trusted=' + e.isTrusted);
});
document.addEventListener('keydown', function(e) {
    window.chrome.webview.postMessage('keydown:' + e.key + ' trusted=' + e.isTrusted);
});
</script></body></html>"#;

fn main() -> ExitCode {
    // WebKitGTK 2.40+ uses a DMABUF-based renderer plus accelerated
    // compositing by default. Both paths require GDK to successfully
    // create a GL context, which can fail with `GDK is not able to
    // create a GL context: The current backend does not support
    // OpenGL` on some GTK 3 + Wayland setups even when GL itself works
    // fine for other processes. The CPU snapshot path
    // (`webkit_web_view_get_snapshot` → cairo `ImageSurface`) does not
    // benefit from accelerated compositing, so force the software
    // rendering path. Hosts that need AC for a future GPU capture path
    // should leave these unset and ensure GDK can create a GL context
    // on their target session.
    // Safety: env-var writes must happen before any other thread spawns;
    // `main` is single-threaded at this point.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("demo-linux: {err}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    output_path: PathBuf,
    url: Option<String>,
    snapshot_test: bool,
    probe_only: bool,
    scripted: bool,
    input_test: bool,
    cookie_test: bool,
    scheme_test: bool,
    popup_test: bool,
    width: u32,
    height: u32,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut out = Args {
            output_path: "scrying-linux-snapshot.png".into(),
            url: None,
            snapshot_test: false,
            probe_only: false,
            scripted: false,
            input_test: false,
            cookie_test: false,
            scheme_test: false,
            popup_test: false,
            width: 800,
            height: 600,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => {
                    out.output_path = args.next().ok_or("--out needs a path")?.into();
                }
                "--url" => {
                    out.url = Some(args.next().ok_or("--url needs a value")?);
                }
                "--width" => {
                    out.width = args
                        .next()
                        .ok_or("--width needs a value")?
                        .parse()
                        .map_err(|e| format!("invalid --width: {e}"))?;
                }
                "--height" => {
                    out.height = args
                        .next()
                        .ok_or("--height needs a value")?
                        .parse()
                        .map_err(|e| format!("invalid --height: {e}"))?;
                }
                "--snapshot-test" => out.snapshot_test = true,
                "--probe-only" => out.probe_only = true,
                "--scripted" => out.scripted = true,
                "--input-test" => out.input_test = true,
                "--cookie-test" => out.cookie_test = true,
                "--scheme-test" => out.scheme_test = true,
                "--popup-test" => out.popup_test = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => return Err(format!("unknown arg: {arg}")),
            }
        }
        Ok(out)
    }
}

fn print_help() {
    println!("demo-linux — WebKitGTK runtime probe for scrying");
    println!();
    println!("USAGE: demo-linux [--url URL] [--out PATH] [--width N] [--height N]");
    println!(
        "                  [--snapshot-test] [--scripted] [--input-test] [--cookie-test] [--scheme-test] [--popup-test] [--probe-only]"
    );
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Capability probe first — exercises detect() + probe() against the
    // current build's feature flags.
    let caps = WebSurfaceCapabilities::probe(None);
    println!("backend: {:?}", caps.backend);
    println!("preferred mode: {:?}", caps.preferred_mode);
    println!("CPU snapshot: {:?}", caps.cpu_snapshot);
    println!("reason: {}", caps.reason);
    if args.probe_only {
        return Ok(());
    }

    let data_dir = std::env::temp_dir().join("scrying-demo-linux-data");
    let config =
        WebKitGtkProducerConfig::new(PhysicalSize::new(args.width, args.height), &data_dir);
    let mut producer = if args.scheme_test {
        let mut schemes: HashMap<String, UrlSchemeHandlerFn> = HashMap::new();
        schemes.insert(
            "scry".to_string(),
            Arc::new(|uri: &str| {
                let body = format!(
                    "<!doctype html><html><body><script>\
                 window.chrome.webview.postMessage('scheme served: {uri}');\
                 </script></body></html>"
                );
                UrlSchemeResponse {
                    mime_type: "text/html".to_string(),
                    body: body.into_bytes(),
                    headers: vec![("X-Scry-Source".to_string(), "demo-linux".to_string())],
                }
            }),
        );
        WebKitGtkProducer::new_with_url_schemes(config, schemes)?
    } else {
        WebKitGtkProducer::new(config)?
    };

    let nav_timeout = Duration::from_secs(5);

    if args.scripted {
        return run_scripted(&mut producer, nav_timeout);
    }
    if args.input_test {
        return run_input_test(&mut producer, nav_timeout);
    }
    if args.cookie_test {
        return run_cookie_test(&producer);
    }
    if args.scheme_test {
        return run_scheme_test(&mut producer, nav_timeout);
    }
    if args.popup_test {
        return run_popup_test(&mut producer, nav_timeout);
    }

    match &args.url {
        Some(url) => {
            println!("navigating to {url}");
            producer.navigate_to_url(url, nav_timeout)?;
        }
        None => {
            println!("navigating to inline HTML");
            producer.navigate_to_string(DEFAULT_HTML, nav_timeout)?;
        }
    }
    println!("committed: {:?}", producer.committed_uri());

    let frame = producer.acquire_frame()?;
    match frame {
        WebSurfaceFrame::CpuRgba {
            size,
            pixels,
            generation,
        } => {
            println!(
                "CpuRgba snapshot: {}x{} gen={}",
                size.width, size.height, generation
            );
            if args.snapshot_test {
                if size.width == 0 || size.height == 0 {
                    return Err("FAIL: empty snapshot".into());
                }
                let nonzero = pixels.as_raw().iter().any(|b| *b != 0);
                if !nonzero {
                    return Err("FAIL: snapshot is all-zero (WebKit did not paint?)".into());
                }
                println!("PASS: snapshot has non-zero pixel data");
            }
            pixels.save(&args.output_path)?;
            println!("wrote {}", args.output_path.display());
        }
        other => {
            return Err(
                format!("FAIL: expected CpuRgba frame, got mode {:?}", other.mode()).into(),
            );
        }
    }
    Ok(())
}

const POPUP_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"></head>
<body>
<a id="lnk" href="https://example.com/popup-target" target="_blank"
   style="position:absolute;left:50px;top:50px;width:300px;height:60px;
   background:#fef3c7;display:block;text-align:center;line-height:60px;
   text-decoration:none;color:#92400e;font:bold 20px system-ui">popup link</a>
</body></html>"#;

/// New-window / popup intercept smoke. Clicks a `target="_blank"`
/// anchor via our native GdkEvent path (isTrusted=true, counts as a
/// user gesture so WebKit doesn't pop-block) and asserts that
/// `connect_create` fires with the popup URL — surfaced as a
/// `NavigationEvent::NewWindowRequested`.
fn run_popup_test(
    producer: &mut WebKitGtkProducer,
    nav_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("loading popup-test page");
    producer.navigate_to_string(POPUP_HTML, nav_timeout)?;

    // Drain any nav events from the load itself so we don't get a
    // stale match from the predicate.
    while let Some(_) = producer.poll_navigation_event() {}

    // Centre of the anchor (x=50..350, y=50..110).
    let target = (200, 80);
    let no_mods = MouseVirtualKeys::default();
    println!("clicking popup anchor @ {target:?}");
    producer.send_mouse_input(MouseInput {
        kind: MouseEventKind::LeftButtonDown,
        virtual_keys: no_mods,
        mouse_data: 0,
        point: target,
    })?;
    producer.send_mouse_input(MouseInput {
        kind: MouseEventKind::LeftButtonUp,
        virtual_keys: no_mods,
        mouse_data: 0,
        point: target,
    })?;

    let evt = producer.wait_for_navigation_event(Duration::from_secs(3), |e| {
        matches!(e, NavigationEvent::NewWindowRequested { .. })
    });
    match evt {
        Some(NavigationEvent::NewWindowRequested { url })
            if url == "https://example.com/popup-target" =>
        {
            println!("PASS: NewWindowRequested fired with the popup URL");
            Ok(())
        }
        Some(NavigationEvent::NewWindowRequested { url }) => {
            Err(format!("FAIL: unexpected popup URL: {url:?}").into())
        }
        Some(other) => Err(format!("FAIL: unexpected event {other:?}").into()),
        None => Err("FAIL: NewWindowRequested never fired".into()),
    }
}

/// Custom URL scheme smoke. The producer was built with a `scry://`
/// scheme handler that returns an HTML body postMessage-ing the
/// served URI back; navigating to `scry://hello` should result in
/// the host observing that message.
fn run_scheme_test(
    producer: &mut WebKitGtkProducer,
    nav_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("navigating to scry://hello");
    producer.navigate_to_url("scry://hello", nav_timeout)?;
    match producer
        .wait_for_web_message(Duration::from_secs(3))
        .as_deref()
    {
        Some("scheme served: scry://hello") => {
            println!("PASS: scry:// scheme handler served the page");
            Ok(())
        }
        Some(other) => Err(format!("FAIL: unexpected scheme message {other:?}").into()),
        None => Err("FAIL: scheme handler never delivered a page-side message".into()),
    }
}

/// Cookie store round-trip smoke. Sets a cookie, reads it back via
/// `request_cookies_for_url`, asserts the value matches; then
/// deletes and re-reads to confirm absence. No navigation involved
/// — exercises the `WebsiteDataManager` cookie store directly.
fn run_cookie_test(producer: &WebKitGtkProducer) -> Result<(), Box<dyn std::error::Error>> {
    let url = "http://test.local/path";
    let cookie = Cookie {
        name: "scrying_test".to_string(),
        value: "phase2d".to_string(),
        domain: "test.local".to_string(),
        path: "/".to_string(),
        expires_at: None,
        is_secure: false,
        is_http_only: false,
    };

    println!("setting cookie scrying_test=phase2d for {url}");
    producer.set_cookie(&cookie)?;

    let cookies = producer.request_cookies_for_url(url)?;
    println!("got {} cookie(s) for {url}", cookies.len());
    match cookies.iter().find(|c| c.name == "scrying_test") {
        Some(c) if c.value == "phase2d" => {
            println!("PASS: cookie round-tripped (name=scrying_test value=phase2d)")
        }
        Some(c) => {
            return Err(format!("FAIL: cookie value differs — got {:?}", c.value).into());
        }
        None => return Err("FAIL: cookie not present after set_cookie".into()),
    }

    println!("deleting cookie");
    producer.delete_cookie(&cookie)?;

    let after = producer.request_cookies_for_url(url)?;
    if after.iter().any(|c| c.name == "scrying_test") {
        return Err("FAIL: cookie still present after delete_cookie".into());
    }
    println!("PASS: cookie absent after delete_cookie");
    Ok(())
}

/// Synthesized input smoke. Loads a page with mouse + keyboard
/// handlers that postMessage back, then drives `send_mouse_input` /
/// `send_keyboard_input` and asserts the page-side listeners observed
/// the synthesized events.
fn run_input_test(
    producer: &mut WebKitGtkProducer,
    nav_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("loading input-test page");
    producer.navigate_to_string(INPUT_HTML, nav_timeout)?;

    // Focus the page so document-level keyboard listeners have a
    // target. `move_focus` is a no-op for handlers attached to
    // `document` itself, but worth calling — it'll matter once we
    // upgrade to native GdkEvent dispatch.
    producer.move_focus(FocusReason::Programmatic)?;

    // Centre of the button (x=100..300, y=100..160).
    let target = (200, 130);
    let no_mods = MouseVirtualKeys::default();

    println!("sending LeftButtonDown @ {target:?}");
    producer.send_mouse_input(MouseInput {
        kind: MouseEventKind::LeftButtonDown,
        virtual_keys: no_mods,
        mouse_data: 0,
        point: target,
    })?;
    match producer
        .wait_for_web_message(Duration::from_secs(2))
        .as_deref()
    {
        Some("mousedown@200,130 trusted=true") => {
            println!("PASS: mousedown — isTrusted=true (native GdkEvent path)")
        }
        Some("mousedown@200,130 trusted=false") => {
            println!("PASS (degraded): mousedown — isTrusted=false (JS fallback path)")
        }
        other => return Err(format!("FAIL: mousedown — got {other:?}").into()),
    }

    println!("sending LeftButtonUp @ {target:?}");
    producer.send_mouse_input(MouseInput {
        kind: MouseEventKind::LeftButtonUp,
        virtual_keys: no_mods,
        mouse_data: 0,
        point: target,
    })?;
    match producer
        .wait_for_web_message(Duration::from_secs(2))
        .as_deref()
    {
        Some("mouseup@200,130 trusted=true") => {
            println!("PASS: mouseup — isTrusted=true (native GdkEvent path)")
        }
        Some("mouseup@200,130 trusted=false") => {
            println!("PASS (degraded): mouseup — isTrusted=false (JS fallback path)")
        }
        other => return Err(format!("FAIL: mouseup — got {other:?}").into()),
    }

    println!("sending keydown 'a'");
    producer.send_keyboard_input(KeyboardInput {
        kind: KeyEventKind::Down,
        virtual_key_code: 0x41, // 'A' physical key
        characters: "a".to_string(),
        characters_ignoring_modifiers: "a".to_string(),
        modifiers: KeyModifierFlags::default(),
        is_repeat: false,
    })?;
    match producer
        .wait_for_web_message(Duration::from_secs(2))
        .as_deref()
    {
        Some("keydown:a trusted=true") => {
            println!("PASS: keydown — isTrusted=true (native GdkEvent path)")
        }
        Some("keydown:a trusted=false") => {
            println!("PASS (degraded): keydown — isTrusted=false (JS fallback path)")
        }
        other => return Err(format!("FAIL: keydown — got {other:?}").into()),
    }
    Ok(())
}

/// Bidirectional JS-messaging smoke. The page sends `"hello from page"`
/// at load time; the host then posts `"ping"` and the page echoes
/// `"echo:ping"` back. Both round-trips must complete or the mode
/// fails with a non-zero exit.
fn run_scripted(
    producer: &mut WebKitGtkProducer,
    nav_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("loading scripted page");
    producer.navigate_to_string(SCRIPTED_HTML, nav_timeout)?;

    let msg = producer.wait_for_web_message(Duration::from_secs(3));
    match msg.as_deref() {
        Some("hello from page") => println!("PASS: page → host initial message arrived"),
        Some(other) => {
            return Err(format!("FAIL: expected 'hello from page', got {other:?}").into());
        }
        None => return Err("FAIL: page → host initial message timed out".into()),
    }

    println!("posting 'ping' to page");
    producer.post_web_message("ping")?;

    let echo = producer.wait_for_web_message(Duration::from_secs(3));
    match echo.as_deref() {
        Some("echo:ping") => println!("PASS: host → page round-trip arrived"),
        Some(other) => {
            return Err(format!("FAIL: expected 'echo:ping', got {other:?}").into());
        }
        None => return Err("FAIL: host → page round-trip timed out".into()),
    }
    Ok(())
}
