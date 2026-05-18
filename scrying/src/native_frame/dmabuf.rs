//! Linux DMABUF → wgpu Vulkan texture import.
//!
//! Phase 4a slice 1 — single-plane DMABUF import through wgpu-hal's
//! Vulkan escape hatch. Uses three Vulkan extensions, all of which
//! Mesa + the Renoir / Intel / NVIDIA-current Linux drivers support
//! out of the box:
//!
//! - `VK_KHR_external_memory_fd` — imports the DMABUF file descriptor
//!   as a `VkDeviceMemory` backing.
//! - `VK_EXT_image_drm_format_modifier` — creates a `VkImage` whose
//!   tiling matches the DMABUF's DRM format modifier so the consumer's
//!   sampling pipeline understands the producer's memory layout.
//! - `VK_KHR_external_memory` — the umbrella extension implied by the
//!   above two.
//!
//! The function-pointer side is intentionally minimal — `vkAllocateMemory`
//! accepts a chained `VkImportMemoryFdInfoKHR` to do the fd import in
//! the standard allocation path, so we don't load any KHR loader
//! structs. `vkGetMemoryFdPropertiesKHR` (which would refine the memory
//! type selection) is also skipped — we pick from the image's
//! `memory_type_bits` directly and rely on `vkAllocateMemory` to
//! reject the choice if it's fd-incompatible.
//!
//! Multi-plane DMABUFs (NV12 / P010 / YUV420) and the
//! `DRM_FORMAT_MOD_INVALID` implicit-modifier path are deferred to
//! Phase 4a.5. The Phase 4a.2 `VK_KHR_external_semaphore_fd` import for
//! the producer-sync semaphore is wired here: when `frame.semaphore_fd`
//! is `Some`, the importer loads `vkImportSemaphoreFdKHR` via
//! `vkGetDeviceProcAddr` (no ash high-level wrapper needed), imports
//! the semaphore, submits a wait-only `vkQueueSubmit` directly on the
//! consumer queue's `vk::Queue` (reached via
//! `wgpu::Queue::as_hal::<Vulkan>().as_raw()`), drains via
//! `vkQueueWaitIdle`, and destroys the semaphore. The CPU-side drain
//! is pessimistic — a future iteration can defer cleanup via a fence
//! ring so the consumer can issue work concurrently with the
//! producer's render.

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::mem;

use ash::vk;
use wgpu::wgc::api::Vulkan;

use super::{
    DmaBufImage, HostWgpuContext, ImportedTexture, InteropError, SyncMechanism, UnsupportedReason,
};

pub(super) fn import(
    frame: &DmaBufImage,
    host: &HostWgpuContext,
) -> Result<ImportedTexture, InteropError> {
    if host.backend != super::InteropBackend::Vulkan {
        return Err(InteropError::BackendMismatch {
            expected: "Vulkan",
            actual: "non-Vulkan",
        });
    }
    if frame.planes.is_empty() {
        return Err(InteropError::InvalidFrame("DmaBufImage has no planes"));
    }
    if frame.planes.len() > 1 {
        // Multi-plane DMABUFs (NV12 / P010 / YUV420) would need
        // separate `VkSubresourceLayout` per plane and matching DRM
        // modifier handling. Deferred to Phase 4a.2.
        return Err(InteropError::Unsupported(
            UnsupportedReason::NativeImportNotYetImplemented,
        ));
    }

    let vk_format = vk_format_from_dmabuf(frame).ok_or_else(|| {
        InteropError::Vulkan(format!(
            "unsupported DRM fourcc {:#010x} (wgpu format {:?})",
            frame.drm_format, frame.format
        ))
    })?;

    let width = frame.size.width;
    let height = frame.size.height;

    let texture = unsafe {
        let hal_device = host
            .device
            .as_hal::<Vulkan>()
            .ok_or(InteropError::BackendMismatch {
                expected: "Vulkan",
                actual: "non-Vulkan",
            })?;
        let raw_device: &ash::Device = hal_device.raw_device();

        // ---- 1. Build VkImage with DMABUF + DRM-modifier chain ----
        let plane_layouts = [vk::SubresourceLayout {
            offset: frame.planes[0].offset as u64,
            size: 0, // implementation-defined; ignored for DMABUFs
            row_pitch: frame.planes[0].stride as u64,
            array_pitch: 0,
            depth_pitch: 0,
        }];

        let mut drm_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(frame.drm_modifier)
            .plane_layouts(&plane_layouts);

        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_memory_info)
            .push_next(&mut drm_modifier_info);

        let vk_image = raw_device
            .create_image(&image_info, None)
            .map_err(|e| InteropError::Vulkan(format!("vkCreateImage: {e}")))?;

        // ---- 2. Memory requirements + memory-type selection ----
        let mem_reqs = raw_device.get_image_memory_requirements(vk_image);
        let memory_type_index = first_set_bit(mem_reqs.memory_type_bits).ok_or_else(|| {
            raw_device.destroy_image(vk_image, None);
            InteropError::Vulkan(
                "vkGetImageMemoryRequirements reported no compatible memory types".into(),
            )
        })?;

        // ---- 3. Allocate VkDeviceMemory importing the DMABUF fd ----
        // Per the Vulkan spec, ownership of `fd` transfers to the
        // driver on success — we must NOT close it ourselves
        // afterwards. On failure, ownership stays with the caller,
        // but the `DmaBufImage` is consumed by the producer's frame
        // path already, so we don't try to recover the fd.
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(frame.planes[0].fd);
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(vk_image);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(memory_type_index)
            .push_next(&mut dedicated_info)
            .push_next(&mut import_info);

        let vk_memory = match raw_device.allocate_memory(&alloc_info, None) {
            Ok(m) => m,
            Err(e) => {
                raw_device.destroy_image(vk_image, None);
                return Err(InteropError::Vulkan(format!("vkAllocateMemory: {e}")));
            }
        };

        if let Err(e) = raw_device.bind_image_memory(vk_image, vk_memory, 0) {
            raw_device.free_memory(vk_memory, None);
            raw_device.destroy_image(vk_image, None);
            return Err(InteropError::Vulkan(format!("vkBindImageMemory: {e}")));
        }

        // ---- 4. Wrap as wgpu_hal::vulkan::Texture ----
        let hal_descriptor = wgpu_hal::TextureDescriptor {
            label: Some("scrying-dmabuf-import"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: frame.format,
            usage: wgpu_types::TextureUses::RESOURCE | wgpu_types::TextureUses::COPY_SRC,
            memory_flags: wgpu_hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        };
        let hal_texture = hal_device.texture_from_raw(
            vk_image,
            &hal_descriptor,
            None,
            // `TextureMemory::Dedicated` hands the VkDeviceMemory to
            // wgpu-hal which frees it when the wgpu::Texture drops.
            // The DMABUF fd already transferred ownership to Vulkan,
            // so the fd cleanup happens automatically too.
            wgpu_hal::vulkan::TextureMemory::Dedicated(vk_memory),
        );

        // ---- 5. Wrap as wgpu::Texture ----
        let wgpu_texture = host.device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("scrying-dmabuf-import"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                format: frame.format,
                dimension: wgpu::TextureDimension::D2,
                mip_level_count: 1,
                sample_count: 1,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        );

        // ---- 6. Optional: producer-sync semaphore wait ----
        if let (Some(semaphore_fd), SyncMechanism::ExplicitExternalSemaphore) =
            (frame.semaphore_fd, frame.producer_sync)
        {
            if let Err(e) = wait_on_producer_semaphore(host, raw_device, semaphore_fd) {
                // The texture is valid; the wait failed. Surface the
                // error so callers can decide whether the data is
                // safe to sample (probably not).
                return Err(e);
            }
        }

        wgpu_texture
    };

    Ok(ImportedTexture {
        texture,
        format: frame.format,
        size: frame.size,
        generation: frame.generation,
        consumer_sync: frame.producer_sync,
    })
}

/// Import the producer's binary `VkSemaphore` from the supplied
/// opaque fd, inject a wait-only `vkQueueSubmit` on the consumer's
/// `vk::Queue`, drain via `vkQueueWaitIdle`, then destroy the
/// semaphore.
///
/// Thread-safety: `wgpu::Queue::as_hal` returns a Deref guard that
/// does **not** lock the underlying queue — Vulkan queues are
/// externally synchronized, so calling `vkQueueSubmit` from this
/// path while wgpu submits from another thread is undefined
/// behaviour. The Phase 4a.2 contract is single-threaded import:
/// callers must ensure no other wgpu queue work is in flight on
/// this queue during `import_frame`. A future iteration that adds
/// a wgpu-hal `Queue::add_wait_semaphore` upstream would lift this
/// restriction.
unsafe fn wait_on_producer_semaphore(
    host: &HostWgpuContext,
    raw_device: &ash::Device,
    semaphore_fd: i32,
) -> Result<(), InteropError> {
    // Load `vkImportSemaphoreFdKHR` by walking from the system
    // `libvulkan.so` loader → `vkGetDeviceProcAddr` → the device-
    // scoped extension function. We don't have an `ash::Instance`
    // (wgpu doesn't expose it from `Device::as_hal`), so we go
    // through `ash::Entry` which dlopens libvulkan itself. Vulkan's
    // spec guarantees `vkGetInstanceProcAddr(VK_NULL_HANDLE,
    // "vkGetDeviceProcAddr")` returns the device-proc loader.
    let entry = unsafe { ash::Entry::load() }
        .map_err(|e| InteropError::Vulkan(format!("ash::Entry::load (libvulkan): {e}")))?;
    let get_device_proc_ptr = unsafe {
        entry.get_instance_proc_addr(vk::Instance::null(), c"vkGetDeviceProcAddr".as_ptr())
    };
    let get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr = match get_device_proc_ptr {
        Some(p) => unsafe { mem::transmute::<_, vk::PFN_vkGetDeviceProcAddr>(p) },
        None => {
            return Err(InteropError::Vulkan(
                "vkGetInstanceProcAddr(NULL, vkGetDeviceProcAddr) returned null".into(),
            ));
        }
    };
    let raw_proc =
        unsafe { get_device_proc_addr(raw_device.handle(), c"vkImportSemaphoreFdKHR".as_ptr()) };
    if raw_proc.is_none() {
        return Err(InteropError::Vulkan(
            "vkGetDeviceProcAddr(vkImportSemaphoreFdKHR) returned null; \
             VK_KHR_external_semaphore_fd not enabled on this device"
                .into(),
        ));
    }
    type PfnImportSemaphoreFd = unsafe extern "system" fn(vk::Device, *const c_void) -> vk::Result;
    let import_fd: PfnImportSemaphoreFd = unsafe { mem::transmute_copy(&raw_proc) };

    // Create an empty VkSemaphore.
    let create_info = vk::SemaphoreCreateInfo::default();
    let vk_semaphore = unsafe {
        raw_device
            .create_semaphore(&create_info, None)
            .map_err(|e| InteropError::Vulkan(format!("vkCreateSemaphore: {e}")))?
    };

    // Import the producer's fd into the semaphore. Per the Vulkan
    // spec, OPAQUE_FD imports transfer ownership of the fd to the
    // driver on success.
    let import_info = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(vk_semaphore)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
        .fd(semaphore_fd);
    let result = unsafe { import_fd(raw_device.handle(), &import_info as *const _ as *const _) };
    if result != vk::Result::SUCCESS {
        unsafe { raw_device.destroy_semaphore(vk_semaphore, None) };
        return Err(InteropError::Vulkan(format!(
            "vkImportSemaphoreFdKHR failed: {result:?}"
        )));
    }

    // Reach the underlying vk::Queue via wgpu's HAL escape. The
    // returned guard is a Deref proxy; the raw handle is valid for
    // direct vkQueueSubmit calls as long as we don't race against
    // other wgpu work (see the function-level note above).
    let queue_guard =
        unsafe { host.queue.as_hal::<Vulkan>() }.ok_or(InteropError::BackendMismatch {
            expected: "Vulkan",
            actual: "non-Vulkan",
        })?;
    let vk_queue = queue_guard.as_raw();

    let wait_stages = [vk::PipelineStageFlags::ALL_COMMANDS];
    let wait_semaphores = [vk_semaphore];
    let submit_info = vk::SubmitInfo::default()
        .wait_semaphores(&wait_semaphores)
        .wait_dst_stage_mask(&wait_stages);

    let submit_result =
        unsafe { raw_device.queue_submit(vk_queue, &[submit_info], vk::Fence::null()) };
    if let Err(e) = submit_result {
        unsafe { raw_device.destroy_semaphore(vk_semaphore, None) };
        return Err(InteropError::Vulkan(format!(
            "vkQueueSubmit(wait_semaphores=[producer]): {e}"
        )));
    }

    // CPU-side drain so we can destroy the semaphore safely. Future
    // iterations can swap this for a fence-tracked deferred cleanup
    // so the consumer keeps running concurrently with the producer.
    let drain_result = unsafe { raw_device.queue_wait_idle(vk_queue) };
    unsafe { raw_device.destroy_semaphore(vk_semaphore, None) };
    if let Err(e) = drain_result {
        return Err(InteropError::Vulkan(format!("vkQueueWaitIdle: {e}")));
    }

    Ok(())
}

/// Translate DRM fourcc (the `drm_format` field) to a Vulkan format.
/// Only the BGRA/RGBA single-plane cases that WebKit's offscreen
/// DMABUF renderer actually produces today. Returns `None` for
/// anything we haven't validated.
fn vk_format_from_dmabuf(frame: &DmaBufImage) -> Option<vk::Format> {
    // linux/drm_fourcc.h — little-endian byte-order so the bytes in
    // memory are reversed vs. the human-readable fourcc.
    const DRM_FORMAT_ARGB8888: u32 = 0x34325241; // 'AR24' (BGRA bytes-in-memory)
    const DRM_FORMAT_ABGR8888: u32 = 0x34324241; // 'AB24' (RGBA bytes-in-memory)
    const DRM_FORMAT_XRGB8888: u32 = 0x34325258; // 'XR24'
    const DRM_FORMAT_XBGR8888: u32 = 0x34324258; // 'XB24'

    match frame.drm_format {
        DRM_FORMAT_ARGB8888 | DRM_FORMAT_XRGB8888 => Some(vk::Format::B8G8R8A8_UNORM),
        DRM_FORMAT_ABGR8888 | DRM_FORMAT_XBGR8888 => Some(vk::Format::R8G8B8A8_UNORM),
        _ => None,
    }
}

/// Lowest set bit in the memory-type-bits mask, or `None` if
/// `mask == 0`.
fn first_set_bit(mask: u32) -> Option<u32> {
    if mask == 0 {
        None
    } else {
        Some(mask.trailing_zeros())
    }
}

/// Capability probe surfaced by [`crate::WebSurfaceCapabilities::probe`]
/// (and any caller that wants to short-circuit before constructing a
/// `DmaBufImage`).
///
/// Returns `Ok(())` if the host's wgpu device has the Vulkan
/// extensions [`import`] needs at runtime. Returns
/// [`super::UnsupportedReason`] otherwise so callers can downgrade
/// `imported_texture` in the capability struct.
///
/// Cheap to call once at host setup; not designed for per-frame use
/// (dlopens libvulkan via `ash::Entry::load`).
pub(crate) fn probe_dmabuf_extensions(
    host: &super::HostWgpuContext,
) -> Result<(), super::UnsupportedReason> {
    if host.backend != super::InteropBackend::Vulkan {
        return Err(super::UnsupportedReason::HostBackendMismatch);
    }

    let hal_device = unsafe { host.device.as_hal::<Vulkan>() }
        .ok_or(super::UnsupportedReason::HostBackendMismatch)?;
    let raw_device: &ash::Device = hal_device.raw_device();

    let entry = unsafe { ash::Entry::load() }
        .map_err(|_| super::UnsupportedReason::NativeImportNotYetImplemented)?;
    let raw_proc = unsafe {
        entry.get_instance_proc_addr(vk::Instance::null(), c"vkGetDeviceProcAddr".as_ptr())
    };
    let get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr = match raw_proc {
        Some(p) => unsafe { mem::transmute::<_, vk::PFN_vkGetDeviceProcAddr>(p) },
        None => return Err(super::UnsupportedReason::NativeImportNotYetImplemented),
    };

    // Signature functions of the device extensions the import path
    // depends on. `vkImportMemoryFdKHR` itself isn't probed directly
    // — `vkAllocateMemory` accepts the import-fd chain struct so the
    // function pointer is never resolved separately; but
    // `vkGetMemoryFdPropertiesKHR` is the marker we can query for
    // VK_KHR_external_memory_fd availability.
    let required = [
        c"vkGetMemoryFdPropertiesKHR", // VK_KHR_external_memory_fd
        c"vkGetImageDrmFormatModifierPropertiesEXT", // VK_EXT_image_drm_format_modifier
    ];
    for name in required {
        let ptr = unsafe { get_device_proc_addr(raw_device.handle(), name.as_ptr()) };
        if ptr.is_none() {
            return Err(super::UnsupportedReason::NativeImportNotYetImplemented);
        }
    }

    Ok(())
}
