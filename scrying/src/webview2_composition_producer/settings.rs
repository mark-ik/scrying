use super::*;

impl WebView2CompositionProducer {
    /// Open the WebView2 DevTools window.
    pub fn open_devtools_window(&self) -> Result<(), WebSurfaceError> {
        unsafe { self.webview.OpenDevToolsWindow() }.map_err(platform("OpenDevToolsWindow"))
    }

    /// Toggle WebView2's page visibility / occlusion state. Browser-shape
    /// consumers call this as tabs become active or inactive.
    pub fn set_visible(&self, visible: bool) -> Result<(), WebSurfaceError> {
        unsafe { self.controller.SetIsVisible(visible) }
            .map_err(platform("controller.SetIsVisible"))
    }

    /// Apply a partial settings update. `None` fields are left at their
    /// current value.
    pub fn apply_settings(
        &self,
        settings: &crate::WebSurfaceSettings,
    ) -> Result<(), WebSurfaceError> {
        if let Some(zoom) = settings.zoom_factor {
            unsafe { self.controller.SetZoomFactor(zoom) }
                .map_err(platform("controller.SetZoomFactor"))?;
        }
        let webview_settings: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings =
            unsafe { self.webview.Settings() }.map_err(platform("webview.Settings"))?;
        if let Some(enabled) = settings.javascript_enabled {
            unsafe { webview_settings.SetIsScriptEnabled(enabled) }
                .map_err(platform("Settings.SetIsScriptEnabled"))?;
        }
        if let Some(enabled) = settings.devtools_enabled {
            unsafe { webview_settings.SetAreDevToolsEnabled(enabled) }
                .map_err(platform("Settings.SetAreDevToolsEnabled"))?;
        }
        if let Some(enabled) = settings.default_context_menus_enabled {
            if let Ok(mut slot) = self.default_context_menus_enabled.lock() {
                *slot = enabled;
            }
            unsafe { webview_settings.SetAreDefaultContextMenusEnabled(enabled) }
                .map_err(platform("Settings.SetAreDefaultContextMenusEnabled"))?;
        }
        if let Some(enabled) = settings.builtin_accelerator_keys_enabled {
            let settings3: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3 =
                webview_settings
                    .cast()
                    .map_err(platform("Settings cast to ICoreWebView2Settings3"))?;
            unsafe { settings3.SetAreBrowserAcceleratorKeysEnabled(enabled) }
                .map_err(platform("Settings3.SetAreBrowserAcceleratorKeysEnabled"))?;
        }
        if let Some(ref ua) = settings.user_agent {
            let settings2: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings2 =
                webview_settings
                    .cast()
                    .map_err(platform("Settings cast to ICoreWebView2Settings2"))?;
            let ua = CoTaskMemPWSTR::from(ua.as_str());
            unsafe { settings2.SetUserAgent(*ua.as_ref().as_pcwstr()) }
                .map_err(platform("Settings2.SetUserAgent"))?;
        }
        Ok(())
    }
}
