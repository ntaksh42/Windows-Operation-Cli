//! `Scroll` tool: mouse wheel scrolling at coordinates, a UI element label,
//! or the current cursor position.

use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT;

use crate::input_sim;
use crate::params::ListOrString;
use crate::tools::support::resolve_point_optional;

const NOTCH_INTERVAL: Duration = Duration::from_millis(50);
const SHIFT_KEY_WAIT: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollType {
    Horizontal,
    Vertical,
}

impl ScrollType {
    fn label(self) -> &'static str {
        match self {
            ScrollType::Horizontal => "horizontal",
            ScrollType::Vertical => "vertical",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDirection {
    fn label(self) -> &'static str {
        match self {
            ScrollDirection::Up => "up",
            ScrollDirection::Down => "down",
            ScrollDirection::Left => "left",
            ScrollDirection::Right => "right",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScrollParams {
    /// Target coordinates `[x, y]`. Optional — defaults to the current
    /// cursor position.
    pub loc: Option<ListOrString<i32>>,
    /// UI element label/id from the most recent Snapshot.
    pub label: Option<i64>,
    /// Scroll axis: `vertical` (default) or `horizontal`.
    #[serde(rename = "type")]
    pub scroll_type: Option<ScrollType>,
    /// Scroll direction: `up`/`down` for vertical, `left`/`right` for
    /// horizontal. Defaults to `down`.
    pub direction: Option<ScrollDirection>,
    /// Number of wheel notches. Defaults to 1.
    pub wheel_times: Option<i64>,
    /// Optional keyboard modifier held atomically during scrolling.
    pub modifier: Option<String>,
}

/// Scrolls at the resolved location (or the current cursor position) and
/// returns the confirmation message, or an error string for an invalid
/// type/direction combination.
pub fn scroll(params: ScrollParams) -> Result<String, String> {
    // Tell the human the desktop is being driven; concurrent manual input
    // steals focus and breaks the capture path.
    let _overlay = crate::overlay::InputOverlay::show();
    let point = resolve_point_optional(params.loc, params.label)?;
    let scroll_type = params.scroll_type.unwrap_or(ScrollType::Vertical);
    let direction = params.direction.unwrap_or(ScrollDirection::Down);
    let wheel_times = params.wheel_times.unwrap_or(1);
    if !(0..=100).contains(&wheel_times) {
        return Err("wheel_times must be between 0 and 100".to_string());
    }
    let wheel_times = wheel_times as i32;

    if scroll_type == ScrollType::Vertical
        && !matches!(direction, ScrollDirection::Up | ScrollDirection::Down)
    {
        return Ok(r#"Invalid direction. Use "up" or "down"."#.to_string());
    }
    if scroll_type == ScrollType::Horizontal
        && !matches!(direction, ScrollDirection::Left | ScrollDirection::Right)
    {
        return Ok(r#"Invalid direction. Use "left" or "right"."#.to_string());
    }

    let modifier = input_sim::ModifierGuard::press(params.modifier.as_deref())?;

    if let Some((x, y)) = point {
        input_sim::set_cursor_pos(x, y)?;
        std::thread::sleep(input_sim::input_settle_delay());
    }

    match (scroll_type, direction) {
        (ScrollType::Vertical, ScrollDirection::Up) => {
            input_sim::wheel(wheel_times, NOTCH_INTERVAL, input_sim::input_settle_delay())?;
        }
        (ScrollType::Vertical, ScrollDirection::Down) => {
            input_sim::wheel(
                -wheel_times,
                NOTCH_INTERVAL,
                input_sim::input_settle_delay(),
            )?;
        }
        (ScrollType::Vertical, _) => unreachable!("direction validated above"),
        (ScrollType::Horizontal, ScrollDirection::Left) => {
            let shift_already_held = modifier
                .as_ref()
                .is_some_and(|guard| guard.virtual_key() == VK_SHIFT.0);
            let shift = (!shift_already_held)
                .then(|| input_sim::KeyGuard::press(VK_SHIFT.0))
                .transpose()?;
            std::thread::sleep(SHIFT_KEY_WAIT);
            input_sim::wheel(wheel_times, NOTCH_INTERVAL, input_sim::input_settle_delay())?;
            std::thread::sleep(NOTCH_INTERVAL);
            drop(shift);
            std::thread::sleep(SHIFT_KEY_WAIT);
        }
        (ScrollType::Horizontal, ScrollDirection::Right) => {
            let shift_already_held = modifier
                .as_ref()
                .is_some_and(|guard| guard.virtual_key() == VK_SHIFT.0);
            let shift = (!shift_already_held)
                .then(|| input_sim::KeyGuard::press(VK_SHIFT.0))
                .transpose()?;
            std::thread::sleep(SHIFT_KEY_WAIT);
            input_sim::wheel(
                -wheel_times,
                NOTCH_INTERVAL,
                input_sim::input_settle_delay(),
            )?;
            std::thread::sleep(NOTCH_INTERVAL);
            drop(shift);
            std::thread::sleep(SHIFT_KEY_WAIT);
        }
        (ScrollType::Horizontal, _) => unreachable!("direction validated above"),
    }

    let (x, y) = point.unwrap_or_else(input_sim::get_cursor_pos);
    Ok(format!(
        "Scrolled {} {} by {wheel_times} wheel times at ({x},{y}).",
        scroll_type.label(),
        direction.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(wheel_times: i64) -> ScrollParams {
        ScrollParams {
            loc: None,
            label: None,
            scroll_type: None,
            direction: None,
            wheel_times: Some(wheel_times),
            modifier: None,
        }
    }

    #[test]
    fn wheel_times_is_limited_to_one_hundred() {
        assert_eq!(
            scroll(params(-1)).unwrap_err(),
            "wheel_times must be between 0 and 100"
        );
        assert_eq!(
            scroll(params(101)).unwrap_err(),
            "wheel_times must be between 0 and 100"
        );
    }
}
