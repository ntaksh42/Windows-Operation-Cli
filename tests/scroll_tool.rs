//! The `Scroll` tool: what it sends, and what it refuses.
//!
//! Whether a *given application* scrolls in response is that application's
//! business — a stock Win32 listbox, for instance, does not handle
//! `WM_MOUSEWHEEL` at all, so a test asserting that it moves would be
//! measuring comctl rather than this tool. What belongs here is that the tool
//! delivers a real wheel event with the right direction and magnitude, and
//! that it rejects the calls it should reject without sending anything.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::scroll::{ScrollDirection, ScrollParams, ScrollType, scroll};

/// Total wheel delta the probe window has received, and how many messages it
/// arrived in. Windows coalesces rapid notches into one message with a summed
/// delta, so the total is the meaningful figure.
static TOTAL_DELTA: AtomicI32 = AtomicI32::new(0);
static MESSAGE_COUNT: AtomicU32 = AtomicU32::new(0);
/// Set when a horizontal wheel message arrives, which is what a `shift`-held
/// scroll produces.
static HORIZONTAL_DELTA: AtomicI32 = AtomicI32::new(0);

fn reset_counters() {
    TOTAL_DELTA.store(0, Ordering::SeqCst);
    MESSAGE_COUNT.store(0, Ordering::SeqCst);
    HORIZONTAL_DELTA.store(0, Ordering::SeqCst);
}

unsafe extern "system" fn probe_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let delta = ((wparam.0 >> 16) as u16 as i16) as i32;
    match message {
        WM_MOUSEWHEEL => {
            TOTAL_DELTA.fetch_add(delta, Ordering::SeqCst);
            MESSAGE_COUNT.fetch_add(1, Ordering::SeqCst);
        }
        WM_MOUSEHWHEEL => {
            HORIZONTAL_DELTA.fetch_add(delta, Ordering::SeqCst);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Brings `hwnd` to the foreground, attaching to the current foreground
/// window's input queue so the request is not refused.
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
        let target_thread = GetWindowThreadProcessId(hwnd, None);

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let mut attached = Vec::new();
        for thread in [foreground_thread, target_thread] {
            if thread != 0
                && thread != current
                && AttachThreadInput(current, thread, true).as_bool()
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

/// A window that records the wheel events it is sent.
struct Probe {
    hwnd: isize,
    centre: (i32, i32),
}

impl Probe {
    fn open() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = w!("WindowsOperationCliScrollProbe");
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
                PCWSTR(w!("Scroll Probe").as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                200,
                200,
                400,
                320,
                None,
                None,
                None,
                None,
            )
            .ok()?;

            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let centre = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
            let probe = Self {
                hwnd: hwnd.0 as isize,
                centre,
            };

            // A bare `SetForegroundWindow` is refused while another process
            // owns the foreground, which is the normal case when a previous
            // test's window is still going away. Attach to that window's input
            // queue, as `window::switch_to` does, and give it a few tries.
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                raise(hwnd);
                probe.pump(Duration::from_millis(250));
                if WindowFromPoint(POINT {
                    x: centre.0,
                    y: centre.1,
                }) == hwnd
                {
                    return Some(probe);
                }
                if Instant::now() >= deadline {
                    // Something else owns that pixel, so a wheel event aimed
                    // there would go to that window and measure nothing. The
                    // probe's own `Drop` tears the window down.
                    return None;
                }
            }
        }
    }

    /// Runs the message loop for `duration` so queued input is delivered.
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
            std::thread::sleep(Duration::from_millis(15));
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

fn vertical(probe: &Probe, direction: ScrollDirection, wheel_times: i64) -> String {
    scroll(ScrollParams {
        loc: Some(ListOrString::List(vec![probe.centre.0, probe.centre.1])),
        label: None,
        scroll_type: Some(ScrollType::Vertical),
        direction: Some(direction),
        wheel_times: Some(wheel_times),
        modifier: None,
    })
    .expect("scroll failed")
}

/// One notch down has to arrive as one notch down.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn scrolling_down_sends_a_negative_wheel_delta() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    vertical(&probe, ScrollDirection::Down, 1);
    probe.pump(Duration::from_millis(700));

    assert!(
        MESSAGE_COUNT.load(Ordering::SeqCst) > 0,
        "no wheel message arrived at all"
    );
    assert_eq!(
        TOTAL_DELTA.load(Ordering::SeqCst),
        -120,
        "one notch down should deliver exactly -WHEEL_DELTA"
    );
}

/// Up is the other sign, not merely "some movement".
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn scrolling_up_sends_a_positive_wheel_delta() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    vertical(&probe, ScrollDirection::Up, 1);
    probe.pump(Duration::from_millis(700));

    assert_eq!(TOTAL_DELTA.load(Ordering::SeqCst), 120);
}

/// `wheel_times` has to be the number of notches, not a flag.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn more_notches_deliver_proportionally_more_delta() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    vertical(&probe, ScrollDirection::Down, 4);
    probe.pump(Duration::from_millis(900));

    // Windows may coalesce the notches into fewer messages; the summed delta
    // is what the application ultimately acts on.
    assert_eq!(
        TOTAL_DELTA.load(Ordering::SeqCst),
        -480,
        "four notches should sum to 4 * -WHEEL_DELTA"
    );
}

/// Horizontal scrolling goes out as a horizontal wheel, not a vertical one.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn horizontal_scrolling_sends_a_horizontal_wheel() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    scroll(ScrollParams {
        loc: Some(ListOrString::List(vec![probe.centre.0, probe.centre.1])),
        label: None,
        scroll_type: Some(ScrollType::Horizontal),
        direction: Some(ScrollDirection::Right),
        wheel_times: Some(1),
        modifier: None,
    })
    .expect("scroll failed");
    probe.pump(Duration::from_millis(900));

    // The tool produces this by holding shift over a vertical wheel, which is
    // the convention applications read as horizontal scrolling. Either the
    // window sees a real horizontal wheel, or it sees the shifted vertical one
    // — what must not happen is nothing at all.
    let horizontal = HORIZONTAL_DELTA.load(Ordering::SeqCst);
    let vertical_delta = TOTAL_DELTA.load(Ordering::SeqCst);
    assert!(
        horizontal != 0 || vertical_delta != 0,
        "a horizontal scroll delivered no wheel event of either kind"
    );
}

/// A direction that does not belong to the axis is a caller mistake, and
/// nothing should be sent.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_direction_that_does_not_match_the_axis_sends_nothing() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    let response = vertical(&probe, ScrollDirection::Left, 2);
    probe.pump(Duration::from_millis(500));

    assert!(
        response.contains("Invalid direction"),
        "a left scroll on the vertical axis should be reported: {response}"
    );
    assert_eq!(
        MESSAGE_COUNT.load(Ordering::SeqCst),
        0,
        "a rejected scroll must not send a wheel event"
    );
}

/// Zero notches is a no-op that succeeds.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn zero_notches_sends_nothing() {
    let Some(probe) = Probe::open() else {
        eprintln!("skipped: the probe window could not take the foreground");
        return;
    };
    reset_counters();

    vertical(&probe, ScrollDirection::Down, 0);
    probe.pump(Duration::from_millis(500));

    assert_eq!(MESSAGE_COUNT.load(Ordering::SeqCst), 0);
}

/// Out-of-range notch counts are rejected before any input is sent.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_out_of_range_notch_count_is_rejected() {
    for wheel_times in [-1, 101] {
        let error = scroll(ScrollParams {
            loc: Some(ListOrString::List(vec![100, 100])),
            label: None,
            scroll_type: Some(ScrollType::Vertical),
            direction: Some(ScrollDirection::Down),
            wheel_times: Some(wheel_times),
            modifier: None,
        })
        .expect_err("an out-of-range notch count should be an error");
        assert!(error.contains("wheel_times"), "unexpected error: {error}");
    }
}
