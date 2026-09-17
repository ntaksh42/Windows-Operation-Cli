//! `Type`'s caret placement, Enter, and the inputs at the edges of what it
//! accepts.
//!
//! The existing tests cover what arrives for ordinary text. These cover where
//! it arrives — appending against inserting at the start — and the inputs a
//! caller will eventually send that nothing had exercised: an empty string,
//! text long enough to matter, and one that is only whitespace.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock, layout};

use windows_operation_cli::tools::typing::{CaretPosition, type_at};

const SETTLE: Duration = Duration::from_secs(8);

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Types `text` into the edit field, optionally clearing first.
fn type_into(app: &TestApp, text: &str, caret: CaretPosition, clear: bool) {
    let (x, y) = app.center_of(layout::EDIT);
    type_at(x, y, text, caret, clear, false).expect("typing failed");
}

/// `caret_position: end` appends; `start` inserts before what is there.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_caret_position_decides_where_text_lands() {
    let _desktop = desktop_lock();
    let Some(app) = app("Caret Placement") else {
        return;
    };

    type_into(&app, "middle", CaretPosition::Idle, true);
    assert!(app.wait_until(SETTLE, || app.edit_text() == "middle"));

    type_into(&app, "END", CaretPosition::End, false);
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "middleEND"),
        "end-caret typing produced {:?}",
        app.edit_text()
    );

    type_into(&app, "START", CaretPosition::Start, false);
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "STARTmiddleEND"),
        "start-caret typing produced {:?}",
        app.edit_text()
    );
}

/// `press_enter` has to submit after the text, not instead of it.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn press_enter_follows_the_text() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Enter") else {
        return;
    };
    let (x, y) = app.center_of(layout::EDIT);

    // A single-line EDIT swallows the Enter rather than adding a newline, so
    // what this checks is that the text still arrived intact around it.
    type_at(x, y, "submitted text", CaretPosition::Idle, true, true).expect("typing failed");
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "submitted text"),
        "the field holds {:?}",
        app.edit_text()
    );
}

/// An empty string is a no-op, not an error and not a clear.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn typing_nothing_leaves_the_field_alone() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Empty") else {
        return;
    };

    type_into(&app, "existing", CaretPosition::Idle, true);
    assert!(app.wait_until(SETTLE, || app.edit_text() == "existing"));

    // Without `clear`, an empty string has nothing to do.
    type_into(&app, "", CaretPosition::End, false);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        app.edit_text(),
        "existing",
        "typing an empty string changed the field"
    );
}

/// `clear` with an empty string is how a caller empties a field.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn clearing_with_no_text_empties_the_field() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Clear") else {
        return;
    };

    type_into(&app, "to be removed", CaretPosition::Idle, true);
    assert!(app.wait_until(SETTLE, || app.edit_text() == "to be removed"));

    type_into(&app, "", CaretPosition::Idle, true);
    assert!(
        app.wait_until(SETTLE, || app.edit_text().is_empty()),
        "the field still holds {:?}",
        app.edit_text()
    );
}

/// Text long enough to take the clipboard path has to arrive whole.
///
/// The harness's single-line `EDIT` is not the place to check this: measured
/// against it, a 608-character paste stops at 29 whether it goes through this
/// tool or a hand-typed Ctrl+V, so the limit is the control's and asserting
/// on it would measure comctl. `tests/real_app_workflow.rs` covers the same
/// text against Notepad, which holds all 608.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn text_over_the_paste_threshold_arrives_whole() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Long") else {
        return;
    };

    // Comfortably over the paste threshold, and still within what the
    // harness's field accepts.
    let text = "START-0123456789-END";
    assert!(text.len() >= 20, "the text must take the clipboard path");

    type_into(&app, text, CaretPosition::Idle, true);
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == text),
        "the field holds {:?}",
        app.edit_text()
    );
}

/// Whitespace is text: a caller indenting or padding a field means it.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn whitespace_is_typed_as_given() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Whitespace") else {
        return;
    };

    let text = "  spaced  out  ";
    type_into(&app, text, CaretPosition::Idle, true);
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == text),
        "the field holds {:?}, not the spacing it was given",
        app.edit_text()
    );
}

/// Characters that mean something to other input APIs must not be treated
/// specially here.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn characters_special_to_sendkeys_are_literal() {
    let _desktop = desktop_lock();
    let Some(app) = app("Type Special") else {
        return;
    };

    // `SendKeys` reads these as syntax; `KEYEVENTF_UNICODE` and `Ctrl+V` do
    // not, and neither should this tool.
    let text = "{braces} +^%~ (parens) [brackets]";
    type_into(&app, text, CaretPosition::Idle, true);
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == text),
        "the field holds {:?}",
        app.edit_text()
    );
}
