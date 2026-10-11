//! `MultiSelect` tool: clicks multiple coordinates/labels in sequence,
//! optionally holding Ctrl for multi-selection.

use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;

use crate::input_sim::{self, MouseButton};
use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::support::InputTarget;

const CTRL_KEY_WAIT: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MultiSelectParams {
    /// Coordinates to click: `[[x, y], ...]`. Provide `locs` and/or `labels`.
    pub locs: Option<ListOrString<Vec<i32>>>,
    /// UI element labels/ids from the most recent Snapshot. Provide `locs`
    /// and/or `labels`.
    pub labels: Option<ListOrString<i64>>,
    /// Hold Ctrl while clicking, for multi-selecting items. Defaults to
    /// true.
    pub press_ctrl: Option<BoolOrString>,
}

/// Clicks each resolved coordinate (holding Ctrl if `press_ctrl`) and
/// returns the confirmation message.
pub fn multi_select(params: MultiSelectParams) -> Result<String, String> {
    if params.locs.is_none() && params.labels.is_none() {
        return Err("Either locs or labels must be provided.".to_string());
    }

    let mut points: Vec<InputTarget> = Vec::new();
    if let Some(locs) = params.locs {
        for loc in locs.into_list()? {
            if loc.len() != 2 {
                return Err("Each loc must be a list of exactly 2 integers [x, y]".to_string());
            }
            points.push(InputTarget::Point(loc[0], loc[1]));
        }
    }
    if let Some(labels) = params.labels {
        let labels = labels.into_list()?;
        let total = points.len() + labels.len();
        for label in labels {
            points.push(InputTarget::from_label(label).map_err(|error| format!("Completed 0 of {total} selections: {error}"))?);
        }
    }
    if points.is_empty() {
        return Err("At least one loc or label must be provided.".to_string());
    }

    let press_ctrl = opt_bool(&params.press_ctrl, true)?;
    let total = points.len();
    for target in &points {
        target.resolve().map_err(|error| format!("Completed 0 of {total} selections: {error}"))?;
    }

    let _overlay = crate::overlay::InputOverlay::show();
    let ctrl = if press_ctrl {
        let guard = input_sim::KeyGuard::press(VK_CONTROL.0)?;
        std::thread::sleep(CTRL_KEY_WAIT);
        Some(guard)
    } else {
        None
    };
    let mut elements = Vec::new();
    for (index, target) in points.iter().enumerate() {
        let (x, y) = target.resolve().map_err(|error| format!("Completed {index} of {total} selections: {error}"))?;
        input_sim::click_once(x, y, MouseButton::Left, input_sim::input_settle_delay())
            .map_err(|error| format!("Completed {index} of {total} selections: {error}"))?;
        elements.push(format!("({x},{y})"));
    }
    drop(ctrl);
    std::thread::sleep(CTRL_KEY_WAIT);

    Ok(format!(
        "Multi-selected elements at:\n{}",
        elements.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_batch_reports_zero_completed_before_any_input() {
        let result = multi_select(MultiSelectParams {
            locs: None, labels: Some(ListOrString::List(vec![-1, -2])), press_ctrl: None,
        });
        assert!(result.unwrap_err().contains("Completed 0 of 2"));
    }
}
