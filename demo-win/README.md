# demo-win

Windows WebView2 composition runtime probe for `scrying`.

This is the Windows-specific counterpart to [`../demo-mac`](../demo-mac/). It is intentionally heavier than [`../demo-scrying-winit`](../demo-scrying-winit/): the catchall demo proves platform selection and dependency gating, while this crate drives the WebView2 CompositionController, WinComp, Windows Graphics Capture, shared D3D texture import, resize, input, navigation events, JS messages, cursor reporting, and optional readback/fence diagnostics from a real winit event loop.

During interactive runs the renderer logs producer capture counters from `capture_metrics()`: WGC frames received, frames emitted to the host, and stale dimension-mismatch frames dropped during resize/restart churn.

Future Windows browser-shape assertions should land here first, mirroring the mode vocabulary used by `demo-mac` (`--scripted`, `--browser-test`, `--cookie-test`, `--profile-test`, `--incognito-test`, `--popup-test`, `--routing-test`, `--process-test`, `--download-test`, `--auth-test`, `--permission-test`, `--visibility-test`, `--find-test`, `--pdf-test`, `--context-test`, `--drop-test`, `--media-test`, `--multi-view-test`, `--two-tabs`, `--capture-test`, `--scale-test`, `--cdp-input-test`, `--accelerator-test`, `--ime-bridge-test`, `--composition-focus-hwnd-test`, `--window-to-visual-test`, `--window-to-visual-multi-test`) as each WebView2 slice gets runtime proof.

Current Windows runtime observations:

- top-level HWND capture imports successfully,
- the direct WebView2 `ICoreWebView2CompositionController` probe completes navigation and a renderer animation-frame wait,
- WebView2 `CapturePreview` produces a valid PNG snapshot from the composition-controller page,
- a plain WinComp sprite visual in the same desktop target also produces a valid `GraphicsCaptureItem` size of `420x260`,
- both the WebView target visual and its root visual produce a valid `GraphicsCaptureItem` size of `420x260`,
- after laying out the direct composition target, a WebView content mutation after `GraphicsCaptureSession::StartCapture` yields a captured `420x260` `Bgra8Unorm` WebView target visual frame,
- `TryGetNextFrame` currently still times out for the plain sprite visual without producing a frame,
- the bounded CompositionController **synthetic-keyboard** smoke (`--keyboard-test` or `WEBVIEW_KEYBOARD_VALIDATE=1`) PASSes 3/3 (2026-05-13): Win32 `SendInput` after host-side `SetForegroundWindow` reaches the focused DOM input. Real hardware keyboard ALSO works natively in pure CompositionController (verified 2026-05-12). The earlier long-standing timeout was a smoke bug — a `producer.send_mouse_input` click before SendInput was shifting DOM focus off the target — not a WebView2 ceiling,
- `--cdp-input-test` proves a limited pure-CompositionController fallback: CDP remains useful as a page automation/probing channel. Current result: `Input.dispatchKeyEvent`, `Input.insertText`, `Input.imeSetComposition`, `Runtime.evaluate`, and WebView2 `ExecuteScript` all produce DOM input under no-overlay composition. This is useful for narrow DOM/form-fill or scripted navigation fallbacks, not a replacement for native OS IME/candidate UI.
- `--accelerator-test` proves the pure CompositionController path surfaces WebView2 `AcceleratorKeyPressed` to host chrome; the current smoke observes F3 with `IsBrowserAcceleratorKeyEnabled=true`.
- `--ime-bridge-test` proves the first host-owned IME bridge slice: WebView2 reports focused editable caret geometry, the winit host sets native IME enablement/candidate area from that state, and Scrying applies composition/commit through CDP into DOM `composition*` / `input` events. The smoke also covers source-side password suppression, textarea scroll/multiline caret geometry, and blur/navigation composition cancellation.
- `--composition-focus-hwnd-test` proves three CompositionController panes can be parented to hidden or 1x1 child HWNDs and captured/imported independently, and that `SetFocus` can land on those child HWNDs; DOM keyboard input still times out for both physical `SendInput` and raw posted keyboard messages, so a hidden child HWND is not enough to solve CompositionController keyboard/IME,
- `--window-to-visual-test` sets `COREWEBVIEW2_FORCED_HOSTING_MODE=COREWEBVIEW2_HOSTING_MODE_WINDOW_TO_VISUAL`, creates a normal WebView2 controller, proves ASCII DOM input via `SendInput`, captures the parent HWND through WGC, and imports the captured frame into wgpu. The stricter post-navigation visibility check rejects this path as a no-overlay target because the live page exposes a visible `Chrome_WidgetWin_0` child HWND.
- `--window-to-visual-multi-test` creates three simultaneous Window-to-Visual pages, waits for all three pages to animate, captures/imports five parent-HWND samples, and rejects the path if any WebView child HWND is visible. The current result proves gross throughput but rejects the path: three visible `Chrome_WidgetWin_0` children are reported.

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
cargo run -p demo-win -- --drop-test
cargo run -p demo-win -- --media-test
cargo run -p demo-win -- --multi-view-test
cargo run -p demo-win -- --capture-test
cargo run -p demo-win -- --scale-test
cargo run -p demo-win -- --cdp-input-test
cargo run -p demo-win -- --accelerator-test
cargo run -p demo-win -- --ime-bridge-test
cargo run -p demo-win -- --composition-focus-hwnd-test
cargo run -p demo-win -- --window-to-visual-test
cargo run -p demo-win -- --window-to-visual-multi-test
```

`--scripted` loads a deterministic inline page, asserts a host-to-JS-to-host message round-trip, verifies mouse forwarding accepts synthetic events, verifies CDP-backed `send_keyboard_input` reaches the DOM input, and requests process shutdown after the synchronous probe. The stricter `WEBVIEW_KEYBOARD_VALIDATE=1` smoke remains a diagnostic ceiling probe for the non-CDP Win32 message path.

`--cdp-input-test` focuses the deterministic inline input, tries CDP `Input.dispatchKeyEvent`, `Input.insertText`, `Input.imeSetComposition`, `Runtime.evaluate`, and finally WebView2 `ExecuteScript`. Current result: all five routes produce DOM input. The smoke also proves injected script can post through `window.chrome.webview` and mutate `window.location.hash`. It is intentionally documented as a constrained form-fill/scripted-navigation compensation path, not native OS IME/candidate UI.

`--accelerator-test` focuses the pure CompositionController WebView, sends a physical-style Win32 F3 key, and verifies the producer emits `NavigationEvent::AcceleratorKeyPressed`. This is the portable WPF-style half of the hybrid: host chrome can observe WebView2-originated accelerators while CDP remains the no-overlay DOM text/composition lane.

`--ime-bridge-test` focuses the deterministic inline input, waits for `NavigationEvent::TextInputFocused`, maps the reported CSS-pixel caret rect through the current composition offset and window scale factor into `Window::set_ime_cursor_area`, enables winit IME on the host window, and verifies `set_ime_composition` plus `insert_text` reach DOM composition/input events. It then verifies textarea scroll/multiline caret geometry and password-field suppression: password-like editables emit only a blur/cancel signal, not selection or caret metadata. This is a hardened baseline host-owned IME bridge proof, not a full TSF text-store implementation.

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

`--drop-test` verifies `NavigationEvent::DropDetected` through the installed document drop bridge using a deterministic `DataTransfer` URL payload.

`--media-test` verifies the media-capture WebMessage bridge used by the injected `getUserMedia` observer to emit `NavigationEvent::MediaCaptureStateChanged`.

`--capture-test` acquires one WebView2 WGC frame, imports it through the host DX12 wgpu device, prints producer `capture_metrics()` counters plus the fixed Windows color target (`ColorPipeline::Srgb`, `Bgra8Unorm`), closes the shared handle, and exits.

`--scale-test` simulates the capture side of a DPI/monitor scale change by resizing the WebView2 producer through two physical capture sizes, acquiring/importing a fresh WGC frame at each size, and checking that stale pre-resize frames are not emitted as the new target.

`--composition-focus-hwnd-test` is the bounded proof for the proposed hidden-child-HWND input sink. It creates three CompositionController panes parented to per-pane child HWNDs, first hidden and then 1x1 visible as a fallback diagnostic. Both variants capture/import the visual tree independently and `SetFocus` reports the child HWND as focused, but DOM keyboard input still times out after physical `SendInput` and raw posted keyboard messages.

`--window-to-visual-test` is the bounded diagnostic for the Windows normal-controller path. It forces WebView2 Window-to-Visual hosting before environment creation, creates a normal controller, verifies focused DOM text input receives physical-style Win32 `SendInput`, reports child HWND visibility after navigation, captures the parent HWND through WGC, imports the frame into wgpu, and rejects the path if the WebView child HWND is visible.

`--window-to-visual-multi-test` applies the same no-overlay rule to three simultaneous Window-to-Visual pages. It waits for all three pages to navigate and animate, prints child-HWND visibility, captures/imports five parent-HWND samples through wgpu, reports average/max capture-import time, and rejects the path if any WebView child HWND is visible.

`--multi-view-test` creates two simultaneous WebView2 composition producers on separate HWNDs and verifies both pages can navigate and post messages independently. The known limitation is same-HWND composition: a single HWND cannot currently host two independent composition roots in this demo setup.

`--keyboard-test` is a bounded synthetic-keyboard smoke for the pure CompositionController path. It defers until the winit event loop is live, asks the page to focus the target input, calls WebView2 `MoveFocus`, foregrounds the parent HWND, and sends physical-style Win32 `SendInput` virtual-key events. PASSes 3/3 (2026-05-13): the SendInput keys reach the focused DOM input directly. Real hardware keystrokes also work in pure CompositionController (verified 2026-05-12 by `dom-keydown` / `dom-input` listeners on the probe page firing while no `[ime]` lines fire, confirming the keystrokes bypass scrying's IME bridge entirely). The earlier long-standing timeout was a smoke bug — a `producer.send_mouse_input` click before SendInput shifted DOM focus off the target input. The smoke now also dumps the parent HWND tree and `GetFocus` on failure for the next reader.
