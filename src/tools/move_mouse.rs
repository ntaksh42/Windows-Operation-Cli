//! `Move` tool: moves (or drags) the mouse cursor.
//!
//! Named `move_mouse` (not `move`) because `move` is a Rust keyword.

use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;

use crate::input_sim::{self, MouseButton};
use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::support::resolve_point_required;

const DRAG_START_WAIT: Duration = Duration::from_millis(50);

struct LeftButtonGuard;

impl Drop for LeftButtonGuard {
    fn drop(&mut self) {
        let _ = input_sim::mouse_up(MouseButton::Left);
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MoveParams {
    /// Target coordinates `[x, y]`. Provide either `loc` or `label`.
    pub loc: Option<ListOrString<i32>>,
    /// UI element label/id from the most recent Snapshot. Provide either
    /// `loc` or `label`.
    pub label: Option<i64>,
    /// Perform a drag-and-drop instead of a simple move. Defaults to false.
    pub drag: Option<BoolOrString>,
    /// Drag start coordinates `[x, y]`. Only valid with `drag=true`;
    /// defaults to the current cursor position.
    pub from_loc: Option<ListOrString<i32>>,
    /// Drag duration in seconds (0-10). Only valid with `drag=true`.
    pub duration: Option<f64>,
}

/// Moves (or drags) the cursor to the resolved location and returns the
/// confirmation message.
pub fn move_mouse(params: MoveParams) -> Result<String, String> {
    let drag = opt_bool(&params.drag, false)?;
    // Tell the human the desktop is being driven; concurrent manual input
    // steals focus and breaks the capture path.
    let _overlay = crate::overlay::InputOverlay::show();

    // Validation order mirrors the Python reference: loc/label first, then
    // from_loc's shape, then the drag-only-options gate, then (only once
    // we know we're dragging) the duration range.
    let (x, y) = resolve_point_required(params.loc, params.label)?;

    let from_loc_vec = match params.from_loc {
        None => None,
        Some(v) => Some(v.into_list()?),
    };
    let from_loc = match from_loc_vec {
        None => None,
        Some(v) if v.len() == 2 => Some((v[0], v[1])),
        Some(_) => return Err("from_loc must be a list of exactly 2 integers [x, y]".to_string()),
    };

    if !drag && (from_loc.is_some() || params.duration.is_some()) {
        return Err("from_loc and duration require drag=True".to_string());
    }

    if drag && let Some(duration) = params.duration {
        if !duration.is_finite() {
            return Err("duration must be a finite number of seconds".to_string());
        }
        if !(0.0..=10.0).contains(&duration) {
            return Err("duration must be between 0 and 10 seconds".to_string());
        }
    }

    if !drag {
        input_sim::set_cursor_pos(x, y)?;
        std::thread::sleep(input_sim::input_settle_delay());
        return Ok(format!("Moved the mouse pointer to ({x},{y})."));
    }

    let (start_x, start_y) = from_loc.unwrap_or_else(input_sim::get_cursor_pos);

    input_sim::set_cursor_pos(start_x, start_y)?;
    input_sim::mouse_down(MouseButton::Left)?;
    let button_guard = LeftButtonGuard;
    std::thread::sleep(DRAG_START_WAIT);
    match params.duration {
        Some(duration) => input_sim::move_smooth_duration(x, y, duration, DRAG_START_WAIT)?,
        None => input_sim::move_smooth(x, y, 1.0, DRAG_START_WAIT)?,
    }
    drop(button_guard);
    std::thread::sleep(input_sim::input_settle_delay());

    match params.duration {
        Some(duration) => Ok(format!(
            "Dragged from ({start_x},{start_y}) to ({x},{y}) over {duration:.3} seconds."
        )),
        None => Ok(format!("Dragged from ({start_x},{start_y}) to ({x},{y}).")),
    }
}
