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
  default context menus, accelerator keys, and WebView visibility with
  page-observed Page Visibility proof.
- Cookies and profiles: `ICoreWebView2CookieManager` request / set / delete,
  best-effort cookie-change pulses for host mutations and page-side
  `document.cookie` writes, plus persistent profile identity through
  `user_data_dir`, and `WebView2CompositionConfig::non_persistent()` for
  InPrivate / non-persistent controllers.
- Downloads: WebView2 `DownloadStarting` maps to the shared download event
  family with `DownloadId`, host destination decisions, progress,
  completion/cancellation, `cancel_download`, and live `pause_download` /
  `resume_download` / `can_resume_download` operation control. WebView2 does
  not expose a portable offline resume-data blob through this path.
- Auth: WebView2 `BasicAuthenticationRequested` maps to
  `NavigationEvent::AuthChallenged` and `set_auth_handler` can provide HTTP
  Basic credentials. Challenges whose URL matches an active download operation
  are classified as `AuthSource::Download`; other WebView-level challenges
  remain `AuthSource::Page`.
- Permissions: WebView2 `PermissionRequested` maps camera, microphone, and
  sensor-like prompts through `set_permission_handler`.
- Snapshots: `ICoreWebView2::CapturePreview` PNG snapshots.
- Runtime proof in `demo-win`: `--scripted`, `--browser-test`,
  `--cookie-test`, `--profile-test`, `--incognito-test`, `--popup-test`,
  `--routing-test`, `--process-test`, `--download-test`, `--auth-test`, and
  `--permission-test`, `--visibility-test`, `--multi-view-test`, and
  `--capture-test`.

### Partially Shipped or Needs Runtime Proof

- Keyboard and IME: API paths are wired, and `demo-win --keyboard-test` now
  provides a bounded repro. Raw `WM_KEYDOWN` / `WM_CHAR` / `WM_KEYUP`
  forwarding still times out before the DOM input event arrives, so this is a
  confirmed message-loop / WebView2 focus-routing blocker rather than an
  unbounded static-analysis caveat.
- Drag-and-drop: WebView2 OLE forwarding helpers are wired. The
  `WebSurfaceProducer::send_drag_input` trait method now returns a
  Windows-specific unsupported message because real WebView2 drops require the
  host's OLE `IDataObject`; use the concrete `drag_enter` / `drag_over` /
  `drag_leave` / `drop_data` methods until a portable data-carrier abstraction
  exists.
- Profile: persistent cookie recreation and InPrivate isolation are proven.
  Simultaneous multi-view behavior is proven for separate HWNDs by
  `demo-win --multi-view-test`; one HWND still cannot own two composition roots
  in the current setup.

### Missing Windows Slices

- Tab-state serialize / restore: WebView2 exposes no opaque interaction-state
  blob equivalent to WKWebView. Windows now has same-named methods for
  cross-platform call sites; serialization returns `None`, restore returns
  `Unsupported`, and hosts should restore URL/history/form state explicitly.
- Portable download resume data: WebView2 exposes live operation
  `pause_download(id)` / `resume_download(id)` / `can_resume_download(id)`, but
  not a macOS-style offline resume-data blob. Windows cancellations therefore
  report `resume_data: None`; hosts that want resume must pause/resume before
  cancelling the operation.
- Browser conveniences: find-in-page, print-to-PDF / PDF request, interactive
  print, autofill/password toggles, and the content-blocking/spellcheck
  ceilings are wired or documented. Find/PDF are covered by `--find-test` /
  `--pdf-test`; autofill toggles and the hard-throttle ceiling are covered by
  `--browser-test`.
- Observability events: external drop detected, context-menu requested, and
  WebRTC capture lifecycle events are wired, and native
  `Set-Cookie` observation is covered through WebView2
  `WebResourceResponseReceived`.
- Capture polish: producer capture metrics, resize stale-frame/dim-match
  guard, fixed Windows color-target reporting, demo host scale-change resize
  routing, and the documented WebView2 hard-throttling ceiling are wired.

## Implementation Lane

### W0 - Keep the Runtime Lane Bounded

Every GUI smoke must be bounded by an external timeout and process-tree kill.
Do not run raw `cargo run -p demo-win -- ...` during validation. The Win32
message pumps used inside test helpers must bound inner `PeekMessageW` drains
so a busy queue cannot defeat the timeout. ✅ The WebView2 producer's internal
message pump now caps each drain slice, and `demo-win --capture-test` has a
hard external process timeout in the validation lane.

### W1 - Input and Visibility Proof

Goal: close the remaining input confidence gap without changing the public
surface unnecessarily.

- ✅ Add a bounded `demo-win --keyboard-test` probe that drives raw window
  messages through `forward_keyboard_message`; current result is a reproducible
  DOM-delivery timeout, so the remaining work is host message-loop/focus
  routing rather than API discovery.
- ASCII text entry, accelerator-modified keys, dead keys, and CJK IME
  composition remain blocked behind that routing issue.
- ✅ Add a visibility smoke that listens for page-side `visibilitychange` and
  confirms `document.hidden`.
- ✅ Keep drag-in Windows-inherent for now: WebView2 needs an OLE `IDataObject`,
  and the portable trait method reports that concrete requirement.

Done condition: a Windows host can confidently route focus, text, IME, mouse,
wheel, touch/pen, cursor, visibility, and drag-in through scrying without
special page JS outside the demo assertions.

### W2 - Browser Shell Ownership

Goal: give a tabbed shell ownership of browser chrome.

- Wire new-window/popup interception.
- Wire process-failure events and recovery smoke. ✅ `--process-test`
- Wire custom scheme / virtual host content routing. ✅ `--routing-test`
- ✅ Decide and document tab-state restore semantics. Windows exposes the same
  method names as macOS, but `serialize_interaction_state()` returns `None` and
  `restore_interaction_state(...)` returns `Unsupported`; browser shells should
  persist URL/history/form state explicitly when they need Windows tab restore.
- Add `demo-win` modes that prove popup routing, crash recovery if practical,
  and app-owned content loading.

Done condition: a host can own tabs, app content, history recovery, and crash
UI without reaching around `WebView2CompositionProducer`.

### W3 - Trust, Downloads, and Permissions

Goal: make Windows viable for real browsing and authenticated/document flows.

- Wire page auth events into the shared auth types. ✅ `--auth-test`
- Document download-channel auth source limits in WebView2. ✅
- Wire permission requests for camera/microphone/device-like prompts. ✅ `--permission-test`
- Wire downloads with id correlation, destination decisions, progress,
  cancellation, and follow-up resume policy. ✅ WebView2 live pause/resume is
  wired; offline resume-data blobs are a documented Windows ceiling. Covered by
  `--download-test` for the deterministic transfer path.
- Add runtime smokes with local deterministic test pages/servers where needed. ✅

Done condition: a browser shell can prompt for credentials and permissions,
route downloads, cancel downloads, and report transfer progress entirely
through scrying.

### W4 - Browser Conveniences

Goal: match the chrome-visible macOS conveniences where WebView2 exposes public
APIs.

- ✅ Find-in-page with result polling. Covered by `--find-test`.
- ✅ PDF generation / print-to-PDF plus interactive print. `request_pdf` uses
  `PrintToPdfStream`; `print()` invokes WebView2's print UI. Covered by
  `--pdf-test` for the non-interactive PDF path.
- ✅ Context-menu requested event and optional default-menu suppression. Native
  WebView2 context-menu events are registered; the deterministic smoke uses
  the document-start bridge and is covered by `--context-test`.
- ✅ External drop detected event. The document-start bridge mirrors the macOS
  `DataTransfer` heuristic and is covered by `--drop-test`; real page delivery
  still goes through the concrete OLE `IDataObject` drag/drop helpers.
- ✅ WebRTC capture lifecycle observability. Covered by `--media-test` for the
  bridge/event path.
- ✅ Document content blocking: WebView2 exposes request events/filters and the
  Windows producer uses them for virtual-host app routing, but there is no
  public `WKContentRuleList`-style compiled rule-list engine in this path.
- ✅ Document spellcheck/autocorrect: the bound WebView2 API exposes no
  producer-level spellcheck/autocorrect setting; page-authored `spellcheck`
  attributes remain the portable path.
- ✅ Wire autofill/password controls: Windows exposes
  `set_password_autosave_enabled` and `set_general_autofill_enabled` through
  WebView2 `ICoreWebView2Settings4`, and `--browser-test` toggles both.

Done condition: Windows matches the browser-class checklist rows that app
chrome surfaces directly.

### W5 - Capture Robustness and Display Quality

Goal: keep the post-composition texture stream trustworthy under real windowing
conditions.

- ✅ Add capture metrics to the Windows producer: `capture_metrics()` reports
  WGC frames received, emitted frames consumed by the host, and stale
  dimension-mismatch frames dropped during resize/restart churn.
- ✅ Add stale-frame / dim-match guards around resize and capture restart.
- ✅ Add DPI monitor-move handling and a runtime smoke that moves or simulates
  scale changes where feasible. `demo-win` routes `ScaleFactorChanged` through
  the same renderer resize path as physical resize events, and `--scale-test`
  simulates scale-induced physical capture-size changes by resizing the
  WebView2 producer and acquiring/importing fresh WGC frames at each size.
- ✅ Decide the Windows color target: the WebView2/WGC path currently reports
  `ColorPipeline::Srgb` and `Bgra8Unorm`. Public WebView2/WGC APIs in the
  bound version do not expose a Display P3 or HDR pixel-format/color-space
  control for this composition capture path, so P3/HDR stay unsupported on
  Windows until Microsoft exposes that surface.
- ✅ Document the hard-throttling limit: WebView2 exposes `SetIsVisible` for
  Page Visibility but no public inactive-scheduling / hard-pause equivalent to
  macOS `WKPreferences.inactiveSchedulingPolicy`. Windows `apply_settings`
  returns `Unsupported` when `inactive_scheduling_policy` is set, and
  `demo-win --browser-test` covers that ceiling.
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