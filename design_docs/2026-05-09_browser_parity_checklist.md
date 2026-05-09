# Browser-class parity checklist

Companion to [`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md).
That doc covers per-platform capability ceilings, GPU sync upgrade
paths, and the macOS producer's slice-by-slice landing notes with
implementation specifics. **This** doc is the flat, scannable
checklist of everything an embeddable system-WebView library needs
to be a "real" browser-class building block — something a tabbed
browser shell (e.g. [`mark-ik/mere`](https://github.com/mark-ik/mere))
can be built on top of without going around it for any meaningful
capability.

The split exists because the platform doc was getting hard to
skim at a glance — every line of "what's left to ship" was
embedded in a paragraph of macOS impl notes. A reader who just
wants "are we done yet?" should be able to read this file in
under a minute.

Status values, applied per row:

- ✅ shipped on macOS (0.4.x)
- ⏳ outstanding — public-API path exists, just hasn't been wired
- 🚫 blocked by platform SPI — would need `_WK*` private API or
  similar; punted unless a downstream consumer makes the case
- 📐 design-only — the capability's surface is a documentation
  question rather than a code question (e.g. "how does a host
  attach DevTools")
- n/a — capability doesn't apply at this layer (browser-shell
  consumers handle it themselves)

Cross-platform: every row also notes whether the slice would need
matching work on Windows (WebView2 + WGC) and Linux (WPE primary,
WebKitGTK fallback). "?" means we haven't audited the equivalent
yet.

---

## Navigation & lifecycle

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| History controls (reload / stop / back / forward) | ✅ | ✅ | ? | Trait methods on `WryWebSurfaceProducer` |
| New-window / popup intercept | ✅ | ✅ | ? | `NavigationEvent::NewWindowRequested { url }` |
| Process-failure recovery | ✅ | ✅ | ? | `NavigationEvent::ContentProcessTerminated`; reload to recover |
| Tab-state serialize / restore | ✅ | ? | ? | `serialize_interaction_state` / `restore_interaction_state` (opaque bytes) |
| Page Visibility / occlusion sync | ✅ | ⏳ | ⏳ | `set_visible(bool)` cascades through `NSView::setHidden:`; WebKit pushes `visibilitychange` so timers / RAF / autoplay throttle. Distinct from the SPI-only `_setSuspended:` heavy pause |
| Throttling control (hard pause) | 🚫 | ? | ⏳ | macOS needs `_setSuspended:` SPI; Page Visibility sync (above) is the public-API alternative |

## Input forwarding

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Mouse + scroll wheel (with modifiers) | ✅ | ✅ | ? | Scroll wheel carries location + `CGEventFlags` |
| Pointer / touch / pen synthesis | ✅ | ? | ? | macOS has no public direct-touch synthesis API — events arrive at JS as `pointerType: "mouse"` |
| Keyboard + IME (composition) | ✅ | ✅ | ? | Korean / Japanese / Chinese composition events round-trip |
| Cursor-change events (host mirrors WebKit cursor) | ✅ | ? | ? | Polled via `NSCursor.currentSystemCursor` after each forwarded event |
| Drag-and-drop *in* (file / URL → page) | ✅ | ⏳ | ⏳ | Capture-phase `drop` user-script reports external-content drops via `NavigationEvent::DropDetected { x, y, file_count, primary_url }`. Observability only — does not call `preventDefault`, so WebKit's default drop behavior (file → navigate, drop on `<input type=file>`) still runs |
| Drag-and-drop *out* (page content → host) | 🚫 | ? | ⏳ | macOS path is `_WK*` SPI |
| Context-menu interception | ✅ | ? | ? | JS user-script + `WKScriptMessageHandler`; `NavigationEvent::ContextMenuRequested` |

## Capture & rendering

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Overlay vs imported-texture mode | ✅ | ✅ | ? | Capability-driven via `WebSurfaceMode` |
| Source-rect crop to webview region | ✅ | ✅ | ? | macOS via per-frame Metal blit; Apple ignores `sourceRect` for single-window filters |
| Chrome (title-bar) offset honored | ✅ | n/a | n/a | Window-frame top-left coords; macOS-specific because SCK captures full window |
| Resize correctness (dim-match guard) | ✅ | ? | ? | Stale pre-resize SCK samples rejected by IOSurface-dim check |
| DPI awareness across monitor moves | ✅ | ? | ? | Backing-scale-change observer re-applies `config.size` |
| Capture cadence probe (`CaptureMetrics`) | ✅ | ⏳ | ⏳ | `samples_received` / `samples_consumed` atomic counters |
| Cross-API GPU sync (explicit fences) | ✅ | ⏳ | ✅ | Producer encodes `MTLCommandBuffer::encodeSignalEvent_value` after each per-frame blit; `MetalTextureRef::signal_value` carries the per-frame event value; `WkWebViewProducer::metal_shared_event` returns the `MTLSharedEvent` handle. Default `WgpuTextureImporter` accepts `ExplicitMetalEvent`; consumer-side wait insertion remains opt-in (IOSurface coherence covers correctness on Apple silicon) |
| Color management — Display P3 SDR | ✅ | ⏳ | ⏳ | `WkWebViewProducerConfig::color_pipeline = ColorPipeline::DisplayP3` (or `set_color_pipeline` live); SCK's `colorSpaceName` switches to `kCGColorSpaceDisplayP3`. Same 8-bit BGRA format as sRGB — only the gamut tag differs |
| Color management — HDR / 16-float | ✅ | ⏳ | ⏳ | `ColorPipeline::Hdr16f`: SCK config flips to `kCVPixelFormatType_64RGBAHalf` + `kCGColorSpaceExtendedLinearDisplayP3`; Metal source/dest become `RGBA16Float`; `MetalTextureRef::format = wgpu::TextureFormat::Rgba16Float`. Per-frame bandwidth ~doubles. Consumers must configure their wgpu surface for HDR (Rgba16Float + EDR alpha mode) to actually display HDR; SDR surfaces clamp >1.0 values to ~SDR-white |
| Pre-composition extraction | 🚫 | ? | ⏳ | macOS would need direct `CALayer.contents` access pre-WindowServer-composite; `_WK*` SPI risk; spike needed. Would also fix the "SCK quiets on a static page" issue |
| Sub-iframe / sub-frame capture | 🚫 | 🚫 | ⏳ | `WKWebView` is a single composition root; per-frame access is `_WK*`. Long-term WPE-first story (WPE exposes per-view buffers natively) |

## Settings / environment

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Settings application (zoom / UA / JS / inspectable) | ✅ | ✅ | ? | `apply_settings(&WebSurfaceSettings)` |
| Custom URL schemes | ✅ | ✅ | ? | `WkWebViewProducer::new_with_url_schemes` on macOS |
| Per-profile data store | ✅ | ✅ | ? | `WKWebsiteDataStore::dataStoreForIdentifier:` (macOS 14+) |
| Incognito / non-persistent profile | ✅ | ✅ | ? | `WkWebViewProducerConfig::non_persistent` |
| Multi-instance verification (cross-talk isolation) | ✅ | ✅ | ? | Two producers, one window, independent event queues |
| Cookie store (read / write / delete) | ✅ | ✅ | ? | Wraps `WKHTTPCookieStore` on the producer's data store |
| Cookie / storage *change events* | ✅ | ⏳ | ⏳ | `set_cookie_change_handler` registers a closure invoked on every `cookiesDidChangeInCookieStore:` callback (page-side `document.cookie` writes, `Set-Cookie` headers, host writes); pair with `request_all_cookies` / `poll_cookies` to read the new state |

## Browser-shape UX

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Find-in-page (with options) | ✅ | ? | ? | `find_in_page` + `poll_find_match`, async via completion blocks |
| Page-to-PDF rendering | ✅ | ? | ? | `request_pdf` + `poll_pdf` |
| Print / `Cmd+P` (interactive) | ✅ | ⏳ | ⏳ | `print()` runs the standard `NSPrintOperation` modally with `NSPrintInfo::sharedPrintInfo`; returns `true` on print, `false` on cancel |
| Auth challenges (events + host-driven disposition) | ✅ | ✅ | ? | Option A (events) + Option B (`set_auth_handler`) both shipped |
| Auth during downloads (mid-stream / post-promotion) | ⏳ | ? | ? | Edge case; current shape adequate for most consumers |
| Permission handlers (camera / mic / orientation) | ✅ | ✅ | ? | `set_permission_handler` returns `Allow` / `Deny` / `Prompt` |
| WebRTC capture lifecycle observability | ✅ | ⏳ | ⏳ | JS user-script monkey-patches `navigator.mediaDevices.getUserMedia`, tracks `track.ended`; emits `NavigationEvent::MediaCaptureStateChanged { audio_active_tracks, video_active_tracks }`. Counters reset per top-level navigation |
| Title-changed notifications | ✅ | ✅ | ? | KVO on `WKWebView::title` |
| Downloads pipeline (id-correlated, host destination, cancel, resume) | ✅ | ✅ | ? | `DownloadId`, `set_download_handler`, `cancel_download`, `resume_download` |
| Content blocking (`WKContentRuleList` / AdBlock-shape) | ✅ | ⏳ | ⏳ | `compile_and_apply_content_rule_list(id, json)` compiles via `WKContentRuleListStore::defaultStore` and attaches to the UCC on main-thread completion; `clear_all_content_rule_lists` detaches all |
| Spellcheck / autocorrect controls | ⏳ | ? | ? | `WKPreferences` toggles + AppKit Look-Up / Services menu integration |
| Autofill / Keychain integration | ⏳ | ⏳ | ⏳ | System-driven on macOS via `NSSecureTextField` + Safari Keychain; host typically wants per-profile opt-in |
| DevTools / Web Inspector remote attach | 📐 | ? | ? | `setInspectable(true)` wired (macOS 13.3+); the "Safari → Develop → attach" flow needs documentation, not code |

---

## Notes on coverage

- **macOS** is the most complete column because it's the primary
  platform pushed in 0.4.x. The remaining ⏳ rows are public-API
  paths that just haven't been prioritized yet; the 🚫 rows are
  the genuine ceiling.
- **Windows** parity is mostly inherited from the WebView2
  producer that landed alongside the macOS scaffolding; "?" rows
  are ones we haven't explicitly audited against the macOS
  surface.
- **Linux** rows are largely "?" because the WPE producer is
  unstarted (out-of-band slice; see roadmap in
  [`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md)).
  WPE structurally wins on a couple of capture rows (per-view
  Vulkan buffers, native VkSemaphore) but loses on the embedding-
  shell rows that require system integration.

## Adding a row

When the team picks up a new ⏳ slice and lands it:

1. Flip the status in the relevant table here.
2. Move the macOS impl detail (the *paragraph* describing the
   API surface, edge cases, and any threading notes) to the
   matching section of
   [`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md) —
   either inside the "Browser-class consumer roadmap" Recently
   Shipped list (if it's a browser-shape capability) or under
   the per-platform ceilings section (if it touches the capture
   or rendering pipeline).
3. Cross-link both directions so a reader landing on either
   doc finds the other.

Don't duplicate impl detail across the two files. The platform
doc is the single source of truth for "how it works on this
platform"; this doc is the index.
