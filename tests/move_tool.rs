//! The `Move` tool: where the cursor ends up, and what a drag delivers.
//!
//! A drag is asserted against a window that records the button and motion
//! messages it receives, so "dragged" means the target saw a press, movement
//! while held, and a release — not merely that the tool returned a string
//! saying so.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use windows_operation_cli::input_sim;
use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::move_mouse::{MoveParams, move_mouse};

/// What the probe window saw during the last drag.
static SAW_BUTTON_DOWN: AtomicBool = AtomicBool::new(false);
static SAW_BUTTON_UP: AtomicBool = AtomicBool::new(false);
/// Mouse-move messages received while the left button was held.
static MOVES_WHILE_HELD: AtomicI32 = AtomicI32::new(0);

fn reset() {
    SAW_BUTTON_DOWN.store(false, Ordering::SeqCst);
    SAW_BUTTON_UP.store(false, Ordering::SeqCst);
    MOVES_WHILE_HELD.store(0, Ordering::SeqCst);
}

unsafe extern "system" fn probe_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_LBUTTONDOWN => {
            SAW_BUTTON_DOWN.store(true, Ordering::SeqCst);
        }
        WM_LBUTTONUP => {
            SAW_BUTTON_UP.store(true, Ordering::SeqCst);
        }
        // MK_LBUTTON is bit 0 of wParam: the button was down for this move.
        WM_MOUSEMOVE if wparam.0 & 0x0001 != 0 => {
            MOVES_WHILE_HELD.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A window that records the mouse messages it is sent.
struct Probe {
    hwnd: isize,
    rect: RECT,
}

impl Probe {
    fn open() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = w!("WindowsOperationCliMoveProbe");
            let descriptor = WNDCLASSW {
                lpfnWndProc: Some(probe_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&descriptor);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(w!("Move Probe").as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                150,
                150,
                500,
                400,
                None,
                None,
                None,
                None,
            )
            .ok()?;

            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let probe = Self {
                hwnd: hwnd.0 as isize,
                rect,
            };

            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                raise(hwnd);
                probe.pump(Duration::from_millis(250));
                let centre = probe.point(0.5, 0.5);
                if WindowFromPoint(POINT {
                    x: centre.0,
                    y: centre.1,
                }) == hwnd
                {
                    return Some(probe);
                }
                if Instant::now() >= deadline {
                    return None;
                }
            }
        }
    }

    /// A point inside the window, given as fractions of its width and height.
    fn point(&self, fx: f64, fy: f64) -> (i32, i32) {
        let width = (self.rect.right - self.rect.left) as f64;
        let height = (self.rect.bottom - self.rect.top) as f64;
        (
            self.rect.left + (width * fx) as i32,
            self.rect.top + (height * fy) as i32,
        )
    }

    fn pump(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let mut message = MSG::default();
        while Instant::now() < deadline {
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(HWND(self.hwnd as *mut _));
        }
    }
}

unsafe fn raise(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};

    unsafe {
        let foreground = GetForegroundWindow();
        let current = GetCurrentThreadId();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let target = GetWindowThreadProcessId(hwnd, None);

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let mut attached = Vec::new();
        for thread in [foreground_thread, target] {
            if thread != 0 && thread != current && AttachThreadInput(current, thread, true).as_bool()
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
            let _ = AttachThreadInput(current, thread, false);
        }
    }
}

fn move_to(x: i32, y: i32) -> Result<String, String> {
    move_mouse(MoveParams {
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        drag: None,
        from_loc: None,
        duration: None,
    })
}

/// A plain move has to leave the cursor where it was asked to.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn a_move_leaves_the_cursor_at_the_target() {
    for target in [(400, 300), (250, 450)] {
        move_to(target.0, target.1).expect("move failed");
        assert_eq!(
            input_sim::get_cursor_pos(),
            target,
            "the cursor did not reach {target:?}"
        );
    }
}

/// A drag has to deliver a press, movement while held, and a release.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn a_drag_presses_moves_and_releases() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset();

    let from = probe.point(0.25, 0.3);
    let to = probe.point(0.75, 0.7);
    move_mouse(MoveParams {
        loc: Some(ListOrString::List(vec![to.0, to.1])),
        label: None,
        drag: Some(windows_operation_cli::params::BoolOrString::Bool(true)),
        from_loc: Some(ListOrString::List(vec![from.0, from.1])),
        duration: Some(0.3),
    })
    .expect("drag failed");
    probe.pump(Duration::from_millis(600));

    assert!(
        SAW_BUTTON_DOWN.load(Ordering::SeqCst),
        "the drag never pressed the button"
    );
    assert!(
        MOVES_WHILE_HELD.load(Ordering::SeqCst) > 0,
        "the cursor never moved while the button was held"
    );
    assert!(
        SAW_BUTTON_UP.load(Ordering::SeqCst),
        "the drag never released the button"
    );
    assert_eq!(
        input_sim::get_cursor_pos(),
        to,
        "the drag did not end at its destination"
    );
}

/// The button must not be left down when a drag ends.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn a_drag_does_not_leave_the_button_held() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };

    let from = probe.point(0.3, 0.3);
    let to = probe.point(0.7, 0.6);
    move_mouse(MoveParams {
        loc: Some(ListOrString::List(vec![to.0, to.1])),
        label: None,
        drag: Some(windows_operation_cli::params::BoolOrString::Bool(true)),
        from_loc: Some(ListOrString::List(vec![from.0, from.1])),
        duration: None,
    })
    .expect("drag failed");
    probe.pump(Duration::from_millis(400));

    // A held button would make this move register as a drag too.
    reset();
    let elsewhere = probe.point(0.4, 0.4);
    move_to(elsewhere.0, elsewhere.1).expect("move failed");
    probe.pump(Duration::from_millis(400));
    assert_eq!(
        MOVES_WHILE_HELD.load(Ordering::SeqCst),
        0,
        "the button was still held after the drag ended"
    );
}

/// Drag-only options passed without `drag` are a caller mistake.
#[test]
fn drag_options_without_drag_are_rejected() {
    let error = move_mouse(MoveParams {
        loc: Some(ListOrString::List(vec![100, 100])),
        label: None,
        drag: None,
        from_loc: Some(ListOrString::List(vec![10, 10])),
        duration: None,
    })
    .expect_err("from_loc without drag should be rejected");
    assert!(error.contains("require drag"), "unexpected error: {error}");
}

/// A duration outside the allowed range is rejected before anything moves.
#[test]
fn an_out_of_range_drag_duration_is_rejected() {
    for duration in [-1.0, 11.0, f64::NAN] {
        let error = move_mouse(MoveParams {
            loc: Some(ListOrString::List(vec![100, 100])),
            label: None,
            drag: Some(windows_operation_cli::params::BoolOrString::Bool(true)),
            from_loc: None,
            duration: Some(duration),
        })
        .expect_err("an out-of-range duration should be rejected");
        assert!(error.contains("duration"), "{duration}: {error}");
    }
}

/// Neither a location nor a label is a caller mistake.
#[test]
fn a_move_without_a_target_is_rejected() {
    let error = move_mouse(MoveParams {
        loc: None,
        label: None,
        drag: None,
        from_loc: None,
        duration: None,
    })
    .expect_err("a move with no target should be rejected");
    assert!(error.contains("loc or label"), "unexpected error: {error}");
}
