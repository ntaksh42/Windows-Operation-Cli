//! A whole sequence of tool calls, in the order a caller would make them.
//!
//! Every defect this suite has turned up so far appeared between calls rather
//! than inside one: text from a previous field still arriving when the next
//! was clicked, an element id resolving against a view that had scrolled, a
//! window left in front by a step that had already finished. The per-tool
//! tests each set up a clean state and act once, which is exactly the shape
//! that hides those. These do not.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, CHECKBOX_TEXT, Event, LIST_ITEMS, TestApp, desktop_lock, layout};

use windows_operation_cli::params::{BoolOrString, ListOrString};
use windows_operation_cli::state::{self, ElementNode};
use windows_operation_cli::tools::click::{ClickParams, click};
use windows_operation_cli::tools::invoke_element::{InvokeElementParams, invoke_element};
use windows_operation_cli::tools::snapshot::{SnapshotParams, snapshot};
use windows_operation_cli::tools::typing::{TypeParams, type_text};
use windows_operation_cli::tools::wait_for::{WaitForParams, wait_for};

const SETTLE: Duration = Duration::from_secs(5);

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Captures the window and returns its elements, as a caller would before
/// deciding what to act on.
fn look(app: &TestApp) -> Vec<ElementNode> {
    snapshot(&SnapshotParams {
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("Snapshot failed");
    let state = state::current_state().expect("Snapshot published no state");
    state
        .interactive_nodes
        .iter()
        .chain(state.scrollable_nodes.iter())
        .filter(|node| node.owner_handle == app.hwnd())
        .cloned()
        .collect()
}

fn find<'a>(nodes: &'a [ElementNode], name: &str) -> &'a ElementNode {
    nodes
        .iter()
        .find(|node| node.name == name)
        .unwrap_or_else(|| panic!("{name:?} was not in the capture: {nodes:#?}"))
}

/// Look, type, confirm, act — the loop a caller actually runs.
///
/// Each step uses the ids from the capture before it, so a step that leaves
/// the desktop in a state the next one does not expect shows up here.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_capture_then_act_loop_holds_together() {
    let _desktop = desktop_lock();
    let Some(app) = app("Workflow Loop") else {
        return;
    };

    // 1. Look, and type into the field the capture found.
    let nodes = look(&app);
    let edit = nodes
        .iter()
        .find(|node| node.control_type == "edit")
        .expect("no edit control in the capture");
    type_text(TypeParams {
        text: "workflow text".to_string(),
        loc: None,
        label: Some(edit.element_id as i64),
        clear: Some(BoolOrString::Bool(true)),
        caret_position: None,
        press_enter: None,
    })
    .expect("typing failed");
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == "workflow text"),
        "the field holds {:?}",
        app.edit_text()
    );

    // 2. Confirm through WaitFor, which polls its own captures.
    wait_for(WaitForParams {
        condition: "element_exists".to_string(),
        text: Some(BUTTON_TEXT.to_string()),
        window_name: None,
        timeout: Some(5.0),
        interval: Some(0.25),
        use_dom: None,
    })
    .expect("WaitFor did not find the button");

    // 3. Look again and act on the fresh ids.
    let nodes = look(&app);
    app.drain_events();
    invoke_element(InvokeElementParams {
        element_id: find(&nodes, BUTTON_TEXT).element_id,
        fallback_to_click: None,
    })
    .expect("invoke failed");
    assert_eq!(
        app.wait_for_event(SETTLE, |event| matches!(event, Event::ButtonClicked)),
        Some(Event::ButtonClicked),
        "the button was not activated"
    );
}

/// Ids from one capture stay usable across several actions, as long as no
/// newer capture has replaced them.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn several_actions_run_off_one_capture() {
    let _desktop = desktop_lock();
    let Some(app) = app("Workflow One Capture") else {
        return;
    };
    let nodes = look(&app);

    let checkbox = find(&nodes, CHECKBOX_TEXT).element_id;
    let button = find(&nodes, BUTTON_TEXT).element_id;
    let item = find(&nodes, LIST_ITEMS[1]).element_id;

    app.drain_events();
    invoke_element(InvokeElementParams {
        element_id: checkbox,
        fallback_to_click: None,
    })
    .expect("toggling the checkbox failed");
    assert!(
        app.wait_until(SETTLE, || app.is_checked()),
        "the checkbox did not toggle"
    );

    invoke_element(InvokeElementParams {
        element_id: item,
        fallback_to_click: None,
    })
    .expect("selecting the list item failed");
    assert!(
        app.wait_until(SETTLE, || app.list_selection() == 1),
        "the list selection is {}",
        app.list_selection()
    );

    app.drain_events();
    invoke_element(InvokeElementParams {
        element_id: button,
        fallback_to_click: None,
    })
    .expect("pressing the button failed");
    assert_eq!(
        app.wait_for_event(SETTLE, |event| matches!(event, Event::ButtonClicked)),
        Some(Event::ButtonClicked),
        "the button was not activated"
    );
}

/// A new capture retires the previous one's ids, and the old ones have to be
/// refused rather than resolving against whatever now sits at that index.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn ids_from_a_superseded_capture_are_refused() {
    let _desktop = desktop_lock();
    let Some(app) = app("Workflow Stale Ids") else {
        return;
    };

    let first = look(&app);
    let stale = find(&first, BUTTON_TEXT).element_id;

    // A second capture supersedes the first.
    let _second = look(&app);

    let error = invoke_element(InvokeElementParams {
        element_id: stale,
        fallback_to_click: None,
    })
    .expect_err("an id from a superseded capture should be refused");
    assert!(
        error.contains("stale"),
        "the refusal should say the id is stale: {error}"
    );
}

/// Typing into two fields in turn must not let one field's characters arrive
/// while the other has focus — the failure `MultiEdit` exhibited, reached
/// here through plain `Type` calls instead.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn typing_into_two_fields_in_turn_keeps_them_separate() {
    let _desktop = desktop_lock();
    let Some(app) = app("Workflow Two Fields") else {
        return;
    };

    for (rect, text) in [
        (layout::EDIT, "field one text"),
        (layout::EDIT2, "field two text"),
    ] {
        let (x, y) = app.center_of(rect);
        type_text(TypeParams {
            text: text.to_string(),
            loc: Some(ListOrString::List(vec![x, y])),
            label: None,
            clear: Some(BoolOrString::Bool(true)),
            caret_position: None,
            press_enter: None,
        })
        .expect("typing failed");
    }

    assert!(
        app.wait_until(SETTLE, || {
            app.edit_text() == "field one text" && app.edit2_text() == "field two text"
        }),
        "the fields hold {:?} and {:?}",
        app.edit_text(),
        app.edit2_text()
    );
}

/// A coordinate click still works alongside the id-based calls, since a
/// caller mixes the two.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn coordinate_and_id_calls_mix() {
    let _desktop = desktop_lock();
    let Some(app) = app("Workflow Mixed") else {
        return;
    };
    let nodes = look(&app);
    let button = find(&nodes, BUTTON_TEXT).element_id;

    // Coordinate click on the checkbox, id invoke on the button.
    let (x, y) = app.center_of(layout::CHECKBOX);
    app.drain_events();
    click(ClickParams {
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        button: None,
        clicks: Some(1),
        modifier: None,
    })
    .expect("coordinate click failed");
    assert!(
        app.wait_until(SETTLE, || app.is_checked()),
        "the coordinate click did not reach the checkbox"
    );

    app.drain_events();
    invoke_element(InvokeElementParams {
        element_id: button,
        fallback_to_click: None,
    })
    .expect("invoke failed");
    assert_eq!(
        app.wait_for_event(SETTLE, |event| matches!(event, Event::ButtonClicked)),
        Some(Event::ButtonClicked),
        "the id-based invoke did not reach the button"
    );
}
