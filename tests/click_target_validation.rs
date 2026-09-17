//! What the input tools do when a Snapshot element's coordinates have gone
//! stale.
//!
//! Snapshot's generation counter only invalidates ids when a *newer* capture
//! replaces them. Everything else that can move a control — the window being
//! minimized, moved, covered, or the view scrolling underneath it — leaves the
//! ids valid and the saved coordinates wrong, and a click then lands wherever
//! those coordinates now point. These tests drive each of those cases against
//! the harness window.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, Event, TestApp, desktop_lock};

use windows_operation_cli::params::ListOrString;
use windows_operation_cli::state::{self, ElementNode};
use windows_operation_cli::tools::click::{self, ClickParams};
use windows_operation_cli::tools::snapshot::{self, SnapshotParams};

/// Captures the harness window and returns its elements.
fn snapshot_app(app: &TestApp) -> Vec<ElementNode> {
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    snapshot::snapshot(&SnapshotParams::default()).expect("Snapshot failed");
    let state = state::current_state().expect("Snapshot did not publish desktop state");
    state
        .interactive_nodes
        .iter()
        .chain(state.scrollable_nodes.iter())
        .filter(|node| node.owner_handle == app.hwnd())
        .cloned()
        .collect()
}

fn click_label(element: &ElementNode) -> Result<String, String> {
    click::click(ClickParams {
        loc: None,
        label: Some(element.element_id as i64),
        button: None,
        clicks: Some(1),
        modifier: None,
    })
}

fn button_of(nodes: &[ElementNode]) -> ElementNode {
    nodes
        .iter()
        .find(|node| node.name == BUTTON_TEXT)
        .unwrap_or_else(|| panic!("the push button was not discovered; found: {nodes:#?}"))
        .clone()
}

/// The baseline the other tests are measured against: an untouched window
/// clicks through and the button reports it.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_click_on_an_unchanged_window_reaches_the_control() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Click Baseline") else {
        return;
    };
    let button = button_of(&snapshot_app(&app));

    app.drain_events();
    click_label(&button).expect("clicking an unchanged element should succeed");

    assert_eq!(
        app.wait_for_event(Duration::from_secs(3), |event| matches!(
            event,
            Event::ButtonClicked
        )),
        Some(Event::ButtonClicked),
        "the click did not reach the button"
    );
}

/// A minimized window still reports a rectangle — parked off-screen near
/// (-32000, -32000) — and the saved element center is parked inside it, so a
/// bounds check alone accepts the click and it goes nowhere.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_minimized_owner_is_rejected_rather_than_clicked_offscreen() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Click Minimized") else {
        return;
    };
    let button = button_of(&snapshot_app(&app));

    app.minimize();
    assert!(
        app.wait_until(Duration::from_secs(3), || {
            windows_operation_cli::window::is_minimized(app.hwnd())
        }),
        "the window never minimized"
    );

    app.drain_events();
    let error = click_label(&button).expect_err("a minimized owner must not be clicked");
    assert!(
        error.contains("minimized"),
        "the error should name the minimized window, got: {error}"
    );
    assert_eq!(
        app.next_event(Duration::from_millis(300)),
        None,
        "no click should have been delivered"
    );
}

/// Moving the window leaves the saved coordinates pointing at empty desktop
/// (or at whatever is now there) while the ids stay valid.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_moved_owner_is_rejected_rather_than_clicked_at_stale_coordinates() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Click Moved") else {
        return;
    };
    let button = button_of(&snapshot_app(&app));
    let before = app.window_rect();

    // Far enough that the button's old center falls outside the window.
    app.move_by(260, 180);
    assert!(
        app.wait_until(Duration::from_secs(3), || {
            app.window_rect().left != before.left
        }),
        "the window never moved"
    );

    app.drain_events();
    let error = click_label(&button).expect_err("a stale coordinate must not be clicked");
    assert!(
        error.contains("moved") || error.contains("no longer at"),
        "the error should explain the element moved, got: {error}"
    );
    assert_eq!(
        app.next_event(Duration::from_millis(300)),
        None,
        "no click should have been delivered"
    );
}

/// The identity probe has to accept the ordinary case: an element that has not
/// moved must still be clickable after an unrelated Snapshot-visible change.
/// A probe that reported `Differs` too eagerly would block every click.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_identity_probe_does_not_reject_an_element_that_stayed_put() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Click Identity") else {
        return;
    };
    let nodes = snapshot_app(&app);
    let button = button_of(&nodes);

    // Toggle an unrelated control: the tree changes, the button does not.
    app.set_checked(true);

    app.drain_events();
    click_label(&button).expect("an element that stayed put must remain clickable");
    assert_eq!(
        app.wait_for_event(Duration::from_secs(3), |event| matches!(
            event,
            Event::ButtonClicked
        )),
        Some(Event::ButtonClicked),
        "the click did not reach the button"
    );
}

/// A coordinate click carries no element identity, so it stays a raw click —
/// the validation above applies to `label`, not `loc`.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_coordinate_click_is_still_delivered_verbatim() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Click Coordinates") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    let (x, y) = app.center_of(harness::layout::BUTTON);

    app.drain_events();
    click::click(ClickParams {
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .expect("a coordinate click should not be blocked");

    assert_eq!(
        app.wait_for_event(Duration::from_secs(3), |event| matches!(
            event,
            Event::ButtonClicked
        )),
        Some(Event::ButtonClicked),
        "the coordinate click did not reach the button"
    );
}
