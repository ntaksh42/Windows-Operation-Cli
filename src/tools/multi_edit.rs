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

    for &(x, y, ref text) in &entries {
        typing::type_at(x, y, text, CaretPosition::Idle, true, false)?;
    }

    let elements: Vec<String> = entries
        .iter()
        .map(|(x, y, text)| format!("({x},{y}) with text '{text}'"))
        .collect();
    Ok(format!("Multi-edited elements at: {}", elements.join(", ")))
}
