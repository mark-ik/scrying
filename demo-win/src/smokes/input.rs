use super::super::*;

pub(crate) fn keyboard_validate_enabled() -> bool {
    std::env::var("WEBVIEW_KEYBOARD_VALIDATE")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
}

pub(crate) fn validate_platform_keyboard_smoke(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::UI::WindowsAndMessaging::{WM_CHAR, WM_KEYDOWN, WM_KEYUP};

    const EXPECTED: &str = "scry42";

    producer.move_focus(scrying::FocusReason::Programmatic)?;
    pump_windows_messages_for(std::time::Duration::from_millis(150));

    for ch in EXPECTED.chars() {
        let virtual_key_code = match ch {
            'a'..='z' => ch.to_ascii_uppercase() as u32,
            'A'..='Z' => ch as u32,
            '0'..='9' => ch as u32,
            _ => return Err(format!("unsupported keyboard smoke character: {ch:?}").into()),
        };
        producer.forward_keyboard_message(WM_KEYDOWN, virtual_key_code as usize, 1)?;
        producer.forward_keyboard_message(WM_CHAR, ch as usize, 1)?;
        producer.forward_keyboard_message(
            WM_KEYUP,
            virtual_key_code as usize,
            1 | (1isize << 30) | (1isize << 31),
        )?;
        pump_windows_messages_for(std::time::Duration::from_millis(30));
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut last_value = String::new();
    while std::time::Instant::now() < deadline {
        pump_windows_messages_for(std::time::Duration::from_millis(16));
        while let Some(message) = producer.poll_web_message() {
            if let Some(value) = message.strip_prefix("keyboard-smoke:") {
                last_value = value.to_string();
                if value == EXPECTED {
                    println!(
                        "demo-win: keyboard-test: PASS - typed {value:?} through raw Win32 keyboard messages"
                    );
                    return Ok(());
                }
            }
        }
    }

    Err(format!(
        "WebView2 keyboard smoke timed out: expected {EXPECTED:?}, last observed {last_value:?}"
    )
    .into())
}

pub(crate) fn validate_platform_scripted(
    producer: &mut scrying::PlatformWebSurfaceProducer,
) -> Result<(), Box<dyn std::error::Error>> {
    const MESSAGE: &str = "ping-from-demo-win";
    const EXPECTED_ECHO: &str = "scripted:host-echo:ping-from-demo-win";

    wait_for_web_message(
        producer,
        "scripted:ready",
        std::time::Duration::from_secs(2),
    )?;
    producer.post_web_message(MESSAGE)?;
    wait_for_web_message(producer, EXPECTED_ECHO, std::time::Duration::from_secs(2))?;

    producer.send_mouse_input(scrying::MouseInput {
        kind: scrying::MouseEventKind::Move,
        virtual_keys: scrying::MouseVirtualKeys::default(),
        mouse_data: 0,
        point: (32, 32),
    })?;
    producer.send_mouse_input(scrying::MouseInput {
        kind: scrying::MouseEventKind::Wheel,
        virtual_keys: scrying::MouseVirtualKeys::default(),
        mouse_data: 120,
        point: (32, 32),
    })?;

    producer.move_focus(scrying::FocusReason::Programmatic)?;
    for ch in ['a', 'b', 'c'] {
        send_scripted_key_pair(producer, ch)?;
    }

    println!(
        "demo-win: scripted: PASS - JS message round-trip plus mouse/keyboard API dispatch verified"
    );
    Ok(())
}

fn send_scripted_key_pair(
    producer: &mut scrying::PlatformWebSurfaceProducer,
    ch: char,
) -> Result<(), Box<dyn std::error::Error>> {
    let virtual_key_code = match ch {
        'a'..='z' => ch.to_ascii_uppercase() as u32,
        'A'..='Z' => ch as u32,
        '0'..='9' => ch as u32,
        _ => return Err(format!("unsupported scripted keyboard character: {ch:?}").into()),
    };
    let characters = ch.to_string();
    producer.send_keyboard_input(scrying::KeyboardInput {
        kind: scrying::KeyEventKind::Down,
        virtual_key_code,
        characters: characters.clone(),
        characters_ignoring_modifiers: characters,
        modifiers: scrying::KeyModifierFlags::default(),
        is_repeat: false,
    })?;
    producer.send_keyboard_input(scrying::KeyboardInput {
        kind: scrying::KeyEventKind::Up,
        virtual_key_code,
        characters: String::new(),
        characters_ignoring_modifiers: String::new(),
        modifiers: scrying::KeyModifierFlags::default(),
        is_repeat: false,
    })?;
    Ok(())
}
