//! Low-level input simulation built on `SendInput`.
//!
//! This intentionally replaces the Python reference implementation's use of
//! the legacy `mouse_event`/`keybd_event` APIs (`windows_mcp.uia.core`) with
//! `SendInput`, and normalizes absolute mouse coordinates against the
//! *virtual* screen (all monitors) instead of the primary monitor only, so
//! clicks on secondary monitors placed left of or above the primary monitor
//! land correctly.

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN,
    VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SetCursorPos,
};

/// WHEEL_DELTA from winuser.h: one "notch" of mouse wheel rotation.
const WHEEL_DELTA: i32 = 120;
const DEFAULT_INPUT_SETTLE_MS: u64 = 50;
const MAX_INPUT_SETTLE_MS: u64 = 5_000;
const DEFAULT_CLICK_HOLD_MS: u64 = 15;
const MAX_CLICK_HOLD_MS: u64 = 1_000;

/// Reads a millisecond duration from `variable`, falling back to `default`.
fn env_duration(variable: &str, default: u64, max: u64) -> Duration {
    let millis = std::env::var(variable)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
        .clamp(0, max);
    Duration::from_millis(millis)
}

/// Delay after a completed input action, configurable for slower applications.
pub(crate) fn input_settle_delay() -> Duration {
    env_duration(
        "WINDOWS_MCP_INPUT_SETTLE_MS",
        DEFAULT_INPUT_SETTLE_MS,
        MAX_INPUT_SETTLE_MS,
    )
}

/// How long a mouse button stays down within one click.
///
/// This used to be a hard-coded 50ms that no environment variable could
/// reach, and every `Type` paid it too because typing clicks to take focus.
/// Windows itself imposes no minimum between `WM_LBUTTONDOWN` and
/// `WM_LBUTTONUP`; the hold exists only for controls that sample button state
/// on a timer, so the default is now the smaller value that still clears a
/// frame, with the old behaviour available through the environment.
pub(crate) fn click_hold_delay() -> Duration {
    env_duration(
        "WINDOWS_MCP_CLICK_HOLD_MS",
        DEFAULT_CLICK_HOLD_MS,
        MAX_CLICK_HOLD_MS,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// A held keyboard modifier that is always released when dropped.
pub struct ModifierGuard(u16);

impl ModifierGuard {
    pub fn press(modifier: Option<&str>) -> Result<Option<Self>, String> {
        let Some(modifier) = modifier else {
            return Ok(None);
        };
        let vk = match modifier.trim().to_ascii_lowercase().as_str() {
            "shift" => VK_SHIFT.0,
            "ctrl" => VK_CONTROL.0,
            "alt" => VK_MENU.0,
            "win" => VK_LWIN.0,
            _ => return Err("modifier must be one of: shift, ctrl, alt, win".to_string()),
        };
        key_down(vk)?;
        Ok(Some(Self(vk)))
    }

    pub fn virtual_key(&self) -> u16 {
        self.0
    }
}

impl Drop for ModifierGuard {
    fn drop(&mut self) {
        let _ = key_up(self.0);
    }
}

/// A keyboard key held for a bounded input sequence. The key is released on
/// every return path, including a failed subsequent input injection.
pub struct KeyGuard(u16);

impl KeyGuard {
    pub fn press(vk: u16) -> Result<Self, String> {
        key_down(vk)?;
        Ok(Self(vk))
    }
}

impl Drop for KeyGuard {
    fn drop(&mut self) {
        let _ = key_up(self.0);
    }
}

/// Current cursor position in screen coordinates.
pub fn get_cursor_pos() -> (i32, i32) {
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    (point.x, point.y)
}

/// How many times to retry a cursor move that the window manager refuses, and
/// how long to wait between attempts.
///
/// `SetCursorPos` fails transiently while the input desktop is switching —
/// a UAC prompt going up or down, the lock screen, a session transition, or
/// simply a burst of window creation. Observed as `0x800700CB` under load.
/// The desktop is back within a few tens of milliseconds, so a short retry
/// turns a hard failure into a pause; a genuinely unavailable desktop still
/// fails, just a moment later.
const CURSOR_RETRY_ATTEMPTS: u32 = 5;
const CURSOR_RETRY_DELAY: Duration = Duration::from_millis(40);

/// Sends one input event, retrying on the same transient desktop
/// unavailability that [`set_cursor_pos`] guards against. `what` names the
/// event for the error message.
fn send_one_input(input: INPUT, what: &str) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 0..CURSOR_RETRY_ATTEMPTS {
        if unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) } == 1 {
            return Ok(());
        }
        last_error = Some(windows::core::Error::from_thread());
        if attempt + 1 < CURSOR_RETRY_ATTEMPTS {
            sleep(CURSOR_RETRY_DELAY);
        }
    }
    Err(format!(
        "SendInput {what} event was rejected after {CURSOR_RETRY_ATTEMPTS} attempts: {}",
        last_error.expect("a failed attempt always records its error")
    ))
}

/// Moves the cursor directly to `(x, y)` with no intermediate steps.
///
/// The failure is confirmed against the cursor's actual position rather than
/// taken from the return value alone. `SetCursorPos` reports failure through
/// the thread's last-error, which it does not clear on success, so a stale
/// error from an unrelated earlier call surfaces here as a spurious `Err` —
/// observed reporting "この操作を正しく終了しました。 (0x00000000)", success
/// dressed as a failure. Asking where the cursor ended up settles it.
pub fn set_cursor_pos(x: i32, y: i32) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 0..CURSOR_RETRY_ATTEMPTS {
        let call = unsafe { SetCursorPos(x, y) };
        if call.is_ok() || get_cursor_pos() == (x, y) {
            return Ok(());
        }
        last_error = call.err();
        if attempt + 1 < CURSOR_RETRY_ATTEMPTS {
            sleep(CURSOR_RETRY_DELAY);
        }
    }
    let detail = match last_error {
        Some(error) => error.to_string(),
        None => format!("the cursor stayed at {:?}", get_cursor_pos()),
    };
    Err(format!(
        "Could not move the cursor to ({x},{y}) after {CURSOR_RETRY_ATTEMPTS} attempts: {detail}. \
         {}",
        input_block_hint()
    ))
}

/// Explains, as far as it can be determined here, why input injection is being
/// refused.
///
/// The common cause is an elevated foreground window: Windows blocks input
/// from a lower-integrity process to a higher-integrity one, and every
/// `SetCursorPos` then fails with `ERROR_INVALID_HANDLE` while the cursor
/// stays put. This layer deliberately keeps to `user32`, so it reports the
/// foreground window's title and lets the reader draw the conclusion rather
/// than reaching up into the window/privilege modules.
fn input_block_hint() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    };

    let title = unsafe {
        let hwnd = GetForegroundWindow();
        let length = GetWindowTextLengthW(hwnd);
        if hwnd.0.is_null() || length <= 0 {
            String::new()
        } else {
            let mut buffer = vec![0u16; length as usize + 1];
            let copied = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        }
    };

    if title.is_empty() {
        return "The session may have no interactive desktop: a locked screen, or a service \
                or disconnected remote session."
            .to_string();
    }
    format!(
        "The foreground window is {title:?}. If it runs elevated, Windows blocks input from \
         this session to it — move it aside or restart the server elevated; otherwise the \
         session may have no interactive desktop."
    )
}

/// The system double-click time, in milliseconds.
pub fn get_double_click_time_ms() -> u32 {
    unsafe { GetDoubleClickTime() }
}

/// Bounding rectangle of the virtual screen (the union of all monitors), as
/// `(x, y, width, height)`.
fn virtual_screen_rect() -> (i32, i32, i32, i32) {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        (x, y, w.max(1), h.max(1))
    }
}

/// Normalizes a screen coordinate to the 0..=65535 range `SendInput` expects
/// for `MOUSEEVENTF_ABSOLUTE`, relative to the virtual screen origin.
fn normalize_absolute(x: i32, y: i32) -> (i32, i32) {
    let (vx, vy, vw, vh) = virtual_screen_rect();
    let nx = (x - vx) as i64 * 65535 / (vw - 1).max(1) as i64;
    let ny = (y - vy) as i64 * 65535 / (vh - 1).max(1) as i64;
    let nx = nx.clamp(0, 65535);
    let ny = ny.clamp(0, 65535);
    (nx as i32, ny as i32)
}

fn send_mouse_input(
    flags: MOUSE_EVENT_FLAGS,
    dx: i32,
    dy: i32,
    mouse_data: i32,
) -> Result<(), String> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_one_input(input, "mouse")
}

fn mouse_button_flags(button: MouseButton, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

fn absolute_button_flags(button: MouseButton, down: bool) -> MOUSE_EVENT_FLAGS {
    mouse_button_flags(button, down)
        | MOUSEEVENTF_MOVE
        | MOUSEEVENTF_ABSOLUTE
        | MOUSEEVENTF_VIRTUALDESK
}

/// Presses `button` down at the cursor's current position.
pub fn mouse_down(button: MouseButton) -> Result<(), String> {
    let (x, y) = get_cursor_pos();
    let (nx, ny) = normalize_absolute(x, y);
    send_mouse_input(absolute_button_flags(button, true), nx, ny, 0)
}

/// Releases `button` at the cursor's current position.
pub fn mouse_up(button: MouseButton) -> Result<(), String> {
    let (x, y) = get_cursor_pos();
    let (nx, ny) = normalize_absolute(x, y);
    send_mouse_input(absolute_button_flags(button, false), nx, ny, 0)
}

/// A single click cycle at `(x, y)`: move, button down, a short gap, button
/// up, then `wait_after`.
pub fn click_once(x: i32, y: i32, button: MouseButton, wait_after: Duration) -> Result<(), String> {
    set_cursor_pos(x, y)?;
    let (nx, ny) = normalize_absolute(x, y);
    // Send the press at the click's own coordinates rather than re-reading the
    // cursor: `SetCursorPos` can be overridden between the two calls (pointer
    // precision, another process moving the cursor), which used to send the
    // button event wherever the cursor had drifted to.
    send_mouse_input(absolute_button_flags(button, true), nx, ny, 0)?;
    sleep(click_hold_delay());
    send_mouse_input(absolute_button_flags(button, false), nx, ny, 0)?;
    sleep(wait_after);
    Ok(())
}

/// Maximum duration (seconds) `move_smooth` allows itself at `move_speed == 1`.
const MAX_MOVE_SECOND: f64 = 1.0;

/// Smoothly moves the cursor to `(x, y)` in stepped `SetCursorPos` calls,
/// porting the pacing algorithm from the Python reference's `uia.MoveTo`.
pub fn move_smooth(x: i32, y: i32, move_speed: f64, wait_after: Duration) -> Result<(), String> {
    let mut move_time = if move_speed > 0.0 {
        MAX_MOVE_SECOND / move_speed
    } else {
        0.0
    };
    let (cur_x, cur_y) = get_cursor_pos();
    let x_count = (x - cur_x).unsigned_abs();
    let y_count = (y - cur_y).unsigned_abs();
    let mut max_point = x_count.max(y_count) as i64;

    let (_, _, vw, vh) = virtual_screen_rect();
    let max_side = vw.max(vh) as i64;
    let min_side = vw.min(vh) as i64;

    if max_point > min_side {
        max_point = min_side;
    }
    if max_point < max_side {
        max_point = 100 + ((max_side - 100) as f64 / max_side as f64 * max_point as f64) as i64;
        move_time = move_time * max_point as f64 / max_side as f64;
    }
    let step_count = max_point / 20;
    if step_count > 1 {
        let x_step = (x - cur_x) as f64 / step_count as f64;
        let y_step = (y - cur_y) as f64 / step_count as f64;
        let interval = move_time / step_count as f64;
        for i in 0..step_count {
            let cx = cur_x + (x_step * i as f64) as i32;
            let cy = cur_y + (y_step * i as f64) as i32;
            set_cursor_pos(cx, cy)?;
            if interval > 0.0 {
                sleep(Duration::from_secs_f64(interval));
            }
        }
    }
    set_cursor_pos(x, y)?;
    sleep(wait_after);
    Ok(())
}

/// Moves the cursor to `(x, y)` over exactly `duration` seconds via linear
/// interpolation, capped at 200 steps (10ms each). Ports `uia.MoveToDuration`.
pub fn move_smooth_duration(
    x: i32,
    y: i32,
    duration: f64,
    wait_after: Duration,
) -> Result<(), String> {
    let (cur_x, cur_y) = get_cursor_pos();
    if duration <= 0.0 {
        set_cursor_pos(x, y)?;
        sleep(wait_after);
        return Ok(());
    }
    let step_count = ((duration / 0.01).ceil() as i64).clamp(2, 200);
    let interval = duration / step_count as f64;
    for i in 1..=step_count {
        let ratio = i as f64 / step_count as f64;
        let cx = cur_x + ((x - cur_x) as f64 * ratio).round() as i32;
        let cy = cur_y + ((y - cur_y) as f64 * ratio).round() as i32;
        set_cursor_pos(cx, cy)?;
        sleep(Duration::from_secs_f64(interval));
    }
    sleep(wait_after);
    Ok(())
}

/// Spins the mouse wheel `notches` times (positive = up, negative = down),
/// waiting `interval` between notches and `wait_after` once all notches have
/// been sent.
pub fn wheel(notches: i32, interval: Duration, wait_after: Duration) -> Result<(), String> {
    let delta = if notches >= 0 {
        WHEEL_DELTA
    } else {
        -WHEEL_DELTA
    };
    for _ in 0..notches.unsigned_abs() {
        send_mouse_input(MOUSEEVENTF_WHEEL, 0, 0, delta)?;
        sleep(interval);
    }
    sleep(wait_after);
    Ok(())
}

fn send_keyboard_input(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> Result<(), String> {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_one_input(input, "keyboard")
}

/// Presses a virtual-key code down (does not release it).
pub fn key_down(vk: u16) -> Result<(), String> {
    send_keyboard_input(VIRTUAL_KEY(vk), KEYBD_EVENT_FLAGS(0))
}

/// Releases a virtual-key code.
pub fn key_up(vk: u16) -> Result<(), String> {
    send_keyboard_input(VIRTUAL_KEY(vk), KEYEVENTF_KEYUP)
}

/// Presses and releases a virtual-key code, waiting `wait_after` afterward.
pub fn key_tap(vk: u16, wait_after: Duration) -> Result<(), String> {
    key_down(vk)?;
    key_up(vk)?;
    sleep(wait_after);
    Ok(())
}

/// Presses `vks` down together, in order, holds briefly, then releases them
/// in reverse order — a simultaneous chord (e.g. Ctrl+Shift+Esc) — waiting
/// `wait_after` once everything has been released.
pub fn chord(vks: &[u16], wait_after: Duration) -> Result<(), String> {
    let mut pressed = Vec::with_capacity(vks.len());
    for &vk in vks {
        if let Err(error) = key_down(vk) {
            for &pressed_vk in pressed.iter().rev() {
                let _ = key_up(pressed_vk);
            }
            return Err(error);
        }
        pressed.push(vk);
        sleep(Duration::from_millis(10));
    }
    sleep(Duration::from_millis(10));
    let mut release_error = None;
    for &vk in pressed.iter().rev() {
        if let Err(error) = key_up(vk) {
            release_error.get_or_insert(error);
        }
        sleep(Duration::from_millis(10));
    }
    sleep(wait_after);
    release_error.map_or(Ok(()), Err)
}

/// Sends a single Unicode character via `KEYEVENTF_UNICODE`, bypassing
/// keyboard-layout translation entirely.
pub fn send_unicode_char(ch: char) -> Result<(), String> {
    let mut buf = [0u16; 2];
    for unit in ch.encode_utf16(&mut buf) {
        send_unicode_unit(*unit, KEYEVENTF_UNICODE)?;
        send_unicode_unit(*unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)?;
    }
    Ok(())
}

/// Sends one UTF-16 code unit. `KEYEVENTF_UNICODE` requires the code unit in
/// `wScan` with `wVk` left at 0: putting it in `wVk` makes Windows read it as a
/// virtual-key code instead, so 'd' (U+0064 = VK_NUMPAD4) types "4", 'o'
/// (U+006F = VK_DIVIDE) types "/", and 'A' (U+0041 = VK_A, no shift) types "a".
fn send_unicode_unit(unit: u16, flags: KEYBD_EVENT_FLAGS) -> Result<(), String> {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_one_input(input, "keyboard")
}

/// Types `text` one character at a time, waiting `interval` between
/// characters. `\n` and `\t` are sent as Enter/Tab key taps (so form
/// navigation still works); `\r` is skipped; everything else goes through
/// `send_unicode_char`.
pub fn type_text_char_by_char(
    text: &str,
    interval: Duration,
    wait_after: Duration,
) -> Result<(), String> {
    for ch in text.chars() {
        match ch {
            '\n' => key_tap(VK_RETURN.0, Duration::ZERO)?,
            '\t' => key_tap(VK_TAB.0, Duration::ZERO)?,
            '\r' => {}
            other => send_unicode_char(other)?,
        }
        sleep(interval);
    }
    sleep(wait_after);
    Ok(())
}

/// Reads CF_UNICODETEXT from the clipboard, if present.
pub fn get_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let result = (|| {
            let handle = GetClipboardData(13 /* CF_UNICODETEXT */).ok()?;
            let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0 as *mut _));
            if ptr.is_null() {
                return None;
            }
            let wide = std::slice::from_raw_parts(ptr as *const u16, wcslen(ptr as *const u16));
            let text = String::from_utf16_lossy(wide);
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0 as *mut _));
            Some(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Writes `text` to the clipboard as CF_UNICODETEXT. Returns `true` on
/// success.
pub fn set_clipboard_text(text: &str) -> bool {
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let ok = (|| {
            let _ = EmptyClipboard();
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let byte_len = wide.len() * std::mem::size_of::<u16>();
            let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_len).ok()?;
            let ptr = GlobalLock(hmem);
            if ptr.is_null() {
                let _ = GlobalFree(Some(hmem));
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
            let _ = GlobalUnlock(hmem);
            if SetClipboardData(
                13, /* CF_UNICODETEXT */
                Some(windows::Win32::Foundation::HANDLE(hmem.0)),
            )
            .is_err()
            {
                let _ = GlobalFree(Some(hmem));
                return None;
            }
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

/// Restores the clipboard to an empty state.
pub fn clear_clipboard() {
    unsafe {
        if OpenClipboard(None).is_ok() {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

/// Minimal `wcslen` for reading a NUL-terminated UTF-16 buffer.
unsafe fn wcslen(mut ptr: *const u16) -> usize {
    let mut len = 0usize;
    unsafe {
        while *ptr != 0 {
            len += 1;
            ptr = ptr.add(1);
        }
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn input_settle_delay_defaults_to_fifty_milliseconds() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("WINDOWS_MCP_INPUT_SETTLE_MS") };
        assert_eq!(input_settle_delay(), Duration::from_millis(50));
    }

    #[test]
    fn input_settle_delay_uses_environment_override_and_clamps_it() {
        let _guard = ENV_LOCK.lock().unwrap();

        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_SETTLE_MS", "125") };
        assert_eq!(input_settle_delay(), Duration::from_millis(125));

        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_SETTLE_MS", "6000") };
        assert_eq!(input_settle_delay(), Duration::from_millis(5000));

        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_SETTLE_MS", "invalid") };
        assert_eq!(input_settle_delay(), Duration::from_millis(50));

        unsafe { std::env::remove_var("WINDOWS_MCP_INPUT_SETTLE_MS") };
    }
    #[test]
    fn click_hold_defaults_below_the_old_fixed_fifty_milliseconds() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("WINDOWS_MCP_CLICK_HOLD_MS") };
        assert_eq!(click_hold_delay(), Duration::from_millis(15));
    }

    #[test]
    fn click_hold_honours_the_environment_override_and_clamps_it() {
        let _guard = ENV_LOCK.lock().unwrap();

        // Applications that sample button state on a slow timer can restore
        // the old behaviour.
        unsafe { std::env::set_var("WINDOWS_MCP_CLICK_HOLD_MS", "50") };
        assert_eq!(click_hold_delay(), Duration::from_millis(50));

        unsafe { std::env::set_var("WINDOWS_MCP_CLICK_HOLD_MS", "9999") };
        assert_eq!(click_hold_delay(), Duration::from_millis(1000));

        unsafe { std::env::set_var("WINDOWS_MCP_CLICK_HOLD_MS", "nonsense") };
        assert_eq!(click_hold_delay(), Duration::from_millis(15));

        unsafe { std::env::remove_var("WINDOWS_MCP_CLICK_HOLD_MS") };
    }

    #[test]
    fn absolute_clicks_target_the_virtual_desktop() {
        let flags = absolute_button_flags(MouseButton::Left, true);
        assert!(flags.contains(MOUSEEVENTF_ABSOLUTE));
        assert!(flags.contains(MOUSEEVENTF_VIRTUALDESK));
        assert!(flags.contains(MOUSEEVENTF_LEFTDOWN));
    }
}
