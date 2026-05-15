//! [`WebSurfaceProducer`] trait implementation for [`WebKitGtkProducer`].

use std::time::Duration;

use dpi::PhysicalSize;
use gtk::prelude::*;
use webkit2gtk::{SettingsExt, WebInspectorExt, WebViewExt};

use crate::{
    NavigationEvent, WebSurfaceCapabilities, WebSurfaceError, WebSurfaceFrame, WebSurfaceMode,
    WebSurfaceProducer, WebSurfaceSettings,
};

use super::navigation::{arm_navigation, wait_for_load};
use super::producer::WebKitGtkProducer;
use super::script_message;

impl WebSurfaceProducer for WebKitGtkProducer {
    fn capabilities(&self) -> WebSurfaceCapabilities {
        self.capabilities.clone()
    }

    fn mode(&self) -> WebSurfaceMode {
        self.capabilities.preferred_mode
    }

    fn acquire_frame(&mut self) -> Result<WebSurfaceFrame, WebSurfaceError> {
        self.capture_cpu_snapshot()
    }

    fn navigate_to_string(&mut self, html: &str, timeout: Duration) -> Result<(), WebSurfaceError> {
        arm_navigation(&self.nav_state);
        self.webview.load_html(html, None);
        wait_for_load(&self.nav_state, timeout)
    }

    fn navigate_to_url(&mut self, url: &str, timeout: Duration) -> Result<(), WebSurfaceError> {
        arm_navigation(&self.nav_state);
        self.webview.load_uri(url);
        wait_for_load(&self.nav_state, timeout)
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WebSurfaceError> {
        if size.width == 0 || size.height == 0 {
            return Err(WebSurfaceError::Platform(format!(
                "WebKitGTK producer size must be non-zero, got {}x{}",
                size.width, size.height
            )));
        }
        self.size = size;
        self.offscreen.resize(size.width as i32, size.height as i32);
        self.webview
            .set_size_request(size.width as i32, size.height as i32);
        Ok(())
    }

    fn set_offset(&mut self, x: f32, y: f32) -> Result<(), WebSurfaceError> {
        self.offset = (x, y);
        Ok(())
    }

    fn reload(&mut self) -> Result<(), WebSurfaceError> {
        self.webview.reload();
        Ok(())
    }

    fn stop(&mut self) -> Result<(), WebSurfaceError> {
        self.webview.stop_loading();
        Ok(())
    }

    fn go_back(&mut self) -> Result<bool, WebSurfaceError> {
        if self.webview.can_go_back() {
            self.webview.go_back();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn go_forward(&mut self) -> Result<bool, WebSurfaceError> {
        if self.webview.can_go_forward() {
            self.webview.go_forward();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn can_go_back(&self) -> bool {
        self.webview.can_go_back()
    }

    fn can_go_forward(&self) -> bool {
        self.webview.can_go_forward()
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        self.nav_state.borrow_mut().events.pop_front()
    }

    fn capture_snapshot_png(&mut self) -> Result<Vec<u8>, WebSurfaceError> {
        WebKitGtkProducer::capture_snapshot_png(self)
    }

    fn apply_settings(&mut self, settings: &WebSurfaceSettings) -> Result<(), WebSurfaceError> {
        if let Some(zoom) = settings.zoom_factor {
            self.webview.set_zoom_level(zoom);
        }
        // `WebKitWebView::settings()` returns `Option<Settings>`; in
        // practice the view always has a `Settings` instance, but be
        // defensive.
        // Explicit trait dispatch: `gtk::WidgetExt::settings()` and
        // `WebViewExt::settings()` both match — pick the WebKit one.
        if let Some(view_settings) = WebViewExt::settings(&self.webview) {
            if let Some(js_enabled) = settings.javascript_enabled {
                view_settings.set_enable_javascript(js_enabled);
            }
            if let Some(devtools_enabled) = settings.devtools_enabled {
                view_settings.set_enable_developer_extras(devtools_enabled);
            }
            if let Some(ua) = settings.user_agent.as_deref() {
                view_settings.set_user_agent(Some(ua));
            }
            // `default_context_menus_enabled`, `builtin_accelerator_keys_enabled`,
            // and `inactive_scheduling_policy` don't map onto
            // WebKitGTK 4.1 settings cleanly — left silently
            // unsupported for now (matches the trait contract:
            // unsupported fields ignored).
        }
        Ok(())
    }

    fn post_web_message(&mut self, message: &str) -> Result<(), WebSurfaceError> {
        let js = format!(
            "if (window.chrome && window.chrome.webview && window.chrome.webview.__scryDispatch) {{ \
                 window.chrome.webview.__scryDispatch({}); \
             }}",
            script_message::escape_for_js(message)
        );
        // `evaluate_javascript` supersedes `run_javascript` from
        // WebKitGTK 2.40+; the `webkit2gtk` crate gates it on the
        // `v2_40` feature, which we have enabled. Default world,
        // no source-URI tagging — this is host-driven dispatch, not
        // page code.
        self.webview.evaluate_javascript(
            &js,
            None,
            None,
            webkit2gtk::gio::Cancellable::NONE,
            |_| { /* fire-and-forget — pages without listeners are not an error */ },
        );
        Ok(())
    }

    fn poll_web_message(&mut self) -> Option<String> {
        self.web_messages.borrow_mut().pop_front()
    }

    fn open_devtools_window(&mut self) -> Result<(), WebSurfaceError> {
        // Explicit trait dispatch: `gtk::WidgetExt::settings()` and
        // `WebViewExt::settings()` both match — pick the WebKit one.
        if let Some(view_settings) = WebViewExt::settings(&self.webview) {
            // Inspector is gated on enable-developer-extras; toggle
            // it on automatically so a host call to
            // `open_devtools_window` Just Works without a prior
            // `apply_settings({ devtools_enabled: Some(true) })`.
            view_settings.set_enable_developer_extras(true);
        }
        match self.webview.inspector() {
            Some(inspector) => {
                inspector.show();
                Ok(())
            }
            None => Err(WebSurfaceError::Platform(
                "WebKitGTK WebView has no inspector".into(),
            )),
        }
    }
}
