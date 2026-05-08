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
  `config.offset` (points) and `config.size` (physical pixels →
  points via the parent window's `backingScaleFactor`), wires a
  navigation delegate, and adds the WebView as a subview.
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

**Windows-0.2.0 parity status:** ✅ achieved by slices A–H. The
remaining minor difference is X-button `buttonNumber` distinction
(currently X1/X2 arrive as Other-mouse with the default button
index); could be added by routing X-button cases through CGEvent in
the same way slice G handled scroll wheel.

**Outstanding for follow-up slices:** keyboard + IME forwarding
(`keyDown:`, `NSTextInputClient`); cursor-change reporting; drag-and-
drop forwarding; explicit `MTLSharedEvent` cross-queue sync (precaution
— implicit IOSurface coherence is sufficient on Apple silicon today);
per-profile `WKWebsiteDataStore` wiring; auto-applying
`SCStreamConfiguration` resize on `resize`.

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
| Keyboard forwarding (basic) | ? | ? | ? | ? |
| IME (CJK / non-Latin) | ? | ? | ? | ? |
| Drag-and-drop into webview | ? | ? | ? | ? |
| Focus management | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Cursor-change reporting | ? | ? | ? | ? |
| Navigation events (start/source/complete) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| Title-changed event | ✅ 0.2.0 | ✅ 0.4.0 (KVO) | ? | ? |
| JS messaging (bidirectional) | ✅ 0.2.0 | ✅ 0.4.0 | ? | ? |
| PNG / CPU snapshot | ✅ 0.2.0 | ✅ 0.4.0 (CPU RGBA) | ? (`get_snapshot`) | ? |
| Settings (zoom, UA, JS, devtools) | ? | ? | ? | ? |
| Profile / cookies / storage | ? | ? | ? | ? |
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

- **macOS producer scaffold**: ✅ landed in 0.4.0. Slices A–H
  (lifecycle / SCK pipeline / nav parity / mouse / JS messaging /
  CPU snapshots / scroll wheel / title-changed via KVO) bring the
  macOS WKWebView producer to full Windows-0.2.0 parity. Follow-up
  slices for keyboard + IME, cursor changes, drag-and-drop, and
  per-profile data stores are documented in the macOS section above.
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
