use rmcp::schemars;
use serde::Deserialize;

use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::state::{self, ElementNode, SupportedAction};
use crate::tools::click::{self, ClickButton, ClickParams};
use crate::tools::support;
use crate::{uia, window};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InvokeElementParams {
    /// Generation-scoped element id from the most recent Snapshot.
    pub element_id: u64,
    /// Click the last validated center when no semantic UIA action exists.
    pub fallback_to_click: Option<BoolOrString>,
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

pub fn invoke_element(params: InvokeElementParams) -> Result<String, String> {
    let element = state::resolve_element(params.element_id)?;
    if choose_action(&element.supported_actions).is_some() {
        let action = uia::invoke_matching_element(&element)?;
        return Ok(format!(
            "{} element {} ({:?}).",
            action.name(),
            element.element_id,
            element.name
        ));
    }
    if !opt_bool(&params.fallback_to_click, false)? {
        return Err(format!(
            "Element {} has no supported semantic UIA action",
            element.element_id
        ));
    }
    let owner_bounds = window::get_window_rect(element.owner_handle)
        .ok_or_else(|| "Element owner window is closed".to_string())?;
    validate_fallback(&element, owner_bounds)?;
    // Run the same window-state and occlusion checks a `label` click gets.
    // Passing `loc` below bypasses them, so they have to happen here.
    let (x, y) = support::validate_element_point(&element)?;
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
    fn fallback_requires_center_inside_element_and_owner() {
        let node = element((10, 10, 30, 30), (20, 20));
        assert!(validate_fallback(&node, (0, 0, 100, 100)).is_ok());
        assert_eq!(
            validate_fallback(&node, (21, 21, 100, 100)).unwrap_err(),
            "Element center is outside the current owner window"
        );
    }
}
