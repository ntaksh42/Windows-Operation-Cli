//! Window enumeration cost.
//!
//! `is_window_on_current_desktop` used to create a fresh
//! `IVirtualDesktopManager` per window from inside the `EnumWindows`
//! callback, which put a COM object creation on every window of every
//! enumeration: measured at 26ms per Snapshot against a 0.45ms raw walk.
//! These tests pin the cost down so the regression cannot come back
//! unnoticed.

#![cfg(target_os = "windows")]

use std::time::Instant;

use windows_operation_cli::window;

/// The raw walk is the floor: no COM, no per-window process queries.
fn raw_enumeration_ms() -> f64 {
    let start = Instant::now();
    let windows = window::list_windows();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    assert!(!windows.is_empty(), "no visible windows to measure against");
    elapsed
}

#[test]
#[ignore = "timing-sensitive; requires an interactive Windows desktop session"]
fn virtual_desktop_filtering_does_not_dominate_enumeration() {
    // Warm the desktop-manager cache and any lazy COM initialization so the
    // measurement below is of steady-state cost, not first-call setup.
    let _ = window::list_current_windows();

    let raw_ms = raw_enumeration_ms();

    let start = Instant::now();
    let current = window::list_current_windows();
    let current_ms = start.elapsed().as_secs_f64() * 1000.0;

    let start = Instant::now();
    let snapshot = window::list_snapshot_windows();
    let snapshot_ms = start.elapsed().as_secs_f64() * 1000.0;

    eprintln!(
        "raw={raw_ms:.2}ms n={} | current={current_ms:.2}ms n={} | snapshot={snapshot_ms:.2}ms n={}",
        window::list_windows().len(),
        current.len(),
        snapshot.len()
    );

    // Before the fix this ratio was ~33x (0.45ms -> 15ms). The desktop filter
    // is one COM call plus a cheap per-window query, so a handful of times the
    // raw walk is the honest ceiling; the old per-window CoCreateInstance
    // cannot fit under it.
    let budget_ms = (raw_ms * 8.0).max(4.0);
    assert!(
        current_ms < budget_ms,
        "list_current_windows took {current_ms:.2}ms against a {budget_ms:.2}ms budget \
         (raw walk {raw_ms:.2}ms) — the per-window COM object is probably back"
    );
    assert!(
        snapshot_ms < budget_ms,
        "list_snapshot_windows took {snapshot_ms:.2}ms against a {budget_ms:.2}ms budget \
         (raw walk {raw_ms:.2}ms)"
    );
}

/// Snapshot derives its window table from the same walk it scans, so the two
/// lists must stay consistent: every titled, non-overlay window the table
/// shows has to be a window the UIA scan actually visited.
#[test]
#[ignore = "requires an interactive Windows desktop session"]
fn the_table_list_is_a_subset_of_the_scan_list() {
    let table = window::list_current_windows();
    let scan = window::list_snapshot_windows();

    for window in &table {
        assert!(
            scan.iter().any(|candidate| candidate.handle == window.handle),
            "window {:?} ({}) is in the table but was never scanned",
            window.title,
            window.handle
        );
    }
}
