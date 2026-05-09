# SPI evaluation: WKWebView frontiers for scrying

Companion to [`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md)
and the parity checklist in
[`2026-05-09_browser_parity_checklist.md`](2026-05-09_browser_parity_checklist.md).
This doc evaluates the `_WK*` private-API frontier — what scrying might
plausibly want from it, what's already reachable via public API
(corrected against trunk WebKit, not stale memory), what shape the
maintenance cost would take if we did pull on the SPI rope, and what
safeguards we'd want before any of it lands.

The **distribution context** that makes this doc relevant rather than
academic: scrying targets Developer-ID-notarized standalone
distribution; Mac App Store consumers can't ship private API anyway.
Notarytool checks signatures + entitlements but doesn't reject private
symbols. So `_WK*` is on the table for a feature-gated, off-by-default
experimental mode — particularly for thin-client / browser-mirror
scenarios where the host wants more aggressive lifecycle control than
the public surface gives.

What follows is **research-grounded** (per a 2026-05-09 audit of the
WebKit trunk source and third-party precedent), not memory-grounded.
Two important corrections to earlier scrying docs land in §1.

---

## 1. Corrections to earlier docs

Two things were misstated in
[`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md)'s
"Outstanding for follow-up slices" list and have been corrected (or
will be by this commit):

### `_setSuspended:` is not on `_WKWebsiteDataStore`

The earlier doc described the throttling SPI as
`_WKWebsiteDataStore::_setSuspended:`. That selector doesn't exist in
current WebKit trunk. The actual per-page suspend SPI lives on
`WKWebView` itself: `-_suspendPage:completionHandler:` /
`-_resumePage:completionHandler:` / `-_isSuspended` (macOS 12 / iOS
15+, declared in
`Source/WebKit/UIProcess/API/Cocoa/WKWebViewPrivate.h`). The data-store
class exposes `_terminateNetworkProcess`,
`_resourceLoadStatisticsEnabled`, and various network-process suspend
helpers — none of which pause page-side JS execution.

Practical: any future feature-gated suspend implementation targets
`-_suspendPage:`, not the previously-quoted data-store selector.

### `WKPreferences.inactiveSchedulingPolicy` is the public answer

The earlier doc said "throttling control needs SPI." Not anymore:
`WKPreferences.inactiveSchedulingPolicy` (macOS 14 / iOS 17+, in
`Source/WebKit/UIProcess/API/Cocoa/WKPreferences.h`) is **public** and
ships exactly the three options scrying wanted:

- `.suspend` — pause JS and timers when the WebView's view isn't in a
  window. Public-API equivalent of `_suspendPage:`.
- `.throttle` — slow timers and stop animation, JS still runs. The
  "more aggressive than Page Visibility, less than full suspend"
  notch.
- `.none` — no throttling beyond Page Visibility (current default).

Compose with the existing `set_visible(false)` /
view-detach behavior the producer already supports, and "inactive tab
gets aggressively throttled" works through public API alone on macOS
14+ / iOS 17+. **scrying ships this in the same commit as this doc**
via `WebSurfaceSettings::inactive_scheduling_policy`. Older OS
versions get a silent no-op (gated on `respondsToSelector:`).

The parity-checklist row "Throttling control" flips from 🚫 → ✅ as a
result.

---

## 2. Public-API state of the art (post-correction)

For the lifecycle / throttling axis specifically, the public-API
toolkit on current macOS / iOS:

1. **Page Visibility** via `set_visible(bool)` / `NSView::setHidden:`.
   Light throttle: `document.hidden=true`, RAF caps ~1 Hz, autoplay
   throttles. Already shipped.
2. **`inactiveSchedulingPolicy`** (macOS 14+ / iOS 17+,
   public). Heavier throttle when the view is out of a window.
   Now shipped.
3. **Detach + rebuild via `interactionState`** (macOS 12+ / iOS 15+,
   public). Tear the WKWebView down, persist
   back-forward + scroll + form state, rebuild on demand. Stronger
   than `.suspend` in that it frees the WebContent process entirely;
   weaker in that running JS state, timers, and WebSocket
   connections are lost. Already exposed via
   `serialize_interaction_state` / `restore_interaction_state`;
   browser-shape consumers can compose it themselves.
4. **Process termination** via `_requestWebProcessTermination:` (SPI,
   macOS 12+) and `_warmInitialProcess` (SPI, all). The heavy hammer
   — useful only when the consumer accepts the cost of a cold rebuild.
   Public-API equivalent is the interactionState round-trip above.
5. **Per-frame SPI** worth knowing about (not currently used):
   - `_hiddenPageDOMTimerThrottlingEnabled` /
     `_hiddenPageDOMTimerThrottlingAutoIncreases` — affects timer
     alignment for Page-Visibility-hidden pages.
   - `_pageVisibilityBasedProcessSuppressionEnabled` — App Nap-style
     WebContent suppression when hidden.
   - `_domTimersThrottlingEnabled` — coarser global timer throttle.

Composing 1+2 gets browser-class lifecycle without any SPI on macOS
14+. Older OS versions are stuck with 1+3 (Page Visibility + manual
teardown), which is the current scrying ceiling on those releases.

### Snapshot APIs (corrected)

Both `takeSnapshotWithConfiguration:completionHandler:` (public) and
`-_takePDFSnapshotWithConfiguration:` (SPI, macOS 10.15.4+) **round
through an IPC to the WebContent process and trigger a fresh render**.
They're not compositor-bypassing — they go through the standard render
path. If anyone's using them as a "fast frame source," they're not
actually fast, and the SCK path scrying already runs is structurally
similar.

`-_takeSnapshotOfNode:completionHandler:` is brand new (macOS 26.4 /
iOS 26.4, fresh in trunk) and similarly IPCs into WebContent.

---

## 3. Pre-composition extraction

### Architecture (the part that matters)

`WKWebView` in the UIProcess does not own the rendered pixels.
WebContent renders into its own CALayer tree, which is serialized over
IPC by `RemoteLayerTreeDrawingArea`. The UIProcess reconstructs a
**parallel CALayer hierarchy** in
`Source/WebKit/UIProcess/RemoteLayerTree/RemoteLayerTreeHost.{h,mm}`
(`rootLayer()` returns `CALayer*`, `layerForID()` looks up nodes).

The leaf "tile" layers in this UIProcess hierarchy hold real
**`IOSurface`-backed `contents`**, populated by
`RemoteLayerBackingStore` updates carrying surface mach-port handles.
**There is no `CAContext`/`CALayerHost` in the macOS WKWebView main
content path** — Apple moved off cross-process layer hosting years
ago for the main hierarchy. (Site-isolation and model-process subtrees
still use hosting; the main content does not.)

So: **walk `WKWebView.layer` recursively → find tile layers → pull
`layer.contents` as `IOSurfaceRef` → blit them with Metal**. WindowServer
is not in the loop. This is the path.

`CARenderer` (public, macOS 10.13+) is a less interesting alternative:
it can rasterize a CALayer subtree into a Metal texture, but for
WKWebView's tile layers it mostly ends up copying the existing
IOSurfaces (the layers don't repaint — they swap `contents` to new
buffers). `CARenderer` is the right choice if you need correct
compositing of effects layers, masks, or non-affine sublayer
transforms; for a flat tile dump, direct IOSurface walking is cheaper
and avoids `CARenderer`'s `CATransaction` semantics.

### Maintenance signal

The Cocoa-facing snapshot API (`WKWebViewPrivate.h`) has been
relatively stable: `-takeSnapshotWithConfiguration:` since 10.13,
`_takePDFSnapshotWithConfiguration:` since 10.15.4,
`-_suspendPage:` / `-_resumePage:` since macOS 12, all still in trunk.
Method renames are rare; new selectors get added (the recent
`-_takeSnapshotOfNode:` in 26.4) without removing existing ones.

The **internal layer-tree code under
`Source/WebKit/UIProcess/RemoteLayerTree/`** is a different story.
Apple has been actively refactoring it for site isolation and
model-process hosting (recent commits in webkit-changes touch
hierarchy invariants on roughly a per-major-release cadence).
Direct-walk implementations have to track that churn.

Concretely:

- `WKWebView.layer` returning the wrapper above
  `RemoteLayerTreeHost::rootLayer()`: not formally guaranteed to be
  the same object across releases, but observable behavior has held.
- IOSurfaces in tile-layer `contents`: same — observable, not
  documented.
- Site-isolation may eventually replace some main-content tile-layers
  with hosted contexts, breaking the direct walk for those subtrees.

**Expect breakage every 1-2 macOS major releases** if we ship this.
Feasible to maintain with discipline (CI on beta, runtime
`respondsToSelector:` defense, fallback to SCK), but it's real work,
not a one-time write.

### Third-party precedent

- **Orion** (Kagi's WebKit-based browser, notarized): uses standard
  `WKWebView`. No public evidence of layer-tree extraction.
- **wry / Tauri** issue #391 (offscreen rendering) and #1358
  (screenshot capability): contributors document that "WKWebView
  refuses to render reliably when offscreen — JavaScript simply won't
  be executed sometimes." Workaround is the "ugly hack" of inserting
  the view between rootViewController and key window. No direct layer
  pull.
- **Electron** PR #20965: used `CAContext` + `CALayerHost`
  (plus `NSAccessibilityRemoteUIElement`, `NSNextStepFrame`,
  `NSThemeFrame`) for a custom path; had to gate them off in MAS
  builds. This is the cleanest data point that **`CALayerHost`-style
  SPI is tolerated by Developer-ID notarization but rejected from the
  Mac App Store**, exactly as expected.
- **Chromium**: maintains two macOS paths — public IOSurface and a
  `CARemoteLayer` path — and has the engineering capacity to keep
  both working across releases.
- **JxBrowser** (TeamDev): the canonical writeup of `CGSMainConnectionID`
  + `CAContext.setLayer:` + `CALayerHost.contextId=`; not specific to
  WKWebView.

The takeaway: this is solved territory, but the survivors are
projects with full-time platform engineers.

### Distribution gating

Layer-tree direct access (the path above) doesn't itself call any
`_WK*` SPI — it calls `CALayer.contents`, which is public. The
*assumption* it makes (that WKWebView's layer tree contains
IOSurface-backed tile layers) isn't documented but isn't private
either. Notarytool won't reject it. MAS submission's static analysis
might flag subviews-of-subviews access patterns it doesn't recognize,
but there's no hard symbol-list violation.

In practice, MAS rejection risk for this path is **low** but not
zero. For Developer-ID, **none**. For internal / enterprise,
**none**.

### Recommendation

**Defer for now**, document the path above as the agreed shape if a
consumer asks. Maintenance cost looks "1-2 engineer-weeks per macOS
major release plus CI," which is real. The "SCK quiets on a static
page" issue we initially worried about is mitigated by the existing
dim-match guard + revision gate, and the new
`inactiveSchedulingPolicy` makes per-tab life management work without
any SPI. So the trigger for this slice is a *measured* problem the
SCK + public-throttle path can't solve, not a theoretical one.

---

## 4. Sub-frame / per-iframe capture

Don't.

Apple is **actively** locking this down for site isolation. Even
WebKit-internal code is moving toward strict per-process boundaries;
each iframe can run in its own process, and the UIProcess only gets
composited handoffs, not per-frame buffers. SPI surface around
per-frame access has churned every release.

The right answer for a consumer with a per-iframe capture requirement
is **Linux WPE**, which exposes per-view DMABUF buffers natively, no
SPI, no fragility. Document this in the parity checklist's WPE column
when WPE work starts.

---

## 5. Safeguards if / when SPI lands

Concrete patterns we'd want before any `_WK*` call goes in:

### 5.1. Cargo feature flag

```toml
[features]
default = []
spi-private-api = []
```

Each SPI usage gated behind `#[cfg(feature = "spi-private-api")]`.
Default-off so the standard build is SPI-clean. Consumers opt in
explicitly with `--features spi-private-api` and accept the
maintenance contract documented at the feature.

### 5.2. Per-SPI module isolation

Each SPI usage gets its own submodule under
`scrying/src/wkwebview_producer/spi/<name>.rs`. So `spi/suspend.rs`,
`spi/precomposition.rs`, etc. The non-SPI surface never sees these
modules; only the gated feature crate root re-exports them.

This means a single SPI breakage doesn't take down unrelated SPI
features, and removing an SPI module is a localized change.

### 5.3. Runtime `respondsToSelector:` gate

Every SPI call site checks the selector exists at runtime before
calling, identically to how `inactiveSchedulingPolicy` already does
on older OSes:

```rust
let obj: &AnyObject = (&*foo).as_ref();
if obj.class().responds_to(sel!(_someSelector:)) {
    unsafe { foo._some_selector(arg); }
} else {
    // Documented fallback — never panic, never silently lie.
    eprintln!("scrying: SPI _someSelector unavailable on this OS, falling back to <X>");
    fallback_path(arg);
}
```

The fallback must always work — the SPI path is an *upgrade*, not a
hard requirement. For `_suspendPage:`, the fallback is "use
`InactiveSchedulingPolicy::Suspend` if available, otherwise
`set_visible(false)`."

### 5.4. CI matrix with macOS beta

Add a job to `.github/workflows/test-mac.yml` that runs on
`macos-latest` plus a `macos-beta` (or whichever GHA tag tracks the
current beta channel). Run the same `scripts/test-mac.sh` against
both. Build with `--features spi-private-api` so the gated paths
exercise. Failures on the beta runner aren't blocking but get a
GitHub issue auto-filed via a workflow notification.

This buys ~6 months of lead time on Apple-side breakage.

### 5.5. WebKit source watch

The relevant headers live in trunk WebKit:

- `Source/WebKit/UIProcess/API/Cocoa/WKWebViewPrivate.h`
- `Source/WebKit/UIProcess/API/Cocoa/WKPreferencesPrivate.h`
- `Source/WebKit/UIProcess/RemoteLayerTree/RemoteLayerTreeHost.h`

A weekly cron (GHA scheduled workflow) runs
`git log --oneline <since-last-week> -- <those-paths>` against a
checkout of `WebKit/WebKit@main` and posts a summary to a
maintenance-channel issue. Rename / removal events get a doc-update
PR within a few days of upstream landing, which is typically months
before the OS release that ships the change.

### 5.6. Per-SPI design-doc section

Each SPI added to scrying gets a section in
[`2026-05-07_platform_ceilings.md`](2026-05-07_platform_ceilings.md)'s
appropriate slice column with:

- What the SPI does
- What public path was tried first
- What the runtime fallback is when the SPI isn't available
- What we monitor for breakage (which header, which behavior)
- Distribution implications spelled out (MAS-incompatible, etc.)

Downstream consumer repos link to those sections so SPI surface
contract is visible at the boundary, not buried in commit messages.

---

## 6. Per-SPI recommendations

### `_suspendPage:` — defer

`InactiveSchedulingPolicy::Suspend` (just shipped) is the public
replacement on macOS 14+ / iOS 17+. The remaining gap is "we want
suspend on macOS 12-13 / iOS 15-16 too," which is a smaller
constituency than browser-class consumers writing for the latest two
OS versions. If a consumer (mere, scrying's thin-client mode) reports
measurable battery / CPU regression on older OSes that the public
path can't fix, ship `_suspendPage:` behind `spi-private-api` then.
Until then, no.

### Pre-composition extraction — feature-gated experimental

Don't ship default-enabled, but *do* implement when a consumer reports
a measured problem the SCK + dim-match-guard path can't paper over.
The path:

1. New module `wkwebview_producer/precomposition.rs` (gated by
   `spi-private-api`)
2. `WkWebViewProducer::start_capture_precomposition()` — alternative
   entry point alongside `start_capture` / `start_capture_async`
3. Walks `WKWebView.layer` for tile layers, grabs IOSurfaces,
   feeds them through the same `MetalTextureRef` emit path the SCK
   pipeline uses
4. Falls back to SCK at runtime if the layer walk finds no
   IOSurface-backed tiles (defensive — covers OSes where the
   architecture changed)
5. Parity tests: a `--precomposition-test` mode that runs alongside
   the existing `--capture-test` and asserts pixel equivalence (or
   close-enough) between the two paths

Maintenance cost: 1-2 engineer-weeks per macOS major release. CI
matrix above catches breakage early.

### Sub-frame capture — punt to WPE

Don't. If a consumer needs it, point them at the Linux WPE producer
(out-of-band slice; un-started). WPE exposes per-view buffers as
public DMABUF/Vulkan-semaphore surfaces; same-machine,
no-SPI-required. Cross-platform consumers ship macOS-mode for "good
enough" composited capture and Linux-WPE-mode for per-frame work.

### `_hiddenPageDOMTimerThrottlingEnabled` and friends — opt-in WebSurfaceSettings

These are smaller `_WK*` knobs that compose with the public throttle.
Worth surfacing as additional `WebSurfaceSettings` fields under the
`spi-private-api` feature flag if a consumer asks. Fallback when the
selector is absent: silent no-op.

---

## 7. Open questions

Carried from the 2026-05-09 research (WebKit source check
inconclusive on these):

- Whether IOSurfaces in tile layers under
  `RemoteLayerTreeHost::rootLayer()` are guaranteed accessible from a
  WKWebView `-layer` walk on every macOS release.
- Whether `inactiveSchedulingPolicy` interacts with iOS's
  `ProcessThrottler` differently from macOS App Nap.
- Whether any current macOS WKWebView path uses `CALayerHost` for any
  *main-content* subtree (model and site-isolation extensions imply
  yes for those subtrees, no for the main content; would need a probe
  build to confirm).

These get resolved when (if) we actually implement the
pre-composition path, by writing a probe test and observing live
output.

---

## 8. Sources

WebKit trunk headers and supporting precedent (2026-05-09 audit):

- [WKWebViewPrivate.h](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WebKit/UIProcess/API/Cocoa/WKWebViewPrivate.h)
- [WKPreferencesPrivate.h](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WebKit/UIProcess/API/Cocoa/WKPreferencesPrivate.h)
- [RemoteLayerTreeHost.h](https://raw.githubusercontent.com/WebKit/WebKit/main/Source/WebKit/UIProcess/RemoteLayerTree/RemoteLayerTreeHost.h)
- [Apple — WKPreferences.InactiveSchedulingPolicy](https://developer.apple.com/documentation/webkit/wkpreferences/inactiveschedulingpolicy)
- [Apple — WKWebView.interactionState](https://developer.apple.com/documentation/webkit/wkwebview/interactionstate)
- [Apple — CARenderer](https://developer.apple.com/documentation/quartzcore/carenderer)
- [Mozilla Gfx — Reduced power usage with Core Animation](https://mozillagfx.wordpress.com/2019/10/22/dramatically-reduced-power-usage-in-firefox-70-on-macos-with-core-animation/)
- [Electron PR #20965 — disable remote-layer SPI in MAS](https://github.com/electron/electron/pull/20965)
- [wry #391 (offscreen rendering)](https://github.com/tauri-apps/wry/issues/391),
  [wry #1246 (background throttling)](https://github.com/tauri-apps/wry/issues/1246)
- [WebKit Bug 161450 — snapshot reliability](https://bugs.webkit.org/show_bug.cgi?id=161450)
