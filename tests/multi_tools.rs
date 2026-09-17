//! `MultiEdit` and `MultiSelect`: the batch forms of Type and Click.
//!
//! Both exist to save round trips, which only pays off if the batch actually
//! reaches every target — a tool that fills the first field and reports
//! success for all of them is worse than one that fails.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{LIST_ITEMS, TestApp, desktop_lock, layout};

use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::multi_edit::{MultiEditParams, multi_edit};
use windows_operation_cli::tools::multi_select::{MultiSelectParams, multi_select};

const SETTLE: Duration = Duration::from_secs(5);

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Every field in the batch has to receive its own text.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_edit_fills_every_field() {
    let _desktop = desktop_lock();
    let Some(app) = app("MultiEdit Fields") else {
        return;
    };
    let (x1, y1) = app.center_of(layout::EDIT);
    let (x2, y2) = app.center_of(layout::EDIT2);

    multi_edit(MultiEditParams {
        locs: Some(ListOrString::List(vec![
            (x1, y1, "first field".to_string()),
            (x2, y2, "second field".to_string()),
        ])),
        labels: None,
    })
    .expect("multi_edit failed");

    assert!(
        app.wait_until(SETTLE, || {
            app.edit_text() == "first field" && app.edit2_text() == "second field"
        }),
        "the fields hold {:?} and {:?}",
        app.edit_text(),
        app.edit2_text()
    );
}

/// Non-ASCII text has to survive the batch as it does a single `Type`.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_edit_carries_non_ascii_text() {
    let _desktop = desktop_lock();
    let Some(app) = app("MultiEdit Unicode") else {
        return;
    };
    let (x1, y1) = app.center_of(layout::EDIT);
    let (x2, y2) = app.center_of(layout::EDIT2);

    multi_edit(MultiEditParams {
        locs: Some(ListOrString::List(vec![
            (x1, y1, "日本語のテキスト".to_string()),
            (x2, y2, "ünïcödé".to_string()),
        ])),
        labels: None,
    })
    .expect("multi_edit failed");

    assert!(
        app.wait_until(SETTLE, || {
            app.edit_text() == "日本語のテキスト" && app.edit2_text() == "ünïcödé"
        }),
        "the fields hold {:?} and {:?}",
        app.edit_text(),
        app.edit2_text()
    );
}

/// `MultiEdit` types with `clear=true`, so a second pass replaces rather than
/// appends — otherwise re-filling a form would double its contents.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_edit_replaces_existing_text() {
    let _desktop = desktop_lock();
    let Some(app) = app("MultiEdit Replace") else {
        return;
    };
    let (x, y) = app.center_of(layout::EDIT);

    let fill = |text: &str| {
        multi_edit(MultiEditParams {
            locs: Some(ListOrString::List(vec![(x, y, text.to_string())])),
            labels: None,
        })
        .expect("multi_edit failed");
    };

    fill("original");
    assert!(app.wait_until(SETTLE, || app.edit_text() == "original"));

    fill("replacement");
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "replacement"),
        "the field holds {:?}; the second pass should have replaced the first",
        app.edit_text()
    );
}

/// Neither `locs` nor `labels` is a caller mistake, not an empty success.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_edit_without_targets_is_rejected() {
    let error = multi_edit(MultiEditParams {
        locs: None,
        labels: None,
    })
    .expect_err("multi_edit with no targets should fail");
    assert!(error.contains("locs or labels"), "unexpected error: {error}");
}

/// `MultiSelect` clicks each point in turn; with Ctrl held the listbox keeps
/// the last one selected, which is what proves every click landed on a row
/// rather than all on one.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_select_clicks_each_point() {
    let _desktop = desktop_lock();
    let Some(app) = app("MultiSelect Rows") else {
        return;
    };

    // Rows are stacked inside the listbox; aim at the first two.
    let (left, top, width, _height) = layout::LISTBOX;
    let row_height = 16;
    let first = app.client_to_screen(left + width / 2, top + row_height / 2);
    let second = app.client_to_screen(left + width / 2, top + row_height + row_height / 2);

    app.drain_events();
    multi_select(MultiSelectParams {
        locs: Some(ListOrString::List(vec![
            vec![first.0, first.1],
            vec![second.0, second.1],
        ])),
        labels: None,
        press_ctrl: None,
    })
    .expect("multi_select failed");

    // A selection means a click reached the listbox at a row.
    assert!(
        app.wait_until(SETTLE, || app.list_selection() >= 0),
        "no row was selected; the clicks did not reach the listbox"
    );
    assert!(
        (app.list_selection() as usize) < LIST_ITEMS.len(),
        "the selection landed outside the rows: {}",
        app.list_selection()
    );
}

/// Neither `locs` nor `labels` is a caller mistake here too.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_select_without_targets_is_rejected() {
    let error = multi_select(MultiSelectParams {
        locs: None,
        labels: None,
        press_ctrl: None,
    })
    .expect_err("multi_select with no targets should fail");
    assert!(error.contains("locs or labels"), "unexpected error: {error}");
}

/// A malformed coordinate is rejected before any click is sent.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_coordinate_that_is_not_a_pair_is_rejected() {
    let error = multi_select(MultiSelectParams {
        locs: Some(ListOrString::List(vec![vec![10, 20, 30]])),
        labels: None,
        press_ctrl: None,
    })
    .expect_err("a three-element coordinate should fail");
    assert!(
        error.contains("exactly 2 integers"),
        "unexpected error: {error}"
    );
}
