# demo-win

Windows WebView2 composition runtime probe for `scrying`.

This is the Windows-specific counterpart to [`../demo-mac`](../demo-mac/). It is intentionally heavier than [`../demo-scrying-winit`](../demo-scrying-winit/): the catchall demo proves platform selection and dependency gating, while this crate drives the WebView2 CompositionController, WinComp, Windows Graphics Capture, shared D3D texture import, resize, input, navigation events, JS messages, cursor reporting, and optional readback/fence diagnostics from a real winit event loop.

During interactive runs the renderer logs producer capture counters from `capture_metrics()`: WGC frames received, frames emitted to the host, and stale dimension-mismatch frames dropped during resize/restart churn.

Future Windows browser-shape assertions should land here first, mirroring the mode vocabulary used by `demo-mac` (`--scripted`, `--browser-test`, `--cookie-test`, `--profile-test`, `--incognito-test`, `--popup-test`, `--routing-test`, `--process-test`, `--download-test`, `--auth-test`, `--permission-test`, `--visibility-test`, `--find-test`, `--pdf-test`, `--context-test`, `--media-test`, `--multi-view-test`, `--two-tabs`, `--capture-test`) as each WebView2 slice gets runtime proof.

Current Windows runtime observations:

- top-level HWND capture imports successfully,
- the direct WebView2 `ICoreWebView2CompositionController` probe completes navigation and a renderer animation-frame wait,
- WebView2 `CapturePreview` produces a valid PNG snapshot from the composition-controller page,
- a plain WinComp sprite visual in the same desktop target also produces a valid `GraphicsCaptureItem` size of `420x260`,
- both the WebView target visual and its root visual produce a valid `GraphicsCaptureItem` size of `420x260`,
- after laying out the direct composition target, a WebView content mutation after `GraphicsCaptureSession::StartCapture` yields a captured `420x260` `Bgra8Unorm` WebView target visual frame,
- `TryGetNextFrame` currently still times out for the plain sprite visual without producing a frame,
- the bounded keyboard smoke (`--keyboard-test` or `WEBVIEW_KEYBOARD_VALIDATE=1`) currently times out after raw `WM_KEYDOWN` / `WM_CHAR` / `WM_KEYUP` forwarding, so DOM keyboard/IME delivery still needs a deeper Windows message-loop path.

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
cargo run -p demo-win -- --profile-test
cargo run -p demo-win -- --incognito-test
cargo run -p demo-win -- --popup-test
cargo run -p demo-win -- --routing-test
cargo run -p demo-win -- --process-test
cargo run -p demo-win -- --download-test
cargo run -p demo-win -- --auth-test
cargo run -p demo-win -- --permission-test
cargo run -p demo-win -- --visibility-test
cargo run -p demo-win -- --find-test
cargo run -p demo-win -- --pdf-test
cargo run -p demo-win -- --context-test
cargo run -p demo-win -- --media-test
cargo run -p demo-win -- --multi-view-test
cargo run -p demo-win -- --capture-test
```

`--scripted` loads a deterministic inline page, asserts a host-to-JS-to-host message round-trip, verifies mouse/keyboard forwarding APIs accept synthetic events, and requests process shutdown after the synchronous probe. It deliberately does not require the DOM keyboard effect to round-trip; the stricter `WEBVIEW_KEYBOARD_VALIDATE=1` smoke remains opt-in until the Windows message-loop path is tightened.

`--browser-test` drives two inline pages through WebView2 history, asserts back/forward/reload through page messages, checks title notifications, and exercises settings plus visibility controls.

`--cookie-test` verifies the WebView2 profile cookie manager by setting a unique HttpOnly cookie, querying it back through `request_all_cookies` / `poll_cookies`, deleting it, confirming the next query no longer sees it, and observing a native `Set-Cookie` response pulse through `WebResourceResponseReceived`.

`--profile-test` sets a persistent cookie, drops the first WebView2 producer, creates a second producer with the same `user_data_dir`, and verifies the cookie store survives producer recreation.

`--incognito-test` creates the first producer with `PlatformWebSurfaceConfig::non_persistent()`, sets a persistent cookie in that InPrivate profile, drops the producer, recreates a normal persistent producer with the same `user_data_dir`, and verifies the InPrivate cookie did not leak.

`--popup-test` triggers `window.open(...)` from the page, verifies `NavigationEvent::NewWindowRequested { url }`, and relies on the producer to suppress WebView2's default popup so the host owns tab creation.

`--routing-test` registers a WebView2 virtual HTTPS host via `register_virtual_host_handler`, navigates to that host, and verifies the app-owned response body can post back through the normal JS message bridge.

`--process-test` triggers a renderer failure through the DevTools `Page.crash` method, verifies `NavigationEvent::ContentProcessTerminated`, and then proves the producer can recover by navigating to fresh inline HTML.

`--download-test` serves an attachment from a virtual HTTPS host, routes `DownloadStarting` through `set_download_handler`, and verifies the `DownloadStarted` / `DownloadFinished` event path plus downloaded bytes.

`--auth-test` starts a bounded loopback HTTP Basic-auth server, supplies credentials through `set_auth_handler`, and verifies the authenticated page resumes and posts back.

`--permission-test` serves a secure virtual-host page that requests microphone access, denies it through `set_permission_handler`, and verifies the page observes the denial.

`--visibility-test` toggles `SetIsVisible(false/true)` and verifies the page receives `document.visibilityState` transitions through `visibilitychange`.

`--find-test` drives WebView2's native `ICoreWebView2Find` surface and verifies match-count reporting.

`--pdf-test` drives `PrintToPdfStream`, reads the returned COM stream, and verifies PDF bytes.

`--context-test` verifies `NavigationEvent::ContextMenuRequested` through the installed context-menu bridge. The producer also registers WebView2's native `ContextMenuRequested` event for real user input.

`--media-test` verifies the media-capture WebMessage bridge used by the injected `getUserMedia` observer to emit `NavigationEvent::MediaCaptureStateChanged`.

`--capture-test` acquires one WebView2 WGC frame, imports it through the host DX12 wgpu device, prints producer `capture_metrics()` counters, closes the shared handle, and exits.

`--multi-view-test` creates two simultaneous WebView2 composition producers on separate HWNDs and verifies both pages can navigate and post messages independently. The known limitation is same-HWND composition: a single HWND cannot currently host two independent composition roots in this demo setup.

`--keyboard-test` is a bounded diagnostic probe, not part of the passing smoke set yet. It forwards raw Win32 keyboard messages and currently reproduces the remaining WebView2 keyboard/IME blocker by timing out before the DOM input event arrives.
