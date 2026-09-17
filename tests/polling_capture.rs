//! The polling capture `WaitFor` uses must see exactly what the full capture
//! sees.
//!
//! `capture_for_polling` skips building the response text — the window table,
//! the UI tree, the virtual-desktop listing — because `WaitFor` reads only the
//! node lists and window titles. Skipping the wrong thing would make a
//! condition that the full capture satisfies quietly never fire.

#![cfg(target_os = "windows")]

mod harness;

use std::collections::HashSet;
use std::time::Duration;

use harness::{BUTTON_TEXT, TestApp, desktop_lock};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, capture, capture_for_polling};

fn polling_params() -> SnapshotParams {
    SnapshotParams {
        use_vision: Some(BoolOrString::Bool(false)),
        use_annotation: Some(BoolOrString::Bool(false)),
        use_ui_tree: Some(BoolOrString::Bool(true)),
        ..Default::default()
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_polling_capture_sees_the_same_elements_and_titles() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Polling Parity") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let full = capture(&polling_params()).expect("full capture failed");
    let polled = capture_for_polling(&polling_params()).expect("polling capture failed");

    // The conditions WaitFor evaluates read exactly these.
    let names = |result: &windows_operation_cli::tools::snapshot::SnapshotResult| {
        result
            .interactive_nodes
            .iter()
            .chain(result.scrollable_nodes.iter())
            .map(|node| node.name.clone())
            .collect::<HashSet<_>>()
    };
    assert_eq!(
        names(&full),
        names(&polled),
        "the polling capture found a different element set"
    );
    // The test window holds the foreground across both captures, so this one
    // title is stable enough to compare directly.
    assert_eq!(
        full.focused_window_title.as_deref(),
        Some("Polling Parity"),
        "the test window was not in the foreground"
    );
    assert_eq!(
        full.focused_window_title, polled.focused_window_title,
        "the polling capture reported a different focused window"
    );

    // Compare how many windows were reported rather than their exact titles.
    // The two captures are taken moments apart, and a title can legitimately
    // differ between them — a spinner ticking from "⠼ DevDeck" to "⠴ DevDeck"
    // is a different string for the same window, and asserting on the set
    // would make this test fail whenever such an app is open.
    let titles = |result: &windows_operation_cli::tools::snapshot::SnapshotResult| {
        result.window_titles.iter().cloned().collect::<HashSet<_>>()
    };
    assert_eq!(
        titles(&full).len(),
        titles(&polled).len(),
        "the polling capture reported a different number of windows:\nfull: {:?}\npolled: {:?}",
        titles(&full),
        titles(&polled)
    );
    // The window under test is stable, so it must appear in both.
    assert!(
        titles(&full).contains("Polling Parity") && titles(&polled).contains("Polling Parity"),
        "the test window is missing from one of the captures"
    );

    // The harness window is what the parity above is being asserted against;
    // if it were missing, the comparison would be vacuous.
    assert!(
        names(&polled).contains(BUTTON_TEXT),
        "the polling capture did not find the harness button"
    );
    assert!(
        polled.text.is_empty(),
        "the polling capture should not build response text"
    );
}

/// The full capture still renders everything the polling one skips.
///
/// Worth stating explicitly: the two paths share one implementation, so a
/// change that made the polling branch unconditional would blank the Snapshot
/// response without failing the parity test above.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_full_capture_still_renders_its_response_text() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Full Capture Text") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let full = capture(&polling_params()).expect("full capture failed");
    for section in ["Cursor Position:", "Focused Window:", "Opened Windows:", "UI Tree:"] {
        assert!(
            full.text.contains(section),
            "the full capture dropped the {section:?} section"
        );
    }
}
