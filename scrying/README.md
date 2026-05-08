# scrying

Capability-driven system-webview adapter — scry into WebView2/WKWebView/WebKitGTK and surface frames the host renderer can consume.

The name comes from *scrying* — gazing into a reflective surface for visions. The webview is the surface; the captured frame is the vision; this crate is the lens.

This crate is the home for system-webview-backed frame production. It is deliberately separate from [`wgpu-native-texture-interop`](https://github.com/mark-ik/wgpu-graft) (sibling repo): the native interop crate imports GPU resources, while this adapter owns system-webview probing, fallback selection, and platform-specific frame-source integration.

## Current slice

The shared contract:

- `WebSurfaceMode` — imported texture, native child overlay, CPU snapshot, or unsupported.
- `WryWebSurfaceCapabilities` — platform/backend capability reporting.
- `WryWebSurfaceFrame` — imported native frame, CPU RGBA frame, PNG snapshot, or overlay-only state.
- `WryWebSurfaceProducer` — producer trait that platform implementations satisfy.
- `OverlayOnlyProducer` — conservative fallback when no capture backend is available.

`WryWebSurfaceProducer` covers the full embeddable-webview surface, not just frame production:

- **Frame acquisition** — `acquire_frame`, plus producer-specific fast paths.
- **Layout** — `resize`, `set_offset`.
- **Navigation** — `navigate_to_string`, `navigate_to_url`. Both block until `NavigationCompleted`.
- **History** — `reload`, `stop`, `go_back`, `go_forward`, `can_go_back`, `can_go_forward`.
- **Input** — `send_mouse_input` (mouse + scroll + leave), `send_pointer_input` (touch / pen with pressure + tilt), `move_focus` (Programmatic / Next / Previous tab order). Drag-and-drop is implemented on the Windows producer's concrete type as `drag_enter` / `drag_over` / `drag_leave` / `drop_data` — the host supplies an `IDataObject` from its OLE drop-target callbacks. The trait-level `send_drag_input` stays platform-abstract; full cross-platform DnD waits for a unified data-carrier abstraction.
- **Lifecycle events** — `poll_navigation_event` drains a FIFO queue of `Starting` / `SourceChanged` / `Completed` / `TitleChanged` events.
- **Cursor reporting** — `poll_cursor_shape` returns the next [`CursorShape`] the engine wants the host to display (Pointer over a link, Text in an input, etc.).
- **JS messaging** — `post_web_message` (Rust → JS via `window.chrome.webview` listeners), `poll_web_message` (JS → Rust via `window.chrome.webview.postMessage`).
- **DevTools** — `open_devtools_window` opens the engine's developer-tools UI.
- **Settings** — `apply_settings(&WebSurfaceSettings)` accepts a partial update of zoom factor, user-agent string, JS-enabled, devtools-enabled, default-context-menus, and built-in accelerator keys. `None` fields are left at the producer's current value.
- **Snapshots** — `capture_snapshot_png` returns encoded PNG bytes via the underlying engine's preview API.

Methods that aren't yet implemented on a given platform return [`WryWebSurfaceError::Unsupported`] rather than panicking, so consumers can probe the surface incrementally.

Per-platform producer modules:

| Platform | Module | Status | Capture path |
| --- | --- | --- | --- |
| Windows | [`webview2_composition_producer`] | **Implemented.** Used by [`demo-wry-winit`]. | WebView2 CompositionController → `Windows.UI.Composition.Visual` → `Windows.Graphics.Capture` → shared D3D11 NT-handle texture → `wgpu` D3D12 import. |
| macOS | [`wkwebview_producer`] | **Skeleton.** Module exists, returns `OverlayOnly` until ScreenCaptureKit + IOSurface plumbing lands. | `WKWebView` hosted in NSView → `ScreenCaptureKit` stream bound to the view → `IOSurfaceRef` → `MTLTexture` → `wgpu` Metal import. |
| Linux | [`webkitgtk_producer`] | **Skeleton.** Module exists, returns `OverlayOnly`; the actual capture path isn't yet wired. | `WebKitWebView` (or WPE) → `WPEViewBackendDMABuf` DMABUF + `VkSemaphore` → `wgpu` Vulkan import. wlroots `zwlr_screencopy_manager_v1` is a possible coarser fallback. |

The Windows producer is the primary proof point and the reference implementation for the producer/consumer split, persistent shared texture, debounced resize, and cache-coherence handling.

## Windows producer details

The Windows producer ([`webview2_composition_producer::WebView2CompositionProducer`]) owns the full WebView2 composition + WGC capture lifecycle:

- WebView2 environment + `ICoreWebView2CompositionController` + `ICoreWebView2Controller`
- `Windows.UI.Composition` compositor + desktop-window-target + root + WebView visuals
- `Windows.Graphics.Capture` item, frame pool, session
- Persistent shared D3D11 destination texture (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX`) reused across frames; one allocation + one wgpu import per size change
- Lazy `start_capture` + bounded first-frame block + post-resize tear-down/rebuild + stall-detection escape hatch (`force_restart_capture`)

`WebView2 TextureStream` is not treated as the primary path because it is a page/media texture stream API, not a whole-webview compositor-output API.

The lower-level building blocks live in [`windows_capture`]:

- `D3D11SharedTextureFactory::create_shared_texture_frame(...)` allocates an NT-handle-shareable D3D11 texture.
- `D3D11SharedTextureFactory::copy_capture_into_existing_target(...)` writes a `Direct3D11CaptureFrame` into the persistent shared destination with proper keyed-mutex + GPU-completion synchronization.
- `capture_graphics_item_frame_once(...)` and `capture_visual_frame_once(...)` are one-shot capture helpers used by the demo's startup probes.
- `DxgiSharedHandleBridge` wraps the `WebView2DxgiSharedHandleFrame` → `WebView2Dx12SharedFrame` → `WryWebSurfaceFrame::Native(NativeFrame::Dx12SharedTexture)` handoff.

## Fallbacks

`NativeChildOverlay` remains the normal Wry fallback on every platform. The macOS skeleton currently advertises `CpuSnapshot` as well (`WKWebView.takeSnapshot` is good enough for thumbnails / previews if you don't need interactive frame rate); the Linux skeleton does not (`webkit_web_view_get_snapshot` would work but no consumer yet wants it).

`CpuSnapshot` is useful for diagnostics, thumbnails, and low-frequency preview paths, but it is not the target for interactive composited web surfaces.

## Cross-API GPU sync (Windows)

The shared D3D11 destination texture is `D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX | D3D11_RESOURCE_MISC_SHARED_NTHANDLE` so D3D12 (via wgpu) can `OpenSharedHandle` it. We coordinate writes and reads as follows:

- **Producer (D3D11):** `IDXGIKeyedMutex::AcquireSync(0, 500ms)` → `CopyResource` from the WGC capture frame → spin on `ID3D11Query(D3D11_QUERY_EVENT)` to wait for GPU completion → `ReleaseSync(0)`. This guarantees that by the time the consumer reads, the producer's GPU work is fully retired.
- **Consumer (wgpu/D3D12):** D3D12 resources opened from a keyed-mutex shared handle do **not** expose `IDXGIKeyedMutex` via `QueryInterface`, so the consumer cannot use the documented mutex path. Instead, the demo issues a throwaway `copy_texture_to_buffer` (1×1 pixel) on the imported texture before each render pass. wgpu's automatic state tracking inserts a `SHADER_RESOURCE → COPY_SRC → SHADER_RESOURCE` transition barrier, and on D3D12 that transition flushes shader caches that would otherwise hold a stale view of the externally-written shared texture.

**Status:** working empirically — verified across 10+ minute runs and many resize cycles, with the persistent shared texture reused (one D3D11 allocation per size change, one wgpu import per size change). However, the transition-barrier cache flush is a driver-level behavior, not a contract.

### Future work: explicit fence sync

A more rigorous alternative is to share a `D3D12_FENCE_FLAG_SHARED` fence across the two APIs:

1. Create the fence on the wgpu-owned D3D12 device, export an NT handle via `ID3D12Device::CreateSharedHandle`, open it on D3D11 via `ID3D11Device5::OpenSharedFence`.
2. Producer signals `value = n+1` on its D3D11 immediate context after `CopyResource`, releases the keyed mutex.
3. Consumer queues `ID3D12CommandQueue::Wait(fence, n+1)` before the render submit.
4. Bump `n` per frame.

**What this buys:** standards-correct ordering between the producer's writes and the consumer's reads (today's design relies on the consumer-side transition barrier flushing caches, which is not contractual); robustness against future driver changes; reusable for D3D12↔Vulkan / cross-process interop.

**Cost:** ~150–250 lines crossing the wgpu-hal escape hatch (`device.as_hal::<Dx12>()` for the queue), `ID3D11Device5` / `ID3D11DeviceContext4` plumbing, fence-value tracking, and a pre-submit injection point (probably a tiny no-op command buffer that runs `Wait` before the real submit). As of 0.2.0 this crate is unified on `windows 0.62` (via `webview2-com 0.39.1`), so the typed-COM bridge is a single-version path; the `demo-wry-winit` host still pulls `windows 0.61` transitively from wry until wry bumps its own webview2-com pin, but that's a demo-side artifact, not a contract on consumers.

Worth doing when (a) the adapter ships beyond this development box and a driver gives someone stale frames, (b) GraphShell's interop story expands beyond WebView2 capture, or (c) the code wants to be canonically correct rather than empirically correct.
