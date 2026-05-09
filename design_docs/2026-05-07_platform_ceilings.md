# Platform ceilings and parity roadmap

**Status:** living document. Last refreshed 2026-05-07 against scrying 0.2.0.

This records, for each of the three target platforms, the **upper bound of
what the platform's native webview can deliver to a host wgpu pipeline**,
the **current state of scrying's implementation**, and the **gap to close**
to hit cross-platform parity.

The intent is not "implement everything everywhere", it's:

1. Know the ceiling so we don't accidentally accept a lower one.
2. Pick a parity baseline that's achievable on all three without
   architecture-bending workarounds.
3. Surface platform-specific upgrades (fences, IME, DMABUF) as
   distinct slices that can ship independently of the parity baseline.

---

## Per-platform ceilings

### Windows — WebView2 + WGC + D3D11/D3D12

**Capture path (what we ship):** ICoreWebView2CompositionController →
Windows.UI.Composition.Visual → Windows.Graphics.Capture →
shared D3D11 keyed-mutex NT-handle texture → wgpu D3D12 `OpenSharedHandle`
import.

**Ceiling:**

- **Frame rate / latency**: capped by the system compositor. Sample
  rate = display refresh rate (typically 60 / 120 / 144 Hz). Latency =
  ~1–2 compositor frames (16–33ms at 60 Hz) between WebView paint and
  importable wgpu texture. Cannot go below 1 frame without Microsoft
  shipping a non-WGC export path (long-standing open WV2Feedback ask;
  no commitment).
- **Pixel quality**: post-DComp composited output. Pixel-exact under
  the WebView's own rasterization. Cannot get pre-composition raw
  layer textures.
- **GPU sync** (today): keyed-mutex on the producer side, transition-
  barrier flush on the consumer side (wgpu D3D12). Empirically stable
  but the consumer-side cache flush rides on driver behavior, not
  contract. **Upgrade target: explicit `D3D12_FENCE_FLAG_SHARED`
  fence** — see "GPU synchronization upgrades" below.
- **Input**: full mouse + scroll (`SendMouseInput`), full touch + pen
  (`SendPointerInput`), full focus management (`MoveFocus` +
  `add_GotFocus` / `add_LostFocus`), drag-and-drop forwarding
  (`DragEnter`/`DragOver`/`DragLeave`/`Drop`), cursor-change reporting
  (`add_CursorChanged`). Keyboard requires a window-subclass message
  forwarder — fully solvable, fiddly with winit. **IME** for non-Latin
  input is the genuine sharp edge; achievable but a separate work
  item.
- **Navigation / lifecycle**: complete. URL + HTML, back/forward/
  stop/reload, NavigationStarting/SourceChanged/NavigationCompleted/
  DocumentTitleChanged events, ProcessFailed for crash recovery.
- **JS interop**: full (`PostWebMessageAsString` + `WebMessageReceived`
  + `AddScriptToExecuteOnDocumentCreated`).
- **Settings / environment**: zoom, user agent, IsVisible (throttling
  control), profile + cookie store (`WKWebsiteDataStore` analog),
  custom URL schemes via `WebResourceRequested`, downloads,
  permissions, new-window interception, DevTools, print + print-to-PDF.
- **Snapshots**: `CapturePreview` (PNG/JPEG) — already wired in 0.2.0.
- **Out of reach without MSFT API additions**: pre-composition
  extraction, sub-iframe capture, capture while visual is hidden
  from the composition tree, sub-frame latency.

**Current scrying state (0.2.0):** frame production complete. Embeddable
surface (mouse + focus + nav + JS messaging + snapshots) shipped.
Keyboard, IME, touch/pen, drag-and-drop, cursor-change, settings, profile,
custom schemes, downloads, devtools, back/forward — not yet implemented.

---

### macOS — WKWebView + ScreenCaptureKit + IOSurface + Metal

**Capture path (planned):** `WKWebView` hosted in `NSView` →
`ScreenCaptureKit` content-filter bound to that view (or to an offscreen
`NSWindow`) → `CMSampleBuffer` carrying an `IOSurface` →
`MTLTexture` via `CVMetalTextureCacheCreateTextureFromImage` → wgpu
Metal import.

**Ceiling:**

- **Frame rate / latency**: similar to Windows. SCK samples per
  compositor frame; latency is 1–2 frames depending on display
  configuration. ProMotion / 120 Hz handled natively.
- **Pixel quality**: post-composition, pixel-exact. Same constraint
  as Windows: cannot get pre-composition raw layer output.
- **GPU sync**: IOSurface has implicit cross-API coherence on Apple
  silicon (unified memory) and via IOSurface locks on Intel. Cleaner
  than the Windows path — there's no equivalent of the keyed-mutex /
  transition-barrier dance. **Upgrade option: explicit
  `MTLSharedEvent`** between producer and consumer command queues if
  empirical coherence ever breaks (analog of the Windows fence work).
- **Input**: WKWebView is a normal `NSView`. In *captured-and-rendered-
  elsewhere* mode, the host forwards events to the WKWebView via the
  NSResponder API: `mouseDown:`, `mouseUp:`, `mouseMoved:`,
  `scrollWheel:`, `keyDown:`, `keyUp:`, `flagsChanged:`. Touch via
  NSTouch (limited), gestures via NSEvent. **IME**: NSTextInputClient
  protocol — Apple's IME story is more uniform than Windows' but still
  non-trivial.
- **Cursor**: `NSCursor.set(_:)` plus the WKWebView's
  `mouseMoved`/cursor-update protocol; clean.
- **Navigation / lifecycle**: complete. `WKNavigationDelegate` covers
  every nav event; `WKUIDelegate` for popups/permissions; KVO on
  `title` / `url` / `estimatedProgress` for state.
- **JS interop**: full bidirectional via `WKUserContentController` +
  `WKScriptMessageHandler` (JS → host) and `evaluateJavaScript` /
  `callAsyncJavaScript` (host → JS).
- **Settings / environment**: per-`WKWebsiteDataStore` cookies +
  storage isolation, custom URL schemes (`WKURLSchemeHandler`),
  downloads (`WKDownload` 11.3+), `WKPreferences` for JS / dev tools /
  fraudulent-content-warning, user agent, content rules
  (`WKContentRuleList`).
- **Snapshots**: `takeSnapshot(with:completionHandler:)` returns an
  `NSImage`. Encode to PNG via `NSBitmapImageRep`. Already documented
  as a fallback advertise; needs wiring.
- **Out of reach without Apple API additions**: pre-composition layer
  textures, capture-while-hidden (SCK requires the view to be in a
  window), sub-iframe capture.

**Current scrying state (0.4.0+):**
The macOS producer is a real working WebView, not a skeleton.
Implementation landed in five cohesive slices on top of the
[`native_frame::metal`](../scrying/src/native_frame/metal.rs)
`MTLTexture` → `wgpu::Texture` import (initial M2 work, lifted
structurally from wgpu-graft and adapted to the wgpu-hal 29 API drift
where `texture_from_raw` now takes
`Retained<ProtocolObject<dyn MTLTexture>>` directly):

- **Slice A — WKWebView lifecycle.** `WkWebViewProducer::new` retains
  the parent `NSView`, builds a default `WKWebViewConfiguration`,
  creates the `WKWebView` with an NSRect derived from
  `config.offset` and `config.size` (both physical pixels → both
  divided by the parent window's `backingScaleFactor` to get
  AppKit points), wires a navigation delegate, and adds the
  WebView as a subview.
  `navigate_to_string` waits on `WKNavigationDelegate.didFinishNavigation:`
  while pumping the main run loop in 16 ms slices; `resize` /
  `set_offset` reshape the live view; `Drop` removes from superview
  and clears the delegate.
- **Slice B — ScreenCaptureKit pipeline.**
  [`WkWebViewProducer::start_capture(host, timeout)`](../scrying/src/wkwebview_producer.rs)
  pulls the host wgpu's `MTLDevice` via
  `as_hal::<Metal>().raw_device().clone()`, walks
  `SCShareableContent.windows` for the WKWebView's host
  `NSWindow.windowNumber`, builds an
  `SCContentFilter::initWithDesktopIndependentWindow:`, configures an
  `SCStream` for `kCVPixelFormatType_32BGRA` /
  `setShowsCursor(false)` / `setQueueDepth(3)`, attaches custom
  `SCStreamDelegate` + `SCStreamOutput` delegates on a dedicated
  `DispatchQueue`, and blocks on `startCaptureWithCompletionHandler:`.
  `try_acquire_frame` then takes the latest `CMSampleBuffer`,
  extracts `IOSurfaceRef` via `CVPixelBufferGetIOSurface`, wraps it
  as `MTLTexture` on the host device, and emits
  `WryWebSurfaceFrame::Native(NativeFrame::MetalTextureRef(...))`.
  A small `SendCFRetained<T>` newtype wraps the dispatch-queue →
  main-thread sample handoff. `acquire_frame` is blocking — pumps
  the run loop until a sample arrives or `frame_timeout` elapses.
- **Slice C — navigation parity.** `navigate_to_url` loads a URL via
  `loadRequest:`. The navigation delegate now fires `Starting` /
  `SourceChanged` / `Completed` events into a FIFO drained by
  `poll_navigation_event`. `move_focus` sends the WKWebView to
  first-responder via the host `NSWindow`.
- **Slice D — mouse forwarding.** `send_mouse_input` synthesizes an
  `NSEvent` (window-coordinates, points, bottom-left origin via
  `convertPoint_toView(None)`), and dispatches directly through the
  WKWebView's NSResponder slots — `mouseDown:` / `mouseUp:` /
  `mouseDragged:` / `mouseMoved:` / `rightMouse*` / `otherMouse*` /
  `mouseExited:`. `Move` differentiates dragged-with-button from
  plain `MouseMoved` based on `MouseVirtualKeys` button state;
  `DoubleClick` rides on `clickCount = 2`. Scroll wheel and
  X-button `buttonNumber` distinction are deferred (need the
  `CGEvent` path).
- **Slice E — bidirectional JS messaging.**
  `WKUserContentController` is pre-loaded on the configuration with
  a `WKScriptMessageHandler` named `scryingHostBridge` and a
  document-start user script that builds a `window.chrome.webview`
  shim. JS-side `window.chrome.webview.postMessage(s)` lands in a
  FIFO drained by `poll_web_message`; host-side `post_web_message(s)`
  runs an `evaluateJavaScript:` with a JSON-encoded literal that
  dispatches to listeners registered via
  `window.chrome.webview.addEventListener('message', ...)`. The shim
  is idempotent and the JS API matches WebView2's surface so
  consumers can write portable bridge code.
- **Slice F — CPU snapshots.** `capture_cpu_snapshot` runs
  `takeSnapshotWithConfiguration:completionHandler:` (main-thread
  callback, no Screen Recording permission needed), pumps the run
  loop until the NSImage arrives or `config.frame_timeout` elapses,
  and decodes the `NSImage::TIFFRepresentation` through the `image`
  crate's TIFF decoder into an `RgbaImage` returned as
  `WryWebSurfaceFrame::CpuRgba`. Independent of `start_capture` —
  works as a fallback diagnostic path, useful for thumbnails or for
  verifying the WebView is rendering before standing up the SCK
  pipeline.
- **Slice G — scroll wheel via CGEvent.** `MouseEventKind::Wheel` /
  `HorizontalWheel` build a `CGEventCreateScrollWheelEvent2` (pixel
  units, AppKit sign convention) and convert through
  `NSEvent::eventWithCGEvent:` before dispatching to
  `webview.scrollWheel:`. Removes the only "deferred" caveat from
  slice D's mouse forwarding.
- **Slice H — title-changed events via KVO.** A `TitleObserver`
  NSObject subclass is registered as a `title` key-path observer on
  the WKWebView at construction time (`addObserver:forKeyPath:options:context:`
  with `NSKeyValueObservingOptions::New`). When the page mutates
  `document.title` after the initial load, the KVO callback pushes
  `NavigationEvent::TitleChanged { title }` into the same FIFO the
  navigation delegate writes to. `Drop` calls `removeObserver:`
  before any retained references cascade so the observed object
  outlives its observer registration.
- **Slice I — keyboard forwarding (with IME baseline).**
  `send_keyboard_input` synthesizes an `NSEvent` via
  `keyEventWithType:...:characters:charactersIgnoringModifiers:isARepeat:keyCode:`
  and dispatches through the WKWebView's `keyDown:` / `keyUp:` /
  `flagsChanged:` slots. `characters` flows through to WebKit's
  `NSTextInputClient` implementation, so IME composition (CJK, dead
  keys, marked text) works without explicit composition-state
  threading on the host side — the host just forwards whatever the
  windowing system reports.
- **Slice J — cursor-change reporting.** After each forwarded
  pointer event, `observe_cursor_change` reads
  `NSCursor.currentSystemCursor` and compares against the canonical
  cursor singletons (`arrowCursor`, `IBeamCursor`,
  `pointingHandCursor`, `crosshairCursor`, `openHandCursor`,
  `closedHandCursor`, `operationNotAllowedCursor`, etc.) to translate
  the WebKit-set cursor into a [`CursorShape`]. Only changes are
  queued, so `poll_cursor_shape` reflects "the engine wants the host
  to display X" without spamming `Default` events.
- **Slice K — drag-and-drop forwarding (documented constraint).**
  `WKWebView` receives drag/drop via the `NSDraggingDestination`
  protocol, whose callbacks require an `NSDraggingInfo` parameter.
  `NSDraggingInfo` instances are constructed only by AppKit's drag
  manager — there is no public API to synthesize one. So
  `send_drag_input` for **capture mode** is genuinely not feasible
  without SPI. **Overlay mode** is automatic — AppKit's drag manager
  delivers drags to the WKWebView through the responder chain
  without producer involvement, so the host doesn't need to forward.
  Producer returns `WryWebSurfaceError::Unsupported` with a message
  that explains both branches.
- **Slice L — per-profile `WKWebsiteDataStore`.** When
  `config.data_dir` is non-empty, the producer derives a stable
  version-8 UUID from the path's bytes via FNV-1a 128 and resolves
  a per-profile persistent store through
  `WKWebsiteDataStore::dataStoreForIdentifier:` (macOS 14+). Empty
  `data_dir` keeps the shared default store. macOS doesn't take an
  arbitrary path for data stores (storage lives in the app
  container by UUID); the deterministic-UUID-from-path scheme is the
  native analog of the per-directory profile model.
- **Slice M — `MTLSharedEvent` synchronizer scaffolding.** New
  `SyncMechanism::ExplicitMetalEvent` variant and a
  `MetalSharedEventSynchronizer` skeleton in
  [`scrying::native_frame`](../scrying/src/native_frame/sync_metal.rs)
  parallel to the Windows `Dx12FenceSynchronizer`. Currently a no-op
  (accepts both `None` and `ExplicitMetalEvent` without
  waiting/signalling) because ScreenCaptureKit doesn't expose its
  render queue for explicit fencing. Infrastructure is in place for
  when Apple extends SCK or a downstream consumer wires manual CPU
  signal points.
- **Slice N — `SCStreamConfiguration` auto-update on resize.**
  `resize` now pushes the new pixel dimensions through to the live
  stream via `stream.updateConfiguration:completionHandler:` (with
  the same non-size params as the original `start_capture` —
  encapsulated in a single `make_stream_configuration` helper so the
  two paths stay consistent). SCK samples post-resize arrive at the
  requested resolution without restarting the stream.

The producer struct accumulates over the slices: parent NSView,
`WKWebView`, navigation delegate (with shared `NavState` carrying both
the navigation completion signal and the events FIFO), script-message
handler, web-message FIFO, and an `Option<CaptureState>` that's `Some`
once `start_capture` has resolved. Capabilities flip from
`NativeChildOverlay` (default) to `ImportedTexture` when capture
starts, and back on `stop_capture` / `Drop`.

Threading: SCK output callbacks fire on a background dispatch queue
and write the latest sample into a `Mutex<Option<SendCFRetained<CMSampleBuffer>>>`
that `try_acquire_frame` reads on the main thread; the
`SCShareableContent` async resolution carries a similar
`unsafe impl Send` wrapper around the matched `Retained<SCWindow>`.

**Windows-0.2.0 parity status:** ✅ achieved by slices A–H. Slices
I–N pushed the macOS producer well past the 0.2.0 baseline into
0.3.0 / 0.4.0 territory: keyboard + IME, cursor reporting,
per-profile data stores, MTLSharedEvent infrastructure, and resize-
applies-to-stream are all live; drag-and-drop is documented as
SPI-blocked.

**Remaining limitations:**

- Drag-and-drop forwarding in capture mode (SPI-required).
- X-button `buttonNumber` distinction (X1/X2 arrive as Other-mouse
  with default index; CGEvent would fix this in a small follow-up).
- `MTLSharedEvent` is scaffolded but inert — needs a producer-side
  signal hook that ScreenCaptureKit's public API doesn't expose
  today. Implicit IOSurface coherence remains the contract.

---

### Linux — WPE (primary) or WebKitGTK (fallback)

Two backends, two ceilings. WPE is the strategically correct choice
for embeddable GPU-handoff frames; WebKitGTK is the ubiquitous-
distribution fallback.

#### WPE + DMABUF + Vulkan (primary)

**Capture path (planned):** `WPEWebView` with `WPEViewBackendDMABuf` →
per-frame DMABUF fd + format/modifier + `VkSemaphore` for ordering →
wgpu Vulkan `VK_KHR_external_memory_fd` import.

**Ceiling:**

- **Frame rate / latency**: potentially the *lowest* of the three
  platforms. WPE renders directly into DMABUFs without going through
  a compositor capture step — the DMABUF fd is the WebView's render
  output. Latency = WebView paint completion + Vulkan import. No
  extra compositor-frame buffering.
- **Pixel quality**: pre-composition (this is the only platform where
  that's true). The WebView paints directly into the DMABUF; if
  scrying's host wants additional compositing, it does it with the
  imported texture.
- **GPU sync**: VkSemaphore is the cross-API contract. Producer
  signals on render completion, consumer waits before sampling.
  Standards-correct out of the box — no driver-empirical hacks.
- **Input**: WPE has a clean `wpe_view_backend_dispatch_*_event` API
  for keyboard, pointer, axis, touch. Host serializes platform events
  (winit / wayland / xkbcommon) into WPE event structs and dispatches.
  No window subclass. **IME**: Wayland text-input-v3 / XIM via
  xkbcommon — solvable but compositor-dependent.
- **Cursor**: WPE reports cursor changes via the view backend; host
  translates to the windowing system's cursor.
- **Navigation / lifecycle**: complete. Same WebKit GTK-family API
  surface (WebKitWebView, signals for load-changed,
  load-failed, document-loaded, title-changed; WebKitNavigationPolicyDecision).
- **JS interop**: `webkit_web_view_run_javascript_in_world` (host →
  JS) + `WebKitUserContentManager` script messages (JS → host).
- **Settings / environment**: `WebKitSettings` (zoom, JS, dev tools,
  user agent), `WebKitWebsiteDataManager` (cookies / storage),
  `webkit_security_manager_register_uri_scheme_as_*` for custom
  schemes, downloads via WebKitDownload.
- **Snapshots**: `webkit_web_view_get_snapshot` → cairo surface →
  encode.
- **Out of reach**: less than the other platforms because WPE is
  designed for embedding; the main "out of reach" is platform-level
  things like printing (less well-developed than WebView2) and
  Wayland/X11 abstraction edges.

#### WebKitGTK + wlroots screencopy (fallback)

**Capture path (fallback):** `WebKitWebView` as a `GtkWidget` in an
offscreen `GtkOffscreenWindow` → `zwlr_screencopy_manager_v1`
(wlroots-class compositors) → wl_buffer (DMABUF or shm) → wgpu
import. Or GTK's PaintCallback compositing-mode → cairo surface →
CPU upload.

**Ceiling (lower than WPE):**

- **Frame rate / latency**: depends on capture path. Screencopy adds
  a compositor-roundtrip frame. PaintCallback / CPU readback is the
  worst.
- **Pixel quality**: same as WPE for the rendered output, but with
  GTK widget chrome possibly leaking in if not isolated.
- **GPU sync**: screencopy gives a wl_buffer; the protocol implies
  ready-on-receive but lacks an explicit semaphore. Cleaner than
  Windows pre-fence but less than WPE's VkSemaphore.
- **Input**: GTK widget event forwarding — automatic if the widget
  has focus, but in capture-and-render-elsewhere mode requires
  forwarding gdk events to the offscreen widget, which is awkward.
- **Compositor compatibility**: zwlr_screencopy_manager_v1 is wlroots
  (Sway, river, Hyprland, KDE on Wayland with extension, GNOME via
  XDG portals). X11 / non-wlroots compositors fall back to the cairo
  snapshot path.

**Strategic position:** WPE is the load-bearing path. WebKitGTK fallback
matters only for distributions where WPE isn't readily packaged.
*Wry forces the WebKitGTK path*, which is one of the reasons scrying
the library doesn't depend on wry.

**Current scrying state (0.2.0):** [`webkitgtk_producer`](../scrying/src/webkitgtk_producer.rs)
is a planning skeleton named for the WebKitGTK fallback. The WPE
producer doesn't exist yet (likely lives in `wpe_producer.rs` once
implemented). All Tier-1 trait methods return `Unsupported`.

---

## Cross-platform parity matrix

What every platform's producer should implement to hit the **parity
baseline**. Every row that's `?` on a platform is a gap to close;
every row marked `—` is structurally not on that platform.

| Capability | Windows WV2 | macOS WKWebView | Linux WPE | Linux WebKitGTK |
| --- | --- | --- | --- | --- |
| Imported GPU texture per frame | ✅ 0.1.0 | ✅ 0.4.0 | ? | ? (degraded) |
| Resize / offset | ✅ | ✅ 0.4.0 | ? | ? |
| Navigate (URL + HTML) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Reload / Stop / Back / Forward | ? | ? | ? | ? |
| Mouse forwarding (buttons + move + leave) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Scroll wheel forwarding | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Touch + pen forwarding | ? | ? | ? | ? |
| Keyboard forwarding (basic) | ? | ✅ 0.4.0 | ? | ? |
| IME (CJK / non-Latin) | ? | ✅ 0.4.0 (via NSTextInputClient) | ? | ? |
| Drag-and-drop into webview | ? | — capture (SPI-blocked) / ✅ overlay (auto) | ? | ? |
| Focus management | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Cursor-change reporting | ? | ✅ 0.4.0 | ? | ? |
| Navigation events (start/source/complete) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Title-changed event | ✅ 0.2.0 | ✅ 0.4.0 (KVO) | ? | ? |
| JS messaging (bidirectional) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| PNG / CPU snapshot | ✅ 0.2.0 | ✅ 0.4.0 (CPU RGBA) | ? (`get_snapshot`) | ? |
| Settings (zoom, UA, JS, devtools) | ? | ? | ? | ? |
| Profile / cookies / storage | ? | ✅ 0.4.0 (per-profile UUID) | ? | ? |
| Custom URL schemes | ? | ? | ? | ? |
| Downloads | ? | ? | ? | ? |
| New-window / popup intercept | ? | ? | ? | ? |
| Process-failure recovery | ? | ? | ? | ? |
| **Cross-API GPU sync** | barrier (empirical) | IOSurface (implicit) | VkSemaphore (explicit) | wl_buffer (implicit) |
| Pre-composition extraction | — | — | ✅ (only platform) | — |
| Sub-iframe / sub-frame capture | — | — | — | — |

The bottom three rows are *structural ceilings* — `—` means "not
possible without upstream API additions". Everything else is just work.

---

## GPU synchronization upgrades

The cross-API sync story is the only place where the platforms
differ in how *contractual* the producer→consumer ordering is today.

### Windows — explicit D3D12 fence (the "fence" work)

Today scrying uses keyed-mutex on the producer side and a throwaway
`copy_texture_to_buffer` on the consumer side to force a
`SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE` transition barrier,
which on D3D12 happens to flush shader caches that would otherwise
hold a stale view of the externally-written shared texture. Works
empirically; not a contract.

The contractual upgrade is a `D3D12_FENCE_FLAG_SHARED` fence:

1. Create the fence on the wgpu D3D12 device, export an NT handle
   via `ID3D12Device::CreateSharedHandle`.
2. Open it on the producer's D3D11 device via
   `ID3D11Device5::OpenSharedFence`.
3. Producer signals `value = n+1` on its D3D11 immediate context after
   `CopyResource`, releases the keyed mutex.
4. Consumer queues `ID3D12CommandQueue::Wait(fence, n+1)` before the
   render submit.
5. Bump `n` per frame.

Cost: ~150–250 lines crossing the wgpu-hal escape hatch
(`device.as_hal::<Dx12>()` for the queue), `ID3D11Device5` /
`ID3D11DeviceContext4` plumbing, fence-value tracking, and a
pre-submit injection point (probably a tiny no-op command buffer that
runs `Wait` before the real submit).

Worth doing when (a) scrying ships beyond the development box and a
driver gives someone stale frames, (b) downstream interop expands
beyond WebView2 capture, or (c) the code wants to be canonically
correct rather than empirically correct.

### macOS — MTLSharedEvent (precautionary)

IOSurface coherence is implicit on Apple silicon and via IOSurface
locks on Intel. **No fence work is required for correctness today.**
If empirical coherence ever fails, `MTLSharedEvent` between the
SCK-side command queue and the wgpu Metal queue is the analog: signal
+ wait, same shape as the D3D12 fence.

### Linux WPE — VkSemaphore (already contractual)

The WPE DMABUF protocol returns a `VkSemaphore` per frame. Wgpu's
Vulkan import accepts external semaphores via
`VK_KHR_external_semaphore_fd`. **No extra fence work is required —
this is the standards-correct path out of the box.** This is the
deepest reason WPE on Linux is the strongest of the three GPU-sync
stories; the others are catching up to it.

### Linux WebKitGTK — wl_buffer release (implicit)

wlroots screencopy hands back a wl_buffer; the protocol's
`zwlr_screencopy_frame_v1::ready` event implies the buffer's contents
are valid at receive time. No explicit semaphore. Acceptable for the
fallback path; consumers who need stricter ordering should use the
WPE producer.

---

## Roadmap to parity

Three releases get scrying to **the parity baseline** across all three
platforms. Naming aspirational, not committed.

### 0.3.0 — input completeness

Per-platform: keyboard forwarding (with IME baseline), touch + pen,
drag-and-drop, cursor-change events. Rounds out the embedding-input
surface so a consumer can build a productivity-grade UI on scrying
without going around it for input.

### 0.4.0 — environment + control

Per-platform: settings (zoom, UA, JS, devtools), reload/back/forward/
stop, profile + cookies, custom URL schemes, downloads, new-window
interception, process-failure recovery. Turns scrying into a complete
"we replace wry on every platform" deliverable.

### 0.5.0 — robustness + parity QA

Per-platform: explicit GPU sync (D3D12 fence on Windows;
MTLSharedEvent if needed on macOS; verify VkSemaphore wiring on
Linux WPE), throttling control for hidden composition WebViews on
Windows, DPI awareness across monitor moves, runtime distribution
strategies (WebView2 fixed-version on Windows, WPE packaging on
Linux). Cross-platform QA pass: every parity-baseline row green on
all three.

### Out-of-band slices

These don't fit on the version-by-version curve and can ship
independently when the work is ready:

- **macOS producer scaffold**: ✅ landed in 0.4.0. Slices A–N
  (lifecycle / SCK pipeline / nav parity / mouse / JS messaging /
  CPU snapshots / scroll wheel / title-changed / keyboard + IME /
  cursor changes / drag-doc / per-profile data stores /
  `MTLSharedEvent` scaffold / resize-applies-to-stream) bring the
  macOS WKWebView producer past Windows-0.2.0 parity. Browser-class
  items 1–9 (history controls / new-window intercept / settings /
  custom URL schemes / process-failure recovery / auth /
  multi-instance verification / downloads / find + PDF) bring it to
  a usable shape for browser-shape consumers like mere. Runtime
  verification via [`demo-mac`](../demo-mac/) covers slices A–N
  (12 of 13; drag is structurally SPI-blocked) plus six dedicated
  `--*-test` modes that exercise items 1, 3, 4, 7, 8, 9 +
  follow-ups (incognito, interactionState round-trip, pointer
  input, downloads). The suite runs headless via
  `bash scripts/test-mac.sh` and on every push via
  `.github/workflows/test-mac.yml`.

---

## Browser-class consumer roadmap

Beyond the parity baseline, an embeddable WebView library has to
support a browser-shape consumer (e.g. [`mark-ik/mere`](https://github.com/mark-ik/mere)):
multiple tabs per process, full navigation control, customizable
chrome, robust lifecycle hooks. Items 1–9 below landed in 0.4.x;
each has a brief notes column describing the API shape and known
limitations.

| # | Slice | Status | Public surface | macOS impl notes |
| --- | --- | --- | --- | --- |
| 1 | History controls | ✅ | `reload` / `stop` / `go_back` / `go_forward` / `can_go_back` / `can_go_forward` (trait) | direct `WKWebView::reload` / `stopLoading` / `goBack` / `goForward`; `go_back/forward` return `Ok(false)` if `canGoBack/Forward` is false |
| 2 | New-window intercept | ✅ | `NavigationEvent::NewWindowRequested { url }` | `WKUIDelegate::webView:createWebViewWithConfiguration:...` returns null to suppress the engine popup; host opens its own tab in response |
| 3 | Settings application | ✅ | `apply_settings(&WebSurfaceSettings)` (trait) | `pageZoom`, `customUserAgent`, `setInspectable` (macOS 13.3+), `WKPreferences::setJavaScriptEnabled`. Context-menu / accelerator-key fields silently ignored |
| 4 | Custom URL schemes | ✅ | `UrlSchemeHandlerFn`, `UrlSchemeResponse`, `WkWebViewProducer::new_with_url_schemes` | `WKURLSchemeHandler` delegate per registered scheme; serves bytes synchronously inside `webView:startURLSchemeTask:` |
| 5 | Process-failure recovery | ✅ | `NavigationEvent::ContentProcessTerminated` | `WKNavigationDelegate::webViewWebContentProcessDidTerminate:` |
| 6 | Auth challenges | ✅ (option A) | `NavigationEvent::AuthChallenged { url, host, auth_method }` | `webView:didReceiveAuthenticationChallenge:` defaults to `PerformDefaultHandling`; option B (host-driven disposition) deferred until mere has auth UI |
| 7 | Multi-instance verification | ✅ | n/a — architectural | `demo-mac --two-tabs` validates two producers in one process / one window cleanly drain independent event streams |
| 8 | Downloads | ✅ | `DownloadId`, `DownloadDestinationRequest`, `DownloadDecision`, `set_download_handler` / `clear_download_handler`, `cancel_download(id)`, `NavigationEvent::DownloadStarted` / `DownloadProgress` / `DownloadFinished` / `DownloadCancelled`, `WkWebViewProducerConfig::download_dir` | Per-download `DownloadId` correlates lifecycle events; `decidePolicyForNavigationResponse:` promotes non-displayable HTTP responses to downloads; per-download throttle (100ms / 1MiB) on progress; `WKDownload::cancel(_:)` for host-driven cancel; host destination handler runs synchronously inside `decideDestination` |
| 9 | Find / PDF | ✅ | `find_in_page` + `poll_find_match`, `request_pdf` + `poll_pdf`, `FindOptions` | both async-only via completion blocks; mirrors the snapshot pattern |

**Recently shipped (post-9):**

- ✅ Auth option B — `WkWebViewProducer::set_auth_handler` /
  `clear_auth_handler` registers a `Fn(AuthChallenge) -> AuthDisposition`
  closure invoked synchronously inside the navigation delegate.
  Translates to `NSURLSessionAuthChallengeDisposition` with
  optional `NSURLCredential` (HTTP basic via
  `AuthDisposition::UseCredential { username, password }`).
- ✅ Cookie store API — `request_all_cookies` / `poll_cookies`
  (async fetch), `set_cookie(&Cookie)` / `delete_cookie(name,
  domain, path)` (fire-and-forget). Wraps the
  `WKHTTPCookieStore` on the producer's `WKWebsiteDataStore`.
  Public `Cookie` struct mirrors `NSHTTPCookie`'s essential
  fields.
- ✅ Permission handlers — `set_permission_handler` /
  `clear_permission_handler` registers a
  `Fn(PermissionRequest) -> PermissionDecision` closure invoked
  for camera / microphone / device-orientation requests.
  No-handler default is `Prompt` (system UI).
- ✅ Incognito / non-persistent profile —
  `WkWebViewProducerConfig::non_persistent` (or the
  `.non_persistent()` builder) wires
  `WKWebsiteDataStore::nonPersistentDataStore`. Cookie / local
  storage / IndexedDB live only for the producer's lifetime.
  Verified by `demo-mac --incognito-test`.
- ✅ Tab-state serialization —
  `WkWebViewProducer::serialize_interaction_state` /
  `restore_interaction_state` round-trip the WKWebView's
  `interactionState` (back-forward list, scroll position, form
  data) as opaque bytes. Verified by
  `demo-mac --interaction-state-test`.
- ✅ Touch / pen / pointer input —
  `WryWebSurfaceProducer::send_pointer_input` synthesizes
  `NSEvent`s through the same path as `send_mouse_input`. WebKit's
  pointer-events JS API observes them as
  `pointerType: "mouse"` (macOS has no public direct-touch
  synthesis API). Verified by
  `demo-mac --pointer-input-test`.
- ✅ DPI awareness across monitor moves — `NSWindowDidChangeBackingPropertiesNotification`
  observer registered in `WkWebViewProducer::new_with_url_schemes`,
  re-applies `config.size` on the next `try_acquire_frame` /
  `resize` so points/pixels stay coherent. Cleaned up explicitly
  in `Drop`.
- ✅ Downloads (item 8 expansion) — see the row in the table
  above. `DownloadId` correlates events; throttled progress;
  host-driven destination and cancellation. Verified by
  `demo-mac --download-test`.
- ✅ Producer module split — the previous single-file
  ~4000-LOC `wkwebview_producer.rs` is now 18 submodules each
  under a 600-LOC ceiling (`producer.rs`, `capture/{mod,
  blocking, async_start}.rs`, `api.rs`, `trait_impl.rs`,
  per-delegate `*_handler.rs`, `helpers.rs`, etc.).
- ✅ Auth-challenge `protectionSpace` panic fix — bypass
  objc2's debug-build `class_getInstanceMethod` check
  (which doesn't see through `WKNSURLAuthenticationChallenge`'s
  forwarding-proxy class) by reading the property via
  `objc_msgSend` directly, gated on `respondsToSelector:` so
  defensive failure modes still surface as empty fields rather
  than a panic.
- ✅ Headless test runs — `demo-mac --*-test` modes default to
  hidden window + `NSApplicationActivationPolicyProhibited`, so
  `bash scripts/test-mac.sh` runs silently in the background.
  `--visible` overrides for debugging.
- ✅ CI — `.github/workflows/test-mac.yml` runs the suite on
  `macos-latest` (currently Apple Silicon).
- ✅ Cursor handler callback parity — `set_cursor_handler` /
  `clear_cursor_handler` registers a
  `Fn(CursorShape) + Send + Sync` closure invoked synchronously
  on every `NSCursor.currentSystemCursor` change observed after a
  forwarded input event. Coexists with the pull-model
  `poll_cursor_shape` queue. Verified by `demo-mac --scripted`.
- ✅ Download auth handler — the existing `AuthHandlerFn`
  registered via `set_auth_handler` now also routes through
  `WKDownloadDelegate::download:didReceiveAuthenticationChallenge:`.
  One handler covers both page-level and download-level auth
  challenges; hosts that need download-specific behavior can
  branch on the URL inside the handler. Verified by phase D of
  `demo-mac --download-test`.
- ✅ Programmatic download initiation — `start_download(url)`
  wraps `WKWebView::startDownloadUsingRequest:` so hosts can
  begin downloads without navigating the WKWebView. Auth
  challenges flow through the *download-level* delegate
  callback (rather than the page-level one), exercising that
  code path that promotion-driven downloads can't.
- ✅ Download cancel / resume — `cancel_download(id)` (which
  passes a non-nil completion block to Apple's `cancel:` to
  un-suppress `didFailWithError`), captures the resume_data
  WebKit emits on a `DownloadCancelled` event;
  `resume_download(&[u8], PathBuf)` calls
  `resumeDownloadFromResumeData:` with a fresh delegate
  registration so resumed transfers fire the normal lifecycle
  events. Verified by phase E of `demo-mac --download-test`.
- ✅ SCK pipeline assertion test —
  `demo-mac --capture-test` is an opt-in smoke test for the
  ScreenCaptureKit path. Not in `scripts/test-mac.sh` because
  Screen Recording permission can't be self-granted; CI runners
  need a `tccutil` pre-grant. Surfaced a producer-side fix
  along the way: `try_acquire_frame` now treats status-only
  `CMSampleBuffer`s (every `SCFrameStatus` except `Complete`)
  as "no frame ready" rather than a fatal error.
- ✅ Capability-probe parity for macOS —
  `WryWebSurfaceCapabilities::probe` now mirrors the Windows
  shape: a Metal-backed host wgpu device gets
  `imported_texture: Supported`,
  `preferred_mode: ImportedTexture`, and
  `supported_frames: [MetalTextureRef]`. Hosts that drive
  fallback selection from `probe` (rather than constructing a
  producer to read its runtime capabilities) now discover the
  SCK / Metal path on macOS.
- ✅ Offset units standardized — `WkWebViewProducerConfig::offset`
  is now physical pixels (matches the trait's `set_offset`
  contract and `config.size`'s units). Pre-fix `with_offset(200, …)`
  and `set_offset(200, …)` landed at different positions on
  Retina; both now resolve to the same point.
- ✅ Final `DownloadProgress` carries the response's announced
  `Content-Length` — `DownloadEntry` captures `total_bytes_expected`
  at `decideDestination` time rather than reusing
  `last_progress_bytes`, so throttled downloads no longer emit
  `bytes_written > total_bytes_expected` on the final tick.
- ✅ Scroll-wheel events carry location + modifier flags —
  `synthesize_scroll_wheel_event` now sets the CGEvent's
  location (webview rect → screen-global, top-left origin) and
  `CGEventFlags` (Shift / Control) before the round-trip
  through `NSEvent::eventWithCGEvent:`. WebKit attributes the
  scroll to the right WKWebView and honors cmd-scroll /
  shift-scroll modifier behavior.
- ✅ `is_http_only` round-trip through `set_cookie` —
  `NSHTTPCookie::cookieWithProperties:` doesn't expose HttpOnly
  as a settable property (Apple's documented set of property
  keys excludes it). `set_cookie` now routes HttpOnly cookies
  through `cookiesWithResponseHeaderFields:forURL:` with a
  synthesized `Set-Cookie` header; non-HttpOnly cookies keep the
  faster property-dict path. Verified by the `is_http_only=true`
  assertion in `--incognito-test`.
- ✅ Multi-instance + persistent-store coverage —
  `--profile-test` is now a one-process self-assertion (two
  persistent producers at the same `data_dir`, set on #1 → see
  on #2; complement to `--incognito-test`'s isolation
  assertion); `--two-tabs` asserts each producer's nav events
  stay in its own queue (no cross-talk). Both run in the
  headless suite — `scripts/test-mac.sh` is now 8 modes / 8 PASS.
- ✅ SCK source-rect crop via per-frame Metal blit —
  `initWithDesktopIndependentWindow:` captures the entire host
  window (Apple ignores `sourceRect` for single-window filters);
  the producer now captures at full window resolution and
  blit-crops to the WKWebView's pixel rect inside
  `try_acquire_frame` before handing the texture out.
  `CaptureState` carries an `MTLCommandQueue` allocated on the
  host's wgpu Metal device; per-frame cost is ~1 ms on Apple
  silicon. The imported texture's dimensions match the
  webview's pixel rect — no host chrome, no recursive capture
  even when the consumer composites the texture back into the
  same window. The crop's Y origin lives in window-frame
  top-left coords (chrome height is added on top of the
  content-view-relative Y flip) so the title-bar region of the
  captured texture is excluded; `try_acquire_frame` rejects
  in-flight pre-resize samples whose IOSurface dimensions
  differ from the current host-window pixel size, so SCK's
  push-model "deliver one or two more samples after
  `updateConfiguration:` then go quiet" doesn't leave a
  stale-stretched frame on screen across a resize.
- ✅ Capture-pipeline cadence probe —
  `WkWebViewProducer::capture_metrics()` returns a
  [`CaptureMetrics`](../scrying/src/wkwebview_producer/capture/mod.rs)
  snapshot with `samples_received` (incremented from the SCK
  background dispatch queue on every `Screen`-typed sample) and
  `samples_consumed` (incremented in `try_acquire_frame` on every
  `Ok(Some(...))`). The demo-mac `--capture` mode prints rates
  once per second; the deltas confirm SCK keeps up with display
  refresh on Apple Silicon (~58 push/s, ~58 consume/s) so the
  perceived "a bit of lag" is pipeline depth (WebKit render →
  AppKit composite → SCK encode → demo blit → wgpu present, ~3
  vsyncs ≈ 50 ms at 60 Hz), not a backlog. Going lower than that
  needs an architecture change like consumer-side crop (skip the
  per-frame Metal blit pass).

- ✅ Context-menu interception via JS user-script —
  `WKUIDelegate` exposes
  `webView:contextMenuConfigurationForElement:completionHandler:`
  on iOS only; macOS has no public-API context-menu hook.
  Rather than reach for `_WK*` SPI, the producer now injects a
  capture-phase `contextmenu` user-script via
  `WKUserContentController` that walks the click target's
  ancestor chain to recover the closest enclosing `<a href>` /
  `<img src>`, calls `event.preventDefault()` to suppress
  WebKit's default `NSMenu`, and posts a NUL-delimited 5-field
  payload to a dedicated `WKScriptMessageHandler`
  (`scryingContextMenu`). The handler parses the payload and
  pushes a [`crate::NavigationEvent::ContextMenuRequested`]
  event with `page_url` / `x` / `y` (CSS pixels relative to
  the WebView viewport) / `link_url` / `image_url`. Verified in
  `--two-tabs --visible` that each producer's right-clicks
  route to its own nav-event queue (no cross-talk).

**Outstanding for follow-up slices.** This list is mirrored as a
flat cross-platform checklist in
[`2026-05-09_browser_parity_checklist.md`](2026-05-09_browser_parity_checklist.md);
the entries below keep the macOS-specific impl notes the
checklist deliberately omits.

- Authentication during downloads — wired through the shared
  page handler, but the download-only-specific challenge path
  (e.g. mid-download, post-promotion auth) only fires for
  programmatic `start_download` flows in practice. Real
  Apple-internal scenarios are rare; current shape is enough
  for browser-class consumers.
- Throttling control — suspending / resuming page activity for
  hidden tabs needs SPI (`_setSuspended:`) and is risky. The
  lighter alternative — Page Visibility sync, see below — is
  the public-API path and probably sufficient for most
  consumers.
- ✅ Page Visibility / occlusion sync —
  `WryWebSurfaceProducer::set_visible(bool)` cascades through
  `NSView::setHidden:`. WebKit observes the
  `viewDidHide` / `viewDidUnhide` chain and pushes
  `visibilitychange` events page-side so `document.hidden` /
  `document.visibilityState` flip. RAF callbacks throttle to
  ~1 Hz, background-tab autoplay / video-decoding throttles per
  the engine's policy, `setInterval` callbacks may coalesce. No
  `_WK*` SPI involved. Distinct from the heavier
  `_setSuspended:` SPI-only path (which fully pauses execution
  and stays out of scope).
- Drag-and-drop in — file / URL drops *onto* the page work
  through the public `NSDraggingDestination` protocol on the
  parent NSView; route a synthesized drag event to WebKit via
  `performDragOperation:`. (Drag-and-drop *out* — initiating
  a drag *from* page content — remains `_WK*` SPI on macOS and
  is staying punted.)
- Print / `Cmd+P` — page-to-PDF rendering shipped in item 9,
  but interactive `NSPrintOperation` (page-range UI, printer
  selection, preview) hasn't. `WKWebView::printOperationWithPrintInfo:`
  is the entry point.
- Content blocking — `WKContentRuleList` accepts JSON rule
  lists (AdBlock-shape: trigger / action pairs). Compile at
  startup via `WKContentRuleListStore::compileContentRuleList`;
  attach to the configuration's `WKUserContentController`. No
  SPI; works on macOS 10.13+.
- Cookie / storage *observation* — `WKHTTPCookieStoreObserver`
  protocol surfaces "cookies changed" callbacks that the
  current cookie API doesn't expose. Useful for browser chrome
  that wants to react to auth-flow cookie writes without
  polling.
- Color management / HDR — capture path is locked at
  `BGRA8Unorm` sRGB. Wide-gamut content (Display P3) and HDR
  video frames are tone-mapped to sRGB before SCK delivers
  them. Future: configurable
  `SCStreamConfiguration::colorSpaceName` /
  `SCStreamConfiguration::pixelFormat` selection; consumer-
  side gamma handling.
- DevTools / Web Inspector remote attach — `setInspectable(true)`
  is wired for macOS 13.3+, but the Safari → Develop menu →
  attach flow isn't documented anywhere; downstream consumers
  hit it cold. Doc-only slice.
- Autofill / Keychain — credential save / suggest plumbing.
  Mostly system-driven via `NSSecureTextField` autofill and
  Safari Keychain on macOS, but the host typically wants
  opt-in (per-profile), and there's no public-API hook.
  Probably ships as "configurable: yes / no" with no finer
  control.
- Spellcheck / autocorrect controls — page-side
  `WKPreferences` toggles, plus AppKit's Look-Up / Services
  menu integration on text selections. Mostly free on macOS;
  just needs API surface.
- WebRTC capture lifecycle observability — once permission is
  granted, the page can `getUserMedia` repeatedly without the
  host knowing. WebKit fires no public-API "camera in use"
  callback. Browser chrome wants this for the red-dot
  indicator; would need to rely on the permission-grant event
  paired with a JS user-script that polls
  `navigator.mediaDevices`.
- Pre-composition extraction — capture the WebView's
  `CALayer.contents` directly via `CARenderer` / `IOSurface`,
  bypassing the WindowServer composite. Would also kill the
  "SCK goes quiet on a static page" cadence dependency that
  motivated the dim-match guard. Unclear whether reachable
  without `_WK*` SPI; spike needed.
- Sub-iframe / sub-frame capture — per-iframe textures rather
  than a single composited window grab. WebKit's
  `WKWebView` is a single composition root; per-frame access
  is `_WK*` SPI today. Likely a long-term Linux-WPE-first
  story (WPE exposes per-view buffers natively).

**Reference implementations.** [`tauri-apps/wry`](https://github.com/tauri-apps/wry)
is a useful reference even though scrying doesn't depend on it. Its
[`wkwebview/`](https://github.com/tauri-apps/wry/tree/dev/src/wkwebview)
producer has solved many of the same Cocoa / objc2 lifetime and
threading footguns ahead of us — when adding a follow-up slice
above, skim wry's matching subsystem first; it'll often have a
ready-made answer to "does this delegate method retain its
arguments?" or "is `_setX:` actually reachable via objc2-web-kit?"

---

- **Linux WPE producer scaffold**: WPE + DMABUF + VkSemaphore
  pipeline alone.
- **Linux WebKitGTK fallback**: probably ships only if a downstream
  consumer needs it; otherwise WPE-only is the cleaner story.

---

## Native-frame import: in-tree as `scrying::native_frame`

Scrying owns its native-frame import path in-tree as the
[`scrying::native_frame`](../scrying/src/native_frame/) module. It is
**not** a dependency on a sibling crate. This was a deliberate split
from the original plan to share `wgpu-native-texture-interop` with
[`wgpu-graft`](https://github.com/mark-ik/wgpu-graft).

**Why in-tree**: the two projects' producers have different shapes.
wgpu-graft consumes Servo via surfman GL framebuffer surfaces and
bridges them to wgpu. scrying consumes platform-native texture
handles directly (D3D12 NT-handle today; IOSurface and DMABUF when
the macOS / Linux producers land). Sharing one crate would force two
genuinely different problems through the same abstraction.

The module is structurally derived from the per-platform shape in
Slint's [Servo embedding example](https://github.com/slint-ui/slint/tree/master/examples/servo),
adapted to take native handles directly (no GL bridge) and folded
together with scrying's explicit-fence wiring. See [NOTICE](../NOTICE)
for the upstream attribution.

What lives in `native_frame/` today:

- `mod.rs` — `HostWgpuContext`, `NativeFrame`, `Dx12SharedTexture`,
  `WgpuTextureImporter` + the import dispatch. Drops the GL FBO
  source variants from wgpu-graft's interop crate; only emits
  variants for which scrying has a working producer.
- `error.rs` — `InteropError`, `UnsupportedReason`.
- `sync.rs` — `SyncMechanism` (`None` / `ExplicitExternalSemaphore` /
  `ExplicitFence`), `InteropSynchronizer` trait,
  `NoopSynchronizer`, `ImplicitOnlySynchronizer`.
- `sync_dx12.rs` — `Dx12FenceSynchronizer` (Windows).

What lands here as the macOS / Linux producers come online:

- `IoSurfaceTexture` variant on `NativeFrame` + `import_io_surface_texture`
  (macOS). Optional `MetalSharedEventSynchronizer` if implicit
  IOSurface coherence ever fails.
- `DmaBufImage` variant on `NativeFrame` + `import_dma_buf_image`
  (Linux WPE). `VulkanSemaphoreSynchronizer` for the per-frame
  `VkSemaphore` the WPE DMABUF protocol carries.

These are scrying-internal additions. No cross-crate coordination
required.

---

## Open questions

- **Linux producer naming**: rename `webkitgtk_producer` →
  `wpe_producer` once the WPE backend is the load-bearing one, and
  introduce `webkitgtk_producer` as a separate fallback module? Or
  keep both modules and let `WryWebSurfaceCapabilities::probe`
  pick at runtime?
- **Windows runtime distribution**: do we document the WebView2
  Evergreen runtime requirement, or also support fixed-version
  bundling? Affects producer construction (different
  `CoreWebView2EnvironmentOptions`).
- **Macro-level: does scrying eventually subsume the demo's wry
  probe?** The demo currently keeps a wry HWND-WebView for sanity
  checking. Once scrying covers input + lifecycle on every platform,
  the wry path inside the demo is just legacy ballast.
