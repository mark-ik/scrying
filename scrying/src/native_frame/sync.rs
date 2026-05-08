use crate::native_frame::{ImportedTexture, InteropError, NativeFrame};

/// Describes how the producer signals that a frame is ready and how the
/// consumer signals that it has finished reading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SyncMechanism {
    None,
    /// An explicit Vulkan/Metal external semaphore is signalled by the
    /// producer. Reserved for future Linux/macOS producer paths.
    ExplicitExternalSemaphore,
    /// An explicit shared D3D12 fence is signalled by the producer.
    ExplicitFence,
}

/// Hook points called by [`WgpuTextureImporter`](crate::native_frame::WgpuTextureImporter)
/// around each frame import.
///
/// Implement this to add custom fence/semaphore logic. Two built-in
/// implementations are provided: [`NoopSynchronizer`] and
/// [`ImplicitOnlySynchronizer`]. Platform-specific synchronizers
/// (e.g. [`Dx12FenceSynchronizer`](crate::native_frame::Dx12FenceSynchronizer))
/// live alongside their producer paths.
pub trait InteropSynchronizer {
    /// Called after the frame is acquired from the producer, before import.
    /// Use this to wait on any producer-side signal.
    fn producer_complete(
        &self,
        frame: &NativeFrame,
        mechanism: SyncMechanism,
    ) -> Result<(), InteropError>;
    /// Called after the texture has been imported and is ready for the
    /// consumer. Use this to signal any consumer-side fence or semaphore.
    fn consumer_ready(
        &self,
        texture: &ImportedTexture,
        mechanism: SyncMechanism,
    ) -> Result<(), InteropError>;
}

/// A synchronizer that does nothing. Suitable when the caller manages all
/// synchronization externally (e.g. via a shared queue or explicit barriers).
#[derive(Default)]
pub struct NoopSynchronizer;

impl InteropSynchronizer for NoopSynchronizer {
    fn producer_complete(
        &self,
        _frame: &NativeFrame,
        _mechanism: SyncMechanism,
    ) -> Result<(), InteropError> {
        Ok(())
    }

    fn consumer_ready(
        &self,
        _texture: &ImportedTexture,
        _mechanism: SyncMechanism,
    ) -> Result<(), InteropError> {
        Ok(())
    }
}

/// Default synchronizer: accepts only [`SyncMechanism::None`]. The
/// keyed-mutex Windows path uses producer-side `IDXGIKeyedMutex` +
/// consumer-side transition-barrier flush, both of which are external to
/// this trait, so the synchronizer just sees `None`.
#[derive(Default)]
pub struct ImplicitOnlySynchronizer;

impl InteropSynchronizer for ImplicitOnlySynchronizer {
    fn producer_complete(
        &self,
        _frame: &NativeFrame,
        mechanism: SyncMechanism,
    ) -> Result<(), InteropError> {
        Self::validate(mechanism)
    }

    fn consumer_ready(
        &self,
        _texture: &ImportedTexture,
        mechanism: SyncMechanism,
    ) -> Result<(), InteropError> {
        Self::validate(mechanism)
    }
}

impl ImplicitOnlySynchronizer {
    pub(crate) fn validate(mechanism: SyncMechanism) -> Result<(), InteropError> {
        match mechanism {
            SyncMechanism::None => Ok(()),
            other => Err(InteropError::UnsupportedSynchronization(other)),
        }
    }
}
