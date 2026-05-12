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

Windows-specific target depth lives in
[`2026-05-11_windows_webview2_target.md`](2026-05-11_windows_webview2_target.md):
it audits the current WebView2 producer, states how good the integration can
be, and tracks the Windows implementation lane.

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
| History controls (reload / stop / back / forward) | ✅ | ✅ | ? | Trait methods on `WebSurfaceProducer` |
| New-window / popup intercept | ✅ | ✅ | ? | `NavigationEvent::NewWindowRequested { url }`; Windows covered by `demo-win --popup-test` |
| Process-failure recovery | ✅ | ✅ | ? | `NavigationEvent::ContentProcessTerminated`; Windows covered by `demo-win --process-test` |
| Tab-state serialize / restore | ✅ | ? | ? | `serialize_interaction_state` / `restore_interaction_state` (opaque bytes) |
| Page Visibility / occlusion sync | ✅ | ✅ | ⏳ | `set_visible(bool)` cascades through `NSView::setHidden:` on macOS and `ICoreWebView2Controller::SetIsVisible` on Windows. Windows page-observed state is covered by `demo-win --visibility-test`. Distinct from the SPI-only `_setSuspended:` / hard pause paths |
| Throttling control (hard pause) | ✅ | ? | ⏳ | `WebSurfaceSettings::inactive_scheduling_policy` (`Suspend` / `Throttle` / `None`) → `WKPreferences.inactiveSchedulingPolicy` on macOS 14+ / iOS 17+. Older OS versions: silent no-op. Composes with `set_visible(false)` for browser-shape inactive-tab handling. SPI alternative (`_suspendPage:` for older macOS) and the wider SPI evaluation live in [`2026-05-09_spi_evaluation.md`](2026-05-09_spi_evaluation.md) |

## Input forwarding

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Mouse + scroll wheel (with modifiers) | ✅ | ✅ | ? | Scroll wheel carries location + `CGEventFlags` |
| Pointer / touch / pen synthesis | ✅ | ✅ | ? | macOS has no public direct-touch synthesis API — events arrive at JS as `pointerType: "mouse"`; Windows uses WebView2 `SendPointerInput` |
| Keyboard + IME (composition) | ✅ | 🟡 | ? | Windows has `send_keyboard_input` plus `forward_keyboard_message` for raw `WM_KEY*` / `WM_CHAR` / `WM_DEADCHAR` / `WM_IME*`; bounded `demo-win --keyboard-test` still times out before DOM input, so host message-loop/focus routing remains blocked |
| Cursor-change events (host mirrors WebKit cursor) | ✅ | ✅ | ? | macOS polls `NSCursor.currentSystemCursor`; Windows listens to WebView2 `CursorChanged` |
| Drag-and-drop *in* (file / URL → page) | ✅ | ✅ | ⏳ | macOS has drop observability via JS; Windows has OLE `IDataObject` drag-enter / over / leave / drop helpers. Trait-level `send_drag_input` now reports that Windows requires the concrete OLE data-object path |
| Drag-and-drop *out* (page content → host) | 🚫 | ? | ⏳ | macOS path is `_WK*` SPI |
| Context-menu interception | ✅ | ⏳ | ? | JS user-script + `WKScriptMessageHandler` always fires `NavigationEvent::ContextMenuRequested` (observability); engine-default menu suppression is gated on `window.__scryingSuppressContextMenu` and respects `WebSurfaceSettings::default_context_menus_enabled` via `evaluateJavaScript:` |

## Capture & rendering

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Overlay vs imported-texture mode | ✅ | ✅ | ? | Capability-driven via `WebSurfaceMode` |
| Source-rect crop to webview region | ✅ | ✅ | ? | macOS via per-frame Metal blit; Apple ignores `sourceRect` for single-window filters |
| Chrome (title-bar) offset honored | ✅ | n/a | n/a | Window-frame top-left coords; macOS-specific because SCK captures full window |
| Resize correctness (dim-match guard) | ✅ | ✅ | ? | macOS rejects stale pre-resize SCK samples; Windows drops WGC frames whose `ContentSize` no longer matches the producer size after resize/restart churn |
| DPI awareness across monitor moves | ✅ | ? | ? | Backing-scale-change observer re-applies `config.size` |
| Capture cadence probe (`CaptureMetrics`) | ✅ | ✅ | ⏳ | macOS exposes `samples_received` / `samples_consumed`; Windows also reports `stale_frames_dropped` for resize/restart diagnostics |
| Cross-API GPU sync (explicit fences) | ✅ | ✅ | ✅ | macOS producer encodes `MTLCommandBuffer::encodeSignalEvent_value` and producer-side waits before handoff; Windows supports `D3D12_FENCE_FLAG_SHARED` when the host supplies a shared fence handle, with the older barrier path as fallback. Architectural follow-up on macOS: consumer-side `encodeWaitForEvent:value:` via the wgpu-hal Metal escape removes the CPU stall entirely (~1ms per acquire saved) |
| Color management — Display P3 SDR | ✅ | ⏳ | ⏳ | `WkWebViewProducerConfig::color_pipeline = ColorPipeline::DisplayP3` (or `set_color_pipeline` live); SCK's `colorSpaceName` switches to `kCGColorSpaceDisplayP3`. Same 8-bit BGRA format as sRGB — only the gamut tag differs |
| Color management — HDR / 16-float | ✅ | ⏳ | ⏳ | `ColorPipeline::Hdr16f`: SCK config flips to `kCVPixelFormatType_64RGBAHalf` + `kCGColorSpaceExtendedLinearDisplayP3`; Metal source/dest become `RGBA16Float`; `MetalTextureRef::format = wgpu::TextureFormat::Rgba16Float`. Per-frame bandwidth ~doubles. Consumers must configure their wgpu surface for HDR (Rgba16Float + EDR alpha mode) to actually display HDR; SDR surfaces clamp >1.0 values to ~SDR-white |
| Pre-composition extraction | 🚫 | 🚫 | ⏳ | macOS would need direct `CALayer.contents` access pre-WindowServer-composite; Windows would need Microsoft to expose pre-DComp WebView2 textures; WPE is the strategic path that can expose pre-composition buffers natively |
| Sub-iframe / sub-frame capture | 🚫 | 🚫 | ⏳ | `WKWebView` is a single composition root; per-frame access is `_WK*`. Long-term WPE-first story (WPE exposes per-view buffers natively) |

## Settings / environment

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Settings application (zoom / UA / JS / inspectable) | ✅ | ✅ | ? | `apply_settings(&WebSurfaceSettings)` |
| Custom URL schemes | ✅ | ✅ | ? | `WkWebViewProducer::new_with_url_schemes` on macOS; `WebView2CompositionProducer::register_virtual_host_handler` on Windows, covered by `demo-win --routing-test` |
| Per-profile data store | ✅ | ✅ | ? | `WKWebsiteDataStore::dataStoreForIdentifier:` (macOS 14+) |
| Incognito / non-persistent profile | ✅ | ✅ | ? | `WkWebViewProducerConfig::non_persistent` on macOS; `WebView2CompositionConfig::non_persistent` creates an InPrivate CompositionController on Windows and is covered by `demo-win --incognito-test` |
| Multi-instance verification (cross-talk isolation) | ✅ | 🟡 | ? | Windows profile smoke proves sequential producer recreation with the same `user_data_dir`; `demo-win --multi-view-test` proves simultaneous two-producer composition on separate HWNDs. Same-HWND composition remains unsupported in the current demo setup |
| Cookie store (read / write / delete) | ✅ | ✅ | ? | Wraps `WKHTTPCookieStore` on macOS and WebView2 `ICoreWebView2CookieManager` on Windows |
| Cookie / storage *change events* | ✅ | 🟡 | ⏳ | macOS uses `cookiesDidChangeInCookieStore:` for page writes, `Set-Cookie` headers, and host writes. Windows fires pulses for host `set_cookie` / `delete_cookie`, page-side `document.cookie` writes, and native `Set-Cookie` response headers via `WebResourceResponseReceived`; broader storage-change observation remains open |

## Browser-shape UX

| Capability | macOS | Windows | Linux | Notes |
| --- | --- | --- | --- | --- |
| Find-in-page (with options) | ✅ | ✅ | ? | macOS: `find_in_page` + `poll_find_match`; Windows: native `ICoreWebView2Find` with match count, covered by `demo-win --find-test` |
| Page-to-PDF rendering | ✅ | ✅ | ? | macOS: `request_pdf` + `poll_pdf`; Windows: native `PrintToPdfStream`, covered by `demo-win --pdf-test` |
| Print / `Cmd+P` (interactive) | ✅ | ✅ | ⏳ | macOS uses `NSPrintOperation`; Windows `print()` invokes WebView2's browser print UI via `ShowPrintUI`. Interactive cancellation is host/user driven and not smoke-tested |
| Auth challenges (events + host-driven disposition) | ✅ | ✅ | ? | Option A (events) + Option B (`set_auth_handler`) both shipped; Windows covers WebView2 `BasicAuthenticationRequested` with `demo-win --auth-test` |
| Auth during downloads (mid-stream / post-promotion) | ✅ | 🟡 | ? | `WKDownloadDelegate::download:didReceiveAuthenticationChallenge:` routes through the same shared auth handler as page-load auth. Windows WebView2 exposes Basic auth at the WebView level; active download URL matching reports `AuthSource::Download` when possible |
| Permission handlers (camera / mic / orientation) | ✅ | ✅ | ? | `set_permission_handler` returns `Allow` / `Deny` / `Prompt`; Windows maps camera / microphone / sensor prompts and is covered by `demo-win --permission-test` |
| WebRTC capture lifecycle observability | ✅ | ✅ | ⏳ | JS user-script monkey-patches `navigator.mediaDevices.getUserMedia`, tracks `track.ended`; emits `NavigationEvent::MediaCaptureStateChanged { audio_active_tracks, video_active_tracks }`. Windows bridge/event path is covered by `demo-win --media-test` |
| Title-changed notifications | ✅ | ✅ | ? | KVO on `WKWebView::title` |
| Downloads pipeline (id-correlated, host destination, cancel, resume) | ✅ | 🟡 | ? | Windows has `DownloadId`, `set_download_handler`, `cancel_download`, progress, finish/cancel events, and `demo-win --download-test`; resume data remains macOS-only |
| Context-menu requested event | ✅ | ✅ | ? | macOS and Windows emit `NavigationEvent::ContextMenuRequested`; Windows registers native WebView2 `ContextMenuRequested` and a deterministic document bridge covered by `demo-win --context-test` |
| Content blocking (`WKContentRuleList` / AdBlock-shape) | ✅ | ⏳ | ⏳ | `compile_and_apply_content_rule_list(id, json)` compiles via `WKContentRuleListStore::defaultStore` and attaches to the UCC on main-thread completion; `clear_all_content_rule_lists` detaches all |
| Spellcheck / autocorrect controls | ✅ | ⏳ | ? | `WkWebViewProducerConfig::spellcheck_override: Option<bool>` injects a document-start user-script that forces `spellcheck="true"\|"false"` on `<input>` / `<textarea>` / `[contenteditable]` plus a `MutationObserver` for added nodes. Best-effort — WKWebView has no public engine-level toggle |
| Autofill / Keychain integration | ✅ | ⏳ | ⏳ | System-driven on macOS — Apple's Keychain + AppKit's `NSSecureTextField` handle credential save / suggest transparently for `<input type="password">` and `autocomplete`-tagged fields when the WKWebView is in an active focused window. No producer-level code; documented in design notes. Per-profile credential isolation rides the `WKWebsiteDataStore` chosen via [`WkWebViewProducerConfig::data_dir`] / `non_persistent` |
| DevTools / Web Inspector remote attach | 📐 | ✅ | ? | macOS has `setInspectable(true)` wired (macOS 13.3+) but the Safari → Develop attach flow needs documentation; Windows has `open_devtools_window` |

---

## Notes on coverage

- **macOS** is the most complete column because it's the primary
  platform pushed in 0.4.x. The remaining ⏳ rows are public-API
  paths that just haven't been prioritized yet; the 🚫 rows are
  the genuine ceiling.
- **Windows** is a real WebView2 composition producer, but its
  remaining work is browser-shape API completion rather than frame
  production. `✅` rows were checked against
  [`webview2_composition_producer.rs`](../scrying/src/webview2_composition_producer.rs);
  `⏳` rows are public-WebView2-shaped follow-ups; `?` rows are still
  unaudited or need a design call. Runtime assertions should land in
  [`demo-win`](../demo-win/) rather than the cross-platform selector
  smoke; it now has `--scripted`, `--browser-test`, and
  `--cookie-test` / `--profile-test` one-shot modes for the shipped
  WebView2 slices. The Windows profile smoke recreates the producer
  because one CompositionController target can be attached to a given
  HWND at a time. The focused target and lane live in
  [`2026-05-11_windows_webview2_target.md`](2026-05-11_windows_webview2_target.md).
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
