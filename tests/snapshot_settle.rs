//! Regression coverage for `Snapshot` called immediately after a window
//! appears.
//!
//! `GetForegroundWindow` reports a new window as soon as it is activated, but
//! `EnumWindows` and the virtual-desktop manager include it only about 200ms
//! later. Snapshot enumerating in that gap used to produce a window list
//! without the very window it was asked to scan, and a `foreground` scan then
//! failed with "No foreground window is available for UI tree scanning" —
//! exactly the `App(launch)` -> `Snapshot` sequence the skill documents.
//!
//! These tests deliberately do **not** wait for the window to become
//! enumerable: that wait is what the production code now owns.

#![cfg(target_os = "windows")]

mod harness;

use harness::{TestApp, desktop_lock};

use windows_operation_cli::tools::snapshot::{self, SnapshotParams};

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn snapshot_succeeds_immediately_after_a_window_is_created() {
    let _desktop = desktop_lock();

    // Repeated because the failure was order-dependent: the first window of a
    // process used to succeed and every later one failed.
    for round in 0..4 {
        let Some(_app) = TestApp::launch(&format!("Settle {round}")) else {
            return;
        };
        snapshot::snapshot(&SnapshotParams::default()).unwrap_or_else(|error| {
            panic!("Snapshot failed on round {round} right after launch: {error}")
        });
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_snapshot_taken_at_once_still_finds_the_new_windows_controls() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Settle Contents") else {
        return;
    };

    // Succeeding is not enough: the scan must cover the new window, not fall
    // back to some other window that was already enumerable.
    snapshot::snapshot(&SnapshotParams::default()).expect("Snapshot failed right after launch");
    let state = windows_operation_cli::state::current_state().expect("no desktop state published");
    let found = state
        .interactive_nodes
        .iter()
        .any(|node| node.owner_handle == app.hwnd() && node.name == harness::BUTTON_TEXT);
    assert!(
        found,
        "the snapshot taken right after launch did not include the new window's button"
    );
}
