//! Verifies the test application itself comes up with the controls the other
//! integration tests rely on. If this fails, failures elsewhere are the
//! harness's fault rather than the code under test.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{CHECKBOX_TEXT, Event, LIST_ITEMS, TestApp, desktop_lock, layout};

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_test_app_starts_with_all_of_its_controls() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Harness Smoke") else {
        return; // no interactive desktop
    };

    assert_ne!(app.hwnd(), 0, "the top-level window was not created");
    assert_ne!(app.button_hwnd(), 0, "the push button was not created");
    assert_ne!(app.edit_hwnd(), 0, "the edit control was not created");
    assert_ne!(app.checkbox_hwnd(), 0, "the checkbox was not created");
    assert_ne!(app.listbox_hwnd(), 0, "the listbox was not created");

    assert_eq!(app.edit_text(), "", "the edit control should start empty");
    assert!(!app.is_checked(), "the checkbox should start unchecked");
    assert_eq!(
        app.list_selection(),
        -1,
        "the listbox should start with no selection"
    );

    let rect = app.window_rect();
    assert!(
        rect.right > rect.left && rect.bottom > rect.top,
        "the window has no area: {rect:?}"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_window_reports_the_events_tests_assert_on() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Harness Events") else {
        return;
    };

    // Drive the controls directly, without going through input injection: this
    // isolates the harness's own reporting from the input pipeline the other
    // tests exercise.
    app.set_checked(true);
    assert!(app.is_checked(), "set_checked did not take effect");

    app.drain_events();
    let (x, y) = app.center_of(layout::BUTTON);
    windows_operation_cli::input_sim::click_once(
        x,
        y,
        windows_operation_cli::input_sim::MouseButton::Left,
        Duration::from_millis(50),
    )
    .expect("click injection failed");

    let event = app.wait_for_event(Duration::from_secs(3), |event| {
        matches!(event, Event::ButtonClicked)
    });
    assert_eq!(
        event,
        Some(Event::ButtonClicked),
        "clicking the button did not produce a ButtonClicked event"
    );

    assert_eq!(LIST_ITEMS.len(), 3);
    assert_eq!(CHECKBOX_TEXT, "Enable Feature");
}
