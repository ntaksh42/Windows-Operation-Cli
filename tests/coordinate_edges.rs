//! Coordinates at and beyond the edges of the desktop.
//!
//! Every test so far aims at a point inside a window. A caller working from a
//! stale capture, or from arithmetic of its own, will eventually pass one that
//! is not — and what happens then decides whether the mistake is visible or
//! silently lands somewhere else.

#![cfg(target_os = "windows")]

use windows_operation_cli::input_sim;
use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::click::{ClickParams, click};
use windows_operation_cli::tools::move_mouse::{MoveParams, move_mouse};

/// The desktop's bounds, as the input layer sees them.
fn virtual_screen() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
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

/// The corners of the desktop are legitimate targets and have to be reachable
/// exactly — a control in the corner is not an edge case to the caller.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn the_corners_of_the_desktop_are_reachable() {
    let (x, y, width, height) = virtual_screen();
    for target in [
        (x, y),
        (x + width - 1, y),
        (x, y + height - 1),
        (x + width - 1, y + height - 1),
    ] {
        move_to(target.0, target.1).expect("move failed");
        assert_eq!(
            input_sim::get_cursor_pos(),
            target,
            "the cursor did not reach the corner {target:?}"
        );
    }
}

/// A point past the desktop is clamped to its edge rather than wrapping or
/// failing. What matters is that it stays on the desktop: a wrapped
/// coordinate would click the far side of the screen.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn a_point_past_the_edge_stays_on_the_desktop() {
    let (x, y, width, height) = virtual_screen();

    for target in [
        (x - 500, y + height / 2),
        (x + width + 500, y + height / 2),
        (x + width / 2, y - 500),
        (x + width / 2, y + height + 500),
    ] {
        // The move itself may fail, which is a fine answer; what must not
        // happen is the cursor landing somewhere unrelated.
        let _ = move_to(target.0, target.1);
        let (cx, cy) = input_sim::get_cursor_pos();
        assert!(
            cx >= x && cx < x + width && cy >= y && cy < y + height,
            "aiming at {target:?} put the cursor at ({cx},{cy}), off the desktop \
             ({x},{y} {width}x{height})"
        );
    }
}

/// An extreme coordinate must not panic or overflow the normalisation, which
/// scales by 65535 before dividing.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn an_extreme_coordinate_is_handled_without_panicking() {
    let (x, y, width, height) = virtual_screen();

    for target in [
        (i32::MAX, i32::MAX),
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MIN),
    ] {
        let _ = move_to(target.0, target.1);
        let (cx, cy) = input_sim::get_cursor_pos();
        assert!(
            cx >= x && cx < x + width && cy >= y && cy < y + height,
            "aiming at {target:?} put the cursor at ({cx},{cy}), off the desktop"
        );
    }
}

/// A click at an extreme coordinate has to be as contained as a move: the
/// press lands where the cursor is, and the cursor stays on the desktop.
#[test]
#[ignore = "sends mouse input; run with --ignored"]
fn a_click_at_an_extreme_coordinate_stays_on_the_desktop() {
    let (x, y, width, height) = virtual_screen();

    // Park the cursor somewhere harmless first: whatever the clamp produces,
    // the click lands there, so this should not be over anything important.
    move_to(x + width / 2, y + height - 2).expect("move failed");

    let _ = click(ClickParams {
        loc: Some(ListOrString::List(vec![i32::MAX, i32::MAX])),
        label: None,
        button: None,
        // Hover: this checks the coordinate handling, and a press at an
        // arbitrary clamped point could hit whatever is under it.
        clicks: Some(0),
        modifier: None,
    });
    let (cx, cy) = input_sim::get_cursor_pos();
    assert!(
        cx >= x && cx < x + width && cy >= y && cy < y + height,
        "the cursor ended at ({cx},{cy}), off the desktop"
    );
}

/// A coordinate list of the wrong length is a caller mistake, and has to be
/// named rather than silently taking the first two values.
#[test]
fn a_coordinate_list_of_the_wrong_length_is_rejected() {
    for loc in [vec![10], vec![10, 20, 30], vec![]] {
        let length = loc.len();
        let error = click(ClickParams {
            loc: Some(ListOrString::List(loc)),
            label: None,
            button: None,
            clicks: Some(1),
            modifier: None,
        })
        .expect_err("a malformed coordinate should be rejected");
        assert!(
            error.contains("exactly 2 integers"),
            "a {length}-element location produced: {error}"
        );
    }
}

/// A `loc` given as a JSON string is accepted — MCP clients send it that way
/// — but a malformed one has to be reported, not parsed into nonsense.
#[test]
fn a_malformed_coordinate_string_is_rejected() {
    let error = click(ClickParams {
        loc: Some(ListOrString::Str("not a list".to_string())),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .expect_err("a malformed coordinate string should be rejected");
    assert!(error.contains("Invalid list value"), "unexpected: {error}");
}

/// The string form has to produce the same point as the list form.
#[test]
#[ignore = "moves the mouse cursor; run with --ignored"]
fn a_coordinate_string_reaches_the_same_point_as_a_list() {
    let (x, y, width, height) = virtual_screen();
    let target = (x + width / 3, y + height / 3);

    move_to(target.0, target.1).expect("list form failed");
    let from_list = input_sim::get_cursor_pos();

    move_to(x, y).expect("reset failed");
    move_mouse(MoveParams {
        loc: Some(ListOrString::Str(format!("[{}, {}]", target.0, target.1))),
        label: None,
        drag: None,
        from_loc: None,
        duration: None,
    })
    .expect("string form failed");
    assert_eq!(
        input_sim::get_cursor_pos(),
        from_list,
        "the string form landed somewhere else"
    );
}
