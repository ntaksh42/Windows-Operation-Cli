//! What the tools do when the window they were told about is gone.
//!
//! A capture is a description of a moment. By the time the caller acts on it
//! the application may have closed the window, crashed, or replaced it — and
//! the element ids still look perfectly valid. Acting on them then has to
//! fail in a way the caller can understand, not click through to whatever
//! took the window's place.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, TestApp, desktop_lock, layout};

use windows_operation_cli::params::{BoolOrString, ListOrString};
use windows_operation_cli::state::{self, ElementNode};
use windows_operation_cli::tools::click::{ClickParams, click};
use windows_operation_cli::tools::invoke_element::{InvokeElementParams, invoke_element};
use windows_operation_cli::tools::snapshot::{SnapshotParams, snapshot};
use windows_operation_cli::tools::typing::{TypeParams, type_text};
use windows_operation_cli::window;

/// Captures the harness and returns the button's node, then lets the caller
/// close the window before acting on it.
fn capture_button(app: &TestApp) -> ElementNode {
    snapshot(&SnapshotParams {
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("Snapshot failed");
    let published = state::current_state().expect("Snapshot published no state");
    published
        .interactive_nodes
        .iter()
        .find(|node| node.owner_handle == app.hwnd() && node.name == BUTTON_TEXT)
        .cloned()
        .unwrap_or_else(|| panic!("the button was not captured"))
}

/// Waits until the window is really gone from the desktop.
fn wait_until_gone(handle: isize) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !window::list_current_windows()
            .iter()
            .any(|candidate| candidate.handle == handle)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Invoking an element whose window has closed has to say the window is
/// gone, not report success or reach for whatever is at those coordinates.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn invoking_into_a_closed_window_is_refused() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Invoke") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let button = capture_button(&app);
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    let error = invoke_element(InvokeElementParams {
        element_id: button.element_id,
        fallback_to_click: None,
    })
    .expect_err("invoking into a closed window should fail");
    assert!(
        error.to_lowercase().contains("closed")
            || error.to_lowercase().contains("window")
            || error.to_lowercase().contains("matched 0"),
        "the refusal should explain the window is gone: {error}"
    );
}

/// The coordinate fallback must not paper over it: those coordinates now
/// belong to whatever is behind.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_click_fallback_into_a_closed_window_is_refused() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Fallback") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let button = capture_button(&app);
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    let error = invoke_element(InvokeElementParams {
        element_id: button.element_id,
        fallback_to_click: Some(BoolOrString::Bool(true)),
    })
    .expect_err("the fallback should not click into a closed window");
    assert!(
        error.to_lowercase().contains("closed") || error.to_lowercase().contains("window"),
        "the refusal should explain the window is gone: {error}"
    );
}

/// Clicking by label has the same obligation.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn clicking_a_label_from_a_closed_window_is_refused() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Click") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let button = capture_button(&app);
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    let error = click(ClickParams {
        loc: None,
        label: Some(button.element_id as i64),
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .expect_err("clicking into a closed window should fail");
    assert!(
        error.to_lowercase().contains("closed") || error.to_lowercase().contains("window"),
        "the refusal should explain the window is gone: {error}"
    );
}

/// Typing has to refuse too — text sent to a closed window's coordinates
/// lands in whatever now has focus, which could be anything.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn typing_into_a_closed_window_is_refused() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Type") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    snapshot(&SnapshotParams {
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("Snapshot failed");
    let published = state::current_state().expect("no state");
    let edit = published
        .interactive_nodes
        .iter()
        .find(|node| node.owner_handle == app.hwnd() && node.control_type == "edit")
        .cloned()
        .expect("no edit control was captured");
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    let error = type_text(TypeParams {
        text: "text with nowhere to go".to_string(),
        loc: None,
        label: Some(edit.element_id as i64),
        clear: Some(BoolOrString::Bool(false)),
        caret_position: None,
        press_enter: None,
    })
    .expect_err("typing into a closed window should fail");
    assert!(
        error.to_lowercase().contains("closed") || error.to_lowercase().contains("window"),
        "the refusal should explain the window is gone: {error}"
    );
}

/// A coordinate click carries no window identity, so it stays a raw click —
/// the tool cannot know the window it was aimed at has gone, and must not
/// pretend otherwise.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_coordinate_click_is_still_delivered_after_the_window_closes() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Coordinates") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let (x, y) = app.center_of(layout::BUTTON);
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    // This is allowed to succeed: the caller asked for a point, not an
    // element, and the point is still a point.
    click(ClickParams {
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .expect("a coordinate click should not be blocked by an unrelated closure");
}

/// A capture taken while nothing of the app remains must not carry its
/// elements forward.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_new_capture_drops_the_closed_windows_elements() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Vanishing Capture") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let _ = capture_button(&app);
    let handle = app.hwnd();

    drop(app);
    assert!(wait_until_gone(handle), "the window did not close");

    snapshot(&SnapshotParams {
        scope: Some(windows_operation_cli::tools::snapshot::SnapshotScope::All),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("Snapshot failed");
    let published = state::current_state().expect("no state");
    assert!(
        !published
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == handle),
        "a fresh capture still lists the closed window's controls"
    );
}
