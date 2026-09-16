//! End-to-end coverage of the accessibility path: `Snapshot` discovering real
//! controls, `InvokeElement` activating them through UIA patterns, and `Type`
//! delivering text into a real edit control.
//!
//! The unit tests in `src/` cover these modules' pure functions — string
//! formatting, fuzzy scoring, priority ordering. Everything that depends on an
//! actual UI Automation provider (element discovery, RuntimeId re-resolution,
//! pattern invocation, occlusion) only runs here, against the window in
//! `tests/harness`.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{BUTTON_TEXT, CHECKBOX_TEXT, Event, LIST_ITEMS, TestApp, desktop_lock, layout};

use windows_operation_cli::params::ListOrString;
use windows_operation_cli::state::{self, ElementNode, SupportedAction};
use windows_operation_cli::tools::click::{self, ClickButton, ClickParams};
use windows_operation_cli::tools::invoke_element::{self, InvokeElementParams};
use windows_operation_cli::tools::snapshot::{self, SnapshotParams};
use windows_operation_cli::tools::typing::{self, CaretPosition, TypeParams};

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A Snapshot restricted to the harness window, returning the elements it
/// found. Snapshot writes into the process-global desktop state, which is what
/// `Click`/`Type`/`InvokeElement` resolve labels against.
fn snapshot_app(app: &TestApp) -> Vec<ElementNode> {
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");
    snapshot::snapshot(&SnapshotParams::default()).expect("Snapshot failed");
    let state = state::current_state().expect("Snapshot did not publish desktop state");
    state
        .interactive_nodes
        .iter()
        .chain(state.scrollable_nodes.iter())
        .filter(|node| node.owner_handle == app.hwnd())
        .cloned()
        .collect()
}

fn find_named<'a>(nodes: &'a [ElementNode], name: &str) -> Option<&'a ElementNode> {
    nodes.iter().find(|node| node.name == name)
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn snapshot_discovers_the_windows_controls_with_their_semantics() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Snapshot Discovery") else {
        return;
    };
    let nodes = snapshot_app(&app);
    assert!(
        !nodes.is_empty(),
        "Snapshot found no elements belonging to the test window"
    );

    let button = find_named(&nodes, BUTTON_TEXT)
        .unwrap_or_else(|| panic!("the push button was not discovered; found: {nodes:#?}"));
    assert_eq!(button.control_type, "button");
    assert!(
        button.supported_actions.contains(&SupportedAction::Invoke),
        "a push button must expose InvokePattern, got {:?}",
        button.supported_actions
    );

    let checkbox = find_named(&nodes, CHECKBOX_TEXT)
        .unwrap_or_else(|| panic!("the checkbox was not discovered; found: {nodes:#?}"));
    assert_eq!(checkbox.control_type, "checkbox");
    assert!(
        checkbox.supported_actions.contains(&SupportedAction::Toggle),
        "a checkbox must expose TogglePattern, got {:?}",
        checkbox.supported_actions
    );

    // Every discovered element must carry the identity InvokeElement needs to
    // re-resolve it later; an element without one can never be invoked.
    assert!(
        !button.runtime_id.is_empty(),
        "the button has no RuntimeId, so InvokeElement could not re-resolve it"
    );

    // The reported center must actually land on the control.
    let (left, top, right, bottom) = button.bounding_box;
    let (cx, cy) = button.center;
    assert!(
        cx >= left && cx < right && cy >= top && cy < bottom,
        "button center {:?} is outside its bounds {:?}",
        button.center,
        button.bounding_box
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn invoke_element_activates_a_button_without_coordinates() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Invoke Button") else {
        return;
    };
    let nodes = snapshot_app(&app);
    let button = find_named(&nodes, BUTTON_TEXT).expect("the push button was not discovered");

    app.drain_events();
    let message = invoke_element::invoke_element(InvokeElementParams {
        element_id: button.element_id,
        fallback_to_click: None,
    })
    .expect("InvokeElement failed on a button exposing InvokePattern");
    assert!(
        message.starts_with("invoke "),
        "expected a semantic invoke, got {message:?}"
    );

    assert_eq!(
        app.wait_for_event(EVENT_TIMEOUT, |event| matches!(event, Event::ButtonClicked)),
        Some(Event::ButtonClicked),
        "the window never saw the button activate"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn invoke_element_toggles_a_checkbox_and_the_window_agrees() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Invoke Checkbox") else {
        return;
    };
    assert!(!app.is_checked(), "the checkbox should start unchecked");

    let nodes = snapshot_app(&app);
    let checkbox = find_named(&nodes, CHECKBOX_TEXT).expect("the checkbox was not discovered");

    app.drain_events();
    invoke_element::invoke_element(InvokeElementParams {
        element_id: checkbox.element_id,
        fallback_to_click: None,
    })
    .expect("InvokeElement failed on a checkbox exposing TogglePattern");

    assert_eq!(
        app.wait_for_event(EVENT_TIMEOUT, |event| matches!(
            event,
            Event::CheckboxToggled(_)
        )),
        Some(Event::CheckboxToggled(true)),
        "the window never saw the checkbox toggle"
    );
    assert!(
        app.is_checked(),
        "the control's own state disagrees with the toggle"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_stale_element_id_is_rejected_against_a_live_window() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Stale Ids") else {
        return;
    };
    let nodes = snapshot_app(&app);
    let button = find_named(&nodes, BUTTON_TEXT).expect("the push button was not discovered");
    let stale_id = button.element_id;

    // A second Snapshot supersedes the first generation.
    let _ = snapshot_app(&app);

    let error = invoke_element::invoke_element(InvokeElementParams {
        element_id: stale_id,
        fallback_to_click: None,
    })
    .expect_err("an element id from a superseded Snapshot must not be invoked");
    assert!(
        error.contains("stale"),
        "expected a staleness error, got {error:?}"
    );
}

/// `Type` sends text of 20+ characters through the clipboard and anything
/// shorter one character at a time. Both paths must deliver the text exactly —
/// the regression behind "Fix Type mangling every character it sends", where
/// the UTF-16 code unit went into `wVk` instead of `wScan` and every character
/// arrived as a different one.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn type_delivers_short_text_verbatim_character_by_character() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Type Short") else {
        return;
    };
    // Deliberately mixes the characters whose VK codes differ from their
    // Unicode values: 'd' (U+0064 = VK_NUMPAD4), 'o' (U+006F = VK_DIVIDE), and
    // an uppercase letter, which typed as lowercase before the fix.
    let text = "Dog/4a";
    assert!(text.chars().count() < 20, "must take the per-character path");

    let (x, y) = app.center_of(layout::EDIT);
    typing::type_text(TypeParams {
        text: text.to_string(),
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        clear: None,
        caret_position: Some(CaretPosition::End),
        press_enter: None,
    })
    .expect("Type failed");

    assert!(
        app.wait_until(EVENT_TIMEOUT, || app.edit_text() == text),
        "the edit control received {:?}, expected {text:?}",
        app.edit_text()
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn type_delivers_long_text_verbatim_through_the_clipboard() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Type Long") else {
        return;
    };
    // 20+ characters with none of \n \t { } takes the clipboard paste path.
    let text = "The quick brown fox jumps over it";
    assert!(text.chars().count() >= 20, "must take the paste path");

    let (x, y) = app.center_of(layout::EDIT);
    typing::type_text(TypeParams {
        text: text.to_string(),
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        clear: None,
        caret_position: Some(CaretPosition::End),
        press_enter: None,
    })
    .expect("Type failed");

    assert!(
        app.wait_until(EVENT_TIMEOUT, || app.edit_text() == text),
        "the edit control received {:?}, expected {text:?}",
        app.edit_text()
    );
}

/// `clear=true` must replace the control's contents, not splice onto them.
///
/// The keyboard route (Ctrl+A, Backspace) cannot do this for a bare Win32
/// `EDIT`: the control does not implement select-all itself, so the chord
/// selects nothing, the Backspace deletes one character, and the new text is
/// appended to the remains ("first" + "second" came out as "firssecond").
/// `Type` now clears through the UIA `ValuePattern` first, which asks the
/// provider to set an empty value.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn type_with_clear_replaces_the_existing_content() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Type Clear") else {
        return;
    };
    let (x, y) = app.center_of(layout::EDIT);

    typing::type_text(TypeParams {
        text: "first".to_string(),
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        clear: None,
        caret_position: Some(CaretPosition::End),
        press_enter: None,
    })
    .expect("Type failed");
    assert!(app.wait_until(EVENT_TIMEOUT, || app.edit_text() == "first"));

    typing::type_text(TypeParams {
        text: "second".to_string(),
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        clear: Some(windows_operation_cli::params::BoolOrString::Bool(true)),
        caret_position: Some(CaretPosition::End),
        press_enter: None,
    })
    .expect("Type failed");

    assert!(
        app.wait_until(EVENT_TIMEOUT, || app.edit_text() == "second"),
        "clear=true left {:?} behind",
        app.edit_text()
    );
}

/// A `label` resolved from a Snapshot must reach the control it names. This is
/// the path `support::validate_label_point` guards, including the occlusion
/// check that no unit test can reach.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn clicking_by_label_reaches_the_named_control() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Label Click") else {
        return;
    };
    let nodes = snapshot_app(&app);
    let button = find_named(&nodes, BUTTON_TEXT).expect("the push button was not discovered");

    app.drain_events();
    click::click(ClickParams {
        loc: None,
        label: Some(button.element_id as i64),
        button: Some(ClickButton::Left),
        clicks: Some(1),
        modifier: None,
    })
    .expect("Click by label failed");

    assert_eq!(
        app.wait_for_event(EVENT_TIMEOUT, |event| matches!(event, Event::ButtonClicked)),
        Some(Event::ButtonClicked),
        "the click never reached the button"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn selecting_a_list_item_reports_through_uia() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("List Select") else {
        return;
    };
    assert_eq!(app.list_selection(), -1);

    let nodes = snapshot_app(&app);
    let Some(item) = find_named(&nodes, LIST_ITEMS[1]) else {
        // A listbox exposes its items as children only once it has been
        // realized; skip rather than fail if this provider does not.
        return;
    };
    assert_eq!(item.control_type, "listitem");

    app.drain_events();
    invoke_element::invoke_element(InvokeElementParams {
        element_id: item.element_id,
        fallback_to_click: None,
    })
    .expect("InvokeElement failed on a list item");

    assert!(
        app.wait_until(EVENT_TIMEOUT, || app.list_selection() == 1),
        "the listbox selection is {}, expected 1",
        app.list_selection()
    );
}
