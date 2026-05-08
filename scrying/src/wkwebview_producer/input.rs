//! Mouse / scroll-wheel event synthesis. Pure functions: take a
//! [`MouseInput`] from the public API and emit an `NSEvent` keyed
//! to a [`MouseTarget`] (the `NSResponder` slot the producer should
//! invoke). No producer state is captured here — the caller owns
//! the dispatch.

use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_core_graphics::{CGEvent, CGScrollEventUnit};
use objc2_foundation::NSPoint;
use objc2_web_kit::WKWebView;

use crate::{MouseEventKind, MouseInput, MouseVirtualKeys, WryWebSurfaceError};

/// Which `NSResponder` method should receive the synthesized event.
#[derive(Clone, Copy)]
pub(super) enum MouseTarget {
    MouseDown,
    MouseUp,
    MouseDragged,
    MouseMoved,
    RightMouseDown,
    RightMouseUp,
    RightMouseDragged,
    OtherMouseDown,
    OtherMouseUp,
    OtherMouseDragged,
    MouseExited,
    ScrollWheel,
}

pub(super) struct MouseDispatch {
    pub(super) event: Retained<NSEvent>,
    pub(super) target: MouseTarget,
}

/// Translate a `MouseInput` into an `NSEvent` to fire at the WKWebView.
///
/// Coordinates: `event.point` is "physical pixels relative to the
/// WebView's top-left." AppKit needs window-coordinates in points with
/// a bottom-left origin. The conversion is:
///
/// 1. Divide by the parent window's `backingScaleFactor` to get points.
/// 2. Flip Y around the WebView's local height to bottom-left origin.
/// 3. `convertPoint_toView(None)` to lift into window space.
pub(super) fn synthesize_mouse_event(
    webview: &WKWebView,
    window: &NSWindow,
    event: MouseInput,
) -> Result<MouseDispatch, WryWebSurfaceError> {
    let scale = window.backingScaleFactor().max(1.0);
    let bounds = webview.bounds();
    let x_local = f64::from(event.point.0) / scale;
    let y_local_top_left = f64::from(event.point.1) / scale;
    let y_local_bottom_left = bounds.size.height - y_local_top_left;
    let local_pt = NSPoint::new(x_local, y_local_bottom_left);
    let window_pt = webview.convertPoint_toView(local_pt, None);

    let modifier_flags = modifier_flags_from_virtual_keys(event.virtual_keys);
    let window_number = window.windowNumber();

    let (event_type, target, click_count, button_number, pressure) = match event.kind {
        MouseEventKind::LeftButtonDown => {
            (NSEventType::LeftMouseDown, MouseTarget::MouseDown, 1, 0, 1.0)
        }
        MouseEventKind::LeftButtonUp => {
            (NSEventType::LeftMouseUp, MouseTarget::MouseUp, 1, 0, 0.0)
        }
        MouseEventKind::LeftButtonDoubleClick => {
            (NSEventType::LeftMouseDown, MouseTarget::MouseDown, 2, 0, 1.0)
        }
        MouseEventKind::RightButtonDown => (
            NSEventType::RightMouseDown,
            MouseTarget::RightMouseDown,
            1,
            0,
            1.0,
        ),
        MouseEventKind::RightButtonUp => (
            NSEventType::RightMouseUp,
            MouseTarget::RightMouseUp,
            1,
            0,
            0.0,
        ),
        MouseEventKind::RightButtonDoubleClick => (
            NSEventType::RightMouseDown,
            MouseTarget::RightMouseDown,
            2,
            0,
            1.0,
        ),
        MouseEventKind::MiddleButtonDown => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            1,
            2,
            1.0,
        ),
        MouseEventKind::MiddleButtonUp => (
            NSEventType::OtherMouseUp,
            MouseTarget::OtherMouseUp,
            1,
            2,
            0.0,
        ),
        MouseEventKind::MiddleButtonDoubleClick => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            2,
            2,
            1.0,
        ),
        MouseEventKind::XButtonDown => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            1,
            event.mouse_data.max(3),
            1.0,
        ),
        MouseEventKind::XButtonUp => (
            NSEventType::OtherMouseUp,
            MouseTarget::OtherMouseUp,
            1,
            event.mouse_data.max(3),
            0.0,
        ),
        MouseEventKind::XButtonDoubleClick => (
            NSEventType::OtherMouseDown,
            MouseTarget::OtherMouseDown,
            2,
            event.mouse_data.max(3),
            1.0,
        ),
        MouseEventKind::Move => {
            // If a button is held, AppKit reports a `*MouseDragged`
            // event instead of `MouseMoved`. Match that — WKWebView
            // gates pointer-move handling on this distinction.
            if event.virtual_keys.left_button {
                (
                    NSEventType::LeftMouseDragged,
                    MouseTarget::MouseDragged,
                    0,
                    0,
                    0.0,
                )
            } else if event.virtual_keys.right_button {
                (
                    NSEventType::RightMouseDragged,
                    MouseTarget::RightMouseDragged,
                    0,
                    0,
                    0.0,
                )
            } else if event.virtual_keys.middle_button {
                (
                    NSEventType::OtherMouseDragged,
                    MouseTarget::OtherMouseDragged,
                    0,
                    2,
                    0.0,
                )
            } else {
                (NSEventType::MouseMoved, MouseTarget::MouseMoved, 0, 0, 0.0)
            }
        }
        MouseEventKind::Leave => {
            (NSEventType::MouseExited, MouseTarget::MouseExited, 0, 0, 0.0)
        }
        MouseEventKind::Wheel | MouseEventKind::HorizontalWheel => {
            // Scroll wheel events have no `NSEvent` factory — build a
            // CGEvent and round-trip through `eventWithCGEvent:`.
            return synthesize_scroll_wheel_event(event);
        }
    };

    // `NSEvent::mouseEventWithType:` does not expose `buttonNumber`
    // directly — Apple infers the button from the event type. So
    // X-button slots and middle-vs-other-mouse distinctions ride on
    // the kind of event we synthesize, not on `mouse_data`. Setting
    // a synthetic per-button `buttonNumber` (so JS observes
    // `event.button == 3/4` for X-buttons) requires the CGEvent path
    // and is deferred along with scroll-wheel.
    let _ = button_number;

    let ns_event = if matches!(event_type, NSEventType::MouseExited) {
        // SAFETY: `userData` is allowed to be null when no tracking
        // area is associated with the synthesized event.
        unsafe {
            NSEvent::enterExitEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_trackingNumber_userData(
                event_type,
                window_pt,
                modifier_flags,
                0.0,
                window_number,
                None,
                0,
                0,
                std::ptr::null_mut(),
            )
        }
    } else {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            event_type,
            window_pt,
            modifier_flags,
            0.0,
            window_number,
            None,
            0,
            click_count,
            pressure,
        )
    };

    let ns_event = ns_event.ok_or_else(|| {
        WryWebSurfaceError::Platform(
            "NSEvent factory returned nil for the synthesized mouse event".into(),
        )
    })?;

    Ok(MouseDispatch {
        event: ns_event,
        target,
    })
}

/// Build a synthetic ScrollWheel `NSEvent` via `CGEventCreateScrollWheelEvent2`.
///
/// `event.mouse_data` carries the wheel delta. Sign convention matches
/// AppKit: positive = up / right. Pixel units (not lines) so the
/// consumer's host-side scroll-amount accounting maps directly to
/// pixel deltas.
pub(super) fn synthesize_scroll_wheel_event(
    event: MouseInput,
) -> Result<MouseDispatch, WryWebSurfaceError> {
    let (wheel_count, wheel1, wheel2) = match event.kind {
        MouseEventKind::Wheel => (1u32, event.mouse_data, 0i32),
        MouseEventKind::HorizontalWheel => (2u32, 0i32, event.mouse_data),
        _ => unreachable!("synthesize_scroll_wheel_event called with non-wheel kind"),
    };
    let cg_event = CGEvent::new_scroll_wheel_event2(
        None,
        CGScrollEventUnit::Pixel,
        wheel_count,
        wheel1,
        wheel2,
        0,
    )
    .ok_or_else(|| {
        WryWebSurfaceError::Platform(
            "CGEventCreateScrollWheelEvent2 returned nil".into(),
        )
    })?;
    let ns_event = NSEvent::eventWithCGEvent(&cg_event).ok_or_else(|| {
        WryWebSurfaceError::Platform("NSEvent::eventWithCGEvent returned nil".into())
    })?;
    Ok(MouseDispatch {
        event: ns_event,
        target: MouseTarget::ScrollWheel,
    })
}

pub(super) fn modifier_flags_from_virtual_keys(
    keys: MouseVirtualKeys,
) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    if keys.shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if keys.control {
        flags |= NSEventModifierFlags::Control;
    }
    flags
}
