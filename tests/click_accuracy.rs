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
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostQuitMessage, RegisterClassW, SetForegroundWindow, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WM_LBUTTONDOWN, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::w;

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
#[ignore = "requires an interactive single-monitor Windows desktop"]
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
        let _ = SetForegroundWindow(hwnd);
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

    let (_hwnd, origin) = window_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let expected = (40, 30);
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
