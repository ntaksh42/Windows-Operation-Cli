//! The `Shortcut` tool: does the chord reach a window as the keys it names?
//!
//! The unit tests cover key-name resolution. What they cannot cover is
//! whether the chord is actually delivered — a tool that resolves `ctrl+c`
//! correctly and then sends nothing would pass all of them.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock, layout};

use windows_operation_cli::input_sim;
use windows_operation_cli::tools::shortcut::{ShortcutParams, shortcut};
use windows_operation_cli::tools::typing::{CaretPosition, type_at};

const SETTLE: Duration = Duration::from_secs(5);

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

fn press(chord: &str) -> Result<String, String> {
    shortcut(ShortcutParams {
        shortcut: chord.to_string(),
    })
}

/// Restores whatever the clipboard held before the test.
struct Preserved(Option<String>);

impl Preserved {
    fn capture() -> Self {
        Self(input_sim::get_clipboard_text())
    }
}

impl Drop for Preserved {
    fn drop(&mut self) {
        match self.0.take() {
            Some(text) => {
                input_sim::set_clipboard_text(&text);
            }
            None => input_sim::clear_clipboard(),
        }
    }
}

/// A chord has to reach the focused control as the keys it names.
///
/// The text is selected with shift+home rather than `ctrl+a`: a bare Win32
/// `EDIT` does not implement select-all itself — in a dialog the dialog
/// manager translates that chord — so `ctrl+a` would be testing comctl, not
/// this tool. `shift+home` is handled by the control.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_chord_reaches_the_focused_field() {
    let _desktop = desktop_lock();
    if !input_sim::set_clipboard_text("clipboard availability probe") {
        eprintln!("skipped: the clipboard is held by another process");
        return;
    }
    let _preserved = Preserved::capture();
    let Some(app) = app("Shortcut Copy") else {
        return;
    };

    let (x, y) = app.center_of(layout::EDIT);
    let text = "copied through a chord";
    type_at(x, y, text, CaretPosition::Idle, true, false).expect("typing failed");
    assert!(app.wait_until(SETTLE, || app.edit_text() == text));

    input_sim::clear_clipboard();
    press("end").expect("end failed");
    press("shift+home").expect("shift+home failed");
    press("ctrl+c").expect("ctrl+c failed");

    let deadline = std::time::Instant::now() + SETTLE;
    let mut clipboard = None;
    while std::time::Instant::now() < deadline {
        if let Some(value) = input_sim::get_clipboard_text()
            && value == text
        {
            clipboard = Some(value);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        clipboard.as_deref(),
        Some(text),
        "the clipboard holds {:?}; the chords did not reach the field",
        input_sim::get_clipboard_text()
    );
}

/// A chord that edits has to change the control, not merely be accepted.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_chord_that_edits_changes_the_field() {
    let _desktop = desktop_lock();
    let Some(app) = app("Shortcut Delete") else {
        return;
    };

    let (x, y) = app.center_of(layout::EDIT);
    type_at(x, y, "text to remove", CaretPosition::Idle, true, false).expect("typing failed");
    assert!(app.wait_until(SETTLE, || app.edit_text() == "text to remove"));

    press("end").expect("end failed");
    press("shift+home").expect("shift+home failed");
    press("delete").expect("delete failed");

    assert!(
        app.wait_until(SETTLE, || app.edit_text().is_empty()),
        "the field still holds {:?}",
        app.edit_text()
    );
}

/// The `plus` alias exists because `+` is the separator; it has to resolve to
/// a key the tool can actually send.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_plus_alias_is_accepted_and_sent() {
    let _desktop = desktop_lock();
    let Some(_app) = app("Shortcut Plus") else {
        return;
    };
    let response = press("ctrl+plus").expect("ctrl+plus should be a valid chord");
    assert!(
        response.contains("ctrl+plus"),
        "unexpected response: {response}"
    );
}

/// An unknown key name is a caller mistake, reported before anything is sent.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_unknown_key_is_rejected() {
    let error = press("ctrl+notakey").expect_err("an unknown key should be rejected");
    assert!(
        error.contains("Unknown shortcut key"),
        "unexpected error: {error}"
    );
}

/// A missing key between separators is rejected rather than sent as a
/// partial chord.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_missing_key_between_separators_is_rejected() {
    for chord in ["ctrl+", "+c", ""] {
        let error = press(chord).expect_err("a malformed chord should be rejected");
        assert!(
            error.contains("must not be empty"),
            "{chord:?} produced: {error}"
        );
    }
}
