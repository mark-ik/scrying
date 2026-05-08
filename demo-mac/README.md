# demo-mac

Minimal winit + wgpu host probe for scrying's macOS WKWebView producer.
Counterpart to [`../demo-wry-winit`](../demo-wry-winit/) on Windows.

The demo's job is to **drive scrying's runtime paths from a real
event loop** so the producer's many slices (lifecycle, navigation,
input forwarding, JS messaging, snapshots, ScreenCaptureKit pipeline,
KVO observers, per-profile data store, etc.) get exercised against
actual AppKit / WebKit / Metal — not just compile-tested.

## Modes

By default the demo opens an overlay-mode WebView at
`https://example.com` and idles, logging navigation events and JS
messages to stdout. The CLI flags below add automated test modes.

### `--probe-snapshot`
Requests a CPU snapshot ~3 s after launch via the non-blocking
[`request_snapshot`](../scrying/src/wkwebview_producer.rs) /
[`poll_snapshot`](../scrying/src/wkwebview_producer.rs) pair, writes
it to `demo-mac-snapshot.png`, and exits. Automated proof-of-life for
slice F (no interactive input required).

### `--scripted`
Loads an inline test page with known DOM (input box + JS message
listener + scrollable region) and drives a state machine that:

- posts a host message and asserts the JS shim echoes it back
  → **slice E** (bidirectional JS messaging) full round-trip
- dispatches 6 scroll-wheel events
  → **slice G** (CGEvent → NSEvent → `scrollWheel:`) API-level
- dispatches 3 keyDown/keyUp pairs typing `abc`
  → **slice I** (`keyEventWithType:` → `keyDown:` / `keyUp:`)
  API-level

Prints `PASS` / `FAIL` and exits. The keyboard / scroll DOM-effect
assertions are best-effort because synthetic events from an offscreen
or unfocused window are filtered by WebKit's hit-testing — slice E,
which round-trips through the `chrome.webview` shim, is the strongest
end-to-end demonstration. G/I are asserted at the API-dispatch level.

### `--capture`
Sets up a Metal-backed wgpu surface tied to the winit window,
positions the WKWebView at the **left half** of the window, kicks
off [`start_capture_async`](../scrying/src/wkwebview_producer.rs)
~3 s after launch, and once `capture_status` reports `Live` starts
rendering each imported `wgpu::Texture` to the right half of the
surface (with a slight tint so the wgpu-rendered region is
distinguishable from the directly-composited WKWebView subview on
the left).

Verifies, end-to-end:

- Slice B (ScreenCaptureKit pipeline)
- Slice M2 (`native_frame::metal::import` — `wgpu::hal::metal::Device::texture_from_raw`)
- Slice N (`SCStream::updateConfiguration:` on resize, when paired
  with `--resize-test`)

### `--capture --dump-every N`
Every Nth imported texture is read back via `copy_texture_to_buffer`
and saved as `demo-mac-frame-NNNN.png`. Pixel-faithful proof of the
IOSurface → MTLTexture → wgpu chain.

### `--capture --resize-test`
Programmatically resizes the window mid-run on a 6/10/14 s schedule.
Each resize triggers the producer's `resize` and (because capture is
live) `SCStream::updateConfiguration:`. Verifies slice N at runtime;
combine with `--dump-every` to see captures at the new dimensions.

## Interactive keys

Outside test modes, the following keys work:

- `S`: request a CPU snapshot. The completion handler writes
  `demo-mac-snapshot.png` when ready.
- `M`: post a JS-side host message
  (`window.chrome.webview.addEventListener('message', ...)`
  recipients see it).

Any other typed character / mouse / scroll event is forwarded to the
WebView via the producer's input methods.

## Architecture

| File | Role |
| --- | --- |
| `src/main.rs` | winit application handler, event routing, scripted state machine |
| `src/render.rs` | wgpu surface + import + render pipeline + readback |

The demo only depends on **stable scrying APIs** — no internal types,
no SPI. If you can build and run this, downstream consumers will be
able to wire up scrying the same way.

## Known runtime caveats observed during development

- **Blocking trait methods deadlock from inside winit handlers.**
  `navigate_to_url`, `navigate_to_string`, `capture_cpu_snapshot`,
  and `start_capture` all pump the main `NSRunLoop` to wait on a
  delegate callback. From inside winit's `resumed` /
  `window_event` (which runs *under* the same NSRunLoop), the pump
  re-enters winit's event handler and trips its
  "no nested event handling" guard, panicking. **Always prefer the
  non-blocking inherent variants** (`load_url`, `load_html`,
  `request_snapshot` + `poll_snapshot`, `start_capture_async` +
  `capture_status`) when calling from a host event-loop callback.
- **Synthetic input is filtered when the window isn't user-focused.**
  WebKit's hit-testing accepts AppKit-routed events but may ignore
  events delivered straight to the responder methods if the window
  isn't on top. The producer dispatches them correctly; whether
  WebKit acts on them is downstream of scrying's contract.
- **Capture mode shows recursive content.** SCK captures the entire
  window framebuffer. Our wgpu surface displays the imported texture.
  In capture mode that texture *includes* our previous render's
  output, so the right half visibly shows nested capture frames.
  Not a bug — a consequence of capturing the same window we render
  into. Production consumers will typically render capture into a
  *different* window or surface.
