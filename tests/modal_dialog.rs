//! What a capture reports while a modal dialog is up.
//!
//! A modal blocks its owner: clicking the controls behind it does nothing, and
//! offering them is worse than offering nothing, because a caller will try
//! them and see no effect. The capture has to lead with the dialog.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, MODAL_BUTTON_TEXT, MODAL_TITLE, TestApp, desktop_lock};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, SnapshotResult, capture};
use windows_operation_cli::window;

fn capture_desktop() -> SnapshotResult {
    capture(&SnapshotParams {
        scope: Some(windows_operation_cli::tools::snapshot::SnapshotScope::All),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("capture failed")
}

/// The dialog's own control has to be reachable — it is the only thing the
/// user can act on while the modal is up.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_modal_dialogs_controls_are_captured() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Modal Controls") else {
        return;
    };
    let modal = app
        .show_modal(Duration::from_secs(3))
        .expect("the modal dialog never appeared");
    // Closing it is what re-enables the owner; leaving it up would strand the
    // harness window disabled for later tests.
    let _guard = ModalGuard(&app);

    // The dialog is an owned popup, so it is its own top-level window.
    assert!(
        window::list_snapshot_windows()
            .iter()
            .any(|candidate| candidate.handle == modal),
        "the modal dialog is not in the scan list"
    );

    let result = capture_desktop();
    let names: Vec<&str> = result
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == modal)
        .map(|node| node.name.as_str())
        .collect();
    assert!(
        names.iter().any(|name| name.contains(MODAL_BUTTON_TEXT)),
        "the dialog's button was not captured; found {names:?}"
    );
}

/// While the modal is up its owner is disabled, so the owner's controls are
/// not actionable and must not be offered as if they were.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_blocked_owners_controls_are_not_offered_as_clickable() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Modal Blocks Owner") else {
        return;
    };

    // Baseline: the owner's button is offered before the dialog goes up.
    let before = capture_desktop();
    assert!(
        before
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == app.hwnd() && node.name == BUTTON_TEXT),
        "the owner's button should be clickable before the modal appears"
    );

    app.show_modal(Duration::from_secs(3))
        .expect("the modal dialog never appeared");
    let _guard = ModalGuard(&app);

    let during = capture_desktop();
    let offered = during
        .interactive_nodes
        .iter()
        .any(|node| node.owner_handle == app.hwnd() && node.name == BUTTON_TEXT);
    assert!(
        !offered,
        "the owner's button is still offered as clickable while a modal dialog blocks it"
    );
}

/// Being unable to click the blocked window is not the same as being unable
/// to read it: what it says is often why the dialog is up.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_blocked_owners_text_is_still_readable() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Modal Text Readable") else {
        return;
    };
    app.show_modal(Duration::from_secs(3))
        .expect("the modal dialog never appeared");
    let _guard = ModalGuard(&app);

    // Text is collected for a targeted capture, not for a whole-desktop sweep
    // (which would nearly double the scan for text the follow-up capture
    // picks up anyway), so ask about this window specifically.
    let result = capture(&SnapshotParams {
        window: Some("Modal Text Readable".to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("capture failed");
    let readable: Vec<&str> = result
        .informative_nodes
        .iter()
        .filter(|node| node.owner_handle == app.hwnd())
        .map(|node| node.name.as_str())
        .collect();
    assert!(
        readable.iter().any(|name| name.contains(harness::STATUS_TEXT)),
        "the blocked window's label should still be readable; found {readable:?}"
    );
}

/// The window the caller must deal with should be identifiable in the
/// response text, not buried among the other windows.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_response_names_the_modal_dialog() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Modal Named") else {
        return;
    };
    app.show_modal(Duration::from_secs(3))
        .expect("the modal dialog never appeared");
    let _guard = ModalGuard(&app);

    let result = capture_desktop();
    assert!(
        result.text.contains(MODAL_TITLE),
        "the capture text does not mention the modal dialog"
    );
}

/// Closes the modal on the way out, whatever the test did.
struct ModalGuard<'a>(&'a TestApp);

impl Drop for ModalGuard<'_> {
    fn drop(&mut self) {
        self.0.close_modal();
    }
}
