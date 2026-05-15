# wgpu-scry

Capability-driven system-webview frame adapter — scry into WebView2/WKWebView/WPE/WebKitGTK and surface frames the host renderer can consume.

This repo was extracted from [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft) on 2026-05-05 so that system-webview frame production has its own place to evolve. `wgpu-graft` is now the Servo testbed (Servo embedding demos + GL-FBO interop). `wgpu-scry` owns its native-frame import path in-tree as the [`scrying::native_frame`](scrying/src/native_frame/) module, structurally derived from Slint's [Servo embedding example](https://github.com/slint-ui/slint/tree/master/examples/servo) (see [NOTICE](NOTICE)).

## Workspace

| Crate | Purpose |
| --- | --- |
| [`scrying`](scrying/) | The library. Capability probe (`WebSurfaceMode`), per-platform `WebSurfaceProducer` impls. Windows (WebView2) and macOS (WKWebView) producers are real implementations. Linux ships three co-equal WebKit-family backends behind mutually-exclusive cargo features: WebKitGTK 4.1 (`webkitgtk-fallback`, currently the only fully-implemented Linux producer), WebKitGTK 6.0 (planned), and WPE (DMABUF scaffold). See [`design_docs/2026-05-14_linux_webkitgtk_phase_2a.md`](design_docs/2026-05-14_linux_webkitgtk_phase_2a.md). |
| [`demo-scrying-winit`](demo-scrying-winit/) | Cross-platform selector smoke. Creates a winit/wgpu host and reports the backend, platform producer/config aliases, capability status, and supported native frame kinds selected for the current target. |
| [`demo-win`](demo-win/) | Windows runtime probe. Drives the WebView2 CompositionController path into a wgpu texture, including WGC capture, shared D3D texture import, resize, input, navigation/message/cursor event drains, and optional readback/fence diagnostics. |
| [`demo-mac`](demo-mac/) | macOS host probe. Hosts a `WkWebViewProducer` against a winit window's `NSView`; flagged modes drive nav / input / JS-messaging / SCK-capture / per-profile-data-store paths so each producer slice gets exercised at runtime. See [`demo-mac/README.md`](demo-mac/README.md). |
| [`demo-linux`](demo-linux/) | Linux WebKitGTK 4.1 runtime probe. Hosts a `WebKitGtkProducer` in a producer-owned `GtkOffscreenWindow`, navigates to inline HTML or a URL, takes a CPU RGBA snapshot via `webkit_web_view_get_snapshot`, and writes it as a PNG. Flags: `--probe-only`, `--snapshot-test`, `--url`, `--out`, `--width`, `--height`. |

See [`scrying/README.md`](scrying/README.md) for the producer/consumer contract, the Windows WGC + shared D3D11 path, and the future explicit-fence-sync work.

## Quick start

```bash
cargo check -p scrying
# Cross-platform backend-selection smoke
cargo run -p demo-scrying-winit
# Windows runtime probe
cargo run -p demo-win
# Windows — automated runtime tests
cargo run -p demo-win -- --scripted                  # JS messaging + input forwarding API smoke
cargo run -p demo-win -- --browser-test              # history / settings / visibility
cargo run -p demo-win -- --cookie-test               # WebView2 cookie read / write / delete
cargo run -p demo-win -- --profile-test              # persistent user_data_dir survives producer recreation
cargo run -p demo-win -- --incognito-test            # InPrivate profile isolation
cargo run -p demo-win -- --popup-test                # host-owned target-blank / window.open routing
cargo run -p demo-win -- --routing-test              # WebResourceRequested virtual-host app content
cargo run -p demo-win -- --process-test              # ProcessFailed event + fresh navigation recovery
cargo run -p demo-win -- --download-test             # WebView2 DownloadStarting + host destination
cargo run -p demo-win -- --auth-test                 # BasicAuthenticationRequested + host credentials
cargo run -p demo-win -- --permission-test           # PermissionRequested + host denial
cargo run -p demo-win -- --visibility-test           # SetIsVisible -> Page Visibility state
cargo run -p demo-win -- --find-test                 # native WebView2 find + match count
cargo run -p demo-win -- --pdf-test                  # native PrintToPdfStream bytes
cargo run -p demo-win -- --context-test              # ContextMenuRequested event bridge
cargo run -p demo-win -- --media-test                # media-capture lifecycle event bridge
cargo run -p demo-win -- --multi-view-test           # simultaneous WebView2 producers on separate HWNDs
# macOS — overlay mode (default)
cargo run -p demo-mac
# macOS — automated runtime tests
cargo run -p demo-mac -- --scripted                  # JS messaging + input forwarding
cargo run -p demo-mac -- --browser-test              # history / settings / URL schemes / find / PDF
cargo run -p demo-mac -- --interaction-state-test    # interactionState round-trip
cargo run -p demo-mac -- --pointer-input-test        # send_pointer_input → JS pointer events
cargo run -p demo-mac -- --incognito-test            # nonPersistentDataStore isolation
cargo run -p demo-mac -- --download-test             # downloads pipeline (HTTP loopback)
cargo run -p demo-mac -- --probe-snapshot            # CPU snapshot via takeSnapshot:
cargo run -p demo-mac -- --capture --dump-every 30   # SCK pipeline + per-N-frame readback
cargo run -p demo-mac -- --capture-test              # SCK assertion smoke test (needs Screen Recording perm)
cargo run -p demo-mac -- --profile-test              # persistent-store-shared-across-producers assertion
cargo run -p demo-mac -- --two-tabs                  # multi-instance independence (no cross-talk between producers)
# All assertion-style runs at once (headless, 8 modes, exit 1 on any FAIL)
bash scripts/test-mac.sh
# Linux — WebKitGTK 4.1 runtime probe (requires the webkitgtk-fallback feature)
cargo run -p demo-linux                                                # default HTML → snapshot.png
cargo run -p demo-linux -- --probe-only                                # capability probe + exit
cargo run -p demo-linux -- --snapshot-test --out /tmp/snap.png         # exit 1 on empty/zero-pixel snapshot
cargo run -p demo-linux -- --scripted                                  # bidirectional JS-messaging round-trip
cargo run -p demo-linux -- --input-test                                # synthesized mouse + keyboard reaches page handlers
cargo run -p demo-linux -- --url https://example.com --out example.png # real-page snapshot
# All assertion modes at once (headless via offscreen WebView)
bash scripts/test-linux.sh
```

Linux system-package prerequisites (Fedora 44 names; translate for Debian / Ubuntu / Arch):

```bash
sudo dnf install -y gcc gcc-c++ \
  webkit2gtk4.1-devel \
  vulkan-loader-devel vulkan-headers mesa-vulkan-drivers \
  libxkbcommon-devel libxkbcommon-x11-devel wayland-devel \
  libX11-devel libXcursor-devel libXrandr-devel libXi-devel libxcb-devel
```

`--*-test` modes default to a hidden window and `NSApplicationActivationPolicyProhibited` so they run silently in the background; pass `--visible` to watch the WKWebView in real time. `--capture-test` is the one exception — it forces visibility because SCK can't capture hidden windows, and is held out of `scripts/test-mac.sh` because Screen Recording permission can't be self-granted (CI runners need a `tccutil` pre-grant). `.github/workflows/test-mac.yml` runs the rest of the suite on every push to master against `macos-latest`.

## Relationship to wgpu-graft and wgpu-weld

`wgpu-scry` is part of a family of sibling projects that split web/rendering-engine embedding by engine target:

- **`wgpu-scry`** (this repo) — **system webviews** (WebKit family). WebView2 on Windows, WKWebView on macOS, WebKitGTK 4.1 / WebKitGTK 6.0 / WPE on Linux.
- **[`wgpu-graft`](https://github.com/mark-ik/wgpu-graft)** — **Servo embedding**. GL-FBO interop and Servo embedding demos in winit / iced / xilem / gpui.
- **`wgpu-weld`** — **CEF / Chromium embedding**. Anything Chromium-shaped (CEF, Electron-flavoured embedders) lives there, not here.

The three projects have no code dependency on each other — each engine's threading model, sync story, and API surface is too different to share a crate. `wgpu-scry` owns its native-frame import in-tree because the producer side is fundamentally different: scrying takes platform-native texture handles directly (D3D12 NT-handle, eventually IOSurface and DMABUF) rather than bridging from a GL framebuffer.

All three are structurally inspired by the same upstream — Slint's [Servo embedding example](https://github.com/slint-ui/slint/tree/master/examples/servo) — but adapt it to different consumers (Servo-on-Slint vs. system-webviews-on-wgpu vs. Chromium-on-wgpu).

## License

[MPL-2.0](LICENSE)
