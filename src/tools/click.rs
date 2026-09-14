//! `Click` tool: mouse clicks at coordinates or a UI element label.

use rmcp::schemars;
use serde::Deserialize;

use crate::input_sim::{self, MouseButton};
use crate::params::ListOrString;
use crate::tools::support::resolve_point_required;

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClickButton {
    Left,
    Right,
    Middle,
}

impl ClickButton {
    fn as_mouse_button(self) -> MouseButton {
        match self {
            ClickButton::Left => MouseButton::Left,
            ClickButton::Right => MouseButton::Right,
            ClickButton::Middle => MouseButton::Middle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ClickButton::Left => "left",
            ClickButton::Right => "right",
            ClickButton::Middle => "middle",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClickParams {
    /// Target coordinates `[x, y]`. Provide either `loc` or `label`.
    pub loc: Option<ListOrString<i32>>,
    /// UI element label/id from the most recent Snapshot. Provide either
    /// `loc` or `label`.
    pub label: Option<i64>,
    /// Mouse button to use. Defaults to `left`.
    pub button: Option<ClickButton>,
    /// Number of clicks: 0 = hover only, 1 = single, 2 = double, 3 = triple.
    /// Defaults to 1.
    pub clicks: Option<i64>,
    /// Optional keyboard modifier held atomically during the click.
    pub modifier: Option<String>,
}

/// Performs `clicks` clicks with `button` at the resolved location.
pub fn click(params: ClickParams) -> Result<String, String> {
    let (x, y) = resolve_point_required(params.loc, params.label)?;
    let button = params.button.unwrap_or(ClickButton::Left);
    let clicks = params.clicks.unwrap_or(1);
    if !(0..=3).contains(&clicks) {
        return Err("clicks must be 0 (hover), 1 (single), 2 (double), or 3 (triple).".to_string());
    }

    let _modifier = input_sim::ModifierGuard::press(params.modifier.as_deref())?;

    if clicks == 0 {
        input_sim::set_cursor_pos(x, y)?;
    } else if button == ClickButton::Left && clicks >= 2 {
        let dbl_wait =
            std::time::Duration::from_millis((input_sim::get_double_click_time_ms() / 2) as u64);
        for i in 0..clicks {
            let wait_after = if i < clicks - 1 {
                dbl_wait
            } else {
                input_sim::input_settle_delay()
            };
            input_sim::click_once(x, y, button.as_mouse_button(), wait_after)?;
        }
    } else {
        for _ in 0..clicks {
            input_sim::click_once(
                x,
                y,
                button.as_mouse_button(),
                input_sim::input_settle_delay(),
            )?;
        }
    }

    let clicks_word = match clicks {
        0 => "Hover",
        1 => "Single",
        2 => "Double",
        3 => "Triple",
        _ => unreachable!("click count validated above"),
    };
    Ok(format!(
        "{clicks_word} {} clicked at ({x},{y}).",
        button.label()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsupported_click_count_before_input() {
        let result = click(ClickParams {
            loc: Some(ListOrString::List(vec![10, 20])),
            label: None,
            button: None,
            clicks: Some(4),
            modifier: None,
        });
        assert!(result.unwrap_err().contains("clicks must be"));
    }

    #[test]
    fn rejects_unknown_modifier_before_input() {
        let result = click(ClickParams {
            loc: Some(ListOrString::List(vec![10, 20])),
            label: None,
            button: None,
            clicks: Some(1),
            modifier: Some("meta".to_string()),
        });
        assert!(result.unwrap_err().contains("modifier must be"));
    }
}
