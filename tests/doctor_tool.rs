//! `Doctor`: the diagnostics have to agree with what the tools can actually
//! do.
//!
//! A diagnostic that reports a capability the tools cannot use is worse than
//! none: it sends the caller looking for the problem somewhere else. This
//! suite cross-checks each claim against the subsystem it describes — which
//! is how the DXGI check was found reporting success while every screenshot
//! came back black.

#![cfg(target_os = "windows")]

use windows_operation_cli::capture::{Backend, capture_rect_with_backend, virtual_screen_rect};
use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::doctor::doctor;
use windows_operation_cli::tools::screenshot::{ScreenshotParams, screenshot};

fn report() -> serde_json::Value {
    let json = doctor();
    serde_json::from_str(&json).unwrap_or_else(|e| panic!("the report was not JSON: {e}\n{json}"))
}

/// The report has to carry every check, with the shape a caller can read.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_report_is_complete_and_well_formed() {
    let value = report();
    let checks = value["checks"]
        .as_object()
        .expect("the report has no checks object");

    for key in [
        "uia_com",
        "monitor_count",
        "dxgi",
        "powershell",
        "administrator",
        "virtual_desktop_api",
    ] {
        assert!(checks.contains_key(key), "the report is missing {key}");
    }
    assert!(
        value["blockers"].is_array(),
        "blockers should be an array: {value}"
    );
}

/// A clean report must have no blockers, and a blocked one must list them —
/// the two have to agree with each other.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn blockers_match_the_failed_checks() {
    let value = report();
    let checks = &value["checks"];
    let blockers = value["blockers"].as_array().expect("blockers is an array");

    let failed = [
        ("uia_com", checks["uia_com"] == false),
        ("dxgi", checks["dxgi"] == false),
        (
            "virtual_desktop_api",
            checks["virtual_desktop_api"] == false,
        ),
        ("powershell", checks["powershell"].is_null()),
    ]
    .into_iter()
    .filter(|(_, failed)| *failed)
    .count();

    assert_eq!(
        blockers.len(),
        failed + usize::from(checks["monitor_count"] == 0),
        "the blocker list does not match the failed checks: {value}"
    );
}

/// `dxgi: true` has to mean a DXGI capture works. This is the claim that was
/// wrong: the check passed on `DuplicateOutput` succeeding while every frame
/// came back empty.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_dxgi_claim_matches_what_a_capture_produces() {
    let value = report();
    if value["checks"]["dxgi"] != true {
        eprintln!("skipped: this session reports DXGI as unavailable");
        return;
    }

    let (image, _) = capture_rect_with_backend(virtual_screen_rect(), Backend::Dxgi)
        .expect("Doctor reported DXGI as available but the capture failed");
    let lit = image
        .pixels()
        .filter(|pixel| pixel.0[0] as u32 + pixel.0[1] as u32 + pixel.0[2] as u32 > 0)
        .count() as f64
        / (image.width() as f64 * image.height() as f64).max(1.0);
    assert!(
        lit > 0.5,
        "Doctor reports dxgi: true but the capture is {:.1}% lit",
        lit * 100.0
    );
}

/// `monitor_count` has to match what a capture can be restricted to.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_monitor_count_matches_the_displays_a_capture_accepts() {
    let value = report();
    let count = value["checks"]["monitor_count"]
        .as_u64()
        .expect("monitor_count should be a number") as usize;
    assert!(count > 0, "no monitors were reported: {value}");

    // Every index the count implies has to be capturable, and the one past it
    // must not be.
    for index in 0..count {
        screenshot(&ScreenshotParams {
            use_annotation: Some(BoolOrString::Bool(false)),
            width_reference_line: None,
            height_reference_line: None,
            display: Some(windows_operation_cli::params::ListOrString::List(vec![
                index as i32,
            ])),
            window: None,
        })
        .unwrap_or_else(|error| {
            panic!("display {index} is within monitor_count but was refused: {error}")
        });
    }

    let past_the_end = screenshot(&ScreenshotParams {
        use_annotation: Some(BoolOrString::Bool(false)),
        width_reference_line: None,
        height_reference_line: None,
        display: Some(windows_operation_cli::params::ListOrString::List(vec![
            count as i32,
        ])),
        window: None,
    });
    assert!(
        past_the_end.is_err(),
        "display {count} is past monitor_count but was accepted"
    );
}

/// `powershell` has to name a shell that actually runs.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_named_shell_can_be_run() {
    let value = report();
    let Some(shell) = value["checks"]["powershell"].as_str() else {
        eprintln!("skipped: no shell was found on this machine");
        return;
    };

    let output = std::process::Command::new(shell)
        .args(["-NoProfile", "-NonInteractive", "-Command", "Write-Output ok"])
        .output()
        .unwrap_or_else(|error| panic!("Doctor named {shell:?} but it could not be run: {error}"));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("ok"),
        "{shell:?} did not produce the expected output"
    );
}
