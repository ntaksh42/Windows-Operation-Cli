//! A window whose provider is slower than the scan budget.
//!
//! The deadline is checked before each window rather than during one, so a
//! single slow provider carries the capture past `timeout_ms` and still
//! returns a complete tree. Interrupting the walk would hand back a
//! half-built tree that looks whole, so the scan finishes — but the response
//! has to say that it overran, or the caller has no way to tell that the
//! budget they set meant nothing.

#![cfg(target_os = "windows")]

use std::process::Command;
use std::time::{Duration, Instant};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, capture};
use windows_operation_cli::window;

/// `msinfo32` is the heaviest dialog that ships with Windows: a tree plus a
/// long detail list, measured at roughly five seconds to walk.
struct SystemInfo {
    pid: u32,
    title: String,
}

impl SystemInfo {
    fn open() -> Option<Self> {
        let _ = Command::new("msinfo32.exe").spawn().ok()?;
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(found) = window::list_current_windows()
                .into_iter()
                .find(|candidate| candidate.title.contains("システム情報"))
            {
                window::switch_to(found.handle);
                // Its detail list fills in after the window appears; a capture
                // taken before that is fast for the wrong reason.
                std::thread::sleep(Duration::from_millis(4000));
                return Some(Self {
                    pid: found.pid,
                    title: found.title,
                });
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

impl Drop for SystemInfo {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/PID", &self.pid.to_string(), "/F", "/T"])
            .output();
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_overrun_is_reported_rather_than_passed_off_as_within_budget() {
    let Some(app) = SystemInfo::open() else {
        eprintln!("skipped: System Information did not open");
        return;
    };

    // A budget far below what this window costs.
    let start = Instant::now();
    let result = capture(&SnapshotParams {
        window: Some(app.title.clone()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(100),
        ..Default::default()
    })
    .expect("capture failed");
    let elapsed = start.elapsed();

    if elapsed <= Duration::from_millis(100) {
        eprintln!("skipped: the provider answered inside the budget ({elapsed:?})");
        return;
    }
    eprintln!(
        "took {:.0}ms against a 100ms budget, {} controls",
        elapsed.as_secs_f64() * 1000.0,
        result.interactive_nodes.len()
    );

    assert!(
        result.text.contains("longer than timeout_ms"),
        "the capture overran its budget without saying so:\n{}",
        result.text
    );
    // The tree still has to be complete: reporting the overrun is the point,
    // truncating is not.
    assert!(
        !result.interactive_nodes.is_empty(),
        "the overrun left the tree empty"
    );
}

/// A capture that finishes inside its budget must not carry the notice.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_capture_within_budget_says_nothing_about_overrunning() {
    let Some(app) = SystemInfo::open() else {
        eprintln!("skipped: System Information did not open");
        return;
    };

    let result = capture(&SnapshotParams {
        window: Some(app.title.clone()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(30_000),
        ..Default::default()
    })
    .expect("capture failed");

    assert!(
        !result.text.contains("longer than timeout_ms"),
        "a capture inside its budget claimed to have overrun:\n{}",
        result.text
    );
}
