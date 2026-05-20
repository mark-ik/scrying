# Phase 4b — WPE Rust bindings: where they live + gir sketch

Resolves checklist item **4b.1** ("Decide where the WPE bindings crates
live") from [`2026-05-15_phase4_strategy.md`](2026-05-15_phase4_strategy.md)
and sketches the `gir` configuration for the generatable portion.

## The decision

**Layered, two-track.** Don't conflate "unblock the scrying WPE
producer" with "publish WPE bindings for the Rust ecosystem" — they
have different scopes, owners, licenses, and release cadences.

1. **Now — inline FFI, in-tree.** The `wpe_producer` gets a small,
   hand-written `extern "C"` surface in
   `scrying/src/wpe_producer/ffi.rs`, covering only the symbols the
   DMABUF frame path actually calls. No new crate, no codegen, no
   publishing dependency on the critical path to 4c.

2. **Later — a dedicated `wpe-rs` repo, NOT a scrying workspace
   member.** The gir-generated `wpe-webkit-sys` + safe `wpe-webkit`
   crates live in their own repository (`mark-ik/wpe-rs`), published
   to crates.io under MIT/Apache-2.0 (gtk-rs convention), with an eye
   toward donating to the gtk-rs org once they're proven. Keeping them
   out of the scrying workspace avoids coupling binding releases to
   scrying's cadence and keeps scrying's MPL-2.0 separate from the
   permissive license the gtk-rs ecosystem expects.

### Why not a scrying workspace member?

- **License**: scrying is MPL-2.0; gtk-rs-family crates are MIT/Apache.
  Bindings published for ecosystem reuse should match the ecosystem.
- **Cadence**: a binding crate tracks the C library's ABI, not
  scrying's feature work. Shared workspace = shared release pressure.
- **Reuse**: Tauri/wry and others have asked for WPE bindings; a
  scrying-internal crate is invisible to them.

### Why inline FFI is correct for the near term

The producer's actual dependency is the **DMABUF view backend**, which
lives in **libwpe + WPEBackend-fdo** — plain C, *not* GObject. `gir`
generates nothing for it. So even with a finished `wpe-webkit` safe
wrapper, the DMABUF interop symbols would still be hand-written. The
critical path to a working 4c does not run through any codegen.

## The libwpe vs WPEWebKit split (the load-bearing nuance)

| Layer | Nature | Bindings approach | Used for |
|---|---|---|---|
| **libwpe** (`libwpe-1.0`) | plain C ABI, toolkit-agnostic, **no GObject / no GIR** | hand-written FFI | view backend, input, the DMABUF export protocol |
| **WPEBackend-fdo** | plain C, loadable backend impl | hand-written FFI (mostly init + `wpe_fdo_initialize_for_egl_display`) | wires libwpe to an EGL/DMABUF producer |
| **WPEWebKit** (`WPEWebKit-2.0`) | GObject, **ships GIR** | `gir`-generated `wpe-webkit-sys` + `wpe-webkit` | webview control: navigation, settings, JS eval, cookies, input forwarding |

The DMABUF frames that feed Phase 4a's importer come out of the
libwpe/fdo layer. The webview *control* surface (mirroring what the
`webkitgtk_producer` already does for GTK) is the WPEWebKit layer.

> **Verify before relying on this:** confirm `libwpe` ships no
> `.gir` (expected — it's deliberately GObject-free) and confirm
> WPEWebKit's GIR namespace/version on the target install
> (`WPEWebKit-2.0` on ≥ 2.44; older runtimes expose `WPEWebKit-1.0`).
> `pkg-config --variable=girdir wpe-webkit-2.0` then
> `ls $girdir | grep -i wpe` settles both.

## Inline FFI surface the producer needs (sketch)

Roughly the symbols `scrying/src/wpe_producer/ffi.rs` declares for 4c.
All plain C; loaded via `#[link]` against `libwpe-1.0` +
`libWPEBackend-fdo-1.0` (or `dlopen` if we want soft-fail when WPE
isn't installed):

```rust
// libwpe — view backend lifecycle + DMABUF export protocol
extern "C" {
    fn wpe_view_backend_create() -> *mut wpe_view_backend;
    fn wpe_view_backend_destroy(b: *mut wpe_view_backend);
    fn wpe_view_backend_dispatch_set_size(b: *mut wpe_view_backend, w: u32, h: u32);
    // backend → client buffer-export callbacks are registered through
    // WPEBackend-fdo's exportable interface, not libwpe directly:
}

// WPEBackend-fdo — EGL/DMABUF exportable backend
extern "C" {
    fn wpe_fdo_initialize_for_egl_display(display: *mut c_void) -> bool;
    fn wpe_view_backend_exportable_fdo_create(
        client: *const wpe_view_backend_exportable_fdo_client,
        data: *mut c_void,
        width: u32,
        height: u32,
    ) -> *mut wpe_view_backend_exportable_fdo;
    fn wpe_view_backend_exportable_fdo_get_view_backend(
        e: *mut wpe_view_backend_exportable_fdo,
    ) -> *mut wpe_view_backend;
    // the DMABUF variant hands us `wpe_dmabuf_pool_entry` /
    // `wpe_dma_buf_export` carrying fd + stride + offset + modifier,
    // which map straight onto `DmaBufImage` / `DmaBufPlane`.
    fn wpe_view_backend_exportable_fdo_dispatch_release_buffer(
        e: *mut wpe_view_backend_exportable_fdo,
        buffer: *mut c_void,
    );
}
```

The export client struct carries the `export_dmabuf` callback —
that's the seam that calls the existing
`WpeProducer::enqueue_dmabuf_frame`. The frame release back to WPE
(`dispatch_release_buffer`) is the producer-side counterpart to the
importer transferring fd ownership to Vulkan.

> Exact struct field layouts come from `wpe/view-backend-exportable.h`
> and `wpe/extensions/video-plane-display-dmabuf.h` in the installed
> headers — to be transcribed when 4c lands, against a pinned WPE
> version.

## Gir.toml sketch — the WPEWebKit (generatable) track

For the long-term `wpe-rs` repo. Two crates, gtk-rs `gir` `sys` +
`normal` modes. Assumes `WPEWebKit-2.0.gir` plus the standard gtk-rs
`gir-files` (GLib/GObject/Gio/Soup/JavaScriptCore) are on the gir
search path.

`wpe-webkit-sys/Gir.toml` (FFI):

```toml
[options]
girs_directories = ["../gir-files", "../wpe-gir-files"]
library = "WPEWebKit"
version = "2.0"
# Match Fedora 44's webkitgtk6.0 2.52 baseline once WPE tracks it;
# 2.44 is the conservative floor for the modern API surface.
min_cfg_version = "2.44"
target_path = "."
work_mode = "sys"

[external_libraries]
# WPEWebKit-2.0.gir references these namespaces; their -sys crates
# come from crates.io (gtk-rs) so gir emits `extern crate` links
# rather than regenerating them.
GLib = "2.0"
GObject = "2.0"
Gio = "2.0"
Soup = "3.0"
JavaScriptCore = "6.0"   # WPEJavaScriptCore on some builds — verify
```

`wpe-webkit/Gir.toml` (safe wrappers):

```toml
[options]
girs_directories = ["../gir-files", "../wpe-gir-files"]
library = "WPEWebKit"
version = "2.0"
min_cfg_version = "2.44"
target_path = "."
work_mode = "normal"
generate_safety_asserts = true
deprecate_by_min_version = true
single_version_file = true

generate = [
    "WPEWebKit.WebView",
    "WPEWebKit.Settings",
    "WPEWebKit.NetworkSession",
    "WPEWebKit.UserContentManager",
    "WPEWebKit.CookieManager",
    "WPEWebKit.URISchemeRequest",
    # … the control surface mirrored from webkit6_producer
]

# Hand-written extensions gir can't infer (signal connectors with
# closures, IsA chains for newtypes) go in src/ as today's gtk-rs
# crates do.
```

> Namespace/version strings (`WPEWebKit` vs `WPEWebKit-2.0`,
> `JavaScriptCore-6.0` vs a WPE-specific JSC) must be confirmed
> against the actual installed `.gir` — these are the usual rough
> edges the strategy doc budgeted "up to two weeks" for.

## What this unblocks / defers

- **Unblocks 4c**: inline FFI is a few dozen lines against a pinned
  WPE; no publishing, no gir, no new repo on the critical path.
- **Defers** the `wpe-rs` repo + publishing to a separate, properly
  scoped effort whose value is ecosystem-wide, not scrying-internal.
- **Still gated** on getting WPE onto this Fedora box at all (Flatpak
  SDK is the leading option per the strategy doc) before any of the
  above can be runtime-verified.

## Updated checklist deltas

- [x] **4b.1** Decide where the WPE bindings crates live → *dedicated
  `wpe-rs` repo (later); inline in-tree FFI (now)*. This doc.
- [ ] **4b.2** `wpe-sys`: superseded — libwpe is plain C, bound via
  inline FFI, not a gir `sys` crate.
- [ ] **4b.3** `wpe-webkit-sys` + `wpe-webkit`: gir-generated in
  `wpe-rs`; Gir.toml sketched above; blocked on a WPE install to
  validate the GIR.
