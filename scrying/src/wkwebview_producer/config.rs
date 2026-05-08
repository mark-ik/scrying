//! Configuration struct for [`super::WkWebViewProducer::new`] /
//! [`super::WkWebViewProducer::new_with_url_schemes`]. Mirrors the shape
//! of `WebView2CompositionConfig` on Windows so consumers can write
//! cross-platform setup with minimal cfg-gating.

use std::path::PathBuf;

use dpi::PhysicalSize;

#[derive(Clone, Debug)]
pub struct WkWebViewProducerConfig {
    /// Initial size of the WKWebView frame and the capture region, in
    /// physical pixels.
    pub size: PhysicalSize<u32>,
    /// Offset of the WKWebView relative to the parent NSView, in
    /// device-independent points (matches AppKit's coordinate system).
    pub offset: (f32, f32),
    /// Directory used as `WKWebsiteDataStore`'s persistent storage.
    /// Hashed into a deterministic UUID and resolved via
    /// `WKWebsiteDataStore::dataStoreForIdentifier:` (macOS 14+).
    /// Empty path falls back to the shared default store. Ignored
    /// when [`Self::non_persistent`] is `true`.
    pub data_dir: PathBuf,
    /// When `true`, use `WKWebsiteDataStore::nonPersistentDataStore`
    /// — cookies / local storage / IndexedDB live only for the
    /// lifetime of the producer and are wiped on `Drop`. The
    /// "incognito tab" / "private window" mode for browser-shape
    /// consumers. Mutually exclusive with [`Self::data_dir`]; when
    /// both are provided, `non_persistent` wins.
    pub non_persistent: bool,
    /// Timeout for `navigate_to_string`, mirroring the Windows
    /// producer's navigation completion wait.
    pub navigation_timeout: std::time::Duration,
    /// Timeout for the initial frame after `start_capture`. Mirrors the
    /// Windows producer's first-frame block.
    pub frame_timeout: std::time::Duration,
    /// Directory where WebKit-managed downloads are written. Each
    /// download lands at `<download_dir>/<suggested_filename>` (with
    /// numeric suffixes appended on collision).
    pub download_dir: PathBuf,
}

impl WkWebViewProducerConfig {
    pub fn new(size: PhysicalSize<u32>, data_dir: impl Into<PathBuf>) -> Self {
        let data_dir: PathBuf = data_dir.into();
        let download_dir = data_dir.join("downloads");
        Self {
            size,
            offset: (0.0, 0.0),
            data_dir,
            navigation_timeout: std::time::Duration::from_secs(5),
            frame_timeout: std::time::Duration::from_secs(2),
            download_dir,
            non_persistent: false,
        }
    }

    /// Switch this config into incognito / non-persistent mode.
    /// Equivalent to setting `non_persistent = true`. Cookie /
    /// local-storage / IndexedDB activity for this producer doesn't
    /// touch any persistent store and is wiped on `Drop`.
    pub fn non_persistent(mut self) -> Self {
        self.non_persistent = true;
        self
    }

    pub fn with_offset(mut self, x: f32, y: f32) -> Self {
        self.offset = (x, y);
        self
    }
}
