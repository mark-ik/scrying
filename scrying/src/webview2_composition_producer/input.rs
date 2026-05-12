use super::*;

impl WebView2CompositionProducer {
    /// Forward a mouse / scroll event to the composition WebView2.
    ///
    /// `event.point` is in physical pixels relative to the webview's
    /// top-left corner (the same coordinate space the controller's
    /// `Bounds` uses).
    pub fn send_mouse_input(&self, event: MouseInput) -> Result<(), WebSurfaceError> {
        let kind = mouse_event_kind(event.kind);
        let virtual_keys =
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(virtual_keys_bits(event.virtual_keys) as i32);
        let point = POINT {
            x: event.point.0,
            y: event.point.1,
        };
        unsafe {
            self.composition_controller
                .SendMouseInput(kind, virtual_keys, event.mouse_data as u32, point)
                .map_err(platform("SendMouseInput"))
        }
    }

    /// Forward a touch / pen pointer event to the composition WebView2.
    ///
    /// Builds an `ICoreWebView2PointerInfo` from `event` and dispatches via
    /// `ICoreWebView2CompositionController::SendPointerInput`. Pen tilt is
    /// in radians on the public API; converted to degrees for WebView2.
    pub fn send_pointer_input(&self, event: crate::PointerInput) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment3;
        let env3: ICoreWebView2Environment3 = self
            .environment
            .cast()
            .map_err(platform("environment cast to ICoreWebView2Environment3"))?;
        let info = unsafe { env3.CreateCoreWebView2PointerInfo() }
            .map_err(platform("CreateCoreWebView2PointerInfo"))?;

        let pointer_kind: u32 = match event.device {
            crate::PointerDevice::Touch => 2,
            crate::PointerDevice::Pen => 3,
            crate::PointerDevice::Mouse => 4,
        };
        let pointer_flags = pointer_flags_for(event.kind);
        let point = POINT {
            x: event.point.0,
            y: event.point.1,
        };
        let mut perf_count: i64 = 0;
        unsafe {
            windows::Win32::System::Performance::QueryPerformanceCounter(&mut perf_count)
                .map_err(platform("QueryPerformanceCounter"))?;
        }

        unsafe {
            info.SetPointerKind(pointer_kind)
                .map_err(platform("SetPointerKind"))?;
            info.SetPointerId(event.pointer_id)
                .map_err(platform("SetPointerId"))?;
            info.SetFrameId(0).map_err(platform("SetFrameId"))?;
            info.SetPointerFlags(pointer_flags)
                .map_err(platform("SetPointerFlags"))?;
            info.SetPixelLocation(point)
                .map_err(platform("SetPixelLocation"))?;
            info.SetPixelLocationRaw(point)
                .map_err(platform("SetPixelLocationRaw"))?;
            info.SetHimetricLocation(POINT { x: 0, y: 0 })
                .map_err(platform("SetHimetricLocation"))?;
            info.SetHimetricLocationRaw(POINT { x: 0, y: 0 })
                .map_err(platform("SetHimetricLocationRaw"))?;
            info.SetPerformanceCount(perf_count as u64)
                .map_err(platform("SetPerformanceCount"))?;
            info.SetHistoryCount(1)
                .map_err(platform("SetHistoryCount"))?;
            info.SetButtonChangeKind(0)
                .map_err(platform("SetButtonChangeKind"))?;

            match event.device {
                crate::PointerDevice::Touch => {
                    info.SetTouchMask(0x4).map_err(platform("SetTouchMask"))?;
                    let pressure = (event.pressure.clamp(0.0, 1.0) * 1024.0) as u32;
                    info.SetTouchPressure(pressure)
                        .map_err(platform("SetTouchPressure"))?;
                    let contact = RECT {
                        left: point.x - 1,
                        top: point.y - 1,
                        right: point.x + 1,
                        bottom: point.y + 1,
                    };
                    info.SetTouchContact(contact)
                        .map_err(platform("SetTouchContact"))?;
                    info.SetTouchContactRaw(contact)
                        .map_err(platform("SetTouchContactRaw"))?;
                }
                crate::PointerDevice::Pen => {
                    info.SetPenMask(0x1 | 0x4 | 0x8)
                        .map_err(platform("SetPenMask"))?;
                    let pressure = (event.pressure.clamp(0.0, 1.0) * 1024.0) as u32;
                    info.SetPenPressure(pressure)
                        .map_err(platform("SetPenPressure"))?;
                    let tilt_x_deg = event.tilt.0.to_degrees().clamp(-90.0, 90.0) as i32;
                    let tilt_y_deg = event.tilt.1.to_degrees().clamp(-90.0, 90.0) as i32;
                    info.SetPenTiltX(tilt_x_deg)
                        .map_err(platform("SetPenTiltX"))?;
                    info.SetPenTiltY(tilt_y_deg)
                        .map_err(platform("SetPenTiltY"))?;
                }
                crate::PointerDevice::Mouse => {}
            }
        }

        let event_kind = pointer_event_kind(event.kind);
        unsafe {
            self.composition_controller
                .SendPointerInput(event_kind, &info)
                .map_err(platform("SendPointerInput"))
        }
    }

    /// Forward a drag-enter event to the composition WebView2 with an
    /// `IDataObject` carrying the dragged content.
    pub fn drag_enter(
        &self,
        data_object: &windows::Win32::System::Com::IDataObject,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.DragEnter(data_object, key_state, p, effects as *mut u32)
                .map_err(platform("DragEnter"))
        }
    }

    /// Forward a drag-over event.
    pub fn drag_over(
        &self,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.DragOver(key_state, p, effects as *mut u32)
                .map_err(platform("DragOver"))
        }
    }

    /// Forward a drag-leave event.
    pub fn drag_leave(&self) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        unsafe { cc3.DragLeave() }.map_err(platform("DragLeave"))
    }

    /// Forward a drop event. Same `IDataObject` shape as
    /// [`drag_enter`](Self::drag_enter).
    pub fn drop_data(
        &self,
        data_object: &windows::Win32::System::Com::IDataObject,
        key_state: u32,
        point: (i32, i32),
        effects: &mut u32,
    ) -> Result<(), WebSurfaceError> {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2CompositionController3;
        let cc3: ICoreWebView2CompositionController3 = self.composition_controller.cast().map_err(
            platform("composition_controller cast to ICoreWebView2CompositionController3"),
        )?;
        let p = POINT {
            x: point.0,
            y: point.1,
        };
        unsafe {
            cc3.Drop(data_object, key_state, p, effects as *mut u32)
                .map_err(platform("Drop"))
        }
    }

    /// Move keyboard focus into the WebView2.
    pub fn move_focus(&self, reason: FocusReason) -> Result<(), WebSurfaceError> {
        let reason = focus_reason(reason);
        unsafe {
            self.controller
                .MoveFocus(reason)
                .map_err(platform("MoveFocus"))
        }
    }

    /// Forward one raw Win32 keyboard / IME message to the WebView2 parent HWND.
    pub fn forward_keyboard_message(
        &self,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> Result<(), WebSurfaceError> {
        if !is_webview2_keyboard_message(message) {
            return Err(WebSurfaceError::Unsupported(
                "WebView2CompositionProducer::forward_keyboard_message only accepts WM_KEY*, WM_CHAR, WM_DEADCHAR, and WM_IME* messages",
            ));
        }
        self.post_keyboard_message(message, wparam, lparam)
    }

    /// Best-effort portable keyboard bridge. For full IME fidelity on Windows,
    /// prefer [`Self::forward_keyboard_message`] with the host's native Win32
    /// message stream.
    pub fn send_keyboard_input(&self, event: KeyboardInput) -> Result<(), WebSurfaceError> {
        let message = keyboard_message_for(&event);
        let lparam = keyboard_lparam(&event);
        self.post_keyboard_message(message, event.virtual_key_code as usize, lparam)?;

        if event.kind == KeyEventKind::Down && !event.characters.is_empty() {
            let char_message = if event.modifiers.alt {
                WM_SYSCHAR
            } else {
                WM_CHAR
            };
            for unit in event.characters.encode_utf16() {
                self.post_keyboard_message(char_message, unit as usize, lparam)?;
            }
        }
        Ok(())
    }

    fn post_keyboard_message(
        &self,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> Result<(), WebSurfaceError> {
        unsafe {
            PostMessageW(
                Some(self.parent_hwnd),
                message,
                WPARAM(wparam),
                LPARAM(lparam),
            )
        }
        .map_err(platform("PostMessageW keyboard message"))
    }

    /// Drain the next cursor-change request from the webview.
    pub fn poll_cursor_shape(&self) -> Option<CursorShape> {
        self.cursor_queue.lock().ok()?.pop_front()
    }
}

fn mouse_event_kind(kind: MouseEventKind) -> COREWEBVIEW2_MOUSE_EVENT_KIND {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL, COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
        COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
        COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOUBLE_CLICK,
        COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN, COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
    };
    match kind {
        MouseEventKind::LeftButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
        MouseEventKind::LeftButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
        MouseEventKind::LeftButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::MiddleButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
        MouseEventKind::MiddleButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
        MouseEventKind::MiddleButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::RightButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
        MouseEventKind::RightButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
        MouseEventKind::RightButtonDoubleClick => {
            COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK
        }
        MouseEventKind::XButtonDown => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOWN,
        MouseEventKind::XButtonUp => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_UP,
        MouseEventKind::XButtonDoubleClick => COREWEBVIEW2_MOUSE_EVENT_KIND_X_BUTTON_DOUBLE_CLICK,
        MouseEventKind::Move => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
        MouseEventKind::Wheel => COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
        MouseEventKind::HorizontalWheel => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
        MouseEventKind::Leave => COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
    }
}

fn virtual_keys_bits(keys: crate::MouseVirtualKeys) -> u32 {
    let mut bits = 0u32;
    if keys.left_button {
        bits |= 0x0001;
    }
    if keys.right_button {
        bits |= 0x0002;
    }
    if keys.shift {
        bits |= 0x0004;
    }
    if keys.control {
        bits |= 0x0008;
    }
    if keys.middle_button {
        bits |= 0x0010;
    }
    if keys.x_button1 {
        bits |= 0x0020;
    }
    if keys.x_button2 {
        bits |= 0x0040;
    }
    bits
}

fn pointer_event_kind(
    kind: crate::PointerEventKind,
) -> webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_POINTER_EVENT_KIND {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_POINTER_EVENT_KIND_ACTIVATE, COREWEBVIEW2_POINTER_EVENT_KIND_DOWN,
        COREWEBVIEW2_POINTER_EVENT_KIND_ENTER, COREWEBVIEW2_POINTER_EVENT_KIND_LEAVE,
        COREWEBVIEW2_POINTER_EVENT_KIND_UP, COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
    };
    match kind {
        crate::PointerEventKind::Enter => COREWEBVIEW2_POINTER_EVENT_KIND_ENTER,
        crate::PointerEventKind::Down => COREWEBVIEW2_POINTER_EVENT_KIND_DOWN,
        crate::PointerEventKind::Update => COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
        crate::PointerEventKind::Up => COREWEBVIEW2_POINTER_EVENT_KIND_UP,
        crate::PointerEventKind::Leave => COREWEBVIEW2_POINTER_EVENT_KIND_LEAVE,
        crate::PointerEventKind::Activate => COREWEBVIEW2_POINTER_EVENT_KIND_ACTIVATE,
        crate::PointerEventKind::CaptureChanged => COREWEBVIEW2_POINTER_EVENT_KIND_UPDATE,
    }
}

fn pointer_flags_for(kind: crate::PointerEventKind) -> u32 {
    const POINTER_FLAG_INRANGE: u32 = 0x00000002;
    const POINTER_FLAG_INCONTACT: u32 = 0x00000004;
    const POINTER_FLAG_PRIMARY: u32 = 0x00002000;
    const POINTER_FLAG_DOWN: u32 = 0x00010000;
    const POINTER_FLAG_UPDATE: u32 = 0x00020000;
    const POINTER_FLAG_UP: u32 = 0x00040000;
    const POINTER_FLAG_CAPTURECHANGED: u32 = 0x00200000;
    match kind {
        crate::PointerEventKind::Down => {
            POINTER_FLAG_DOWN | POINTER_FLAG_INCONTACT | POINTER_FLAG_INRANGE | POINTER_FLAG_PRIMARY
        }
        crate::PointerEventKind::Up => POINTER_FLAG_UP | POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Update => {
            POINTER_FLAG_UPDATE
                | POINTER_FLAG_INCONTACT
                | POINTER_FLAG_INRANGE
                | POINTER_FLAG_PRIMARY
        }
        crate::PointerEventKind::Enter => POINTER_FLAG_INRANGE | POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Leave => POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::Activate => POINTER_FLAG_PRIMARY,
        crate::PointerEventKind::CaptureChanged => POINTER_FLAG_CAPTURECHANGED,
    }
}

fn focus_reason(reason: FocusReason) -> COREWEBVIEW2_MOVE_FOCUS_REASON {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MOVE_FOCUS_REASON_NEXT, COREWEBVIEW2_MOVE_FOCUS_REASON_PREVIOUS,
        COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
    };
    match reason {
        FocusReason::Programmatic => COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC,
        FocusReason::Next => COREWEBVIEW2_MOVE_FOCUS_REASON_NEXT,
        FocusReason::Previous => COREWEBVIEW2_MOVE_FOCUS_REASON_PREVIOUS,
    }
}

fn keyboard_message_for(event: &KeyboardInput) -> u32 {
    match event.kind {
        KeyEventKind::Down => {
            if event.modifiers.alt {
                WM_SYSKEYDOWN
            } else {
                WM_KEYDOWN
            }
        }
        KeyEventKind::Up => {
            if event.modifiers.alt {
                WM_SYSKEYUP
            } else {
                WM_KEYUP
            }
        }
        KeyEventKind::ModifiersChanged => {
            if modifier_is_down(event.virtual_key_code, event.modifiers) {
                WM_KEYDOWN
            } else {
                WM_KEYUP
            }
        }
    }
}

fn keyboard_lparam(event: &KeyboardInput) -> isize {
    let repeat_count = 1isize;
    let previous_down = match event.kind {
        KeyEventKind::Down => event.is_repeat,
        KeyEventKind::Up => true,
        KeyEventKind::ModifiersChanged => {
            !modifier_is_down(event.virtual_key_code, event.modifiers)
        }
    };
    let transition_up = matches!(keyboard_message_for(event), WM_KEYUP | WM_SYSKEYUP);
    repeat_count | ((previous_down as isize) << 30) | ((transition_up as isize) << 31)
}

fn modifier_is_down(virtual_key_code: u32, modifiers: crate::KeyModifierFlags) -> bool {
    match virtual_key_code {
        0x10 | 0xA0 | 0xA1 => modifiers.shift,
        0x11 | 0xA2 | 0xA3 => modifiers.control,
        0x12 | 0xA4 | 0xA5 => modifiers.alt,
        0x5B | 0x5C => modifiers.meta,
        0x14 => modifiers.caps_lock,
        _ => false,
    }
}

fn is_webview2_keyboard_message(message: u32) -> bool {
    matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_SYSCHAR
            | WM_DEADCHAR
            | WM_SYSDEADCHAR
            | WM_IME_STARTCOMPOSITION
            | WM_IME_ENDCOMPOSITION
            | WM_IME_COMPOSITION
            | WM_IME_COMPOSITIONFULL
            | WM_IME_CONTROL
            | WM_IME_NOTIFY
            | WM_IME_REQUEST
            | WM_IME_SELECT
            | WM_IME_SETCONTEXT
            | WM_IME_CHAR
            | WM_IME_KEYDOWN
            | WM_IME_KEYUP
    )
}

pub(super) fn register_cursor_changed_handler(
    composition_controller: &ICoreWebView2CompositionController,
    cursor_queue: Arc<Mutex<VecDeque<CursorShape>>>,
) -> Result<i64, WebSurfaceError> {
    use webview2_com::CursorChangedEventHandler;
    let cc = composition_controller.clone();
    let handler = CursorChangedEventHandler::create(Box::new(move |_, _| {
        let mut hcursor: HCURSOR = HCURSOR::default();
        if unsafe { cc.Cursor(&mut hcursor) }.is_ok() {
            let shape = hcursor_to_shape(hcursor);
            if let Ok(mut q) = cursor_queue.lock() {
                q.push_back(shape);
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    unsafe {
        composition_controller
            .add_CursorChanged(&handler, &mut token)
            .map_err(platform("add_CursorChanged"))?;
    }
    Ok(token)
}

fn hcursor_to_shape(cursor: HCURSOR) -> CursorShape {
    let pairs: [(windows::core::PCWSTR, CursorShape); 13] = [
        (IDC_ARROW, CursorShape::Default),
        (IDC_HAND, CursorShape::Pointer),
        (IDC_IBEAM, CursorShape::Text),
        (IDC_WAIT, CursorShape::Wait),
        (IDC_CROSS, CursorShape::Crosshair),
        (IDC_SIZEALL, CursorShape::ResizeAll),
        (IDC_SIZENS, CursorShape::ResizeNs),
        (IDC_SIZEWE, CursorShape::ResizeEw),
        (IDC_SIZENESW, CursorShape::ResizeNeSw),
        (IDC_SIZENWSE, CursorShape::ResizeNwSe),
        (IDC_NO, CursorShape::NotAllowed),
        (IDC_HELP, CursorShape::Help),
        (IDC_APPSTARTING, CursorShape::Progress),
    ];
    for (id, shape) in pairs {
        let h = unsafe { LoadCursorW(None, id) };
        if let Ok(h) = h
            && h.0 == cursor.0
        {
            return shape;
        }
    }
    CursorShape::Custom(format!("hcursor:{:?}", cursor.0))
}
