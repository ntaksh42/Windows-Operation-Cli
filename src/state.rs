//! Global desktop state, populated by the Snapshot tool and consumed here so
//! Click/Type/Scroll/Move/MultiSelect/MultiEdit can resolve a UI element
//! `label` to screen coordinates without re-querying the accessibility tree.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedAction {
    Invoke,
    SelectionItem,
    Toggle,
    ExpandCollapse,
}

impl SupportedAction {
    pub fn highest_priority(actions: &[Self]) -> Option<Self> {
        [
            Self::Invoke,
            Self::SelectionItem,
            Self::Toggle,
            Self::ExpandCollapse,
        ]
        .into_iter()
        .find(|candidate| actions.contains(candidate))
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Invoke => "invoke",
            Self::SelectionItem => "select",
            Self::Toggle => "toggle",
            Self::ExpandCollapse => "expand_collapse",
        }
    }
}

/// A single UI element discovered by Snapshot's accessibility-tree walk.
#[derive(Debug, Clone)]
pub struct ElementNode {
    pub element_id: u64,
    pub parent_id: Option<u64>,
    pub owner_handle: isize,
    pub runtime_id: Vec<i32>,
    pub automation_id: String,
    pub supported_actions: Vec<SupportedAction>,
    pub name: String,
    pub control_type: String,
    pub center: (i32, i32),
    /// Screen-coordinate bounding box `(left, top, right, bottom)`, used to
    /// draw the annotated-screenshot bounding box.
    pub bounding_box: (i32, i32, i32, i32),
    /// Whether this element held keyboard focus at capture time (used by
    /// WaitFor's `focused_element` condition).
    pub has_focus: bool,
}

/// Flat lists of interactive/scrollable elements from the most recent
/// Snapshot call. Labels index into `interactive_nodes` first, then
/// `scrollable_nodes` for the remainder.
#[derive(Debug, Clone, Default)]
pub struct DesktopState {
    pub generation: u32,
    pub interactive_nodes: Vec<ElementNode>,
    pub scrollable_nodes: Vec<ElementNode>,
}

static STATE: OnceLock<Mutex<Option<DesktopState>>> = OnceLock::new();
static GENERATION: AtomicU32 = AtomicU32::new(0);

pub fn next_generation() -> u32 {
    GENERATION.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

pub fn element_id(generation: u32, index: usize) -> u64 {
    ((generation as u64) << 32) | index as u64
}

fn state_lock() -> &'static Mutex<Option<DesktopState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// Replaces the current desktop state (called by Snapshot after a capture).
pub fn set_state(state: DesktopState) {
    *state_lock().lock().unwrap() = Some(state);
}

/// Clears the desktop state.
#[allow(dead_code)] // symmetry with set_state; no production caller yet
pub fn clear_state() {
    *state_lock().lock().unwrap() = None;
}

const EMPTY_STATE_ERROR: &str = "Desktop state is empty. Please call Snapshot first.";

pub fn resolve_element(id: u64) -> Result<ElementNode, String> {
    let guard = state_lock().lock().unwrap();
    let state = guard
        .as_ref()
        .ok_or_else(|| EMPTY_STATE_ERROR.to_string())?;
    if id >> 32 != state.generation as u64 {
        return Err(format!("Element id {id} is stale"));
    }
    state
        .interactive_nodes
        .iter()
        .chain(&state.scrollable_nodes)
        .find(|node| node.element_id == id)
        .cloned()
        .ok_or_else(|| format!("Element id {id} was not found"))
}

/// Resolves a single UI element `label` to screen coordinates.
///
/// Labels index `interactive_nodes` first; values beyond that range index
/// into `scrollable_nodes` as an offset.
#[cfg(test)]
pub fn resolve_label(label: usize) -> Result<(i32, i32), String> {
    Ok(resolve_label_node(label)?.center)
}

/// Resolves a single UI element label to its complete Snapshot identity.
/// Callers that inject input use this to reject a label whose owner window
/// has moved or closed since the Snapshot was taken.
pub fn resolve_label_node(label: usize) -> Result<ElementNode, String> {
    let guard = state_lock().lock().unwrap();
    let state = guard
        .as_ref()
        .ok_or_else(|| EMPTY_STATE_ERROR.to_string())?;
    if label < state.interactive_nodes.len() {
        Ok(state.interactive_nodes[label].clone())
    } else {
        let idx = label - state.interactive_nodes.len();
        state
            .scrollable_nodes
            .get(idx)
            .cloned()
            .ok_or_else(|| format!("Label {label} out of range"))
    }
}

/// Resolves multiple UI element labels to screen coordinates in bulk.
#[cfg(test)]
pub fn resolve_labels(labels: &[usize]) -> Result<Vec<(i32, i32)>, String> {
    let guard = state_lock().lock().unwrap();
    let state = guard
        .as_ref()
        .ok_or_else(|| EMPTY_STATE_ERROR.to_string())?;
    let mut out = Vec::with_capacity(labels.len());
    for &label in labels {
        if label < state.interactive_nodes.len() {
            out.push(state.interactive_nodes[label].center);
        } else {
            let idx = label - state.interactive_nodes.len();
            match state.scrollable_nodes.get(idx) {
                Some(n) => out.push(n.center),
                None => return Err(format!("Label {label} out of range")),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // Tests share process-global STATE; serialize them so they don't clobber
    // each other under `cargo test`'s default multi-threaded runner.
    static TEST_GUARD: StdMutex<()> = StdMutex::new(());

    fn node(x: i32, y: i32) -> ElementNode {
        ElementNode {
            element_id: 0,
            parent_id: None,
            owner_handle: 0,
            runtime_id: Vec::new(),
            automation_id: String::new(),
            supported_actions: Vec::new(),
            name: "n".into(),
            control_type: "Button".into(),
            center: (x, y),
            bounding_box: (x, y, x, y),
            has_focus: false,
        }
    }

    #[test]
    fn empty_state_errors() {
        let _g = TEST_GUARD.lock().unwrap();
        clear_state();
        assert_eq!(resolve_label(0), Err(EMPTY_STATE_ERROR.to_string()));
        assert_eq!(resolve_labels(&[0]), Err(EMPTY_STATE_ERROR.to_string()));
    }

    #[test]
    fn resolves_interactive_then_scrollable_offset() {
        let _g = TEST_GUARD.lock().unwrap();
        set_state(DesktopState {
            generation: 0,
            interactive_nodes: vec![node(1, 1), node(2, 2)],
            scrollable_nodes: vec![node(3, 3)],
        });
        assert_eq!(resolve_label(0), Ok((1, 1)));
        assert_eq!(resolve_label(1), Ok((2, 2)));
        assert_eq!(resolve_label(2), Ok((3, 3)));
        assert_eq!(resolve_label(3), Err("Label 3 out of range".to_string()));
        clear_state();
    }

    #[test]
    fn resolves_labels_in_bulk() {
        let _g = TEST_GUARD.lock().unwrap();
        set_state(DesktopState {
            generation: 0,
            interactive_nodes: vec![node(1, 1)],
            scrollable_nodes: vec![node(2, 2)],
        });
        assert_eq!(resolve_labels(&[0, 1]), Ok(vec![(1, 1), (2, 2)]));
        assert_eq!(
            resolve_labels(&[0, 5]),
            Err("Label 5 out of range".to_string())
        );
        clear_state();
    }

    #[test]
    fn supported_actions_use_semantic_priority_order() {
        assert_eq!(
            SupportedAction::highest_priority(&[
                SupportedAction::Toggle,
                SupportedAction::Invoke,
                SupportedAction::SelectionItem,
            ]),
            Some(SupportedAction::Invoke)
        );
    }

    #[test]
    fn a_new_generation_invalidates_old_element_ids() {
        let _g = TEST_GUARD.lock().unwrap();
        clear_state();
        let first_generation = next_generation();
        let mut first = node(10, 20);
        first.element_id = element_id(first_generation, 0);
        set_state(DesktopState {
            generation: first_generation,
            interactive_nodes: vec![first.clone()],
            scrollable_nodes: vec![],
        });
        assert_eq!(resolve_element(first.element_id).unwrap().center, (10, 20));

        let second_generation = next_generation();
        set_state(DesktopState {
            generation: second_generation,
            interactive_nodes: vec![],
            scrollable_nodes: vec![],
        });
        assert_eq!(
            resolve_element(first.element_id).unwrap_err(),
            format!("Element id {} is stale", first.element_id)
        );
        clear_state();
    }
}
