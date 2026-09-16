//! `MultiSelect` tool: clicks multiple coordinates/labels in sequence,
//! optionally holding Ctrl for multi-selection.

use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL;

use crate::input_sim::{self, MouseButton};
use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::support::resolve_labels_checked;

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

    // Tell the human the desktop is being driven; concurrent manual input
    // steals focus and breaks the capture path.
    let _overlay = crate::overlay::InputOverlay::show();

    let mut points: Vec<(i32, i32)> = Vec::new();
    if let Some(locs) = params.locs {
        for loc in locs.into_list()? {
            if loc.len() != 2 {
                return Err("Each loc must be a list of exactly 2 integers [x, y]".to_string());
            }
            points.push((loc[0], loc[1]));
        }
    }
    if let Some(labels) = params.labels {
        let labels = labels.into_list()?;
        points.extend(resolve_labels_checked(&labels)?);
    }
    if points.is_empty() {
        return Err("At least one loc or label must be provided.".to_string());
    }

    let press_ctrl = opt_bool(&params.press_ctrl, true)?;

    let ctrl = if press_ctrl {
        let guard = input_sim::KeyGuard::press(VK_CONTROL.0)?;
        std::thread::sleep(CTRL_KEY_WAIT);
        Some(guard)
    } else {
        None
    };
    for &(x, y) in &points {
        input_sim::click_once(x, y, MouseButton::Left, input_sim::input_settle_delay())?;
    }
    drop(ctrl);
    std::thread::sleep(CTRL_KEY_WAIT);

    let elements: Vec<String> = points.iter().map(|(x, y)| format!("({x},{y})")).collect();
    Ok(format!(
        "Multi-selected elements at:\n{}",
        elements.join("\n")
    ))
}
