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

One-shot smoke modes:

```bash
cargo run -p demo-win -- --scripted
cargo run -p demo-win -- --browser-test
cargo run -p demo-win -- --cookie-test
```

`--scripted` loads a deterministic inline page, asserts a host-to-JS-to-host message round-trip, verifies mouse/keyboard forwarding APIs accept synthetic events, and requests process shutdown after the synchronous probe. It deliberately does not require the DOM keyboard effect to round-trip; the stricter `WEBVIEW_KEYBOARD_VALIDATE=1` smoke remains opt-in until the Windows message-loop path is tightened.

`--browser-test` drives two inline pages through WebView2 history, asserts back/forward/reload through page messages, checks title notifications, and exercises settings plus visibility controls.

`--cookie-test` verifies the WebView2 profile cookie manager by setting a unique HttpOnly cookie, querying it back through `request_all_cookies` / `poll_cookies`, deleting it, and confirming the next query no longer sees it.
