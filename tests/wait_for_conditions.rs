//! `WaitFor`'s conditions, against a window that actually changes.
//!
//! The unit tests cover parameter validation and alias resolution; what they
//! cannot cover is whether each condition sees the live desktop correctly.
//! A condition that never fires turns every call into a timeout, and one that
//! fires too eagerly returns before the UI is ready — both are silent.

#![cfg(target_os = "windows")]

mod harness;

use std::time::{Duration, Instant};

use harness::{BUTTON_TEXT, STATUS_TEXT, TestApp, desktop_lock};

use windows_operation_cli::tools::wait_for::{WaitForParams, wait_for};

fn params(condition: &str, text: Option<&str>) -> WaitForParams {
    WaitForParams {
        condition: condition.to_string(),
        text: text.map(str::to_string),
        window_name: None,
        timeout: Some(6.0),
        interval: Some(0.2),
        use_dom: None,
    }
}

/// A condition that is already true must return promptly, not sit out its
/// whole timeout.
fn assert_satisfied_quickly(result: Result<String, String>, what: &str, elapsed: Duration) {
    let message = result.unwrap_or_else(|error| panic!("{what} was not satisfied: {error}"));
    assert!(
        message.contains("satisfied"),
        "{what}: unexpected message {message}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "{what} took {elapsed:?}; a condition that already holds should return at once"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn element_exists_finds_a_control_by_name() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("WaitFor Element") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let start = Instant::now();
    let result = wait_for(params("element_exists", Some(BUTTON_TEXT)));
    assert_satisfied_quickly(result, "element_exists", start.elapsed());
}

/// `element_enabled` only matches controls that are actually actionable.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn element_enabled_matches_an_enabled_control() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("WaitFor Enabled") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let start = Instant::now();
    let result = wait_for(params("element_enabled", Some(BUTTON_TEXT)));
    assert_satisfied_quickly(result, "element_enabled", start.elapsed());
}

/// `text_exists` searches the window's readable text, which is what the
/// informative-node fix made available outside browsers.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn text_exists_finds_a_label() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("WaitFor Label") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let start = Instant::now();
    let result = wait_for(params("text_exists", Some(STATUS_TEXT)));
    assert_satisfied_quickly(result, "text_exists", start.elapsed());
}

/// `active_window` takes the short path — foreground title only, no UIA walk.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn active_window_matches_the_foreground_title() {
    let _desktop = desktop_lock();
    let title = "WaitFor Foreground";
    let Some(app) = TestApp::launch(title) else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let start = Instant::now();
    let result = wait_for(WaitForParams {
        condition: "active_window".to_string(),
        text: None,
        window_name: Some(title.to_string()),
        timeout: Some(6.0),
        interval: Some(0.2),
        use_dom: None,
    });
    assert_satisfied_quickly(result, "active_window", start.elapsed());
}

/// A condition that never becomes true has to time out and say what it was
/// looking for, rather than returning as if it had matched.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_unmet_condition_times_out_with_an_explanation() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("WaitFor Timeout") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let needle = "ThisControlDoesNotExistAnywhere";
    let start = Instant::now();
    let error = wait_for(WaitForParams {
        condition: "element_exists".to_string(),
        text: Some(needle.to_string()),
        window_name: None,
        timeout: Some(1.5),
        interval: Some(0.2),
        use_dom: None,
    })
    .expect_err("a condition that cannot hold must not report success");

    assert!(
        error.contains(needle),
        "the timeout message should name what was awaited: {error}"
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1_400),
        "it gave up after {elapsed:?}, before its own timeout"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "it overran its 1.5s timeout by too much: {elapsed:?}"
    );
}
