//! The `Click` tool's buttons, counts, and modifiers.
//!
//! Only a single plain left click was covered. Right and middle buttons, the
//! double and triple counts, hover, and a held modifier each produce a
//! different message — or a different `wParam` on the same message — and none
//! of them was checked against a window that could tell the difference.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use windows_operation_cli::input_sim;
use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::click::{ClickButton, ClickParams, click};

static LEFT_DOWN: AtomicU32 = AtomicU32::new(0);
static RIGHT_DOWN: AtomicU32 = AtomicU32::new(0);
static MIDDLE_DOWN: AtomicU32 = AtomicU32::new(0);
static DOUBLE_CLICKS: AtomicU32 = AtomicU32::new(0);
/// `wParam` of the most recent button-down, which carries the modifier keys
/// Windows saw held at the time.
static LAST_KEYS: AtomicI32 = AtomicI32::new(0);

fn reset() {
    for counter in [&LEFT_DOWN, &RIGHT_DOWN, &MIDDLE_DOWN, &DOUBLE_CLICKS] {
        counter.store(0, Ordering::SeqCst);
    }
    LAST_KEYS.store(0, Ordering::SeqCst);
}

unsafe extern "system" fn probe_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_LBUTTONDOWN => {
            LEFT_DOWN.fetch_add(1, Ordering::SeqCst);
            LAST_KEYS.store(wparam.0 as i32, Ordering::SeqCst);
        }
        WM_RBUTTONDOWN => {
            RIGHT_DOWN.fetch_add(1, Ordering::SeqCst);
            LAST_KEYS.store(wparam.0 as i32, Ordering::SeqCst);
        }
        WM_MBUTTONDOWN => {
            MIDDLE_DOWN.fetch_add(1, Ordering::SeqCst);
        }
        WM_LBUTTONDBLCLK => {
            DOUBLE_CLICKS.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// A window that counts the button messages it receives.
struct Probe {
    hwnd: isize,
    centre: (i32, i32),
}

impl Probe {
    fn open() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?;
            let class = w!("WindowsOperationCliClickProbe");
            let descriptor = WNDCLASSW {
                // CS_DBLCLKS is what makes Windows synthesise
                // WM_LBUTTONDBLCLK; without it a double click arrives as two
                // ordinary presses and the distinction cannot be observed.
                style: CS_DBLCLKS,
                lpfnWndProc: Some(probe_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&descriptor);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(w!("Click Probe").as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                180,
                180,
                460,
                360,
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
                    return None;
                }
            }
        }
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

    fn click(&self, button: Option<ClickButton>, clicks: i64, modifier: Option<&str>) {
        click(ClickParams {
            loc: Some(ListOrString::List(vec![self.centre.0, self.centre.1])),
            label: None,
            button,
            clicks: Some(clicks),
            modifier: modifier.map(str::to_string),
        })
        .expect("click failed");
        self.pump(Duration::from_millis(900));
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

fn probe() -> Option<Probe> {
    let probe = Probe::open();
    if probe.is_none() {
        eprintln!("skipped: the probe window could not take the foreground");
    }
    probe
}

/// Each button has to arrive as its own message, not as a left click.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn each_button_arrives_as_itself() {
    let Some(probe) = probe() else { return };

    reset();
    probe.click(Some(ClickButton::Left), 1, None);
    assert_eq!(LEFT_DOWN.load(Ordering::SeqCst), 1, "left click");
    assert_eq!(RIGHT_DOWN.load(Ordering::SeqCst), 0);

    reset();
    probe.click(Some(ClickButton::Right), 1, None);
    assert_eq!(RIGHT_DOWN.load(Ordering::SeqCst), 1, "right click");
    assert_eq!(LEFT_DOWN.load(Ordering::SeqCst), 0);

    reset();
    probe.click(Some(ClickButton::Middle), 1, None);
    assert_eq!(MIDDLE_DOWN.load(Ordering::SeqCst), 1, "middle click");
    assert_eq!(LEFT_DOWN.load(Ordering::SeqCst), 0);
}

/// Two clicks have to register as a double click, which is a different
/// message — not merely two presses close together.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn two_clicks_register_as_a_double_click() {
    let Some(probe) = probe() else { return };
    reset();

    probe.click(Some(ClickButton::Left), 2, None);
    assert!(
        DOUBLE_CLICKS.load(Ordering::SeqCst) > 0,
        "two clicks did not reach the window as a double click (presses: {})",
        LEFT_DOWN.load(Ordering::SeqCst)
    );
}

/// Three clicks have to deliver three presses, whatever Windows makes of the
/// second one.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn three_clicks_deliver_three_presses() {
    let Some(probe) = probe() else { return };
    reset();

    probe.click(Some(ClickButton::Left), 3, None);
    // The second press arrives as WM_LBUTTONDBLCLK rather than
    // WM_LBUTTONDOWN, so the two counters together make up the three.
    let presses = LEFT_DOWN.load(Ordering::SeqCst) + DOUBLE_CLICKS.load(Ordering::SeqCst);
    assert_eq!(presses, 3, "a triple click delivered {presses} presses");
}

/// Hover moves the cursor and sends nothing.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn zero_clicks_hovers_without_pressing() {
    let Some(probe) = probe() else { return };
    // Start somewhere else so the move is observable.
    input_sim::set_cursor_pos(probe.centre.0 - 80, probe.centre.1 - 60).expect("cursor");
    reset();

    probe.click(None, 0, None);
    assert_eq!(
        input_sim::get_cursor_pos(),
        probe.centre,
        "hover did not move the cursor"
    );
    assert_eq!(
        LEFT_DOWN.load(Ordering::SeqCst),
        0,
        "hover pressed the button"
    );
}

/// A modifier has to be held while the click lands, which the window sees in
/// the message's key flags.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn a_modifier_is_held_during_the_click() {
    let Some(probe) = probe() else { return };

    reset();
    probe.click(Some(ClickButton::Left), 1, Some("ctrl"));
    // MK_CONTROL is bit 3 of wParam.
    assert!(
        LAST_KEYS.load(Ordering::SeqCst) & 0x0008 != 0,
        "the window did not see Ctrl held (keys={:#06x})",
        LAST_KEYS.load(Ordering::SeqCst)
    );

    reset();
    probe.click(Some(ClickButton::Left), 1, Some("shift"));
    // MK_SHIFT is bit 2.
    assert!(
        LAST_KEYS.load(Ordering::SeqCst) & 0x0004 != 0,
        "the window did not see Shift held (keys={:#06x})",
        LAST_KEYS.load(Ordering::SeqCst)
    );
}

/// The modifier must not stay held once the click is done.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn a_modifier_is_released_afterwards() {
    let Some(probe) = probe() else { return };

    probe.click(Some(ClickButton::Left), 1, Some("ctrl"));
    reset();
    probe.click(Some(ClickButton::Left), 1, None);
    assert_eq!(
        LAST_KEYS.load(Ordering::SeqCst) & 0x0008,
        0,
        "Ctrl was still held on the next click"
    );
}

/// An unknown modifier is a caller mistake, rejected before any input.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn an_unknown_modifier_is_rejected_without_clicking() {
    let Some(probe) = probe() else { return };
    reset();

    let error = click(ClickParams {
        loc: Some(ListOrString::List(vec![probe.centre.0, probe.centre.1])),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: Some("hyper".to_string()),
    })
    .expect_err("an unknown modifier should be rejected");
    assert!(error.contains("modifier must be"), "unexpected: {error}");

    probe.pump(Duration::from_millis(300));
    assert_eq!(
        LEFT_DOWN.load(Ordering::SeqCst),
        0,
        "a rejected click still pressed the button"
    );
}
