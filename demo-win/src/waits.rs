use super::*;

pub(crate) fn drain_web_messages(producer: &mut scrying::PlatformWebSurfaceProducer) {
    while producer.poll_web_message().is_some() {}
}

pub(crate) fn drain_navigation_events(producer: &mut scrying::PlatformWebSurfaceProducer) {
    while producer.poll_navigation_event().is_some() {}
}

pub(crate) fn wait_for_new_window_request(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::NewWindowRequested { url } => return Ok(url),
                other => last_event = format!("{other:?}"),
            }
        }
    }

    Err(
        format!("timed out waiting for NewWindowRequested; last navigation event {last_event:?}")
            .into(),
    )
}

pub(crate) fn wait_for_web_message_prefix(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    expected_prefix: &str,
    timeout: std::time::Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_message = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(message) = producer.poll_web_message() {
            if message.starts_with(expected_prefix) {
                return Ok(message);
            }
            last_message = message;
        }
    }
    Err(format!(
        "timed out waiting for web message prefix {expected_prefix:?}; last message {last_message:?}"
    )
    .into())
}

pub(crate) fn wait_for_find_result(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<
    Result<scrying::webview2_composition_producer::WebView2FindResult, String>,
    Box<dyn std::error::Error>,
> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        if let Some(result) = producer.poll_find_match() {
            return Ok(result);
        }
    }
    Err("timed out waiting for native find completion".into())
}

pub(crate) fn wait_for_pdf(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<Result<Vec<u8>, String>, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        if let Some(result) = producer.poll_pdf() {
            return Ok(result);
        }
    }
    Err("timed out waiting for native PDF completion".into())
}

pub(crate) fn wait_for_context_menu(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<(String, Option<String>), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::ContextMenuRequested {
                    page_url, link_url, ..
                } => return Ok((page_url, link_url)),
                other => last_event = format!("{other:?}"),
            }
        }
    }
    Err(
        format!("timed out waiting for ContextMenuRequested; last navigation event {last_event:?}")
            .into(),
    )
}

pub(crate) fn wait_for_media_capture_state(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    timeout: std::time::Duration,
) -> Result<(u32, u32), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::MediaCaptureStateChanged {
                    audio_active_tracks,
                    video_active_tracks,
                } => return Ok((audio_active_tracks, video_active_tracks)),
                other => last_event = format!("{other:?}"),
            }
        }
    }
    Err(format!(
        "timed out waiting for MediaCaptureStateChanged; last navigation event {last_event:?}"
    )
    .into())
}

pub(crate) fn wait_for_title(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    expected: &str,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_event = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(event) = producer.poll_navigation_event() {
            match event {
                NavigationEvent::TitleChanged { title } if title == expected => return Ok(()),
                other => last_event = format!("{other:?}"),
            }
        }
    }

    Err(
        format!("timed out waiting for title {expected:?}; last navigation event {last_event:?}")
            .into(),
    )
}

pub(crate) fn wait_for_web_message(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    expected: &str,
    timeout: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_message = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(message) = producer.poll_web_message() {
            if message == expected {
                return Ok(());
            }
            last_message = message;
        }
    }

    Err(
        format!("timed out waiting for web message {expected:?}; last observed {last_message:?}")
            .into(),
    )
}

pub(crate) fn pump_windows_messages_for(duration: std::time::Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        unsafe {
            let mut message = MSG::default();
            let mut drained = 0u32;
            while std::time::Instant::now() < deadline
                && PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool()
            {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                drained += 1;
                if drained >= 256 {
                    break;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}
