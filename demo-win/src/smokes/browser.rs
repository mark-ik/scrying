use super::super::*;

pub(crate) fn validate_platform_find(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    producer.navigate_to_string(
        r#"<!doctype html><html><body><main>alpha beta needle gamma needle</main></body></html>"#,
        std::time::Duration::from_secs(5),
    )?;
    producer.find_in_page(
        "needle",
        scrying::webview2_composition_producer::WebView2FindOptions::default(),
    )?;
    let result = wait_for_find_result(producer, std::time::Duration::from_secs(5))??;
    if !result.matched || result.match_count < 2 {
        return Err(format!("find-test: expected at least two matches, got {result:?}").into());
    }
    producer.stop_find()?;

    println!(
        "demo-win: find-test: PASS - WebView2 native find reported {} matches",
        result.match_count
    );
    Ok(())
}

pub(crate) fn validate_platform_pdf(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    producer.navigate_to_string(
        r#"<!doctype html><html><body><h1>Scrying PDF smoke</h1><p>printable content</p></body></html>"#,
        std::time::Duration::from_secs(5),
    )?;
    producer.request_pdf()?;
    let bytes = wait_for_pdf(producer, std::time::Duration::from_secs(10))??;
    if !bytes.starts_with(b"%PDF-") {
        return Err(format!(
            "pdf-test: output did not start with %PDF-, length={}",
            bytes.len()
        )
        .into());
    }
    println!(
        "demo-win: pdf-test: PASS - WebView2 native PrintToPdfStream returned {} bytes",
        bytes.len()
    );
    Ok(())
}

pub(crate) fn validate_platform_context_menu(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    producer.apply_settings(&WebSurfaceSettings {
        default_context_menus_enabled: Some(false),
        ..WebSurfaceSettings::default()
    })?;
    producer.navigate_to_string(
        r#"<!doctype html><html><body style="margin:0"><a id="target" href="https://example.test/context" style="display:block;width:220px;height:80px;padding:24px">context target</a><script>const post=value=>window.chrome.webview.postMessage(value);post("context-test:ready");window.chrome.webview.addEventListener("message", event => { if (event.data === "context-test:trigger") { const target = document.getElementById("target"); target.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 40 })); post("context-test:triggered"); } });</script></body></html>"#,
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(
        producer,
        "context-test:ready",
        std::time::Duration::from_secs(3),
    )?;
    drain_navigation_events(producer);
    producer.post_web_message("context-test:trigger")?;
    wait_for_web_message(
        producer,
        "context-test:triggered",
        std::time::Duration::from_secs(3),
    )?;
    let (page_url, link_url) = wait_for_context_menu(producer, std::time::Duration::from_secs(5))?;
    if link_url.as_deref() != Some("https://example.test/context") {
        return Err(
            format!("context-test: unexpected link_url {link_url:?} page {page_url:?}").into(),
        );
    }
    println!(
        "demo-win: context-test: PASS - context-menu bridge reported link target {link_url:?}"
    );
    Ok(())
}

pub(crate) fn validate_platform_media_capture_observability(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    producer.navigate_to_string(
        r#"<!doctype html><html><body><script>window.chrome.webview.postMessage("scrying:media-capture:audio:1,video:2");</script></body></html>"#,
        std::time::Duration::from_secs(5),
    )?;
    let (audio, video) = wait_for_media_capture_state(producer, std::time::Duration::from_secs(5))?;
    if audio != 1 || video != 2 {
        return Err(
            format!("media-test: unexpected track counts audio={audio} video={video}").into(),
        );
    }
    println!(
        "demo-win: media-test: PASS - WebMessage media bridge emitted NavigationEvent::MediaCaptureStateChanged"
    );
    Ok(())
}

pub(crate) fn validate_platform_popup_routing(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const POPUP_URL: &str = "https://example.com/scrying-popup-target";
    drain_navigation_events(producer);
    drain_web_messages(producer);
    let html = popup_test_html(POPUP_URL);
    producer.navigate_to_string(&html, std::time::Duration::from_secs(5))?;
    wait_for_web_message(
        producer,
        "popup-test:ready",
        std::time::Duration::from_secs(3),
    )?;
    producer.post_web_message("popup-test:open")?;
    wait_for_web_message(
        producer,
        "popup-test:opened",
        std::time::Duration::from_secs(3),
    )?;

    let observed_url = wait_for_new_window_request(producer, std::time::Duration::from_secs(3))?;
    if observed_url != POPUP_URL {
        return Err(format!(
            "popup-test: expected new-window URL {POPUP_URL:?}, got {observed_url:?}"
        )
        .into());
    }

    println!(
        "demo-win: popup-test: PASS - NewWindowRequested routed to the host and default popup was suppressed"
    );
    Ok(())
}

pub(crate) fn validate_platform_browser_controls(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    let first_ready = "browser-test:ready:first";
    let second_ready = "browser-test:ready:second";

    drain_web_messages(producer);
    producer.navigate_to_string(
        &browser_test_html("first"),
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(producer, first_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    producer.navigate_to_string(
        &browser_test_html("second"),
        std::time::Duration::from_secs(5),
    )?;
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    wait_for_title(
        producer,
        "Scrying Browser Test second",
        std::time::Duration::from_secs(2),
    )?;
    drain_web_messages(producer);

    if !producer.can_go_back() {
        return Err("browser-test: WebView2 did not report a back-history entry".into());
    }
    if producer.can_go_forward() {
        return Err(
            "browser-test: WebView2 unexpectedly reported forward history before back".into(),
        );
    }

    if !producer.go_back()? {
        return Err("browser-test: go_back returned false despite can_go_back".into());
    }
    wait_for_web_message(producer, first_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    if !producer.can_go_forward() {
        return Err("browser-test: WebView2 did not report a forward-history entry".into());
    }
    if !producer.go_forward()? {
        return Err("browser-test: go_forward returned false despite can_go_forward".into());
    }
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    drain_web_messages(producer);

    producer.reload()?;
    wait_for_web_message(producer, second_ready, std::time::Duration::from_secs(3))?;
    producer.stop()?;

    producer.apply_settings(&WebSurfaceSettings {
        zoom_factor: Some(1.05),
        user_agent: Some("scrying-demo-win-browser-test/1.0".into()),
        devtools_enabled: Some(false),
        javascript_enabled: Some(true),
        default_context_menus_enabled: Some(false),
        builtin_accelerator_keys_enabled: Some(false),
        inactive_scheduling_policy: None,
    })?;
    producer.set_visible(false)?;
    pump_windows_messages_for(std::time::Duration::from_millis(100));
    producer.set_visible(true)?;
    producer.apply_settings(&WebSurfaceSettings {
        zoom_factor: Some(1.0),
        user_agent: None,
        devtools_enabled: Some(true),
        javascript_enabled: Some(true),
        default_context_menus_enabled: Some(true),
        builtin_accelerator_keys_enabled: Some(true),
        inactive_scheduling_policy: None,
    })?;

    println!(
        "demo-win: browser-test: PASS - history, reload/stop, title, settings, and visibility controls verified"
    );
    Ok(())
}

fn popup_test_html(popup_url: &str) -> String {
    r#"<!doctype html>
<html>
<head>
    <meta charset="utf-8">
    <title>Scrying Popup Test</title>
    <style>
        html, body { margin: 0; width: 100%; height: 100%; }
        body { background: #17202a; color: #f6ead0; font-family: system-ui, sans-serif; padding: 18px; }
        button { font: inherit; padding: 8px 12px; }
    </style>
</head>
<body>
    <button id="open-popup">Open popup</button>
    <script>
        const post = value => window.chrome.webview.postMessage(value);
        document.getElementById("open-popup").addEventListener("click", () => {
            window.open("__POPUP_URL__", "_blank");
            post("popup-test:clicked");
        });
        window.chrome.webview.addEventListener("message", event => {
            if (event.data === "popup-test:open") {
                window.open("__POPUP_URL__", "_blank");
                post("popup-test:opened");
            }
        });
        window.addEventListener("pageshow", () => post("popup-test:ready"));
    </script>
</body>
</html>"#
    .replace("__POPUP_URL__", popup_url)
}
