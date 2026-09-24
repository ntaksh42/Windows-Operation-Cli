use std::time::{Duration, Instant};

use rmcp::schemars;
use serde::Deserialize;

use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::state::{self, ElementNode, SupportedAction};
use crate::tools::click::{self, ClickButton, ClickParams};
use crate::tools::snapshot;
use crate::tools::support;
use crate::{uia, window};

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct InvokeElementParams {
    /// Generation-scoped element id from the most recent Snapshot.
    pub element_id: Option<u64>,
    /// Several element ids from the most recent Snapshot, invoked in order
    /// (after `element_id`, when both are given) — e.g. the buttons 8, ×, 9, =.
    pub element_ids: Option<ListOrString<u64>>,
    /// Click the last validated center when no semantic UIA action exists.
    pub fallback_to_click: Option<BoolOrString>,
    /// Report the text the element's window shows afterwards that it did not
    /// show at the last Snapshot. Defaults to true.
    pub report_text: Option<BoolOrString>,
}

fn choose_action(actions: &[SupportedAction]) -> Option<SupportedAction> {
    SupportedAction::highest_priority(actions)
}

/// Checks the saved center still sits inside the element's own saved bounds.
///
/// This is the part specific to the fallback: the element's geometry has to be
/// self-consistent before its center is worth clicking. Whether the *window*
/// can receive that click — minimized, moved, occluded — is
/// [`support::validate_element_point`]'s job, which the caller runs next.
fn validate_fallback(
    element: &ElementNode,
    owner_bounds: (i32, i32, i32, i32),
) -> Result<(), String> {
    let (left, top, right, bottom) = element.bounding_box;
    let (x, y) = element.center;
    if x < left || x >= right || y < top || y >= bottom {
        return Err("Element center is outside its saved bounds".to_string());
    }
    let (owner_x, owner_y, owner_width, owner_height) = owner_bounds;
    if x < owner_x || x >= owner_x + owner_width || y < owner_y || y >= owner_y + owner_height {
        return Err("Element center is outside the current owner window".to_string());
    }
    Ok(())
}

/// Invokes every requested element in order, then reports what text changed
/// in their windows.
///
/// Taking several ids and reporting the result saves the caller a round trip
/// per button and another for the Snapshot that would only confirm it: a
/// calculator's "8 × 9 =" is one call that answers with "72".
pub fn invoke_element(params: InvokeElementParams) -> Result<String, String> {
    let mut ids: Vec<u64> = params.element_id.into_iter().collect();
    if let Some(more) = params.element_ids {
        ids.extend(more.into_list()?);
    }
    if ids.is_empty() {
        return Err("Provide element_id or element_ids".to_string());
    }
    let fallback_to_click = opt_bool(&params.fallback_to_click, false)?;
    let report_text = opt_bool(&params.report_text, true)?;
    // Read before acting: the last Snapshot's text is what "changed" is
    // measured against.
    let before = state::current_state();

    let mut messages = Vec::with_capacity(ids.len());
    let mut owners: Vec<isize> = Vec::new();
    for id in ids {
        let element = state::resolve_element(id)?;
        match invoke_one(&element, fallback_to_click) {
            Ok(message) => messages.push(message),
            Err(error) if messages.is_empty() => return Err(error),
            Err(error) => {
                return Err(format!(
                    "{}\nStopped at element {id}: {error}",
                    messages.join("\n")
                ));
            }
        }
        let owner = window::root_window(element.owner_handle);
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }

    if report_text {
        for owner in owners {
            let seen = before
                .as_ref()
                .map(|state| window_text(state_nodes(state), &state.value_text, owner));
            messages.push(text_change_report(owner, seen.unwrap_or_default()));
        }
    }
    Ok(messages.join("\n"))
}

fn state_nodes(state: &state::DesktopState) -> impl Iterator<Item = &ElementNode> {
    state
        .informative_nodes
        .iter()
        .chain(&state.scrollable_nodes)
}

/// The readable text the top-level window `owner` shows — element names, then
/// field values — in document order, without repeats. Entries are matched by
/// their window's current root, since the window a Snapshot recorded may have
/// been reparented since.
fn window_text<'a>(
    nodes: impl Iterator<Item = &'a ElementNode>,
    values: &[(isize, String)],
    owner: isize,
) -> Vec<String> {
    let names = nodes
        .filter(|node| window::root_window(node.owner_handle) == owner)
        .map(|node| node.name.as_str());
    let values = values
        .iter()
        .filter(|(handle, _)| window::root_window(*handle) == owner)
        .map(|(_, value)| value.as_str());
    let mut text: Vec<String> = Vec::new();
    for name in names.chain(values) {
        let name = name.trim();
        // Icon fonts label buttons with private-use code points, which read
        // as nothing to the caller.
        let is_glyph = name.chars().all(|c| ('\u{E000}'..='\u{F8FF}').contains(&c));
        if !name.is_empty() && !is_glyph && !text.iter().any(|seen| seen == name) {
            text.push(name.to_string());
        }
    }
    text
}

/// How long to keep re-reading a window for its text to change after the
/// action. Providers update asynchronously, so the first read can come too
/// early; most settle within a frame or two.
const TEXT_CHANGE_TIMEOUT: Duration = Duration::from_millis(400);
const TEXT_CHANGE_INTERVAL: Duration = Duration::from_millis(60);
/// Most changed lines to report; a window that redrew wholesale needs a
/// Snapshot, not a list.
const MAX_REPORTED_TEXT: usize = 15;

/// Describes the text `owner` shows now that is not in `seen`.
fn text_change_report(owner: isize, seen: Vec<String>) -> String {
    let deadline = Instant::now() + TEXT_CHANGE_TIMEOUT;
    let new_text = loop {
        std::thread::sleep(TEXT_CHANGE_INTERVAL);
        let result = match snapshot::capture_window_for_polling(owner) {
            Ok(result) => result,
            Err(error) => {
                return format!("Window text: could not be read after the action: {error}");
            }
        };
        let now = window_text(
            result
                .informative_nodes
                .iter()
                .chain(&result.scrollable_nodes),
            &result.value_text,
            owner,
        );
        let new_text: Vec<String> = now
            .into_iter()
            .filter(|text| !seen.contains(text))
            .collect();
        if !new_text.is_empty() || Instant::now() >= deadline {
            break new_text;
        }
    };
    if new_text.is_empty() {
        return "Window text: no change since the last Snapshot.".to_string();
    }
    let shown: Vec<String> = new_text
        .iter()
        .take(MAX_REPORTED_TEXT)
        .map(|text| format!("{:?}", text.chars().take(120).collect::<String>()))
        .collect();
    let more = new_text.len().saturating_sub(MAX_REPORTED_TEXT);
    let mut report = format!("Window text now showing: {}", shown.join(", "));
    if more > 0 {
        report += &format!(" (and {more} more; call Snapshot for the rest)");
    }
    report
}

fn invoke_one(element: &ElementNode, fallback_to_click: bool) -> Result<String, String> {
    if choose_action(&element.supported_actions).is_some() {
        let action = uia::invoke_matching_element(element)?;
        return Ok(format!(
            "{} element {} ({:?}).",
            action.name(),
            element.element_id,
            element.name
        ));
    }
    if !fallback_to_click {
        return Err(format!(
            "Element {} has no supported semantic UIA action",
            element.element_id
        ));
    }
    let owner_bounds = window::get_window_rect(element.owner_handle)
        .ok_or_else(|| "Element owner window is closed".to_string())?;
    validate_fallback(element, owner_bounds)?;
    // Run the same window-state and occlusion checks a `label` click gets.
    // Passing `loc` below bypasses them, so they have to happen here.
    let (x, y) = support::validate_element_point(element)?;
    click::click(ClickParams {
        loc: Some(ListOrString::List(vec![x, y])),
        label: None,
        button: Some(ClickButton::Left),
        clicks: Some(1),
        modifier: None,
    })?;
    Ok(format!("Clicked element {} fallback.", element.element_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ElementNode, SupportedAction};

    fn element(bounds: (i32, i32, i32, i32), center: (i32, i32)) -> ElementNode {
        ElementNode {
            element_id: 1,
            parent_id: None,
            owner_handle: 10,
            runtime_id: vec![42, 7],
            automation_id: "settings".to_string(),
            supported_actions: Vec::new(),
            name: "Settings".to_string(),
            control_type: "menuitem".to_string(),
            center,
            bounding_box: bounds,
            has_focus: false,
        }
    }

    #[test]
    fn semantic_action_uses_documented_priority() {
        assert_eq!(
            choose_action(&[
                SupportedAction::ExpandCollapse,
                SupportedAction::SelectionItem,
                SupportedAction::Invoke,
            ]),
            Some(SupportedAction::Invoke)
        );
    }

    #[test]
    fn window_text_keeps_one_windows_readable_names_once() {
        let mut result = element((0, 0, 10, 10), (5, 5));
        result.name = "72".to_string();
        let repeated = result.clone();
        let mut glyph = result.clone();
        glyph.name = "\u{e0e3}".to_string();
        let mut other_window = result.clone();
        other_window.owner_handle = 11;
        other_window.name = "elsewhere".to_string();
        let nodes = [result, repeated, glyph, other_window];
        let values = [(10, "typed".to_string()), (11, "not ours".to_string())];
        assert_eq!(
            window_text(nodes.iter(), &values, 10),
            vec!["72".to_string(), "typed".to_string()]
        );
    }

    #[test]
    fn fallback_requires_center_inside_element_and_owner() {
        let node = element((10, 10, 30, 30), (20, 20));
        assert!(validate_fallback(&node, (0, 0, 100, 100)).is_ok());
        assert_eq!(
            validate_fallback(&node, (21, 21, 100, 100)).unwrap_err(),
            "Element center is outside the current owner window"
        );
    }
}
