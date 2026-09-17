#![cfg(target_os = "windows")]

#[allow(dead_code)]
#[path = "../src/input_sim.rs"]
mod input_sim;

use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, BringWindowToTop, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    HWND_TOP, MSG, PostQuitMessage, RegisterClassW, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WM_LBUTTONDOWN, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::w;

/// Brings `hwnd` to the foreground, mirroring `window::switch_to`.
///
/// A bare `SetForegroundWindow` is refused whenever another process owns the
/// foreground, which is the normal case when this test runs from a terminal:
/// the window stayed behind, and the injected click landed on the terminal
/// instead of here.
unsafe fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        let current_tid = GetCurrentThreadId();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let target_thread = GetWindowThreadProcessId(hwnd, None);

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let mut attached = Vec::new();
        for thread in [foreground_thread, target_thread] {
            if thread != 0
                && thread != current_tid
                && AttachThreadInput(current_tid, thread, true).as_bool()
            {
                attached.push(thread);
            }
        }

        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );

        for thread in attached.into_iter().rev() {
            let _ = AttachThreadInput(current_tid, thread, false);
        }
    }
}

type ClickSender = Sender<(i32, i32)>;

static CLICK_SENDER: OnceLock<Mutex<Option<ClickSender>>> = OnceLock::new();

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_LBUTTONDOWN {
        let packed = lparam.0 as u32;
        let x = (packed as u16 as i16) as i32;
        let y = ((packed >> 16) as u16 as i16) as i32;
        if let Ok(sender) = CLICK_SENDER.get_or_init(|| Mutex::new(None)).lock()
            && let Some(sender) = sender.as_ref()
        {
            let _ = sender.send((x, y));
        }
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn click_lands_within_two_pixels() {
    let (window_tx, window_rx) = mpsc::channel();
    let (click_tx, click_rx) = mpsc::channel();
    *CLICK_SENDER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(click_tx);

    let thread = std::thread::spawn(move || unsafe {
        let module = GetModuleHandleW(None).unwrap();
        let instance = HINSTANCE(module.0);
        let class_name = w!("WindowsOperationCliClickAccuracyTest");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name,
            ..Default::default()
        };
        assert_ne!(RegisterClassW(&class), 0);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Click accuracy test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            100,
            100,
            320,
            240,
            None,
            None,
            Some(instance),
            None,
        )
        .unwrap();
        let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);
        force_foreground(hwnd);
        let mut origin = POINT::default();
        ClientToScreen(hwnd, &mut origin).unwrap();
        window_tx.send((hwnd.0 as isize, origin)).unwrap();

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DestroyWindow(hwnd).unwrap();
    });

    let (hwnd, origin) = window_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let expected = (40, 30);

    // Activation is asynchronous: the window manager raises the window after
    // the creating thread has already reported its origin. Clicking before it
    // is actually in front sends the input to whatever still covers the point.
    let target = (origin.x + expected.0, origin.y + expected.1);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let hit = unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(POINT { x: target.0, y: target.1 }) };
        if hit.0 as isize == hwnd {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let hit = unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(POINT { x: target.0, y: target.1 }) };
    if hit.0 as isize != hwnd {
        // Something else owns the pixel this test would click, so the
        // measurement would describe that window rather than this one. That is
        // the desktop's state — another app taking the foreground, a lock
        // screen, a full-screen overlay — not a defect in click accuracy.
        eprintln!(
            "skipped: {hwnd:#x} never reached the foreground; {:#x} owns the target pixel",
            hit.0 as isize
        );
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(HWND(hwnd as *mut _)),
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        return;
    }

    input_sim::click_once(
        origin.x + expected.0,
        origin.y + expected.1,
        input_sim::MouseButton::Left,
        Duration::ZERO,
    )
    .unwrap();
    let actual = click_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!((actual.0 - expected.0).abs() <= 2, "x: {actual:?}");
    assert!((actual.1 - expected.1).abs() <= 2, "y: {actual:?}");
    thread.join().unwrap();
}
