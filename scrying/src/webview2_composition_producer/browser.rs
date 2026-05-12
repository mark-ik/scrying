use super::*;

impl WebView2CompositionProducer {
    pub fn find_in_page(
        &self,
        query: &str,
        options: WebView2FindOptions,
    ) -> Result<(), WebSurfaceError> {
        if let Ok(mut slot) = self.pending_find.lock() {
            *slot = None;
        }
        let environment15 = self
            .environment
            .cast::<ICoreWebView2Environment15>()
            .map_err(platform("environment.cast<ICoreWebView2Environment15>"))?;
        let find_options = unsafe { environment15.CreateFindOptions() }
            .map_err(platform("Environment15.CreateFindOptions"))?;
        let query = CoTaskMemPWSTR::from(query);
        unsafe {
            find_options
                .SetFindTerm(*query.as_ref().as_pcwstr())
                .map_err(platform("FindOptions.SetFindTerm"))?;
            find_options
                .SetIsCaseSensitive(options.case_sensitive)
                .map_err(platform("FindOptions.SetIsCaseSensitive"))?;
            find_options
                .SetShouldHighlightAllMatches(options.highlight_all_matches)
                .map_err(platform("FindOptions.SetShouldHighlightAllMatches"))?;
            find_options
                .SetShouldMatchWord(options.match_word)
                .map_err(platform("FindOptions.SetShouldMatchWord"))?;
            find_options
                .SetSuppressDefaultFindDialog(options.suppress_default_find_dialog)
                .map_err(platform("FindOptions.SetSuppressDefaultFindDialog"))?;
        }

        let webview28 = self
            .webview
            .cast::<ICoreWebView2_28>()
            .map_err(platform("webview.cast<ICoreWebView2_28>"))?;
        let find = unsafe { webview28.Find() }.map_err(platform("WebView2_28.Find"))?;
        let pending = self.pending_find.clone();
        let find_for_completion = find.clone();
        let handler =
            FindStartCompletedHandler::create(Box::new(move |result: windows::core::Result<()>| {
                let next = result
                    .map_err(|err| err.message().to_string())
                    .and_then(|()| unsafe {
                        let mut match_count = 0i32;
                        let mut active_match_index = 0i32;
                        find_for_completion
                            .MatchCount(&mut match_count)
                            .map_err(|err| err.message().to_string())?;
                        find_for_completion
                            .ActiveMatchIndex(&mut active_match_index)
                            .map_err(|err| err.message().to_string())?;
                        Ok(WebView2FindResult {
                            matched: match_count > 0,
                            active_match_index,
                            match_count,
                        })
                    });
                if let Ok(mut slot) = pending.lock() {
                    *slot = Some(next);
                }
                Ok(())
            }));
        unsafe { find.Start(&find_options, &handler) }.map_err(platform("Find.Start"))?;
        if options.backwards {
            unsafe { find.FindPrevious() }.map_err(platform("Find.FindPrevious"))?;
        }
        Ok(())
    }

    pub fn poll_find_match(&self) -> Option<Result<WebView2FindResult, String>> {
        self.pending_find
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    pub fn stop_find(&self) -> Result<(), WebSurfaceError> {
        let webview28 = self
            .webview
            .cast::<ICoreWebView2_28>()
            .map_err(platform("webview.cast<ICoreWebView2_28>"))?;
        let find = unsafe { webview28.Find() }.map_err(platform("WebView2_28.Find"))?;
        unsafe { find.Stop() }.map_err(platform("Find.Stop"))
    }

    pub fn request_pdf(&self) -> Result<(), WebSurfaceError> {
        if let Ok(mut slot) = self.pending_pdf.lock() {
            *slot = None;
        }
        let environment6 = self
            .environment
            .cast::<ICoreWebView2Environment6>()
            .map_err(platform("environment.cast<ICoreWebView2Environment6>"))?;
        let print_settings = unsafe { environment6.CreatePrintSettings() }
            .map_err(platform("Environment6.CreatePrintSettings"))?;
        let webview16 = self
            .webview
            .cast::<ICoreWebView2_16>()
            .map_err(platform("webview.cast<ICoreWebView2_16>"))?;
        let pending = self.pending_pdf.clone();
        let handler = PrintToPdfStreamCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>, stream: Option<IStream>| {
                let next = result
                    .map_err(|err| err.message().to_string())
                    .and_then(|()| {
                        let stream = stream
                            .ok_or_else(|| "PrintToPdfStream returned no stream".to_string())?;
                        stream_to_bytes(&stream).map_err(|err| err.message().to_string())
                    });
                if let Ok(mut slot) = pending.lock() {
                    *slot = Some(next);
                }
                Ok(())
            },
        ));
        unsafe { webview16.PrintToPdfStream(&print_settings, &handler) }
            .map_err(platform("WebView2_16.PrintToPdfStream"))
    }

    pub fn poll_pdf(&self) -> Option<Result<Vec<u8>, String>> {
        self.pending_pdf
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
    }

    pub fn print(&self) -> Result<bool, WebSurfaceError> {
        let webview16 = self
            .webview
            .cast::<ICoreWebView2_16>()
            .map_err(platform("webview.cast<ICoreWebView2_16>"))?;
        unsafe { webview16.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER) }
            .map_err(platform("WebView2_16.ShowPrintUI"))?;
        Ok(true)
    }
}

pub(super) fn install_context_menu_bridge(webview: &ICoreWebView2) -> Result<(), WebSurfaceError> {
    let script = format!(
        r##"(() => {{
            if (window.__scryingContextMenuBridgeInstalled) return;
            Object.defineProperty(window, "__scryingContextMenuBridgeInstalled", {{ value: true }});
            const prefix = {prefix:?};
            const clean = value => String(value || "").replace(/[\t\r\n]/g, " ");
            const closest = (node, selector) => {{
                for (let current = node; current && current !== document; current = current.parentElement) {{
                    if (current.matches && current.matches(selector)) return current;
                }}
                return null;
            }};
            window.addEventListener("contextmenu", event => {{
                const target = event.target;
                const link = closest(target, "a[href]");
                const image = closest(target, "img[src]");
                const payload = [
                    clean(location.href),
                    Math.round(event.clientX),
                    Math.round(event.clientY),
                    clean(link && link.href),
                    clean(image && image.src),
                ].join("\t");
                try {{ window.chrome.webview.postMessage(prefix + payload); }} catch (_) {{}}
            }}, true);
        }})()"##,
        prefix = CONTEXT_MENU_BRIDGE_PREFIX,
    );
    add_script_to_execute_on_document_created_blocking(webview, script)
}

pub(super) fn install_drop_detected_bridge(webview: &ICoreWebView2) -> Result<(), WebSurfaceError> {
    let script = format!(
        r##"(() => {{
            if (window.__scryingDropBridgeInstalled) return;
            Object.defineProperty(window, "__scryingDropBridgeInstalled", {{ value: true }});
            const prefix = {prefix:?};
            const clean = value => String(value || "").replace(/[\t\r\n]/g, " ");
            window.addEventListener("drop", event => {{
                const dt = event.dataTransfer;
                if (!dt) return;
                const types = Array.from(dt.types || []);
                const hasFiles = dt.files && dt.files.length > 0;
                const hasUri = types.includes("text/uri-list") || !!dt.getData("text/uri-list");
                const hasImage = types.some(type => String(type).toLowerCase().startsWith("image/"));
                if (!hasFiles && !hasUri && !hasImage) return;
                let primaryUrl = dt.getData("text/uri-list") || "";
                if (primaryUrl.includes("\n")) primaryUrl = primaryUrl.split(/\r?\n/).find(line => line && !line.startsWith("#")) || "";
                if (!primaryUrl) primaryUrl = dt.getData("text/plain") || "";
                const payload = [
                    Math.round(event.clientX),
                    Math.round(event.clientY),
                    dt.files ? dt.files.length : 0,
                    clean(primaryUrl),
                ].join("\t");
                try {{ window.chrome.webview.postMessage(prefix + payload); }} catch (_) {{}}
            }}, true);
        }})()"##,
        prefix = DROP_DETECTED_BRIDGE_PREFIX,
    );
    add_script_to_execute_on_document_created_blocking(webview, script)
}

pub(super) fn install_media_capture_bridge(webview: &ICoreWebView2) -> Result<(), WebSurfaceError> {
    let script = format!(
        r#"(() => {{
            if (window.__scryingMediaCaptureBridgeInstalled) return;
            Object.defineProperty(window, "__scryingMediaCaptureBridgeInstalled", {{ value: true }});
            const prefix = {prefix:?};
            const tracks = new Set();
            const publish = () => {{
                let audio = 0;
                let video = 0;
                for (const track of Array.from(tracks)) {{
                    if (!track || track.readyState === "ended") {{ tracks.delete(track); continue; }}
                    if (track.kind === "audio") audio += 1;
                    if (track.kind === "video") video += 1;
                }}
                try {{ window.chrome.webview.postMessage(`${{prefix}}audio:${{audio}},video:${{video}}`); }} catch (_) {{}}
            }};
            const attach = stream => {{
                if (!stream || !stream.getTracks) return stream;
                for (const track of stream.getTracks()) {{
                    tracks.add(track);
                    track.addEventListener("ended", publish, {{ once: true }});
                }}
                publish();
                return stream;
            }};
            if (navigator.mediaDevices && navigator.mediaDevices.getUserMedia) {{
                const originalGetUserMedia = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
                navigator.mediaDevices.getUserMedia = async constraints => attach(await originalGetUserMedia(constraints));
            }}
        }})()"#,
        prefix = MEDIA_CAPTURE_BRIDGE_PREFIX,
    );
    add_script_to_execute_on_document_created_blocking(webview, script)
}

pub(super) fn register_context_menu_requested_handler(
    webview: &ICoreWebView2,
    nav_queue: Arc<Mutex<VecDeque<NavigationEvent>>>,
    default_context_menus_enabled: Arc<Mutex<bool>>,
) -> Result<i64, WebSurfaceError> {
    let webview11 = webview
        .cast::<ICoreWebView2_11>()
        .map_err(platform("webview.cast<ICoreWebView2_11>"))?;
    let handler = ContextMenuRequestedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            let mut point = POINT::default();
            let _ = unsafe { args.Location(&mut point) };
            let mut page_url = String::new();
            let mut link_url = None;
            let mut image_url = None;
            if let Ok(target) = unsafe { args.ContextMenuTarget() } {
                page_url = unsafe { read_pwstr_from(|out| target.PageUri(out)) };
                if unsafe { read_bool_from(|out| target.HasLinkUri(out)) } {
                    let uri = unsafe { read_pwstr_from(|out| target.LinkUri(out)) };
                    if !uri.is_empty() {
                        link_url = Some(uri);
                    }
                }
                if unsafe { read_bool_from(|out| target.HasSourceUri(out)) } {
                    let uri = unsafe { read_pwstr_from(|out| target.SourceUri(out)) };
                    if !uri.is_empty() {
                        image_url = Some(uri);
                    }
                }
            }
            let allow_default_menu = default_context_menus_enabled
                .lock()
                .map(|enabled| *enabled)
                .unwrap_or(false);
            unsafe { args.SetHandled(!allow_default_menu)? };
            if let Ok(mut q) = nav_queue.lock() {
                q.push_back(NavigationEvent::ContextMenuRequested {
                    page_url,
                    x: point.x as f64,
                    y: point.y as f64,
                    link_url,
                    image_url,
                });
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { webview11.add_ContextMenuRequested(&handler, &mut token) }
        .map_err(platform("add_ContextMenuRequested"))?;
    Ok(token)
}

pub(super) fn register_web_resource_response_received_handler(
    webview: &ICoreWebView2,
    cookie_change_handler: Arc<Mutex<Option<WebView2CookieChangeHandlerFn>>>,
) -> Result<i64, WebSurfaceError> {
    let webview2 = webview
        .cast::<ICoreWebView2_2>()
        .map_err(platform("webview.cast<ICoreWebView2_2>"))?;
    let handler = WebResourceResponseReceivedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args
            && let Ok(response) = unsafe { args.Response() }
            && let Ok(headers) = unsafe { response.Headers() }
        {
            if response_headers_have_set_cookie(&headers)
                && let Ok(slot) = cookie_change_handler.lock()
                && let Some(handler) = slot.as_ref()
            {
                handler();
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe { webview2.add_WebResourceResponseReceived(&handler, &mut token) }
        .map_err(platform("add_WebResourceResponseReceived"))?;
    Ok(token)
}

fn response_headers_have_set_cookie(
    headers: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2HttpResponseHeaders,
) -> bool {
    let name = CoTaskMemPWSTR::from("Set-Cookie");
    let mut contains = windows::core::BOOL::default();
    if unsafe { headers.Contains(*name.as_ref().as_pcwstr(), &mut contains) }.is_ok()
        && contains.as_bool()
    {
        return true;
    }
    let Ok(iterator) = (unsafe { headers.GetIterator() }) else {
        return false;
    };
    loop {
        let mut has_current = windows::core::BOOL::default();
        if unsafe { iterator.HasCurrentHeader(&mut has_current) }.is_err() || !has_current.as_bool()
        {
            return false;
        }
        let mut header_name = PWSTR::null();
        let mut header_value = PWSTR::null();
        if unsafe { iterator.GetCurrentHeader(&mut header_name, &mut header_value) }.is_err() {
            return false;
        }
        let header_name = unsafe { consume_pwstr(header_name) };
        let _ = unsafe { consume_pwstr(header_value) };
        if header_name.eq_ignore_ascii_case("set-cookie") {
            return true;
        }
        let mut has_next = windows::core::BOOL::default();
        if unsafe { iterator.MoveNext(&mut has_next) }.is_err() || !has_next.as_bool() {
            return false;
        }
    }
}

pub(super) fn parse_media_capture_bridge_message(message: &str) -> Option<NavigationEvent> {
    let payload = message.strip_prefix(MEDIA_CAPTURE_BRIDGE_PREFIX)?;
    let mut audio_active_tracks = 0u32;
    let mut video_active_tracks = 0u32;
    for part in payload.split(',') {
        if let Some(value) = part.strip_prefix("audio:") {
            audio_active_tracks = value.parse().ok()?;
        } else if let Some(value) = part.strip_prefix("video:") {
            video_active_tracks = value.parse().ok()?;
        }
    }
    Some(NavigationEvent::MediaCaptureStateChanged {
        audio_active_tracks,
        video_active_tracks,
    })
}

pub(super) fn parse_context_menu_bridge_message(message: &str) -> Option<NavigationEvent> {
    let payload = message.strip_prefix(CONTEXT_MENU_BRIDGE_PREFIX)?;
    let mut parts = payload.splitn(5, '\t');
    let page_url = parts.next()?.to_string();
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let link_url = optional_bridge_string(parts.next()?);
    let image_url = optional_bridge_string(parts.next().unwrap_or_default());
    Some(NavigationEvent::ContextMenuRequested {
        page_url,
        x,
        y,
        link_url,
        image_url,
    })
}

pub(super) fn parse_drop_detected_bridge_message(message: &str) -> Option<NavigationEvent> {
    let payload = message.strip_prefix(DROP_DETECTED_BRIDGE_PREFIX)?;
    let mut parts = payload.splitn(4, '\t');
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let file_count = parts.next()?.parse().ok()?;
    let primary_url = optional_bridge_string(parts.next().unwrap_or_default());
    Some(NavigationEvent::DropDetected {
        x,
        y,
        file_count,
        primary_url,
    })
}

fn optional_bridge_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
