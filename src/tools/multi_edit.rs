//! `MultiEdit` tool: types text into multiple fields, identified by
//! coordinates or UI element labels.

use rmcp::schemars;
use serde::Deserialize;

use crate::params::ListOrString;
use crate::tools::support::InputTarget;
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

    let mut entries: Vec<(InputTarget, String)> = Vec::new();
    if let Some(locs) = params.locs {
        entries.extend(locs.into_list()?.into_iter().map(|(x, y, text)| (InputTarget::Point(x, y), text)));
    }
    if let Some(labels) = params.labels {
        let labels = labels.into_list()?;
        let total = entries.len() + labels.len();
        for (label, text) in labels {
            let target = InputTarget::from_label(label).map_err(|error| format!("Completed 0 of {total} edits: {error}"))?;
            entries.push((target, text));
        }
    }
    if entries.is_empty() {
        return Err("At least one loc or label entry must be provided.".to_string());
    }

    let total = entries.len();
    for (target, _) in &entries {
        target.resolve().map_err(|error| format!("Completed 0 of {total} edits: {error}"))?;
    }
    let _overlay = crate::overlay::InputOverlay::show();
    let mut elements = Vec::new();
    for (index, (target, text)) in entries.iter().enumerate() {
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
        let (x, y) = target.resolve().map_err(|error| format!("Completed {index} of {total} edits: {error}"))?;
        let warning = typing::type_at_report(x, y, text, CaretPosition::Idle, true, false)
            .map_err(|error| format!("Completed {index} of {total} edits; current field may be partially modified: {error}"))?;
        elements.push(format!("({x},{y}) with text '{text}'{}", warning.map(|warning| format!(" ({warning})")).unwrap_or_default()));
    }

    Ok(format!("Multi-edited elements at: {}", elements.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_batch_reports_zero_completed_before_any_input() {
        let result = multi_edit(MultiEditParams {
            locs: None, labels: Some(ListOrString::List(vec![(-1, "a".into()), (-2, "b".into())])),
        });
        assert!(result.unwrap_err().contains("Completed 0 of 2"));
    }
}
