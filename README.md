# scrying

Capability-driven system-webview frame adapter — scry into WebView2/WKWebView/WebKitGTK and surface frames the host renderer can consume.

The name comes from *scrying* — gazing into a reflective surface for visions. The webview is the surface; the captured frame is the vision; this crate is the lens.

This repo was extracted from [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft) on 2026-05-05 so that system-webview frame production has its own place to evolve. `wgpu-graft` is now the Servo testbed (Servo embedding demos + GL-FBO interop). `scrying` owns its native-frame import path in-tree as the [`scrying::native_frame`](scrying/src/native_frame/) module, structurally derived from Slint's [Servo embedding example](https://github.com/slint-ui/slint/tree/master/examples/servo) (see [NOTICE](NOTICE)).

## Workspace

| Crate | Purpose |
| --- | --- |
| [`scrying`](scrying/) | The library. Capability probe (`WebSurfaceMode`), per-platform `WryWebSurfaceProducer` impls, fallbacks. Windows + macOS producers are real implementations; Linux is a skeleton awaiting WPE/WebKitGTK work. |
| [`demo-wry-winit`](demo-wry-winit/) | Windows host probe. Creates a Wry webview, asks `scrying` which surface mode is viable, and captures the WebView2 composition target into a wgpu texture. |
| [`demo-mac`](demo-mac/) | macOS host probe. Hosts a `WkWebViewProducer` against a winit window's `NSView`; flagged modes drive nav / input / JS-messaging / SCK-capture / per-profile-data-store paths so each producer slice gets exercised at runtime. See [`demo-mac/README.md`](demo-mac/README.md). |

See [`scrying/README.md`](scrying/README.md) for the producer/consumer contract, the Windows WGC + shared D3D11 path, and the future explicit-fence-sync work.

## Quick start

```bash
cargo check -p scrying
# Windows
cargo run -p demo-wry-winit
# macOS — overlay mode (default)
cargo run -p demo-mac
# macOS — automated runtime tests
cargo run -p demo-mac -- --scripted              # JS messaging + input forwarding
cargo run -p demo-mac -- --probe-snapshot        # CPU snapshot via takeSnapshot:
cargo run -p demo-mac -- --capture --dump-every 30   # SCK pipeline + per-N-frame readback
cargo run -p demo-mac -- --profile-test          # per-profile WKWebsiteDataStore persistence
```

## Relationship to wgpu-graft

`scrying` and [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft) are sibling projects with no code dependency. `wgpu-graft` is the Servo testbed (GL-FBO interop, Servo embedding demos in winit/iced/xilem/gpui). `scrying` owns its native-frame import in-tree because the producer side is fundamentally different: scrying takes platform-native texture handles directly (D3D12 NT-handle, eventually IOSurface and DMABUF) rather than bridging from a GL framebuffer.

Both projects are structurally inspired by the same upstream — Slint's [Servo embedding example](https://github.com/slint-ui/slint/tree/master/examples/servo) — but adapt it to different consumers (Servo-on-Slint vs. system-webviews-on-wgpu).

## License

[MPL-2.0](LICENSE)
