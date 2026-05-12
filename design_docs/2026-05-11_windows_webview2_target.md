# Windows WebView2 integration target

**Status:** target and audit doc, split from
[`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md) on
2026-05-11.

This document sets the Windows target for scrying's system-webview
integration. It is deliberately Windows-specific: the cross-platform index
stays in
[`2026-05-09_browser_parity_checklist.md`](2026-05-09_browser_parity_checklist.md),
and the platform ceiling overview stays in
[`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md).

## Target

Windows should be a production-quality WebView2 embedding backend for a
browser-shaped host: a tabbed shell should be able to own navigation,
input, profiles, downloads, permissions, auth prompts, context menus, and
capture diagnostics through scrying without reaching around the producer for
normal browser chrome work.

The rendering target is more constrained: the best public Windows path is
WebView2 CompositionController into a WinComp visual, captured by
Windows.Graphics.Capture, copied through a shared D3D11 texture, and
imported by wgpu as a D3D12 texture. That can be robust, low-friction, and
explicitly synchronized, but it is still post-DComp compositor output. The
target is therefore **browser-shell complete over WebView2 plus robust WGC
texture import**, not pre-composition renderer access.

## How Good Can It Be?

Very good for app/browser integration:

- Navigation and chrome ownership can be first-class: history, title,
  popups, crashes, auth, permissions, downloads, DevTools, settings, and
  cookie/profile state all have public WebView2-shaped paths.
- Input can be native enough for serious text fields: mouse, wheel, touch,
  pen, focus, cursor, OLE drag-in, and raw `WM_KEY*` / `WM_CHAR` /
  `WM_IME*` forwarding are the right Windows shapes. The missing piece is
  validation and host-loop ergonomics, especially CJK IME.
- Texture handoff can be reliable: the producer already supports explicit
  D3D12 fence sync when the host passes a shared fence handle, with the old
  barrier/cache fallback retained.
- Profile/cookie behavior can be browser-like for persistent stores:
  `user_data_dir` is the WebView2 profile identity and persistent cookies
  survive producer recreation.

But it cannot be a raw WebView renderer:

- WebView2 does not expose pre-DComp textures. WGC observes the composed
  visual, so latency is at least one compositor frame and often 1-2 frames.
- Sub-frame / iframe capture is out of reach without upstream API additions.
- Capture while the visual is detached or hidden from the composition tree is
  not a public contract.
- A single HWND cannot host two independent `DesktopWindowTarget` roots at
  the same time in the current composition setup; the `--profile-test` smoke
  validates profile persistence by recreating the producer sequentially.

The right Windows ambition is: **excellent browser-host semantics and a
diagnosable, synchronized post-composition texture stream**.

## Audited Current State

Audited against
[`../scrying/src/webview2_composition_producer.rs`](../scrying/src/webview2_composition_producer.rs),
[`../scrying/src/lib.rs`](../scrying/src/lib.rs), and
[`../demo-win/`](../demo-win/).

### Shipped

- Composition path: `ICoreWebView2CompositionController` attached to a
  WinComp visual, `GraphicsCaptureItem::CreateFromVisual`,
  `Direct3D11CaptureFramePool`, persistent shared D3D11 destination texture,
  and `NativeFrame::Dx12SharedTexture` handoff.
- Explicit GPU sync: `WebView2CompositionConfig::with_fence_shared_handle`
  opens a host-created D3D12 shared fence on the producer side; emitted frames
  carry `SyncMechanism::ExplicitFence` and a monotonic fence value. No-handle
  mode keeps the fallback path.
- Lifecycle and navigation: inline HTML, URL navigation, reload, stop,
  back/forward, `CanGoBack`, `CanGoForward`, `NavigationStarting`,
  `SourceChanged`, `NavigationCompleted`, document-title events, and
  `ProcessFailed` mapping to `NavigationEvent::ContentProcessTerminated`.
- Input: mouse and wheel via `SendMouseInput`; touch/pen/pointer via
  `SendPointerInput`; focus via `MoveFocus`; cursor-change events; OLE
  `DragEnter` / `DragOver` / `DragLeave` / `Drop`; portable keyboard events;
  and raw Win32 keyboard/IME message forwarding through
  `forward_keyboard_message`.
- JS interop: host-to-page strings via `PostWebMessageAsString`, page-to-host
  strings via `WebMessageReceived`, and document-start script injection.
- New-window / popup ownership: WebView2 `NewWindowRequested` maps to
  `NavigationEvent::NewWindowRequested { url }`, and the producer suppresses
  the default popup so the host owns tab creation.
- App-owned content routing: `register_virtual_host_handler(host, handler)`
  maps `https://{host}/...` requests through WebView2 `WebResourceRequested`
  and serves `UrlSchemeResponse` bodies/headers without network access.
- Settings: zoom, user agent, JavaScript enablement, DevTools enablement,
  default context menus, accelerator keys, and WebView visibility.
- Cookies and profiles: `ICoreWebView2CookieManager` request / set / delete,
  best-effort cookie-change pulses for host mutations and page-side
  `document.cookie` writes, plus persistent profile identity through
  `user_data_dir`, and `WebView2CompositionConfig::non_persistent()` for
  InPrivate / non-persistent controllers.
- Snapshots: `ICoreWebView2::CapturePreview` PNG snapshots.
- Runtime proof in `demo-win`: `--scripted`, `--browser-test`,
  `--cookie-test`, `--profile-test`, `--incognito-test`, `--popup-test`,
  `--routing-test`, and `--process-test`.

### Partially Shipped or Needs Runtime Proof

- Keyboard and IME: API paths are wired, but the optional
  `WEBVIEW_KEYBOARD_VALIDATE=1` DOM round-trip still times out. The target is
  a bounded `demo-win` smoke that validates ASCII text, accelerators / system
  keys, dead keys, and one CJK IME path through raw host-window messages.
- Drag-and-drop: WebView2 OLE forwarding helpers are wired, but portable
  trait-level shape and a runtime smoke still need cleanup.
- Visibility: `SetIsVisible` is wired and `--browser-test` exercises the call,
  but a page-observed hidden/visible round-trip should prove throttling and
  Page Visibility behavior.
- Profile: persistent cookie recreation and InPrivate isolation are proven.
  Simultaneous multi-view behavior still needs a host-window strategy because
  one HWND cannot own two composition roots in the current setup.

### Missing Windows Slices

- Tab-state serialize / restore: design a Windows equivalent for macOS's
  opaque interaction-state blob, or document the WebView2 limitation and set
  the target to URL/history/form-state best effort.
- Downloads: wire WebView2 download events, destination decisions, progress,
  cancellation, and auth source correlation to the existing cross-platform
  download types.
- Auth challenges: wire page-load and download-channel authentication
  challenges into `AuthChallenge` / `AuthDisposition` or document any
  WebView2-specific disposition mismatch.
- Permission handlers: wire camera, microphone, and related WebView2
  permission requests into `PermissionRequest` / `PermissionDecision`.
- Browser conveniences: find-in-page, print-to-PDF / PDF request, interactive
  print, content rules or WebView2 equivalents, spellcheck controls, and
  autofill/credential integration notes.
- Observability events: context-menu requested, external drop detected,
  WebRTC capture lifecycle, native cookie-change / `Set-Cookie` observation if
  a newer WebView2 binding exposes it.
- Capture polish: capture metrics, resize stale-frame/dim-match guard, DPI
  monitor-move handling, Display P3 / HDR pipeline decision, and documented
  hard-throttling limit.

## Implementation Lane

### W0 - Keep the Runtime Lane Bounded

Every GUI smoke must be bounded by an external timeout and process-tree kill.
Do not run raw `cargo run -p demo-win -- ...` during validation. The Win32
message pumps used inside test helpers must bound inner `PeekMessageW` drains
so a busy queue cannot defeat the timeout.

### W1 - Input and Visibility Proof

Goal: close the remaining input confidence gap without changing the public
surface unnecessarily.

- Add a `demo-win` keyboard/IME smoke that drives raw window messages through
  `forward_keyboard_message` when possible, not only the portable synthetic
  `send_keyboard_input` bridge.
- Prove ASCII text entry, accelerator-modified keys, dead keys, and at least
  one CJK IME composition path.
- Add a visibility smoke that listens for page-side `visibilitychange`,
  confirms `document.hidden`, and records throttled animation cadence when
  hidden.
- Decide whether drag-in observability should become a portable event or stay
  Windows-inherent OLE forwarding.

Done condition: a Windows host can confidently route focus, text, IME, mouse,
wheel, touch/pen, cursor, visibility, and drag-in through scrying without
special page JS outside the demo assertions.

### W2 - Browser Shell Ownership

Goal: give a tabbed shell ownership of browser chrome.

- Wire new-window/popup interception.
- Wire process-failure events and recovery smoke. ✅ `--process-test`
- Wire custom scheme / virtual host content routing. ✅ `--routing-test`
- Decide and document tab-state restore semantics.
- Add `demo-win` modes that prove popup routing, crash recovery if practical,
  and app-owned content loading.

Done condition: a host can own tabs, app content, history recovery, and crash
UI without reaching around `WebView2CompositionProducer`.

### W3 - Trust, Downloads, and Permissions

Goal: make Windows viable for real browsing and authenticated/document flows.

- Wire page and download auth events into the shared auth types.
- Wire permission requests for camera/microphone/device-like prompts.
- Wire downloads with id correlation, destination decisions, progress,
  cancellation, and follow-up resume policy.
- Add runtime smokes with local deterministic test pages/servers where needed.

Done condition: a browser shell can prompt for credentials and permissions,
route downloads, cancel downloads, and report transfer progress entirely
through scrying.

### W4 - Browser Conveniences

Goal: match the chrome-visible macOS conveniences where WebView2 exposes public
APIs.

- Find-in-page with result polling.
- PDF generation / print-to-PDF plus interactive print if WebView2 exposes a
  host-safe path.
- Context-menu requested event and optional default-menu suppression.
- WebRTC capture lifecycle observability.
- Content blocking / request filtering policy or a documented WebView2
  equivalent.
- Spellcheck/autocorrect and autofill/credential integration notes.

Done condition: Windows matches the browser-class checklist rows that app
chrome surfaces directly.

### W5 - Capture Robustness and Display Quality

Goal: keep the post-composition texture stream trustworthy under real windowing
conditions.

- Add capture metrics to the Windows producer, mirroring macOS's
  `CaptureMetrics` shape.
- Add stale-frame / dim-match guards around resize and capture restart.
- Add DPI monitor-move handling and a runtime smoke that moves or simulates
  scale changes where feasible.
- Decide the Windows color target: document that WGC/WebView2 currently emits
  BGRA8 sRGB, or wire a real Display P3 / HDR path if public APIs allow it.
- Keep explicit D3D12 fence sync as the preferred path and retain the fallback
  invalidation escape hatch.

Done condition: a host can diagnose capture cadence, resize and monitor moves
do not produce stale/badly-scaled frames, and color expectations are explicit.

## Non-Goals Unless Microsoft Ships New APIs

- Pre-composition WebView2 texture access.
- Sub-frame / iframe texture extraction.
- Capture while the visual is not part of the composition tree.
- Lower-than-compositor-frame latency.
- Native cookie-change observation for response-header `Set-Cookie` when the
  bound WebView2 version exposes no such event.

## Audit Notes

- `webview2-com = 0.39.1` is the current binding version. Several target rows
  may become simpler after auditing newer WebView2 interfaces, but the target
  should not assume APIs that the crate does not expose.
- The shared `NavigationEvent`, `Download*`, `Auth*`, `Permission*`, and
  `Cookie` public types are already broad enough for most Windows parity work;
  the likely code work is producer wiring, not wholesale API invention.
- `demo-win` is the right home for Windows runtime proof. The catchall
  `demo-scrying-winit` should remain a backend-selection and dependency-gating
  smoke.