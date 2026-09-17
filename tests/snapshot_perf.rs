//! End-to-end `Snapshot` cost, and the invariants the enumeration rework
//! had to preserve.
//!
//! Snapshot used to run two independent `EnumWindows` passes — one for the
//! UIA walk, one for the response table — each paying a per-window
//! `CoCreateInstance` for the virtual-desktop filter. Together that was ~26ms
//! of fixed overhead on every capture and every `WaitFor` poll.

#![cfg(target_os = "windows")]

use std::time::Instant;

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, snapshot};
use windows_operation_cli::window;

/// A capture with no UI tree and no screenshot is almost entirely window
/// enumeration, so it isolates what the rework targeted.
#[test]
#[ignore = "timing-sensitive; requires an interactive Windows desktop session"]
fn a_table_only_capture_is_not_dominated_by_enumeration() {
    let params = || SnapshotParams {
        use_ui_tree: Some(BoolOrString::Bool(false)),
        ..Default::default()
    };

    // Warm the desktop-manager cache and the registry reads.
    let _ = snapshot(&params()).expect("warm-up capture failed");

    let start = Instant::now();
    let result = snapshot(&params()).expect("capture failed");
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    eprintln!("table-only Snapshot = {elapsed_ms:.2}ms");
    assert!(
        result.text.contains("Opened Windows:"),
        "the capture did not produce a window table"
    );
    // The old double enumeration alone cost ~26ms. Leave generous headroom for
    // the virtual-desktop registry reads and the display query that remain.
    assert!(
        elapsed_ms < 25.0,
        "a table-only Snapshot took {elapsed_ms:.2}ms; enumeration is dominating again"
    );
}

/// The table is now derived from the scan list rather than enumerated
/// separately, so it must still exclude exactly what it excluded before:
/// untitled windows, the shell's own chrome, and the input overlay.
#[test]
#[ignore = "requires an interactive Windows desktop session"]
fn the_derived_table_excludes_shell_chrome_and_untitled_windows() {
    let scanned = window::list_snapshot_windows();
    let table = window::windows_for_table(&scanned);

    for window in &table {
        assert!(
            !window.title.is_empty(),
            "an untitled window reached the table: {}",
            window.handle
        );
        assert!(
            !window.title.trim().contains("Overlay"),
            "the input overlay reached the table: {:?}",
            window.title
        );
    }

    for shell_class in ["Progman", "Shell_TrayWnd"] {
        let shell_handles: Vec<_> = scanned
            .iter()
            .filter(|window| window.class_name == shell_class)
            .map(|window| window.handle)
            .collect();
        for handle in shell_handles {
            assert!(
                !table.iter().any(|window| window.handle == handle),
                "{shell_class} reached the table"
            );
        }
    }
}
