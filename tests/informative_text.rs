//! Reading what a window *says*, not just what it offers to click.
//!
//! A capture that lists every button and none of the window's own text leaves
//! the caller unable to tell what happened — a calculator's result, a dialog's
//! message, a status line are all read-only elements. Informative text used to
//! be collected only when a browser DOM root was found, so every ordinary
//! desktop application reported its controls and none of its readable state.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, STATUS_TEXT, TestApp, desktop_lock};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, capture};
use windows_operation_cli::tools::wait_for::{WaitForParams, wait_for};

fn capture_app(app: &TestApp) -> windows_operation_cli::tools::snapshot::SnapshotResult {
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    capture(&SnapshotParams {
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("capture failed")
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_plain_labels_text_is_captured_without_a_browser() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Informative Text") else {
        return;
    };
    let result = capture_app(&app);

    let informative: Vec<_> = result
        .informative_nodes
        .iter()
        .filter(|node| node.owner_handle == app.hwnd())
        .collect();
    assert!(
        informative.iter().any(|node| node.name == STATUS_TEXT),
        "the status label was not captured; informative nodes were {:?}",
        informative.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
}

/// The label must not be offered as something to click: it has no action, and
/// listing it among the interactive controls would invite the caller to try.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_label_is_not_listed_as_an_interactive_control() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Label Not Clickable") else {
        return;
    };
    let result = capture_app(&app);

    assert!(
        result
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == app.hwnd() && node.name == BUTTON_TEXT),
        "the button should still be interactive"
    );
    assert!(
        !result
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == app.hwnd() && node.name == STATUS_TEXT),
        "a read-only label must not be listed as interactive"
    );
}

/// The rendered capture has to show the text too — `informative_nodes` feeding
/// the response is what makes it readable to the caller.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_rendered_ui_tree_includes_the_label() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Tree Shows Text") else {
        return;
    };
    let result = capture_app(&app);
    assert!(
        result.text.contains(STATUS_TEXT),
        "the UI Tree did not render the status label"
    );
}

/// A whole-desktop sweep answers "which window", and pulling every window's
/// text into it nearly doubled the scan (2.6s -> 4.5s measured). The text is
/// picked up by the foreground capture that follows.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_whole_desktop_sweep_skips_informative_text() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Sweep Skips Text") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let swept = capture(&SnapshotParams {
        scope: Some(windows_operation_cli::tools::snapshot::SnapshotScope::All),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(30_000),
        ..Default::default()
    })
    .expect("sweep failed");

    assert!(
        swept.informative_nodes.is_empty(),
        "scope=all collected {} informative nodes; it should collect none",
        swept.informative_nodes.len()
    );
    // The sweep still has to find the controls it exists to find.
    assert!(
        swept
            .interactive_nodes
            .iter()
            .any(|node| node.owner_handle == app.hwnd()),
        "the sweep found no controls in the test window"
    );
}

/// A whole-desktop sweep must finish within its own default budget.
///
/// `scope=all` walks every window, so the foreground default truncated it
/// almost every time: measured at ~1.8s of work against a 2s budget, the
/// caller got a partial tree plus a "truncated" note for a scan that had
/// essentially completed.
#[test]
#[ignore = "timing-sensitive; requires an interactive Windows desktop session"]
fn a_whole_desktop_sweep_completes_within_its_default_budget() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Sweep Budget") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let result = capture(&SnapshotParams {
        scope: Some(windows_operation_cli::tools::snapshot::SnapshotScope::All),
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("sweep failed");

    assert!(
        !result.text.contains("truncated"),
        "the sweep hit its default timeout; the default is too tight for the work"
    );
}

/// `WaitFor text_exists` searches informative nodes, so gating them on a
/// browser DOM also made this condition blind to ordinary applications.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn wait_for_text_finds_a_plain_label() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("WaitFor Text") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let message = wait_for(WaitForParams {
        condition: "text_exists".to_string(),
        text: Some(STATUS_TEXT.to_string()),
        window_name: None,
        timeout: Some(5.0),
        interval: Some(0.25),
        use_dom: None,
    })
    .expect("WaitFor did not find the label text");
    assert!(message.contains("satisfied"), "unexpected message: {message}");
}
