//! Phase 4a.4 round-trip integration test:
//!
//! 1. Open `/dev/dri/renderD128`.
//! 2. Allocate a 256×256 ARGB8888 DMABUF via libgbm with the LINEAR
//!    modifier (so the byte layout is predictable + the stride is
//!    just `width * 4`).
//! 3. Write a known checkerboard pattern into the BO via `write()`.
//! 4. Build a [`scrying::DmaBufImage`] around the DMABUF fd, stride,
//!    modifier.
//! 5. Stand up a wgpu Vulkan device on this host and feed the frame
//!    through [`scrying::WgpuTextureImporter::import_frame`].
//! 6. Copy the imported texture into a CPU-mapped readback buffer.
//! 7. Assert the readback bytes match the original pattern.
//!
//! Skipped (test exits PASS without asserting) when this box can't
//! satisfy the prerequisites:
//! - no readable render node (CI runners without a DRM device)
//! - wgpu can't acquire a Vulkan adapter
//! - the chosen Vulkan device is missing the required DMABUF
//!   extensions
//!
//! Run with `cargo test --manifest-path scrying/Cargo.toml --test dmabuf_roundtrip -- --nocapture`
//! to see the diagnostic print statements.

#![cfg(target_os = "linux")]

use std::fs::OpenOptions;
use std::os::fd::IntoRawFd;

use dpi::PhysicalSize;
use scrying::{
    DmaBufImage, DmaBufPlane, HostWgpuContext, ImportOptions, NativeFrame, SyncMechanism,
    TextureImporter, WgpuTextureImporter,
};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const DRM_FORMAT_ARGB8888: u32 = 0x34325241; // 'AR24'

/// Mirror image of the import path's format choice: wgpu BGRA8 maps
/// to DRM_FORMAT_ARGB8888 on little-endian (bytes in memory: B G R A).
const WGPU_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

fn checkerboard_pattern() -> Vec<u8> {
    let mut bytes = vec![0u8; (WIDTH as usize) * (HEIGHT as usize) * 4];
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let idx = (y * WIDTH as usize + x) * 4;
            let on_white = ((x / 16) + (y / 16)) % 2 == 0;
            // Two distinguishable colors: scrying-navy (#0F172A) and
            // scrying-yellow (#FACC15), written in BGRA byte order.
            if on_white {
                // Yellow: R=0xFA G=0xCC B=0x15 A=0xFF
                bytes[idx + 0] = 0x15; // B
                bytes[idx + 1] = 0xCC; // G
                bytes[idx + 2] = 0xFA; // R
                bytes[idx + 3] = 0xFF; // A
            } else {
                // Navy:   R=0x0F G=0x17 B=0x2A A=0xFF
                bytes[idx + 0] = 0x2A; // B
                bytes[idx + 1] = 0x17; // G
                bytes[idx + 2] = 0x0F; // R
                bytes[idx + 3] = 0xFF; // A
            }
        }
    }
    bytes
}

#[test]
fn dmabuf_import_round_trip() {
    // ---- Step 1: open the render node ----
    let render_node_path = "/dev/dri/renderD128";
    let render_node = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(render_node_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("SKIP: cannot open {render_node_path}: {e}");
            return;
        }
    };

    // ---- Step 2: allocate gbm buffer object ----
    let gbm_device = match gbm::Device::new(render_node) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("SKIP: gbm::Device::new failed: {e}");
            return;
        }
    };

    // LINEAR modifier so the on-disk byte layout matches our writes
    // and stride == width * 4 with high probability. This is also
    // the closest analogue to "what a CPU-side producer would emit"
    // — implicit-modifier and tiled formats are deferred.
    let modifier = gbm::Modifier::Linear;
    let bo: gbm::BufferObject<()> = match gbm_device.create_buffer_object_with_modifiers::<()>(
        WIDTH,
        HEIGHT,
        gbm::Format::Argb8888,
        std::iter::once(modifier),
    ) {
        Ok(bo) => bo,
        Err(e) => {
            eprintln!("SKIP: gbm BO allocation failed: {e}");
            return;
        }
    };

    // ---- Step 3: write the known pattern ----
    let pattern = checkerboard_pattern();
    let stride = bo.stride();
    eprintln!("gbm BO: {WIDTH}x{HEIGHT} stride={stride} modifier={modifier:?}");
    let mut bo = bo;
    // `BufferObject::write` requires `BUFFEROBJECT_FLAGS::WRITE` at
    // creation time, which the modifier-aware constructor doesn't
    // surface. `map_mut` is the portable path — it mmap's the BO
    // for CPU r/w and lets us copy per-row, respecting whatever
    // stride Mesa chose.
    let stride_usize = stride as usize;
    let row_bytes = (WIDTH as usize) * 4;
    let map_result = bo.map_mut(0, 0, WIDTH, HEIGHT, |mapped| {
        let dst = mapped.buffer_mut();
        for y in 0..HEIGHT as usize {
            let dst_row = &mut dst[y * stride_usize..y * stride_usize + row_bytes];
            let src_row = &pattern[y * row_bytes..(y + 1) * row_bytes];
            dst_row.copy_from_slice(src_row);
        }
    });
    if let Err(e) = map_result {
        eprintln!("SKIP: gbm_bo_map failed: {e}");
        return;
    }

    // ---- Step 4: build the DmaBufImage ----
    let dmabuf_fd = match bo.fd() {
        Ok(fd) => fd.into_raw_fd(),
        Err(e) => {
            eprintln!("SKIP: gbm_bo_get_fd failed: {e:?}");
            return;
        }
    };
    let drm_modifier: u64 = u64::from(bo.modifier());

    let frame = DmaBufImage {
        size: PhysicalSize::new(WIDTH, HEIGHT),
        format: WGPU_FORMAT,
        drm_format: DRM_FORMAT_ARGB8888,
        drm_modifier,
        planes: vec![DmaBufPlane {
            fd: dmabuf_fd,
            offset: bo.offset(0),
            stride,
        }],
        generation: 1,
        producer_sync: SyncMechanism::None,
        semaphore_fd: None,
    };

    // ---- Step 5: stand up a wgpu Vulkan device + import ----
    let (device, queue, host) = match make_vulkan_host() {
        Some(host) => host,
        None => {
            eprintln!("SKIP: no Vulkan wgpu adapter available");
            return;
        }
    };

    let importer = WgpuTextureImporter::new(host);
    let imported =
        match importer.import_frame(&NativeFrame::DmaBufImage(frame), &ImportOptions::default()) {
            Ok(t) => t,
            Err(e) => {
                panic!("FAIL: import_frame errored: {e}");
            }
        };
    assert_eq!(imported.size.width, WIDTH);
    assert_eq!(imported.size.height, HEIGHT);
    assert_eq!(imported.format, WGPU_FORMAT);
    eprintln!(
        "imported texture: {}x{} format={:?} gen={}",
        imported.size.width, imported.size.height, imported.format, imported.generation
    );

    // ---- Step 6: copy imported → readback buffer ----
    let bytes_per_row = align_up(WIDTH * 4, 256); // wgpu requires 256-aligned rows on copy
    let readback_size = (bytes_per_row as u64) * (HEIGHT as u64);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scrying-dmabuf-roundtrip-readback"),
        size: readback_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("scrying-dmabuf-roundtrip-encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &imported.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    // Map + read.
    let buffer_slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("device poll");
    receiver
        .recv()
        .expect("map_async sender dropped")
        .expect("map_async failed");

    let mapped = buffer_slice.get_mapped_range();
    let mut readback_bytes = Vec::with_capacity((WIDTH as usize) * (HEIGHT as usize) * 4);
    for row in 0..HEIGHT as usize {
        let row_start = row * (bytes_per_row as usize);
        let row_end = row_start + (WIDTH as usize) * 4;
        readback_bytes.extend_from_slice(&mapped[row_start..row_end]);
    }
    drop(mapped);
    readback.unmap();

    // ---- Step 7: compare pixels ----
    let mut mismatches = 0usize;
    let mut first_mismatch: Option<(usize, [u8; 4], [u8; 4])> = None;
    for i in 0..pattern.len() / 4 {
        let want = [
            pattern[i * 4],
            pattern[i * 4 + 1],
            pattern[i * 4 + 2],
            pattern[i * 4 + 3],
        ];
        let got = [
            readback_bytes[i * 4],
            readback_bytes[i * 4 + 1],
            readback_bytes[i * 4 + 2],
            readback_bytes[i * 4 + 3],
        ];
        if want != got {
            mismatches += 1;
            if first_mismatch.is_none() {
                first_mismatch = Some((i, want, got));
            }
        }
    }

    if mismatches == 0 {
        eprintln!("PASS: {} pixels matched", pattern.len() / 4);
    } else {
        let (idx, want, got) = first_mismatch.unwrap();
        let x = idx % WIDTH as usize;
        let y = idx / WIDTH as usize;
        panic!(
            "FAIL: {mismatches} / {} pixels differ; first mismatch at ({x}, {y}): expected {:02x?}, got {:02x?}",
            pattern.len() / 4,
            want,
            got
        );
    }
}

fn make_vulkan_host() -> Option<(wgpu::Device, wgpu::Queue, HostWgpuContext)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        backend_options: wgpu::BackendOptions::default(),
        display: None,
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scrying-dmabuf-roundtrip-device"),
        ..Default::default()
    }))
    .ok()?;
    let host = HostWgpuContext::new(device.clone(), queue.clone());
    Some((device, queue, host))
}

fn align_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) & !(alignment - 1)
}
