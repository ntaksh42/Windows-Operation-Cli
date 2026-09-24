//! A border drawn around the screen while the server injects input.
//!
//! The point is to tell a human "the CLI is driving the desktop right now, do
//! not touch it": concurrent manual input steals focus and moves the cursor
//! mid-action, and it also invalidates the DXGI desktop duplication the
//! capture path relies on, which then returns black frames.
//!
//! The window is excluded from capture (`WDA_EXCLUDEFROMCAPTURE`), so it stays
//! invisible to Screenshot/Snapshot and never annotates the very images the
//! caller is reasoning about. It is also layered, transparent to hit-testing,
//! and tool-window styled, so it takes no focus, swallows no clicks, and does
//! not appear in the taskbar or in window enumeration for UIA scans.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::thread;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, HBRUSH, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, HWND_TOPMOST, LWA_COLORKEY, MSG, RegisterClassExW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
    TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WM_PAINT, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// Border thickness in physical pixels.
const BORDER_PX: i32 = 4;
/// Border color, as COLORREF (0x00BBGGRR): a saturated red.
const BORDER_COLOR: u32 = 0x0000_30E8;
/// The color key painted as "transparent"; must differ from BORDER_COLOR.
const TRANSPARENT_KEY: u32 = 0x0000_0000;
/// The overlay window, created once on its own message-pump thread and then
/// only shown and hidden; `None` when it could not be created. Building a
/// full-screen layered window per action cost 11-35ms on every input tool call.
static OVERLAY_WINDOW: OnceLock<Option<isize>> = OnceLock::new();
/// Whether a guard currently owns the border.
static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);

/// True unless the operator disabled the overlay via the environment.
fn overlay_enabled() -> bool {
    !matches!(
        std::env::var("WINDOWS_MCP_INPUT_OVERLAY").as_deref(),
        Ok("0") | Ok("false") | Ok("off")
    )
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let brush: HBRUSH = CreateSolidBrush(COLORREF(BORDER_COLOR));
                let mut rc = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
                // Four edges; the interior keeps the color key and stays see-through.
                let edges = [
                    RECT { left: rc.left, top: rc.top, right: rc.right, bottom: rc.top + BORDER_PX },
                    RECT { left: rc.left, top: rc.bottom - BORDER_PX, right: rc.right, bottom: rc.bottom },
                    RECT { left: rc.left, top: rc.top, right: rc.left + BORDER_PX, bottom: rc.bottom },
                    RECT { left: rc.right - BORDER_PX, top: rc.top, right: rc.right, bottom: rc.bottom },
                ];
                for edge in &edges {
                    FillRect(hdc, edge, brush);
                }
                let _ = DeleteObject(brush.into());
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the borderless topmost window, hidden until [`InputOverlay::show`]
/// sizes it to the virtual desktop.
///
/// Returns the handle on success. Every failure is reported to the caller so
/// the guard can degrade to "no overlay" rather than failing the input action.
unsafe fn create_overlay() -> Result<HWND, String> {
    unsafe {
        let instance = GetModuleHandleW(PCWSTR::null())
            .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
        let class_name = w!("WindowsOperationCliInputOverlay");

        // Registering twice is harmless: the second call fails and we reuse it.
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowExW failed: {e}"))?;

        // Keep the overlay out of Screenshot/Snapshot. If this fails the
        // overlay would contaminate captures, so treat it as fatal and bail.
        SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).map_err(|e| {
            let _ = DestroyWindow(hwnd);
            format!("SetWindowDisplayAffinity failed: {e}")
        })?;

        SetLayeredWindowAttributes(hwnd, COLORREF(TRANSPARENT_KEY), 0, LWA_COLORKEY)
            .map_err(|e| format!("SetLayeredWindowAttributes failed: {e}"))?;
        Ok(hwnd)
    }
}

/// The overlay window, creating it (and the thread that pumps its messages)
/// on first use.
fn overlay_window() -> Option<HWND> {
    let handle = *OVERLAY_WINDOW.get_or_init(|| {
        let (ready_tx, ready_rx) = sync_channel::<Option<isize>>(0);
        thread::spawn(move || {
            let hwnd = match unsafe { create_overlay() } {
                Ok(hwnd) => hwnd,
                Err(_) => {
                    let _ = ready_tx.send(None);
                    return;
                }
            };
            let _ = ready_tx.send(Some(hwnd.0 as isize));
            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });
        ready_rx.recv().ok().flatten()
    });
    handle.map(|handle| HWND(handle as *mut _))
}

/// Shows the border for as long as the guard is alive.
///
/// The window lives on its own thread with its own message pump, so a slow or
/// blocking input sequence never stalls repaints, and the border disappears on
/// every return path including an early `?`.
pub struct InputOverlay {
    window: Option<isize>,
}

impl InputOverlay {
    /// Starts the overlay. Never fails the caller: if the window cannot be
    /// created the input action still proceeds, silently, without a border.
    pub fn show() -> Self {
        if !overlay_enabled() || OVERLAY_ACTIVE.swap(true, Ordering::SeqCst) {
            // Disabled, or an outer guard already owns the border: nest as a
            // no-op so the inner guard's Drop does not tear down the outer one.
            return Self { window: None };
        }
        let Some(hwnd) = overlay_window() else {
            OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
            return Self { window: None };
        };
        // Sized on every show so a display change since the window was created
        // is still covered. `SetWindowPos` on another thread's window is a sent
        // message, so the border is up before the first click lands rather
        // than racing it.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        Self {
            window: Some(hwnd.0 as isize),
        }
    }
}

impl Drop for InputOverlay {
    fn drop(&mut self) {
        if let Some(handle) = self.window.take() {
            unsafe {
                let _ = ShowWindow(HWND(handle as *mut _), SW_HIDE);
            }
            OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_enabled_unless_explicitly_disabled() {
        unsafe { std::env::remove_var("WINDOWS_MCP_INPUT_OVERLAY") };
        assert!(overlay_enabled());
        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_OVERLAY", "0") };
        assert!(!overlay_enabled());
        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_OVERLAY", "off") };
        assert!(!overlay_enabled());
        unsafe { std::env::set_var("WINDOWS_MCP_INPUT_OVERLAY", "1") };
        assert!(overlay_enabled());
        unsafe { std::env::remove_var("WINDOWS_MCP_INPUT_OVERLAY") };
    }

    #[test]
    fn nested_guards_do_not_tear_down_the_outer_border() {
        // The inner guard must be inert while the outer one owns the overlay,
        // otherwise a Type (which clicks, then types) would flash the border.
        OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
        let inner = InputOverlay::show();
        assert!(inner.window.is_none());
        drop(inner);
        assert!(OVERLAY_ACTIVE.load(Ordering::SeqCst));
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
    }
}
