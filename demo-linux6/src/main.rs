//! Minimal Linux runtime probe for scrying's WebKitGTK 6.0 / GTK 4
//! producer (Phase 5).
//!
//! Sibling to [`demo-linux`] which targets the GTK 3 / WebKitGTK 4.1
//! line. This binary uses the parallel `webkit6_producer` module
//! behind the `webkit6` feature flag on scrying.
//!
//! ```sh
//! cargo run -p demo-linux6                              # default HTML page → snapshot.png
//! cargo run -p demo-linux6 -- --probe-only              # capability probe + exit
//! cargo run -p demo-linux6 -- --snapshot-test           # exit-1 on empty/zero-pixel snapshot
//! cargo run -p demo-linux6 -- --url https://example.com # real-page snapshot
//! ```

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use dpi::PhysicalSize;
use scrying::webkit6_producer::{WebKit6Producer, WebKit6ProducerConfig};
use scrying::{WebSurfaceCapabilities, WebSurfaceFrame, WebSurfaceProducer};

const DEFAULT_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>scrying linux6 smoke</title></head>
<body style="margin:0;display:flex;align-items:center;justify-content:center;
height:100vh;background:linear-gradient(135deg,#1e3a8a,#1e293b);color:#a5f3fc;
font:bold 64px system-ui,sans-serif">scrying · linux · gtk4</body></html>"#;

fn main() -> ExitCode {
    // Same WebKit env-var workaround as `demo-linux` — disables
    // accelerated compositing / DMABUF renderer so GDK doesn't try
    // (and on some Wayland sessions, fail) to create a GL context
    // we don't actually need for CPU snapshot.
    // Safety: env-var writes must happen before any other thread
    // spawns; `main` is single-threaded at this point.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("demo-linux6: {err}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    output_path: PathBuf,
    url: Option<String>,
    snapshot_test: bool,
    probe_only: bool,
    width: u32,
    height: u32,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut out = Args {
            output_path: "scrying-linux6-snapshot.png".into(),
            url: None,
            snapshot_test: false,
            probe_only: false,
            width: 800,
            height: 600,
        };
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--out" => out.output_path = args.next().ok_or("--out needs a path")?.into(),
                "--url" => out.url = Some(args.next().ok_or("--url needs a value")?),
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
                "--help" | "-h" => {
                    println!(
                        "demo-linux6 — WebKitGTK 6.0 / GTK 4 runtime probe for scrying\n\n\
                         USAGE: demo-linux6 [--url URL] [--out PATH] [--width N] [--height N]\n\
                                            [--snapshot-test] [--probe-only]"
                    );
                    std::process::exit(0);
                }
                _ => return Err(format!("unknown arg: {arg}")),
            }
        }
        Ok(out)
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let caps = WebSurfaceCapabilities::probe(None);
    println!("backend: {:?}", caps.backend);
    println!("preferred mode: {:?}", caps.preferred_mode);
    println!("CPU snapshot: {:?}", caps.cpu_snapshot);
    println!("reason: {}", caps.reason);
    if args.probe_only {
        return Ok(());
    }

    let data_dir = std::env::temp_dir().join("scrying-demo-linux6-data");
    let config = WebKit6ProducerConfig::new(PhysicalSize::new(args.width, args.height), &data_dir);
    let mut producer = WebKit6Producer::new(config)?;

    let nav_timeout = Duration::from_secs(5);
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
