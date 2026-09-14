//! Shared helpers for resolving a `loc`/`label` pair to screen coordinates,
//! used by Click, Type, Scroll, Move, MultiSelect, and MultiEdit.

use crate::params::ListOrString;
use crate::state;
use crate::window;

fn point_inside_rect(x: i32, y: i32, bounds: (i32, i32, i32, i32)) -> bool {
    let (left, top, width, height) = bounds;
    x >= left && x < left + width && y >= top && y < top + height
}

fn validate_label_point(node: state::ElementNode) -> Result<(i32, i32), String> {
    let (owner_x, owner_y, owner_width, owner_height) = window::get_window_rect(node.owner_handle)
        .ok_or_else(|| "Label owner window is closed. Please call Snapshot again.".to_string())?;
    let (x, y) = node.center;
    if !point_inside_rect(x, y, (owner_x, owner_y, owner_width, owner_height)) {
        return Err("Label owner window moved. Please call Snapshot again.".to_string());
    }
    Ok((x, y))
}

/// Resolves one `label` value to its Snapshot node.
///
/// Snapshot prints `[id=<element_id>]`, where `element_id` packs the capture
/// generation into the high 32 bits (`state::element_id`). Generations start
/// at 1, so every printed id is >= 2^32 while a positional label is bounded
/// by the element count — the two spaces cannot overlap. Accept both: an id
/// resolves through the generation-checked path (so one from a superseded
/// Snapshot is reported as stale instead of silently indexing to an
/// unrelated element), a smaller value stays a positional label.
fn resolve_label_value(label: i64) -> Result<state::ElementNode, String> {
    if label < 0 {
        return Err(format!("Label {label} out of range"));
    }
    let label = label as u64;
    if label >= (1u64 << 32) {
        return state::resolve_element(label);
    }
    state::resolve_label_node(label as usize)
}

/// Resolves a UI element `label` to coordinates, rejecting negative labels
/// up front (the Python reference silently wraps negative labels via
/// Python's list-negative-indexing; a label is never meant to be negative,
/// so this reports it as out of range instead).
pub fn resolve_label_checked(label: i64) -> Result<(i32, i32), String> {
    validate_label_point(resolve_label_value(label)?)
}

/// Resolves multiple labels to coordinates in bulk.
pub fn resolve_labels_checked(labels: &[i64]) -> Result<Vec<(i32, i32)>, String> {
    labels
        .iter()
        .map(|&label| resolve_label_value(label).and_then(validate_label_point))
        .collect()
}

/// Converts an optional `loc` param into `Option<Vec<i32>>`, resolving the
/// JSON-stringified-array fallback.
pub fn as_loc_vec(loc: Option<ListOrString<i32>>) -> Result<Option<Vec<i32>>, String> {
    match loc {
        None => Ok(None),
        Some(v) => Ok(Some(v.into_list()?)),
    }
}

/// Resolves a required `loc`/`label` pair to a single `(x, y)` point.
/// `label`, when present, always takes priority over `loc` (matching the
/// Python reference).
pub fn resolve_point_required(
    loc: Option<ListOrString<i32>>,
    label: Option<i64>,
) -> Result<(i32, i32), String> {
    let loc_vec = as_loc_vec(loc)?;
    if loc_vec.is_none() && label.is_none() {
        return Err("Either loc or label must be provided.".to_string());
    }
    if let Some(label) = label {
        return resolve_label_checked(label);
    }
    let v = loc_vec.unwrap();
    if v.len() != 2 {
        return Err("Location must be a list of exactly 2 integers [x, y]".to_string());
    }
    Ok((v[0], v[1]))
}

/// Resolves an optional `loc`/`label` pair. Returns `None` when neither is
/// provided (used by Scroll, which defaults to the current cursor position).
pub fn resolve_point_optional(
    loc: Option<ListOrString<i32>>,
    label: Option<i64>,
) -> Result<Option<(i32, i32)>, String> {
    let loc_vec = as_loc_vec(loc)?;
    if let Some(label) = label {
        return Ok(Some(resolve_label_checked(label)?));
    }
    match loc_vec {
        None => Ok(None),
        Some(v) if v.len() == 2 => Ok(Some((v[0], v[1]))),
        Some(_) => Err("Location must be a list of exactly 2 integers [x, y]".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_bounds_detect_a_moved_label() {
        assert!(point_inside_rect(20, 20, (10, 10, 20, 20)));
        assert!(!point_inside_rect(20, 20, (21, 21, 20, 20)));
    }
}
