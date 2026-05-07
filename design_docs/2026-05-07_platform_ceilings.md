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

**Current scrying state (0.2.0):** [`wkwebview_producer`](../scrying/src/wkwebview_producer.rs)
is a planning skeleton. Capabilities advertise `OverlayOnly` +
`CpuSnapshot`. No WKWebView hosting, no SCK binding, no Metal handoff.
All Tier-1 trait methods return `Unsupported`.

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
| Imported GPU texture per frame | ✅ 0.1.0 | ? | ? | ? (degraded) |
| Resize / offset | ✅ | ? | ? | ? |
| Navigate (URL + HTML) | ✅ 0.2.0 | ? | ? | ? |
| Reload / Stop / Back / Forward | ? | ? | ? | ? |
| Mouse + scroll forwarding | ✅ 0.2.0 | ? | ? | ? |
| Touch + pen forwarding | ? | ? | ? | ? |
| Keyboard forwarding (basic) | ? | ? | ? | ? |
| IME (CJK / non-Latin) | ? | ? | ? | ? |
| Drag-and-drop into webview | ? | ? | ? | ? |
| Focus management | ✅ 0.2.0 | ? | ? | ? |
| Cursor-change reporting | ? | ? | ? | ? |
| Navigation events (start/source/complete/title) | ✅ 0.2.0 | ? | ? | ? |
| JS messaging (bidirectional) | ✅ 0.2.0 | ? | ? | ? |
| PNG snapshot | ✅ 0.2.0 | ? (`takeSnapshot`) | ? (`get_snapshot`) | ? |
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

- **macOS producer scaffold**: WKWebView + SCK + IOSurface + Metal
  pipeline alone, before driving the input surface.
- **Linux WPE producer scaffold**: WPE + DMABUF + VkSemaphore
  pipeline alone.
- **Linux WebKitGTK fallback**: probably ships only if a downstream
  consumer needs it; otherwise WPE-only is the cleaner story.

---

## Dependency: changes required in `wgpu-native-texture-interop`

The parity roadmap can't be delivered entirely from inside scrying.
Three concrete pieces of work fall in the sibling crate
[`wgpu-native-texture-interop`](https://crates.io/crates/wgpu-native-texture-interop)
(currently 0.1.1).

### 1. Built-in synchronizers for explicit GPU sync

`SyncMechanism` already has `ExplicitFence` and
`ExplicitExternalSemaphore` variants. The `InteropSynchronizer` trait
is in place. But only `NoopSynchronizer` and `ImplicitOnlySynchronizer`
ship as built-ins, and both reject the explicit variants.

The roadmap needs:

- `Dx12FenceSynchronizer` — opens a `D3D12_FENCE_FLAG_SHARED` shared
  handle on the wgpu D3D12 device, queues
  `ID3D12CommandQueue::Wait(fence, value)` before the consumer's
  submit. This is the **fence work** for Windows.
- `VulkanSemaphoreSynchronizer` — accepts a per-frame `VkSemaphore`
  fd from the producer (matches the WPE DMABUF protocol's per-frame
  semaphore), waits on the wgpu Vulkan queue.
- `MetalSharedEventSynchronizer` — precautionary; not required for
  correctness today (IOSurface coherence is implicit).

### 2. Complete the `VulkanExternalImage` import path

`NativeFrame::VulkanExternalImage` exists for API symmetry but the
import dispatch in `WgpuTextureImporter::import_frame` currently
returns `InteropError::Unsupported`. For the Linux WPE producer this
needs to be a real path: DMABUF fd → `VkImage` via
`VK_KHR_external_memory_fd` (with DRM modifier handling) →
`wgpu::Texture` via the Vulkan hal. The structural code in
`raw_gl/linux.rs` for the GL→Vulkan import is ~80% of what's needed;
the new path skips the GL framebuffer source and imports the DMABUF
directly.

### 3. Possibly: pre-submit hook on `InteropSynchronizer`

The current trait hooks fire at *import time* (`producer_complete`
post-acquire, `consumer_ready` post-import). A fence wait wants to
enqueue `Wait(fence, value)` *immediately before the consumer's
render submit*. Putting that in `producer_complete` works for the
common one-frame-per-submit pattern (the Wait persists in the queue
until drained), but is fragile for multi-import-per-submit. A
`pre_submit(&queue)` hook would be the cleaner shape. Worth deciding
before implementing the synchronizers.

### What does *not* need interop-crate changes

- `MetalTextureRef` import is already wired — macOS producer can
  hand it MTLTextures from `CVMetalTextureCache` directly.
- `Dx12SharedTexture` import is already wired — the Windows
  producer's frame handoff doesn't change for the fence upgrade,
  only the synchronizer plumbing does.
- Every scrying-side embedding-API expansion (input forwarding,
  settings, profile, custom URL schemes, navigation control,
  drag-and-drop, IME) lives entirely in scrying.

### Versioning impact

Synchronizer additions are additive (new public types). Completing
`VulkanExternalImage` is additive (replaces an `Unsupported` arm
with real behaviour). Adding a `pre_submit` trait method is technically
breaking — the surface together is sized as a `0.2.0` bump of
`wgpu-native-texture-interop`. scrying then updates its dep to
`version = "0.2"` and gains the new synchronizers; the producer-side
code in scrying can ship the fence wiring at the same release where
it picks up `wgpu-native-texture-interop 0.2`.

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
