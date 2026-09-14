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
    let vks: Vec<u16> = params
        .shortcut
        .split('+')
        .map(keys::resolve_key)
        .collect::<Result<_, _>>()?;

    if vks.is_empty() {
        return Err("shortcut must not be empty".to_string());
    }

    input_sim::chord(&vks, input_sim::input_settle_delay())?;
    Ok(format!("Pressed {}.", params.shortcut))
}
