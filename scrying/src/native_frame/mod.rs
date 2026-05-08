//! Native-frame import: import platform-native GPU texture handles
//! (D3D12 NT-handle today, IOSurface and DMABUF eventually) into wgpu
//! textures owned by the host device.
//!
//! Derived structurally from the per-platform `rendering_context/` shape
//! in the [Slint Servo embedding example][1] and adapted to take native
//! handles directly (no surfman GL FBO bridge).
//!
//! [1]: https://github.com/slint-ui/slint/tree/master/examples/servo

mod error;
mod sync;

#[cfg(target_os = "windows")]
mod sync_dx12;

use dpi::PhysicalSize;

pub use error::{InteropError, UnsupportedReason};
pub use sync::{ImplicitOnlySynchronizer, InteropSynchronizer, NoopSynchronizer, SyncMechanism};

#[cfg(target_os = "windows")]
pub use sync_dx12::Dx12FenceSynchronizer;

/// The wgpu graphics backend in use on the host device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InteropBackend {
    Vulkan,
    Metal,
    Dx12,
    Unknown,
}

/// Discriminant for [`NativeFrame`] variants without carrying frame data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeFrameKind {
    Dx12SharedTexture,
    // Reserved for future producers:
    //   IoSurfaceTexture (macOS WKWebView)
    //   DmaBufImage (Linux WPE)
}

/// Whether a particular interop capability is available on this device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityStatus {
    Supported,
    Unsupported(UnsupportedReason),
}

/// The set of [`NativeFrameKind`]s a producer can emit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProducerCapabilities {
    pub supported_frames: Vec<NativeFrameKind>,
}

/// Wraps a `wgpu::Device` and `wgpu::Queue` together with the detected
/// backend.
#[derive(Clone, Debug)]
pub struct HostWgpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub backend: InteropBackend,
}

impl HostWgpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            backend: detect_backend(&device),
            device,
            queue,
        }
    }
}

/// Options that control how [`WgpuTextureImporter`] processes each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImportOptions {
    /// Currently unused; reserved for future use (e.g. CPU-fallback gates).
    pub allow_copy_fallback: bool,
}

/// A successfully imported wgpu texture, ready for use in a render pipeline.
#[derive(Debug)]
pub struct ImportedTexture {
    pub texture: wgpu::Texture,
    pub format: wgpu::TextureFormat,
    pub size: PhysicalSize<u32>,
    pub generation: u64,
    pub consumer_sync: SyncMechanism,
}

/// A frame backed by a D3D12 resource shared via a DXGI NT handle.
///
/// Obtain the handle by calling `IDXGIResource1::CreateSharedHandle` on
/// your `ID3D12Resource` (or the equivalent on a D3D11 producer). The
/// importer opens its own D3D12 reference via
/// `ID3D12Device::OpenSharedHandle`; the caller is responsible for closing
/// its copy of the handle after constructing this struct.
#[derive(Clone, Copy, Debug)]
pub struct Dx12SharedTexture {
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
    pub producer_sync: SyncMechanism,
    /// Fence value the producer signalled at on its `ID3D11Fence` /
    /// `ID3D12Fence` (opened from
    /// [`Dx12FenceSynchronizer::shared_handle`]). The synchronizer waits
    /// for this value on the wgpu D3D12 queue before the next consumer
    /// submit.
    ///
    /// Only meaningful when `producer_sync == SyncMechanism::ExplicitFence`.
    /// `0` for the keyed-mutex path; the synchronizer treats `0` as "no
    /// wait recorded for this frame".
    pub fence_value: u64,
    /// NT `HANDLE` from `IDXGIResource1::CreateSharedHandle`. Windows only.
    #[cfg(target_os = "windows")]
    pub handle: *mut std::ffi::c_void,
}

/// A native frame from a producer, ready to be imported by a
/// [`TextureImporter`]. Today only the Windows `Dx12SharedTexture` variant
/// is wired; macOS / Linux variants land alongside their producers.
#[non_exhaustive]
pub enum NativeFrame {
    Dx12SharedTexture(Dx12SharedTexture),
}

impl NativeFrame {
    pub fn kind(&self) -> NativeFrameKind {
        match self {
            NativeFrame::Dx12SharedTexture(_) => NativeFrameKind::Dx12SharedTexture,
        }
    }

    pub fn producer_sync(&self) -> SyncMechanism {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.producer_sync,
        }
    }
}

/// Imports a [`NativeFrame`] into a `wgpu::Texture`.
pub trait TextureImporter {
    fn import_frame(
        &self,
        frame: &NativeFrame,
        options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError>;
}

/// Main entry point. Create one per wgpu device, reuse across frames.
pub struct WgpuTextureImporter {
    host: HostWgpuContext,
    synchronizer: Box<dyn InteropSynchronizer>,
}

impl WgpuTextureImporter {
    /// Default importer with [`ImplicitOnlySynchronizer`].
    pub fn new(host: HostWgpuContext) -> Self {
        Self {
            host,
            synchronizer: Box::new(ImplicitOnlySynchronizer),
        }
    }

    /// Importer with a custom [`InteropSynchronizer`].
    pub fn with_synchronizer(
        host: HostWgpuContext,
        synchronizer: Box<dyn InteropSynchronizer>,
    ) -> Self {
        Self { host, synchronizer }
    }

    pub fn host(&self) -> &HostWgpuContext {
        &self.host
    }
}

impl TextureImporter for WgpuTextureImporter {
    fn import_frame(
        &self,
        frame: &NativeFrame,
        _options: &ImportOptions,
    ) -> Result<ImportedTexture, InteropError> {
        self.synchronizer
            .producer_complete(frame, frame.producer_sync())?;

        let imported = match frame {
            NativeFrame::Dx12SharedTexture(frame) => import_dx12_shared_texture(frame, &self.host),
        }?;

        self.synchronizer
            .consumer_ready(&imported, imported.consumer_sync)?;
        Ok(imported)
    }
}

fn import_dx12_shared_texture(
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))] frame: &Dx12SharedTexture,
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))] host: &HostWgpuContext,
) -> Result<ImportedTexture, InteropError> {
    #[cfg(target_os = "windows")]
    {
        if host.backend != InteropBackend::Dx12 {
            return Err(InteropError::BackendMismatch {
                expected: "Dx12",
                actual: "non-Dx12",
            });
        }

        let texture = unsafe {
            let hal_device = host.device.as_hal::<wgpu::wgc::api::Dx12>().ok_or(
                InteropError::BackendMismatch {
                    expected: "Dx12",
                    actual: "non-Dx12",
                },
            )?;

            let d3d_device = hal_device.raw_device().clone();
            let mut resource: Option<windows::Win32::Graphics::Direct3D12::ID3D12Resource> = None;
            d3d_device
                .OpenSharedHandle(
                    windows::Win32::Foundation::HANDLE(frame.handle as *mut std::ffi::c_void),
                    &mut resource,
                )
                .map_err(|e| InteropError::Dx12(e.to_string()))?;
            let resource = resource
                .ok_or_else(|| InteropError::Dx12("OpenSharedHandle returned null".into()))?;

            let hal_texture = wgpu_hal::dx12::Device::texture_from_raw(
                resource,
                frame.format,
                wgpu::TextureDimension::D2,
                wgpu::Extent3d {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth_or_array_layers: 1,
                },
                1,
                1,
            );

            host.device.create_texture_from_hal::<wgpu::wgc::api::Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("scrying-dx12-shared-texture-import"),
                    size: wgpu::Extent3d {
                        width: frame.size.width,
                        height: frame.size.height,
                        depth_or_array_layers: 1,
                    },
                    format: frame.format,
                    dimension: wgpu::TextureDimension::D2,
                    mip_level_count: 1,
                    sample_count: 1,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
            )
        };

        return Ok(ImportedTexture {
            texture,
            format: frame.format,
            size: frame.size,
            generation: frame.generation,
            consumer_sync: frame.producer_sync,
        });
    }

    #[cfg(not(target_os = "windows"))]
    Err(InteropError::Unsupported(
        UnsupportedReason::HostBackendMismatch,
    ))
}

fn detect_backend(device: &wgpu::Device) -> InteropBackend {
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
        if device.as_hal::<wgpu::wgc::api::Vulkan>().is_some() {
            return InteropBackend::Vulkan;
        }

        #[cfg(target_vendor = "apple")]
        if device.as_hal::<wgpu::wgc::api::Metal>().is_some() {
            return InteropBackend::Metal;
        }

        #[cfg(target_os = "windows")]
        if device.as_hal::<wgpu::wgc::api::Dx12>().is_some() {
            return InteropBackend::Dx12;
        }
    }

    InteropBackend::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_synchronizer_accepts_none() {
        assert!(ImplicitOnlySynchronizer::validate(SyncMechanism::None).is_ok());
    }

    #[test]
    fn implicit_synchronizer_rejects_explicit_fence() {
        assert!(matches!(
            ImplicitOnlySynchronizer::validate(SyncMechanism::ExplicitFence),
            Err(InteropError::UnsupportedSynchronization(
                SyncMechanism::ExplicitFence
            ))
        ));
    }
}
