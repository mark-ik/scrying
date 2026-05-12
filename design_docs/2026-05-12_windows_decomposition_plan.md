# Windows Producer / Demo Decomposition Plan

The Windows browser-parity tranche proved the next WebView2 surface, but it
also made two files violate the repository's 600-LOC module discipline:

- `scrying/src/webview2_composition_producer.rs`: 3666 lines
- `demo-win/src/main.rs`: 2901 lines

The target is the same shape already used by the macOS producer split: small
modules organized by ownership boundary, each kept under roughly 600 lines.
The split should be mechanical first, with behavior-preserving moves and the
existing bounded Windows smokes used after each lane.

## Library Split

Convert `webview2_composition_producer.rs` into a module directory:

- `mod.rs` — public config/types, producer struct, constructor, exports.
- `trait_impl.rs` — `WebSurfaceProducer` implementation.
- `capture.rs` — WGC session setup, frame acquisition, restart, snapshot, and
  resize capture lifecycle.
- `browser.rs` — find-in-page, PDF/print, context-menu events, media-capture
  bridge, and Set-Cookie response observation.
- `cookies.rs` — cookie manager requests, parsing, set/delete, callback API.
- `downloads.rs` — download registry, destination decisions, progress,
  pause/resume/cancel.
- `auth_permissions.rs` — Basic auth and permission request mapping.
- `resources.rs` — virtual-host routing, response construction, stream helpers.
- `input.rs` — mouse, pointer, keyboard, focus, cursor, and OLE drag helpers.
- `helpers.rs` — COM string, message-pump, timeout, and small shared utilities.

The first move should extract `browser.rs` because the newest tranche is a
cohesive unit and has clear demo coverage: `--find-test`, `--pdf-test`,
`--context-test`, `--media-test`, and the Set-Cookie part of `--cookie-test`.

## Demo Split

Convert `demo-win/src/main.rs` into a small app shell plus smoke modules:

- `main.rs` — CLI, `ApplicationHandler`, top-level one-shot dispatch.
- `renderer.rs` — wgpu renderer and imported texture presentation.
- `probe.rs` — startup GraphicsCapture / D3D shared-texture probes.
- `input.rs` — winit input forwarding helpers.
- `smokes/browser.rs` — scripted/browser/visibility/find/PDF/context/media.
- `smokes/network.rs` — virtual host, process recovery, downloads, auth,
  permissions, cookies, and loopback test servers.
- `smokes/profile.rs` — persistent profile, incognito, multi-view.

Start with `smokes/browser.rs` after the library browser split so the newest
runtime lanes stay paired.

## Validation After Each Lane

Use targeted checks, not broad/open-ended GUI runs:

```bash
rustfmt --edition 2024 --check <edited rust files>
cargo check --manifest-path repos/scrying/Cargo.toml -p scrying -p demo-win
```

For GUI/runtime coverage, use the existing bounded PowerShell wrapper with
process-tree cleanup. Do not run raw open-ended `cargo run -p demo-win`.

Minimum smoke set for the first browser split:

```bash
cargo run --manifest-path repos/scrying/Cargo.toml -p demo-win -- --find-test
cargo run --manifest-path repos/scrying/Cargo.toml -p demo-win -- --pdf-test
cargo run --manifest-path repos/scrying/Cargo.toml -p demo-win -- --context-test
cargo run --manifest-path repos/scrying/Cargo.toml -p demo-win -- --media-test
cargo run --manifest-path repos/scrying/Cargo.toml -p demo-win -- --cookie-test
```

All GUI commands above must be wrapped by an external timeout and process-tree
kill during validation.