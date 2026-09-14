//! `Type` tool: types text at coordinates or a UI element label.
//!
//! Named `typing` (not `type`) because `type` is a Rust keyword.

use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_CONTROL, VK_END, VK_HOME, VK_RETURN,
};

use crate::input_sim::{self, MouseButton};
use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::support::resolve_point_required;

/// Strings at least this long, with no control characters, are typed via a
/// clipboard paste instead of per-character `SendInput` calls — bypasses the
/// keyboard event queue, which can drop keystrokes under load for long text.
const LONG_TEXT_PASTE_THRESHOLD: usize = 20;

const KEY_WAIT: Duration = Duration::from_millis(50);
const TYPE_INTERVAL: Duration = Duration::from_millis(40);
const PASTE_SETTLE_WAIT: Duration = Duration::from_millis(50);

#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaretPosition {
    Start,
    Idle,
    End,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TypeParams {
    /// The text to type.
    pub text: String,
    /// Target coordinates `[x, y]`. Provide either `loc` or `label`.
    pub loc: Option<ListOrString<i32>>,
    /// UI element label/id from the most recent Snapshot. Provide either
    /// `loc` or `label`.
    pub label: Option<i64>,
    /// Clear existing text (Ctrl+A, Backspace) before typing. Defaults to
    /// false.
    pub clear: Option<BoolOrString>,
    /// Where to place the caret before typing: `start`, `idle` (default), or
    /// `end`.
    pub caret_position: Option<CaretPosition>,
    /// Press Enter after typing. Defaults to false.
    pub press_enter: Option<BoolOrString>,
}

/// Types `text` at the resolved location and returns the confirmation
/// message.
pub fn type_text(params: TypeParams) -> Result<String, String> {
    let (x, y) = resolve_point_required(params.loc, params.label)?;
    let clear = opt_bool(&params.clear, false)?;
    let press_enter = opt_bool(&params.press_enter, false)?;
    let caret_position = params.caret_position.unwrap_or(CaretPosition::Idle);

    type_at(x, y, &params.text, caret_position, clear, press_enter)?;
    Ok(format!("Typed {} at ({x},{y}).", params.text))
}

/// Core typing sequence shared by the `Type` tool and `MultiEdit`: focus via
/// click, position the caret, optionally clear, then send `text`.
pub fn type_at(
    x: i32,
    y: i32,
    text: &str,
    caret_position: CaretPosition,
    clear: bool,
    press_enter: bool,
) -> Result<(), String> {
    input_sim::click_once(x, y, MouseButton::Left, input_sim::input_settle_delay())?;

    match caret_position {
        CaretPosition::Start => input_sim::key_tap(VK_HOME.0, KEY_WAIT)?,
        CaretPosition::End => input_sim::key_tap(VK_END.0, KEY_WAIT)?,
        CaretPosition::Idle => {}
    }

    if clear {
        input_sim::chord(&[VK_CONTROL.0, b'A' as u16], KEY_WAIT)?;
        input_sim::key_tap(VK_BACK.0, KEY_WAIT)?;
    }

    let has_control_chars = text.contains(['\n', '\t', '{', '}']);
    let pasted = text.chars().count() >= LONG_TEXT_PASTE_THRESHOLD
        && !has_control_chars
        && paste_text(text)?;
    if !pasted {
        input_sim::type_text_char_by_char(text, TYPE_INTERVAL, KEY_WAIT)?;
    }

    if press_enter {
        input_sim::key_tap(VK_RETURN.0, KEY_WAIT)?;
    }
    Ok(())
}

/// Stashes `text` on the clipboard, pastes via Ctrl+V, then restores the
/// prior clipboard contents.
fn paste_text(text: &str) -> Result<bool, String> {
    let prior = input_sim::get_clipboard_text();
    if !input_sim::set_clipboard_text(text) {
        return Ok(false);
    }
    std::thread::sleep(PASTE_SETTLE_WAIT);
    let paste_result = input_sim::chord(&[VK_CONTROL.0, b'V' as u16], KEY_WAIT);
    std::thread::sleep(PASTE_SETTLE_WAIT);
    match prior {
        Some(prior) => {
            input_sim::set_clipboard_text(&prior);
        }
        None => input_sim::clear_clipboard(),
    }
    paste_result?;
    Ok(true)
}
