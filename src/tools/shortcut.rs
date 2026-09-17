//! `Shortcut` tool: presses a `+`-separated key combination as a
//! simultaneous chord (e.g. `"ctrl+shift+esc"`).

use rmcp::schemars;
use serde::Deserialize;

use crate::input_sim;
use crate::keys;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShortcutParams {
    /// Key combination, e.g. `"ctrl+c"`, `"alt+tab"`, `"win+r"`.
    pub shortcut: String,
}

/// Presses the `+`-separated keys in `shortcut` together and returns the
/// confirmation message.
pub fn shortcut(params: ShortcutParams) -> Result<String, String> {
    // Tell the human the desktop is being driven; concurrent manual input
    // steals focus and breaks the capture path.
    let _overlay = crate::overlay::InputOverlay::show();
    // `split('+')` always yields at least one token, and an empty one is
    // rejected by `resolve_key`, so an empty shortcut fails here rather than
    // reaching an emptiness check of its own.
    let vks: Vec<u16> = params
        .shortcut
        .split('+')
        .map(keys::resolve_key)
        .collect::<Result<_, _>>()?;

    input_sim::chord(&vks, input_sim::input_settle_delay())?;
    Ok(format!("Pressed {}.", params.shortcut))
}
