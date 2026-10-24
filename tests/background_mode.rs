//! Background mode (`WINDOWS_MCP_BACKGROUND=1`): tools work through UI
//! Automation and never move the cursor or send keys, so they do not compete
//! with a person using the desktop. The same routes are what keep working
//! while the session is locked.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock};

use windows_operation_cli::input_sim;
use windows_operation_cli::params::{BoolOrString, ListOrString};
use windows_operation_cli::state::{self, ElementNode};
use windows_operation_cli::tools::click::{self, ClickParams};
use windows_operation_cli::tools::multi_edit::{self, MultiEditParams};
use windows_operation_cli::tools::scroll::{self, ScrollParams};
use windows_operation_cli::tools::snapshot::{self, SnapshotParams};
use windows_operation_cli::tools::typing::{self, CaretPosition, TypeParams};

const SETTLE: Duration = Duration::from_secs(5);

/// Turns background mode on for the life of the guard.
struct BackgroundMode;

impl BackgroundMode {
    fn on() -> Self {
        unsafe { std::env::set_var("WINDOWS_MCP_BACKGROUND", "1") };
        Self
    }
}

impl Drop for BackgroundMode {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("WINDOWS_MCP_BACKGROUND") };
    }
}

fn edit_nodes(app: &TestApp, title: &str) -> Vec<ElementNode> {
    snapshot::snapshot(&SnapshotParams {
        window: Some(title.to_string()),
        ..SnapshotParams::default()
    })
    .expect("Snapshot failed");
    let state = state::current_state().expect("Snapshot did not publish desktop state");
    state
        .interactive_nodes
        .into_iter()
        .filter(|node| node.owner_handle == app.hwnd() && node.control_type == "edit")
        .collect()
}

fn type_params(label: u64, text: &str, clear: bool, caret: CaretPosition) -> TypeParams {
    TypeParams {
        text: text.to_string(),
        loc: None,
        label: Some(label as i64),
        clear: Some(BoolOrString::Bool(clear)),
        caret_position: Some(caret),
        press_enter: None,
    }
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn typing_sets_the_field_without_moving_the_cursor() {
    let _desktop = desktop_lock();
    let title = "Background Type";
    let Some(app) = TestApp::launch(title) else {
        return;
    };
    let _mode = BackgroundMode::on();
    let edit = edit_nodes(&app, title)
        .into_iter()
        .find(|node| node.owner_handle == app.hwnd())
        .expect("no edit field found");
    input_sim::set_cursor_pos(5, 5).unwrap();
    let cursor = input_sim::get_cursor_pos();

    typing::type_text(type_params(
        edit.element_id,
        "middle",
        true,
        CaretPosition::Idle,
    ))
    .unwrap();
    typing::type_text(type_params(
        edit.element_id,
        "END",
        false,
        CaretPosition::End,
    ))
    .unwrap();
    typing::type_text(type_params(
        edit.element_id,
        "START",
        false,
        CaretPosition::Start,
    ))
    .unwrap();

    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "STARTmiddleEND"),
        "background typing produced {:?}",
        app.edit_text()
    );
    assert_eq!(
        input_sim::get_cursor_pos(),
        cursor,
        "background typing moved the cursor"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn multi_edit_fills_labelled_fields_and_refuses_coordinates() {
    let _desktop = desktop_lock();
    let title = "Background MultiEdit";
    let Some(app) = TestApp::launch(title) else {
        return;
    };
    let _mode = BackgroundMode::on();
    let edits = edit_nodes(&app, title);
    assert!(
        edits.len() >= 2,
        "expected two edit fields, found {}",
        edits.len()
    );

    multi_edit::multi_edit(MultiEditParams {
        locs: None,
        labels: Some(ListOrString::List(vec![
            (edits[0].element_id as i64, "one".to_string()),
            (edits[1].element_id as i64, "two".to_string()),
        ])),
    })
    .unwrap();
    assert!(app.wait_until(SETTLE, || {
        let mut texts = [app.edit_text(), app.edit2_text()];
        texts.sort();
        texts == ["one".to_string(), "two".to_string()]
    }));

    let error = multi_edit::multi_edit(MultiEditParams {
        locs: Some(ListOrString::List(vec![(1, 1, "x".to_string())])),
        labels: None,
    })
    .unwrap_err();
    assert!(error.to_lowercase().contains("background mode"), "{error}");
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn input_only_tools_refuse_instead_of_sending_input() {
    let _desktop = desktop_lock();
    let title = "Background Refusals";
    let Some(app) = TestApp::launch(title) else {
        return;
    };
    let _mode = BackgroundMode::on();
    let edit = edit_nodes(&app, title).remove(0);
    app.drain_events();

    let error = click::click(ClickParams {
        loc: Some(ListOrString::List(vec![10, 10])),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .unwrap_err();
    assert!(error.to_lowercase().contains("background mode"), "{error}");

    let mut enter = type_params(edit.element_id, "x", true, CaretPosition::Idle);
    enter.press_enter = Some(BoolOrString::Bool(true));
    assert!(
        typing::type_text(enter)
            .unwrap_err()
            .contains("press_enter")
    );

    let error = scroll::scroll(ScrollParams {
        loc: Some(ListOrString::List(vec![10, 10])),
        label: None,
        scroll_type: None,
        direction: None,
        wheel_times: Some(1),
        modifier: None,
    })
    .unwrap_err();
    assert!(error.contains("label"), "{error}");
    assert_eq!(
        app.edit_text(),
        "",
        "a refused call still changed the field"
    );
}
