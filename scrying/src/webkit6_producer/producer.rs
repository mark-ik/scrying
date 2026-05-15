//! [`WebKit6Producer`] struct, construction, Drop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dpi::PhysicalSize;
use webkit6::gtk;
use webkit6::gtk::prelude::*;
use webkit6::prelude::*;
use webkit6::{WebContext, WebView, WebsiteDataManager};

use crate::{WebSurfaceCapabilities, WebSurfaceError};

use super::config::WebKit6ProducerConfig;
use super::helpers::ensure_gtk_init;
use super::navigation::{NavState, install_load_signals};

/// Linux WebKitGTK 6.0 producer. Hosts a `WebKitWebView` inside a
/// hidden top-level `gtk4::Window` and emits CPU RGBA snapshots via
/// `webkit_web_view_get_snapshot` (→ `gdk::Texture`).
pub struct WebKit6Producer {
    pub(crate) capabilities: WebSurfaceCapabilities,
    pub(crate) webview: WebView,
    pub(crate) window: gtk::Window,
    pub(crate) _context: WebContext,
    pub(crate) _data_manager: WebsiteDataManager,
    pub(crate) size: PhysicalSize<u32>,
    pub(crate) offset: (f32, f32),
    pub(crate) generation: Cell<u64>,
    pub(crate) nav_state: Rc<RefCell<NavState>>,
}

impl WebKit6Producer {
    /// Construct the producer. Initializes GTK 4 if needed (must be
    /// on the GTK main thread), creates a persistent `WebContext`
    /// rooted at `config.data_dir`, builds the WebView inside a
    /// hidden top-level `gtk4::Window`, and wires load-event
    /// signal handlers.
    pub fn new(config: WebKit6ProducerConfig) -> Result<Self, WebSurfaceError> {
        ensure_gtk_init()?;

        if config.size.width == 0 || config.size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WebKit6 producer size must be non-zero, got {}x{}",
                config.size.width, config.size.height
            )));
        }
        std::fs::create_dir_all(&config.data_dir).map_err(|err| {
            WebSurfaceError::Platform(format!(
                "could not create data_dir {:?}: {err}",
                config.data_dir
            ))
        })?;

        let data_dir_str = config.data_dir.to_string_lossy().to_string();
        let data_manager = WebsiteDataManager::builder()
            .base_data_directory(&data_dir_str)
            .base_cache_directory(&data_dir_str)
            .build();
        let context = WebContext::builder()
            .website_data_manager(&data_manager)
            .build();
        let webview = WebView::builder().web_context(&context).build();

        // GTK 4 has no GtkOffscreenWindow — use a hidden top-level
        // window. WebKit's GPU process renders independently of
        // widget visibility, so snapshots still work while
        // `present()` is never called.
        let window = gtk::Window::new();
        window.set_decorated(false);
        window.set_default_size(config.size.width as i32, config.size.height as i32);
        webview.set_size_request(config.size.width as i32, config.size.height as i32);
        window.set_child(Some(&webview));
        // Force widget hierarchy to realize without becoming visible.
        // `realize()` allocates the GdkSurface so WebKit has something
        // to render against.
        window.realize();

        let nav_state = Rc::new(RefCell::new(NavState::default()));
        install_load_signals(&webview, &nav_state);

        Ok(Self {
            capabilities: super::linux_webkit6_capabilities(),
            webview,
            window,
            _context: context,
            _data_manager: data_manager,
            size: config.size,
            offset: config.offset,
            generation: Cell::new(0),
            nav_state,
        })
    }

    pub(crate) fn next_generation(&self) -> u64 {
        let next = self.generation.get().saturating_add(1);
        self.generation.set(next);
        next
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }
}
