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

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use dpi::PhysicalSize;
use scrying::webkitgtk_producer::{WebKitGtkProducer, WebKitGtkProducerConfig};
use scrying::{WebSurfaceCapabilities, WebSurfaceFrame, WebSurfaceProducer};

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
    println!("                  [--snapshot-test] [--scripted] [--probe-only]");
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
    let mut producer = WebKitGtkProducer::new(config)?;

    let nav_timeout = Duration::from_secs(5);

    if args.scripted {
        return run_scripted(&mut producer, nav_timeout);
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
