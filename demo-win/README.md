# demo-win

Windows WebView2 composition runtime probe for `scrying`.

This is the Windows-specific counterpart to [`../demo-mac`](../demo-mac/). It is intentionally heavier than [`../demo-scrying-winit`](../demo-scrying-winit/): the catchall demo proves platform selection and dependency gating, while this crate drives the WebView2 CompositionController, WinComp, Windows Graphics Capture, shared D3D texture import, resize, input, navigation events, JS messages, cursor reporting, and optional readback/fence diagnostics from a real winit event loop.

Future Windows browser-shape assertions should land here first, mirroring the mode vocabulary used by `demo-mac` (`--scripted`, `--browser-test`, `--cookie-test`, `--profile-test`, `--two-tabs`, `--capture-test`) as each WebView2 slice gets runtime proof.

Current Windows runtime observations:

- top-level HWND capture imports successfully,
- the direct WebView2 `ICoreWebView2CompositionController` probe completes navigation and a renderer animation-frame wait,
- WebView2 `CapturePreview` produces a valid PNG snapshot from the composition-controller page,
- a plain WinComp sprite visual in the same desktop target also produces a valid `GraphicsCaptureItem` size of `420x260`,
- both the WebView target visual and its root visual produce a valid `GraphicsCaptureItem` size of `420x260`,
- after laying out the direct composition target, a WebView content mutation after `GraphicsCaptureSession::StartCapture` yields a captured `420x260` `Bgra8Unorm` WebView target visual frame,
- `TryGetNextFrame` currently still times out for the plain sprite visual without producing a frame,
- the optional keyboard smoke (`WEBVIEW_KEYBOARD_VALIDATE=1`) currently times out, so keyboard forwarding still needs a deeper Windows message-loop path.

On Windows, the probe requests the DX12 backend because the intended WebView2 capture path feeds `NativeFrame::Dx12SharedTexture`.

Run:

```bash
cargo run -p demo-win
```
