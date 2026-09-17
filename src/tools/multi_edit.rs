//! `MultiEdit` tool: types text into multiple fields, identified by
//! coordinates or UI element labels.

use rmcp::schemars;
use serde::Deserialize;

use crate::params::ListOrString;
use crate::tools::support::resolve_labels_checked;
use crate::tools::typing::{self, CaretPosition};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MultiEditParams {
    /// Coordinates and text to type: `[[x, y, text], ...]`. Provide `locs`
    /// and/or `labels`.
    pub locs: Option<ListOrString<(i32, i32, String)>>,
    /// UI element labels/ids and text to type: `[[label, text], ...]`.
    /// Provide `locs` and/or `labels`.
    pub labels: Option<ListOrString<(i64, String)>>,
}

/// How long to let the keyboard queue drain before moving to the next field.
///
/// Scaled off the configured input settle delay so a machine that needs the
/// slower setting gets a proportionally longer gap here too.
fn field_settle_delay() -> std::time::Duration {
    crate::input_sim::input_settle_delay() * 3
}

/// Types each `(x, y, text)` entry (with `clear=true`) and returns the
/// confirmation message.
pub fn multi_edit(params: MultiEditParams) -> Result<String, String> {
    if params.locs.is_none() && params.labels.is_none() {
        return Err("Either locs or labels must be provided.".to_string());
    }

    let mut entries: Vec<(i32, i32, String)> = Vec::new();
    if let Some(locs) = params.locs {
        entries.extend(locs.into_list()?);
    }
    if let Some(labels) = params.labels {
        let labels = labels.into_list()?;
        let label_ids: Vec<i64> = labels.iter().map(|(label, _)| *label).collect();
        let coords = resolve_labels_checked(&label_ids)?;
        for ((x, y), (_, text)) in coords.into_iter().zip(labels) {
            entries.push((x, y, text));
        }
    }
    if entries.is_empty() {
        return Err("At least one loc or label entry must be provided.".to_string());
    }

    for (index, &(x, y, ref text)) in entries.iter().enumerate() {
        if index > 0 {
            // `SendInput` queues keystrokes; it does not wait for the target
            // to consume them. Moving to the next field immediately clicked
            // away while the previous field's characters were still arriving,
            // and they landed in whichever field had focus by then — observed
            // as "first field" and "replacement" coming out interleaved as
            // "fitrsat field" and "rcephlaoceument". Let the queue drain
            // before taking focus somewhere else.
            std::thread::sleep(field_settle_delay());
        }
        typing::type_at(x, y, text, CaretPosition::Idle, true, false)?;
    }

    let elements: Vec<String> = entries
        .iter()
        .map(|(x, y, text)| format!("({x},{y}) with text '{text}'"))
        .collect();
    Ok(format!("Multi-edited elements at: {}", elements.join(", ")))
}
