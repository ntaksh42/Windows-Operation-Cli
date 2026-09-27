//! Coverage for `window::is_point_occluded`, using two windows this test owns.
//!
//! The unit tests this replaces took their geometry from whatever window
//! happened to be in the foreground and returned early when there was none.
//! On a headless CI runner that meant they asserted nothing while still
//! reporting success. Stacking two known windows makes both directions of the
//! check deterministic, and the assertions no longer depend on the state of
//! the developer's desktop.

#![cfg(target_os = "windows")]

mod harness;

use std::time::{Duration, Instant};

use harness::{BUTTON_TEXT, TestApp, desktop_lock, layout};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::WindowFromPoint;
use windows_operation_cli::tools::snapshot::{SnapshotParams, capture};
use windows_operation_cli::window::{get_window_rect, is_point_occluded};

/// Waits until `(x, y)` is owned by `hwnd` or one of its child controls.
///
/// `TestApp::wait_until_on_top` cannot be used here: it derives its probe
/// point from the client origin captured when the window was created, which a
/// later move leaves stale. This waits on the point the assertion actually
/// cares about instead.
fn wait_until_point_belongs_to(hwnd: isize, x: i32, y: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if !is_point_occluded(x, y, hwnd) {
            return true;
        }
        if Instant::now() >= deadline {
            let hit = unsafe { WindowFromPoint(POINT { x, y }) };
            eprintln!("point ({x},{y}) is owned by {:#x}, expected {hwnd:#x}", hit.0 as isize);
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_window_owns_its_own_pixels_but_a_covered_one_does_not() {
    let _desktop = desktop_lock();

    // The first window is created, then covered by the second. Both are ours,
    // so the expected answer is known rather than inferred from the desktop.
    let Some(back) = TestApp::launch("Occlusion Back") else {
        return;
    };
    let back_handle = back.hwnd();
    let (x, y) = back.center_of(layout::BUTTON);

    // Before anything covers it, the back window owns the point.
    assert!(
        !is_point_occluded(x, y, back_handle),
        "a window with nothing in front of it must own its own pixels"
    );

    let Some(front) = TestApp::launch("Occlusion Front") else {
        return;
    };
    // Put the second window exactly over the first, so the point under test is
    // inside both.
    let back_rect = get_window_rect(back_handle).expect("the back window has no bounds");
    windows_operation_cli::window::resize_window(
        front.hwnd(),
        Some((back_rect.0, back_rect.1)),
        Some((back_rect.2, back_rect.3)),
    )
    .expect("failed to move the front window over the back one");
    assert!(
        wait_until_point_belongs_to(front.hwnd(), x, y, Duration::from_secs(3)),
        "the front window never covered the point under test"
    );

    assert!(
        is_point_occluded(x, y, back_handle),
        "a window covered by another must not be treated as clickable at ({x},{y})"
    );
    assert!(
        !is_point_occluded(x, y, front.hwnd()),
        "the covering window itself must own that point"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_child_control_counts_as_its_own_top_level_window() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Occlusion Children") else {
        return;
    };

    // The point over the push button resolves to the button's HWND, not the
    // frame's. A hit on a window's own control is not occlusion.
    let (x, y) = app.center_of(layout::BUTTON);
    assert!(
        !is_point_occluded(x, y, app.hwnd()),
        "a window's own child control must not count as covering it"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_owner_handle_that_no_longer_resolves_is_occluded() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Occlusion Bogus") else {
        return;
    };
    let (x, y) = app.center_of(layout::BUTTON);

    // A handle that does not resolve to a live window cannot own the point.
    assert!(
        is_point_occluded(x, y, 1),
        "a bogus owner handle must be reported as occluded"
    );
}

/// Stacks two windows of this one process, the back one exactly under the
/// front one — the shape of two Notepad windows.
fn stacked_pair(back_title: &str, front_title: &str) -> Option<(TestApp, TestApp)> {
    let back = TestApp::launch(back_title)?;
    let front = TestApp::launch(front_title)?;
    let rect = get_window_rect(back.hwnd()).expect("the back window has no bounds");
    windows_operation_cli::window::resize_window(
        front.hwnd(),
        Some((rect.0, rect.1)),
        Some((rect.2, rect.3)),
    )
    .expect("failed to move the front window over the back one");
    let (x, y) = back.center_of(layout::BUTTON);
    assert!(
        wait_until_point_belongs_to(front.hwnd(), x, y, Duration::from_secs(3)),
        "the front window never covered the back one"
    );
    Some((back, front))
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_foreground_snapshot_leaves_out_a_same_process_window_hidden_behind_it() {
    let _desktop = desktop_lock();
    let Some((back, front)) = stacked_pair("Stacked Back", "Stacked Front") else {
        return;
    };
    assert!(
        front.wait_until(Duration::from_secs(3), || {
            windows_operation_cli::window::foreground_window()
                .is_some_and(|window| window.handle == front.hwnd())
        }),
        "the front window never reached the foreground"
    );

    let result = capture(&SnapshotParams::default()).expect("capture failed");
    assert!(
        result
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == front.hwnd() && node.name == BUTTON_TEXT),
        "{}",
        result.text
    );
    assert!(
        !result
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == back.hwnd()),
        "the covered window's controls were reported:\n{}",
        result.text
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_covered_window_asked_for_by_name_marks_its_controls_covered() {
    let _desktop = desktop_lock();
    let Some(_pair) = stacked_pair("Stacked Named Back", "Stacked Named Front") else {
        return;
    };

    let result = capture(&SnapshotParams {
        window: Some("Stacked Named Back".to_string()),
        ..Default::default()
    })
    .expect("capture failed");
    let line = result
        .text
        .lines()
        .find(|line| line.contains(&format!("button \"{BUTTON_TEXT}\"")))
        .unwrap_or_else(|| panic!("the button was not listed:\n{}", result.text));
    assert!(line.contains("action: covered by another window"), "{line}");
}
