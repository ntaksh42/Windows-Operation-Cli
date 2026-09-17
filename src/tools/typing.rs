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
///
/// Measured in UTF-8 bytes rather than characters. Counting characters made
/// the threshold miss non-Latin text: 19 Japanese characters fell under it and
/// went out one `SendInput` at a time at `TYPE_INTERVAL` each, ~760ms for a
/// string a paste delivers at once. Bytes put CJK text (3 bytes per character)
/// on the paste path at around 7 characters, which is where the per-character
/// route stops being the cheaper option.
const LONG_TEXT_PASTE_THRESHOLD: usize = 20;

/// Whether `text` should go out as a clipboard paste rather than character by
/// character.
///
/// Newlines and tabs are sent as Enter/Tab key taps so form navigation still
/// works, which a paste cannot reproduce — those keep the per-character route.
/// Braces do not: `{` and `}` need escaping in `SendKeys`-style APIs, but this
/// code uses `KEYEVENTF_UNICODE` and `Ctrl+V`, where they are ordinary
/// characters. Excluding them only pushed JSON and code onto the slow path.
fn should_paste(text: &str) -> bool {
    text.len() >= LONG_TEXT_PASTE_THRESHOLD && !text.contains(['\n', '\t', '\r'])
}

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
    // Tell the human the desktop is being driven; concurrent manual input
    // steals focus and breaks the capture path.
    let _overlay = crate::overlay::InputOverlay::show();
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
        clear_focused_text()?;
    }

    let pasted = should_paste(text) && paste_text(text)?;
    if !pasted {
        input_sim::type_text_char_by_char(text, TYPE_INTERVAL, KEY_WAIT)?;
    }

    if press_enter {
        input_sim::key_tap(VK_RETURN.0, KEY_WAIT)?;
    }
    Ok(())
}

/// Empties the focused control before new text is typed into it.
///
/// Prefers the UIA `ValuePattern`, which asks the provider to set an empty
/// value. The keyboard route below only works when the control implements
/// select-all itself: a bare Win32 `EDIT` does not, so Ctrl+A selects nothing
/// and the Backspace removes one character, splicing the new text onto the
/// remains of the old. The chord stays as the fallback for controls that
/// expose no writable `ValuePattern`.
fn clear_focused_text() -> Result<(), String> {
    if crate::uia::clear_focused_element_value()? {
        return Ok(());
    }
    input_sim::chord(&[VK_CONTROL.0, b'A' as u16], KEY_WAIT)?;
    input_sim::key_tap(VK_BACK.0, KEY_WAIT)?;
    Ok(())
}

/// How long to keep checking for the pasted text to appear before giving up
/// and letting the caller type it instead.
const PASTE_CONFIRM_TIMEOUT: Duration = Duration::from_millis(400);
const PASTE_CONFIRM_INTERVAL: Duration = Duration::from_millis(25);

/// Stashes `text` on the clipboard, pastes via Ctrl+V, confirms the paste
/// landed, then restores the prior clipboard contents.
///
/// Returns `Ok(false)` when the paste could not be confirmed, so the caller
/// falls back to sending the text character by character. A fixed sleep was
/// not enough on its own: the clipboard is restored right after, and a target
/// that reads it late — an Electron app, a remote desktop — pasted the
/// *restored* contents or nothing at all, and the tool still reported success.
fn paste_text(text: &str) -> Result<bool, String> {
    let prior = input_sim::get_clipboard_text();
    if !input_sim::set_clipboard_text(text) {
        return Ok(false);
    }
    // Read the target before pasting so an unchanged value afterwards is
    // distinguishable from one that happens to look similar.
    let before = crate::uia::focused_element_value();
    std::thread::sleep(PASTE_SETTLE_WAIT);
    let paste_result = input_sim::chord(&[VK_CONTROL.0, b'V' as u16], KEY_WAIT);
    let landed = paste_result.is_ok() && wait_for_pasted_text(text, before.as_deref());
    match prior {
        Some(prior) => {
            input_sim::set_clipboard_text(&prior);
        }
        None => input_sim::clear_clipboard(),
    }
    paste_result?;
    Ok(landed)
}

/// Polls the focused control for `text` until it shows up or the timeout
/// passes.
///
/// Only a control that still holds `before` — visibly unchanged — is reported
/// as not pasted. Anything else counts as landed, including a control that
/// exposes no readable value at all. Retyping is only safe when nothing
/// arrived: a paste that half-landed would otherwise be spliced together with
/// a full retype, which is worse than the silent failure this guards against.
fn wait_for_pasted_text(text: &str, before: Option<&str>) -> bool {
    let deadline = std::time::Instant::now() + PASTE_CONFIRM_TIMEOUT;
    loop {
        match crate::uia::focused_element_value() {
            // The value moved on from what was there before the paste.
            Some(value) if value.contains(text) => return true,
            Some(value) if Some(value.as_str()) != before => return true,
            // Nothing readable to compare against; assume the paste worked
            // rather than risk typing the text a second time.
            None => return true,
            Some(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(PASTE_CONFIRM_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_text_reaches_the_paste_path() {
        // 7 characters, 21 bytes. Counting characters kept strings like this
        // on the per-character route, where each character costs TYPE_INTERVAL.
        let text = "こんにちは世界";
        assert_eq!(text.chars().count(), 7);
        assert!(should_paste(text));
    }

    #[test]
    fn braces_do_not_force_the_slow_path() {
        // Brace escaping is a SendKeys constraint; this code pastes via
        // Ctrl+V and types via KEYEVENTF_UNICODE, where braces are ordinary.
        assert!(should_paste(r#"{"key": "value", "n": 1}"#));
    }

    #[test]
    fn newlines_and_tabs_keep_the_per_character_route() {
        // These go out as Enter/Tab key taps so form navigation still works,
        // which a clipboard paste cannot reproduce.
        assert!(!should_paste("a line that is plenty long\nand another"));
        assert!(!should_paste("a value that is plenty long\tnext field"));
    }

    #[test]
    fn short_text_is_typed_directly() {
        assert!(!should_paste("ok"));
        assert!(!should_paste("short"));
    }
}
