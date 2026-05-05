# wry-scry

Capability-driven system-webview frame adapter — scry into WebView2/WKWebView/WebKitGTK and surface frames the host renderer can consume.

The name comes from *scrying* — gazing into a reflective surface for visions. The webview is the surface; the captured frame is the vision; `wry-scry` is the lens.

This repo was extracted from [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft) on 2026-05-05 so that Wry/WebView frame production has its own place to evolve. `wgpu-graft` continues to own the `wgpu-native-texture-interop` core (native GPU resource import/export) plus Servo-side adapters; `wry-scry` consumes the interop crate as a downstream consumer.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`wry-scry`](wry-scry/) | The library. Capability probe (`WebSurfaceMode`), per-platform `WryWebSurfaceProducer` impls, fallbacks. Windows producer is the reference implementation; macOS and Linux producers are skeletons. |
| [`demo-wry-winit`](demo-wry-winit/) | Minimal winit + wgpu host probe. Creates a Wry webview, asks `wry-scry` which surface mode is viable, and on Windows captures the WebView2 composition target into a wgpu texture. |

See [`wry-scry/README.md`](wry-scry/README.md) for the producer/consumer contract, the Windows WGC + shared D3D11 path, and the future explicit-fence-sync work.

## Quick start

```bash
cargo check -p wry-scry
cargo run   -p demo-wry-winit
```

## Relationship to wgpu-graft

`wry-scry` depends on `wgpu-native-texture-interop` from the sibling `wgpu-graft` repo. Locally the two repos sit side by side under `repos/`, and the dep is wired as a path dep:

```toml
wgpu-native-texture-interop = { path = "../../wgpu-graft/wgpu-native-texture-interop" }
```

If you're consuming `wry-scry` from outside this layout, point at a git or registry source for `wgpu-native-texture-interop` instead.

## License

MIT OR Apache-2.0
