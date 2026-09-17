//! `Snapshot` tool: accessibility-tree-aware desktop capture.
//!
//! The performance-critical piece is `uia::walk_window`, which fetches an
//! entire window's interactive/scrollable subtree via a single
//! `FindAllBuildCache` call (see `src/uia.rs`) instead of walking the tree
//! element-by-element over COM. This module owns the response-text
//! formatting, the window bookkeeping (focused/opened windows, retry,
//! ordering), and the optional annotated-screenshot rendering; `state.rs`
//! is where the resulting `interactive_nodes`/`scrollable_nodes` end up so
//! Click/Type/Scroll/Move/MultiSelect/MultiEdit can resolve a `label`.

use std::time::{Duration, Instant};

use image::ImageEncoder;
use image::imageops::FilterType;
use rmcp::schemars;
use serde::Deserialize;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationCacheRequest, IUIAutomationCondition,
};

use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::screenshot;
use crate::{capture, display, ia2, state, uia, vdm, window};

const DEFAULT_UIA_TIMEOUT_MS: u64 = 2_000;
/// A whole-desktop sweep walks every window rather than one, so the same
/// budget truncates it. Measured on a desktop of 22 windows: the sweep
/// completes in about 1.8s, which the 2s default clipped almost every time —
/// the caller got a partial tree and a "truncated" note for a scan that was
/// essentially done. The larger default is for the work, not for slack.
const DEFAULT_ALL_SCOPE_TIMEOUT_MS: u64 = 8_000;
const MIN_UIA_TIMEOUT_MS: u64 = 100;
const MAX_UIA_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotScope {
    Foreground,
    All,
}

#[derive(Debug)]
struct ScanOptions {
    scope: SnapshotScope,
    window: Option<String>,
    timeout: Duration,
}

impl ScanOptions {
    fn resolve(
        scope: Option<SnapshotScope>,
        window: Option<String>,
        timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        let scope = scope.unwrap_or(SnapshotScope::Foreground);
        if scope == SnapshotScope::All && window.is_some() {
            return Err("window cannot be combined with scope=all".to_string());
        }
        let timeout_ms = timeout_ms.unwrap_or(match scope {
            SnapshotScope::All => DEFAULT_ALL_SCOPE_TIMEOUT_MS,
            SnapshotScope::Foreground => DEFAULT_UIA_TIMEOUT_MS,
        });
        if !(MIN_UIA_TIMEOUT_MS..=MAX_UIA_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(format!(
                "timeout_ms must be between {MIN_UIA_TIMEOUT_MS} and {MAX_UIA_TIMEOUT_MS}"
            ));
        }
        Ok(Self {
            scope,
            window,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

fn select_scan_targets<'a>(
    options: &ScanOptions,
    foreground: Option<&window::WindowInfo>,
    windows: &'a [window::SnapshotWindow],
) -> Result<Vec<&'a window::SnapshotWindow>, String> {
    if let Some(query) = options.window.as_deref() {
        let query = query.to_lowercase();
        // Window titles carry document names, counts, and the application
        // name ("repo and 6 more pages - Personal - Microsoft Edge"), so a
        // caller names the part they know. Whole-string `ratio` scores such a
        // query by how much of the title it fails to cover, which put even a
        // verbatim prefix of a long title under the cutoff. Score by the best
        // matching window of the title instead, keeping `ratio` so a query
        // that does span the whole title still wins over a partial hit.
        let mut scored: Vec<_> = windows
            .iter()
            .map(|candidate| {
                let title = candidate.title.to_lowercase();
                let mut score = crate::fuzzy::ratio(&query, &title);
                // `partial_ratio` slides the shorter string across the longer
                // one, so a title shorter than the query scores 100 whenever
                // the title appears anywhere in it — "Claude" would beat
                // "Claude Settings" for the query "Claude Settings". Trust it
                // only when the query is the shorter side, which is the case
                // it exists for: naming part of a long title.
                if query.chars().count() <= title.chars().count() {
                    score = score.max(crate::fuzzy::partial_ratio(&query, &title));
                }
                (candidate, score)
            })
            .filter(|(_, score)| *score >= 70.0)
            .collect();
        // Partial matching scores every title containing the query at 100, so
        // ties are now common ("GitHub" against two open tabs). Prefer the
        // shortest title among equals: it is the one the query covers most of,
        // and therefore the closest thing to what was asked for. Only a tie on
        // both score and length is genuinely ambiguous.
        scored.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| a.0.title.chars().count().cmp(&b.0.title.chars().count()))
        });
        let Some((best, best_score)) = scored.first() else {
            return Err(format!("Window not found: {query}"));
        };
        let best_len = best.title.chars().count();
        if scored.get(1).is_some_and(|(candidate, score)| {
            (*score - *best_score).abs() < f64::EPSILON
                && candidate.title.chars().count() == best_len
        }) {
            return Err(format!("Window query is ambiguous: {query}"));
        }
        let mut application_windows = vec![*best];
        application_windows.extend(
            windows
                .iter()
                .filter(|candidate| candidate.handle != best.handle && candidate.pid == best.pid),
        );
        return Ok(application_windows);
    }

    let foreground_handle = foreground.map(|window| window.handle);
    let foreground_window = foreground_handle
        .and_then(|handle| windows.iter().find(|candidate| candidate.handle == handle));
    match options.scope {
        SnapshotScope::Foreground => {
            let foreground_window = foreground_window.ok_or_else(|| {
                "No foreground window is available for UI tree scanning".to_string()
            })?;
            let mut application_windows = vec![foreground_window];
            application_windows.extend(windows.iter().filter(|candidate| {
                candidate.handle != foreground_window.handle
                    && candidate.pid == foreground_window.pid
            }));
            Ok(application_windows)
        }
        SnapshotScope::All => {
            let mut ordered = Vec::with_capacity(windows.len());
            if let Some(window) = foreground_window {
                ordered.push(window);
            }
            ordered.extend(
                windows
                    .iter()
                    .filter(|candidate| Some(candidate.handle) != foreground_handle),
            );
            Ok(ordered)
        }
    }
}

/// Parameters for the `Snapshot` tool (docs/SPEC.md §6).
///
/// Every field is optional, so `Default` is the "no arguments given" call the
/// MCP client makes when it sends `{}`.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct SnapshotParams {
    #[schemars(description = "UI tree scan scope. Defaults to foreground.")]
    pub scope: Option<SnapshotScope>,
    #[schemars(description = "Fuzzy title query for scanning one explicit window.")]
    pub window: Option<String>,
    #[schemars(
        description = "Total UIA scan deadline in milliseconds (100-30000). Defaults to 2000 for a single window, 8000 for scope=all."
    )]
    pub timeout_ms: Option<u64>,
    #[schemars(description = "Include a PNG screenshot in the response. Defaults to false.")]
    pub use_vision: Option<BoolOrString>,
    #[schemars(
        description = "Extract browser page content through UI Automation. Requires use_ui_tree=true."
    )]
    pub use_dom: Option<BoolOrString>,
    #[schemars(
        description = "Draw numbered bounding boxes for interactive elements on the screenshot (only applies when use_vision=true). Defaults to true."
    )]
    pub use_annotation: Option<BoolOrString>,
    #[schemars(
        description = "Walk the UIA accessibility tree and include interactive/scrollable elements plus the UI Tree text. Defaults to true."
    )]
    pub use_ui_tree: Option<BoolOrString>,
    #[schemars(
        description = "Number of vertical grid divisions to overlay; only takes effect when height_reference_line is also set."
    )]
    pub width_reference_line: Option<i64>,
    #[schemars(
        description = "Number of horizontal grid divisions to overlay; only takes effect when width_reference_line is also set."
    )]
    pub height_reference_line: Option<i64>,
    #[schemars(
        description = "Zero-based active display indices to restrict the capture to; omit for the full virtual desktop."
    )]
    pub display: Option<ListOrString<i32>>,
}

/// Text + optional PNG bytes making up a successful `Snapshot` response.
pub struct SnapshotOutput {
    pub text: String,
    pub png_bytes: Option<Vec<u8>>,
}

/// Internal capture result: the public [`SnapshotOutput`] plus the raw
/// element lists WaitFor polls and Snapshot writes into `state.rs`.
pub struct SnapshotResult {
    pub generation: u32,
    pub text: String,
    pub png_bytes: Option<Vec<u8>>,
    pub interactive_nodes: Vec<state::ElementNode>,
    pub scrollable_nodes: Vec<state::ElementNode>,
    pub informative_nodes: Vec<state::ElementNode>,
    pub dom_found: bool,
    pub dom_scroll_percent: f64,
    pub focused_window_title: Option<String>,
    pub window_titles: Vec<String>,
}

impl SnapshotResult {
    pub(crate) fn to_desktop_state(&self) -> state::DesktopState {
        state::DesktopState {
            generation: self.generation,
            interactive_nodes: self.interactive_nodes.clone(),
            scrollable_nodes: self.scrollable_nodes.clone(),
        }
    }
}

/// Executes the `Snapshot` tool. Any capture failure is wrapped as
/// `"Error capturing desktop state: {e}. Please try again."`.
/// On success, the accessibility-tree state is written to `state.rs` so
/// subsequent Click/Type/Scroll/Move calls can resolve `label`s.
pub fn snapshot(params: &SnapshotParams) -> Result<SnapshotOutput, String> {
    let result = capture(params)
        .map_err(|e| format!("Error capturing desktop state: {e}. Please try again."))?;
    state::set_state(result.to_desktop_state());
    Ok(SnapshotOutput {
        text: result.text,
        png_bytes: result.png_bytes,
    })
}

/// One window's UI-tree children, pre-formatted for the `UI Tree:` section.
struct WindowTree {
    name: String,
    children: Vec<String>,
}

/// Runs `uia::walk_window` with up to 3 retries and exponential backoff
/// (0.5s, 1s, 2s — docs/SPEC.md "Snapshot 性能設計"). Returns `None` if the
/// window never succeeds, matching the Python reference's
/// `failed_handles`: that window silently contributes nothing.
enum WalkWindowResult {
    Success((uia::RawElement, Vec<uia::RawElement>)),
    Failed,
    DeadlineExceeded,
}

/// How long to keep re-enumerating while the foreground window is missing
/// from the scan list, and how long to wait between attempts.
const ENUMERATION_SETTLE_TIMEOUT: Duration = Duration::from_millis(500);
const ENUMERATION_SETTLE_INTERVAL: Duration = Duration::from_millis(50);

/// Enumerates the windows Snapshot walks, waiting briefly for a just-created
/// foreground window to show up.
///
/// `GetForegroundWindow` reports a new window as soon as it is activated, but
/// `EnumWindows` and the virtual-desktop manager only include it a moment
/// later — measured at roughly 200ms. Enumerating in that gap yields a list
/// without the very window the caller is trying to scan, and a `foreground`
/// scan then fails with "No foreground window is available for UI tree
/// scanning". That gap is exactly the `App(launch)` -> `Snapshot` sequence,
/// so wait it out instead of reporting a missing window.
fn enumerate_scan_windows(foreground: Option<&window::WindowInfo>) -> Vec<window::SnapshotWindow> {
    let deadline = Instant::now() + ENUMERATION_SETTLE_TIMEOUT;
    loop {
        let windows = window::list_snapshot_windows();
        let Some(foreground) = foreground else {
            return windows;
        };
        if windows
            .iter()
            .any(|candidate| candidate.handle == foreground.handle)
            || Instant::now() >= deadline
        {
            return windows;
        }
        std::thread::sleep(ENUMERATION_SETTLE_INTERVAL);
    }
}

fn bounded_retry_delay(now: Instant, deadline: Instant, requested: Duration) -> Option<Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.min(requested))
}

fn walk_window_with_retry(
    automation: &IUIAutomation,
    cache_request: &IUIAutomationCacheRequest,
    condition: &IUIAutomationCondition,
    hwnd: HWND,
    reverse_children: bool,
    deadline: Instant,
    max_retries: u32,
) -> WalkWindowResult {
    for attempt in 0..=max_retries {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return WalkWindowResult::DeadlineExceeded;
        };
        // UIA providers run out-of-process. Bound the next provider transaction
        // to the remaining Snapshot budget instead of allowing its default
        // timeout to overrun `timeout_ms`.
        let timeout_ms = remaining.as_millis().clamp(1, u32::MAX as u128) as u32;
        let _ = uia::set_transaction_timeout(automation, timeout_ms);
        match uia::walk_window(automation, cache_request, condition, hwnd, reverse_children) {
            Ok(result) => return WalkWindowResult::Success(result),
            Err(_) if attempt < max_retries => {
                let Some(delay) = bounded_retry_delay(
                    Instant::now(),
                    deadline,
                    Duration::from_millis(500 * (1u64 << attempt)),
                ) else {
                    return WalkWindowResult::DeadlineExceeded;
                };
                std::thread::sleep(delay);
            }
            Err(_) => return WalkWindowResult::Failed,
        }
    }
    WalkWindowResult::Failed
}

fn format_tree_line(node: &state::ElementNode, action: &str) -> String {
    let actions = node
        .supported_actions
        .iter()
        .map(|action| action.name())
        .collect::<Vec<_>>()
        .join(",");
    let parent = node
        .parent_id
        .map_or_else(|| "none".to_string(), |id| id.to_string());
    format!(
        "({},{}) {} \"{}\"  [id={}, parent={}, actions={}, action: {action}]",
        node.center.0,
        node.center.1,
        node.control_type,
        node.name,
        node.element_id,
        parent,
        actions
    )
}

/// How far apart two same-named controls can sit and still be the same thing
/// drawn twice. Measured against real pages, the stacked pairs land within a
/// few pixels of each other (a list item and the link inside it differ by 4).
const STACKED_NODE_TOLERANCE: i32 = 12;

/// Removes elements that repeat a name already claimed at effectively the same
/// place.
///
/// Web content routinely nests an interactive control inside a wrapper that
/// carries the same accessible name — a `listitem` holding a `hyperlink`, a
/// button inside its own group — and both satisfy the interactive filter. The
/// model then sees two ids for one visible control and can pick the wrapper,
/// which is not always the thing that responds to a click. Keep the first
/// occurrence, which is the outermost element in document order and the one
/// whose bounds the later duplicate sits inside.
///
/// Two same-named nodes of *different* control types merge when they are
/// merely close: that is the wrapper case — a `listitem` holding a
/// `hyperlink`, a button inside its own `group`. Two of the *same* type merge
/// only when they sit at essentially the same point, because a row of
/// same-typed controls sharing a name (a toolbar of `button`s labelled "More",
/// icons 16px apart) is a set of genuinely distinct targets, while a provider
/// reporting one control twice puts both copies on the identical pixel — which
/// Files' address bar and filename field both do.
///
/// Candidates are grouped by name rather than compared against everything kept
/// so far. The scan used to be quadratic, which a DOM capture makes expensive:
/// `use_dom=true` feeds this every text node on the page.
struct KeptNode {
    center: (i32, i32),
    control_type: String,
}

/// How far apart two nodes of the same type and name can sit and still be one
/// control the provider reported twice, rather than two adjacent controls.
const EXACT_DUPLICATE_TOLERANCE: i32 = 2;

fn dedupe_stacked_nodes(nodes: &mut Vec<state::ElementNode>) {
    // Keyed by name, so a candidate is only compared against the handful of
    // nodes sharing its name instead of everything kept so far.
    let mut kept: std::collections::HashMap<String, Vec<KeptNode>> =
        std::collections::HashMap::new();
    nodes.retain(|node| {
        let name = node.name.trim();
        if name.is_empty() {
            return true;
        }
        let (x, y) = node.center;
        let entries = kept.entry(name.to_string()).or_default();
        let duplicate = entries.iter().any(|kept| {
            let tolerance = if kept.control_type == node.control_type {
                EXACT_DUPLICATE_TOLERANCE
            } else {
                STACKED_NODE_TOLERANCE
            };
            (kept.center.0 - x).abs() <= tolerance && (kept.center.1 - y).abs() <= tolerance
        });
        if duplicate {
            return false;
        }
        entries.push(KeptNode {
            center: (x, y),
            control_type: node.control_type.clone(),
        });
        true
    });
}

fn format_informative_line(node: &state::ElementNode) -> String {
    format!(
        "({},{}) {} \"{}\"",
        node.center.0, node.center.1, node.control_type, node.name
    )
}

fn raw_to_node(
    el: &uia::RawElement,
    generation: u32,
    owner_handle: isize,
    element_index: usize,
    window_element_base: usize,
) -> Option<state::ElementNode> {
    let (left, top, right, bottom) = (el.rect.left, el.rect.top, el.rect.right, el.rect.bottom);
    if right <= left || bottom <= top {
        return None;
    }
    Some(state::ElementNode {
        element_id: state::element_id(generation, window_element_base + element_index),
        parent_id: el
            .parent_index
            .map(|index| state::element_id(generation, window_element_base + index)),
        owner_handle,
        runtime_id: el.runtime_id.clone(),
        automation_id: el.automation_id.clone(),
        supported_actions: el.supported_actions.clone(),
        name: el.name.clone(),
        control_type: uia::control_type_name(el.control_type),
        center: element_center(el, (left, top, right, bottom)),
        bounding_box: (left, top, right, bottom),
        has_focus: el.has_keyboard_focus,
    })
}

/// The point to click for an element.
///
/// Prefers the provider's own clickable point, which is the only value that
/// accounts for non-rectangular and partly covered controls. It is accepted
/// only when it actually falls inside the element's bounds — some providers
/// report a stale or out-of-bounds point — and otherwise the geometric
/// center stands.
fn element_center(el: &uia::RawElement, bounds: (i32, i32, i32, i32)) -> (i32, i32) {
    let (left, top, right, bottom) = bounds;
    if let Some((x, y)) = el.clickable_point
        && x >= left
        && x < right
        && y >= top
        && y < bottom
    {
        return (x, y);
    }
    (left + (right - left) / 2, top + (bottom - top) / 2)
}

fn inside_rect(el: &uia::RawElement, bounds: &windows::Win32::Foundation::RECT) -> bool {
    el.rect.right > bounds.left
        && el.rect.left < bounds.right
        && el.rect.bottom > bounds.top
        && el.rect.top < bounds.bottom
}

fn rects_intersect(
    a: &windows::Win32::Foundation::RECT,
    b: &windows::Win32::Foundation::RECT,
) -> bool {
    a.right > b.left && a.left < b.right && a.bottom > b.top && a.top < b.bottom
}

fn clip_node_to_rect(
    mut node: state::ElementNode,
    region: Option<&windows::Win32::Foundation::RECT>,
) -> Option<state::ElementNode> {
    let Some(region) = region else {
        return Some(node);
    };
    let (left, top, right, bottom) = node.bounding_box;
    let left = left.max(region.left);
    let top = top.max(region.top);
    let right = right.min(region.right);
    let bottom = bottom.min(region.bottom);
    if right <= left || bottom <= top {
        return None;
    }
    node.bounding_box = (left, top, right, bottom);
    node.center = (left + (right - left) / 2, top + (bottom - top) / 2);
    Some(node)
}

/// Renders the `UI Tree:` box-drawing text: `desktop` root, one `window
/// "..."` child per window that contributed elements, each followed by its
/// interactive/scrollable element lines.
fn render_tree(window_trees: &[WindowTree]) -> String {
    if window_trees.is_empty() {
        return "No elements found.".to_string();
    }
    let mut lines = vec!["desktop".to_string()];
    let window_count = window_trees.len();
    for (i, tree) in window_trees.iter().enumerate() {
        let is_last_window = i == window_count - 1;
        let connector = if is_last_window {
            "\u{2514}\u{2500}\u{2500} "
        } else {
            "\u{251c}\u{2500}\u{2500} "
        };
        lines.push(format!("{connector}window \"{}\"", tree.name));
        let prefix = if is_last_window {
            "    "
        } else {
            "\u{2502}   "
        };
        let child_count = tree.children.len();
        for (j, child) in tree.children.iter().enumerate() {
            let child_connector = if j == child_count - 1 {
                "\u{2514}\u{2500}\u{2500} "
            } else {
                "\u{251c}\u{2500}\u{2500} "
            };
            lines.push(format!("{prefix}{child_connector}{child}"));
        }
    }
    lines.join("\n")
}

/// Renders a simple space-padded table (header, dashed rule, rows) —
/// structurally equivalent to the Python reference's `tabulate(tablefmt=
/// "simple")` output without pulling in a table-formatting dependency.
fn format_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(
        headers
            .iter()
            .enumerate()
            .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string(),
    );
    lines.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    for row in rows {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string(),
        );
    }
    lines.join("\n")
}

fn window_status(handle: isize) -> String {
    if window::is_minimized(handle) {
        "Minimized".to_string()
    } else if window::is_maximized(handle) {
        "Maximized".to_string()
    } else {
        "Normal".to_string()
    }
}

const WINDOW_TABLE_HEADERS: &[&str] = &["Name", "Depth", "Status", "Width", "Height", "Handle"];

fn window_table_row(window: &window::WindowInfo, depth: usize) -> Vec<String> {
    let status = window_status(window.handle);
    let (_, _, width, height) = window::get_window_rect(window.handle).unwrap_or((0, 0, 0, 0));
    vec![
        window.title.clone(),
        depth.to_string(),
        status,
        width.to_string(),
        height.to_string(),
        window.handle.to_string(),
    ]
}

fn focused_window_text(foreground: &Option<window::WindowInfo>) -> String {
    match foreground {
        None => "No active window found".to_string(),
        Some(w) => format_table(WINDOW_TABLE_HEADERS, &[window_table_row(w, 0)]),
    }
}

fn desktop_table(desktops: &[vdm::VirtualDesktop]) -> String {
    format_table(
        &["Name"],
        &desktops
            .iter()
            .map(|desktop| vec![desktop.name.clone()])
            .collect::<Vec<_>>(),
    )
}

// --- Annotated-screenshot drawing -----------------------------------------

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> image::Rgba<u8> {
    let c = value * saturation;
    let hh = (hue / 60.0) % 6.0;
    let x = c * (1.0 - (hh % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hh as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = value - c;
    image::Rgba([
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
        255,
    ])
}

/// A distinct, deterministic color per element index (golden-angle hue
/// spacing keeps adjacent indices visually distinguishable).
fn index_color(index: usize) -> image::Rgba<u8> {
    let hue = (index as f64 * 137.508) % 360.0;
    hsv_to_rgb(hue, 0.65, 0.95)
}

fn put_pixel_checked(image: &mut image::RgbaImage, x: i32, y: i32, color: image::Rgba<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < image.width() && (y as u32) < image.height() {
        image.put_pixel(x as u32, y as u32, color);
    }
}

fn draw_rect_outline(
    image: &mut image::RgbaImage,
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
    color: image::Rgba<u8>,
    thickness: i32,
) {
    for t in 0..thickness {
        for x in x1..x2 {
            put_pixel_checked(image, x, y1 + t, color);
            put_pixel_checked(image, x, y2 - 1 - t, color);
        }
        for y in y1..y2 {
            put_pixel_checked(image, x1 + t, y, color);
            put_pixel_checked(image, x2 - 1 - t, y, color);
        }
    }
}

/// 3x5 bitmap digits (no embedded font needed — SPEC allows a simple
/// bitmap/box readout for element numbers). Each row's low 3 bits are
/// pixels, MSB-first (left column = bit 2).
const DIGIT_FONT: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b001, 0b001], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

const DIGIT_SCALE: i32 = 2;

fn digit_width() -> i32 {
    3 * DIGIT_SCALE + 1
}

fn digit_height() -> i32 {
    5 * DIGIT_SCALE
}

fn draw_digit(image: &mut image::RgbaImage, x: i32, y: i32, digit: usize, color: image::Rgba<u8>) {
    for (row_idx, row) in DIGIT_FONT[digit].iter().enumerate() {
        for col in 0..3 {
            if row & (1 << (2 - col)) != 0 {
                for dy in 0..DIGIT_SCALE {
                    for dx in 0..DIGIT_SCALE {
                        put_pixel_checked(
                            image,
                            x + col * DIGIT_SCALE + dx,
                            y + row_idx as i32 * DIGIT_SCALE + dy,
                            color,
                        );
                    }
                }
            }
        }
    }
}

/// Draws a filled label rectangle with `index`'s digits in white at `(x, y)`
/// (top-left corner).
fn draw_label(image: &mut image::RgbaImage, x: i32, y: i32, index: usize, color: image::Rgba<u8>) {
    let digits: Vec<usize> = index
        .to_string()
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| d as usize)
        .collect();
    let width = digit_width() * digits.len() as i32 + 2;
    let height = digit_height() + 2;
    for dy in 0..height {
        for dx in 0..width {
            put_pixel_checked(image, x + dx, y + dy, color);
        }
    }
    let white = image::Rgba([255, 255, 255, 255]);
    for (i, digit) in digits.iter().enumerate() {
        draw_digit(
            image,
            x + 1 + i as i32 * digit_width(),
            y + 1,
            *digit,
            white,
        );
    }
}

/// Draws a numbered bounding box for each `interactive_nodes` entry
/// (scrollable nodes are not drawn — they have no visual box in the Python
/// reference either, only `label` addressability).
fn draw_annotations(
    image: &mut image::RgbaImage,
    nodes: &[state::ElementNode],
    capture_left: i32,
    capture_top: i32,
    scale: f64,
) {
    for (index, node) in nodes.iter().enumerate() {
        let (left, top, right, bottom) = node.bounding_box;
        let x1 = (((left - capture_left) as f64) * scale).round() as i32;
        let y1 = (((top - capture_top) as f64) * scale).round() as i32;
        let x2 = (((right - capture_left) as f64) * scale).round() as i32;
        let y2 = (((bottom - capture_top) as f64) * scale).round() as i32;
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        let color = index_color(index);
        draw_rect_outline(image, x1, y1, x2, y2, color, 2);
        let label_y = if y1 - digit_height() - 3 >= 0 {
            y1 - digit_height() - 3
        } else {
            y2 + 2
        };
        draw_label(image, x1, label_y, index, color);
    }
}

// --- Capture ---------------------------------------------------------------

/// Runs the full capture: window enumeration, the UIA tree walk (if
/// `use_ui_tree`), the screenshot + annotation (if `use_vision`), and
/// assembles the response text. Returns a caller-facing error message (not
/// yet wrapped with "Error capturing desktop state: ...").
pub fn capture(params: &SnapshotParams) -> Result<SnapshotResult, String> {
    capture_inner(params, true)
}

/// A capture for a caller that reads the element lists rather than the
/// rendered response — `WaitFor`, polling several times a second. Skips the
/// table, tree and desktop-listing formatting, which that caller discards.
pub fn capture_for_polling(params: &SnapshotParams) -> Result<SnapshotResult, String> {
    capture_inner(params, false)
}

fn capture_inner(params: &SnapshotParams, needs_text: bool) -> Result<SnapshotResult, String> {
    let generation = state::next_generation();
    let scan_options =
        ScanOptions::resolve(params.scope, params.window.clone(), params.timeout_ms)?;
    let use_vision = opt_bool(&params.use_vision, false)?;
    let use_dom = opt_bool(&params.use_dom, false)?;
    let use_annotation = opt_bool(&params.use_annotation, true)?;
    let use_ui_tree = opt_bool(&params.use_ui_tree, true)?;
    if use_dom && !use_ui_tree {
        return Err("use_dom=true requires use_ui_tree=true".to_string());
    }
    let display_indices: Option<Vec<usize>> = match &params.display {
        None => None,
        Some(list) => {
            let raw = list.clone().into_list()?;
            if let Some(index) = raw.iter().find(|index| **index < 0) {
                return Err(format!("Invalid display index {index}"));
            }
            Some(raw.into_iter().map(|v| v as usize).collect())
        }
    };
    let selected_rect = display_indices
        .as_ref()
        .map(|indices| display::get_display_union_rect(indices))
        .transpose()?;

    let profile = screenshot::profiling_enabled();
    let total_start = Instant::now();

    // --- Window enumeration ---
    // One `EnumWindows` pass feeds both the UIA walk and the response table:
    // the table list is a filtered view of the scan list (see
    // `window::windows_for_table`), so enumerating twice only duplicated work.
    let window_start = Instant::now();
    let foreground = window::foreground_window();
    // Waiting for a just-activated window to appear only matters for the UIA
    // walk; a screenshot-only capture takes the list as it stands.
    let walk_windows = if use_ui_tree {
        enumerate_scan_windows(foreground.as_ref())
    } else {
        window::list_snapshot_windows()
    };
    let table_windows = window::windows_for_table(&walk_windows);
    let window_ms = window_start.elapsed().as_secs_f64() * 1000.0;

    // --- UIA tree walk ---
    let uia_start = Instant::now();
    let mut interactive_nodes: Vec<state::ElementNode> = Vec::new();
    let mut scrollable_nodes: Vec<state::ElementNode> = Vec::new();
    let mut informative_nodes: Vec<state::ElementNode> = Vec::new();
    let mut dom_found = false;
    let mut dom_scroll_percent = 0.0;
    let mut window_trees: Vec<WindowTree> = Vec::new();
    let mut uia_truncated = false;
    let mut window_element_base = 0usize;
    // Windows that answered the scan with nothing because this process is not
    // allowed to see into them.
    let mut blocked_windows: Vec<String> = Vec::new();

    if use_ui_tree && !walk_windows.is_empty() {
        uia::ensure_com_initialized()?;
        let automation = uia::create_automation().map_err(|e| e.to_string())?;
        // A whole-desktop sweep is a "which window do I want" question, and
        // collecting every window's text for it nearly doubles the scan. The
        // foreground capture that follows picks the text up.
        let collect_text = scan_options.scope != SnapshotScope::All;
        let condition =
            uia::build_condition(&automation, collect_text).map_err(|e| e.to_string())?;
        let cache_request =
            uia::build_cache_request(&automation, &condition).map_err(|e| e.to_string())?;
        let dom_resources = if use_dom {
            let dom_condition = uia::build_dom_condition(&automation).map_err(|e| e.to_string())?;
            let dom_cache_request =
                uia::build_cache_request(&automation, &dom_condition).map_err(|e| e.to_string())?;
            Some((dom_condition, dom_cache_request))
        } else {
            None
        };

        let ordered = select_scan_targets(&scan_options, foreground.as_ref(), &walk_windows)?;

        let deadline = Instant::now() + scan_options.timeout;
        let max_retries = if scan_options.scope == SnapshotScope::All {
            0
        } else {
            3
        };
        for win in ordered {
            let hwnd = HWND(win.handle as *mut _);
            let browser_dom = use_dom && win.is_browser();
            let window_condition = if browser_dom {
                &dom_resources
                    .as_ref()
                    .expect("DOM resources are initialized when use_dom=true")
                    .0
            } else {
                &condition
            };
            let window_cache_request = if browser_dom {
                &dom_resources
                    .as_ref()
                    .expect("DOM resources are initialized when use_dom=true")
                    .1
            } else {
                &cache_request
            };
            let (root_raw, elements) = match walk_window_with_retry(
                &automation,
                window_cache_request,
                window_condition,
                hwnd,
                !browser_dom,
                deadline,
                max_retries,
            ) {
                WalkWindowResult::Success(result) => result,
                WalkWindowResult::Failed => continue,
                WalkWindowResult::DeadlineExceeded => {
                    uia_truncated = true;
                    break;
                }
            };

            let window_label = if !win.title.is_empty() {
                win.title.clone()
            } else if !root_raw.name.is_empty() {
                root_raw.name.clone()
            } else {
                win.class_name.clone()
            };
            if window_label.trim().contains("Overlay") {
                continue;
            }
            // An elevated window looks much like an empty one through UI
            // Automation: the provider answers, and reports its title bar and
            // nothing else. Saying which it was is the difference between a
            // caller retrying forever and one that knows to restart the
            // server elevated.
            // "Nothing usable" means no control to act on and no text to read.
            // A window showing only a message still has something to report,
            // so text counts — but the window and title-bar elements do not:
            // they are the frame, and they are exactly what an elevated
            // window exposes when its contents are hidden.
            let nothing_usable = elements.iter().all(|el| {
                let is_frame = el.control_type == uia::WINDOW_CONTROL_TYPE
                    || el.control_type == uia::TITLE_BAR_CONTROL_TYPE;
                is_frame
                    || (!uia::INTERACTIVE_CONTROL_TYPES.contains(&el.control_type)
                        && !el.is_scrollable
                        && el.name.trim().is_empty())
            });
            if nothing_usable {
                if !crate::win::is_elevated() && crate::win::runs_at_higher_integrity(win.pid) {
                    blocked_windows.push(window_label);
                }
                continue;
            }

            // A window that has put up a modal dialog disables itself, and
            // every click on it is discarded. UI Automation does not pass that
            // on — the disabled window's controls keep reporting
            // `IsEnabled = true` — so the capture would keep offering buttons
            // that do nothing. Its text is still worth reading, so this only
            // moves the controls out of the actionable list.
            let window_accepts_input = window::is_enabled(win.handle);

            let mut local_interactive: Vec<state::ElementNode> = Vec::new();
            let mut local_scrollable: Vec<state::ElementNode> = Vec::new();
            let mut local_informative: Vec<state::ElementNode> = Vec::new();

            // A provider can report bounds that extend past its own window
            // (stale layout, or a control scrolled out of view). Clipping each
            // node to the owner window keeps every center inside the window
            // that will receive the click.
            let owner_bounds = window::get_window_rect(win.handle).map(|(x, y, width, height)| {
                windows::Win32::Foundation::RECT {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                }
            });

            let element_count = elements.len();
            // Every Chromium-family renderer (Chrome, Edge, and the Electron
            // shells built on them) and Firefox expose the page as a Document
            // element. An earlier `automation_id == "RootWebArea"` probe never
            // matched: Chromium does not surface that name through UI
            // Automation, so the DOM branch found no root and dropped the
            // window's elements entirely.
            let dom_root = if use_dom && win.is_browser() {
                elements
                    .iter()
                    .enumerate()
                    .filter(|(_, el)| el.control_type == uia::DOCUMENT_CONTROL_TYPE)
                    // Chromium exposes a legacy accessibility bridge — a
                    // `Chrome_WidgetWin_1` pane holding an empty Document that
                    // reports full-window bounds. Electron shells surface that
                    // bridge alongside the real page, and taking the first
                    // Document found the empty one, whose bounds then filtered
                    // every actual element away. Choose the Document that
                    // actually contains elements.
                    .max_by_key(|(index, el)| {
                        elements
                            .iter()
                            .skip(*index)
                            .filter(|child| inside_rect(child, &el.rect))
                            .count()
                    })
                    .map(|(index, _)| index)
            } else {
                None
            };
            // A Document that contributes nothing must not blank the window:
            // fall back to the plain element set rather than reporting an
            // empty tree.
            let dom_root = dom_root.filter(|index| {
                let bounds = elements[*index].rect;
                elements
                    .iter()
                    .skip(*index)
                    .any(|el| inside_rect(el, &bounds) && !el.is_offscreen)
            });
            let dom_bounds = dom_root.map(|index| elements[index].rect);
            if dom_root.is_some()
                && dom_bounds.as_ref().is_some_and(|bounds| {
                    selected_rect
                        .as_ref()
                        .is_none_or(|region| rects_intersect(bounds, region))
                })
            {
                dom_found = true;
                if let Some(index) = dom_root {
                    dom_scroll_percent = elements[index].vertical_scroll_percent;
                }
            }

            for (element_index, el) in elements.into_iter().enumerate() {
                if let (Some(root_index), Some(bounds)) = (dom_root, dom_bounds) {
                    if element_index < root_index || !inside_rect(&el, &bounds) {
                        continue;
                    }
                } else if use_dom && win.is_browser() {
                    continue;
                }
                if el.control_type == uia::WINDOW_CONTROL_TYPE {
                    // Nested modal dialog: discard everything accumulated
                    // for this window so far (docs/SPEC.md §6 item 4).
                    if el.is_modal {
                        local_interactive.clear();
                        local_scrollable.clear();
                        local_informative.clear();
                    }
                    continue;
                }
                let Some(node) = raw_to_node(
                    &el,
                    generation,
                    win.handle,
                    element_index,
                    window_element_base,
                )
                .and_then(|node| clip_node_to_rect(node, selected_rect.as_ref()))
                .and_then(|node| clip_node_to_rect(node, owner_bounds.as_ref())) else {
                    continue;
                };
                let is_interactive_type = uia::INTERACTIVE_CONTROL_TYPES.contains(&el.control_type);
                if is_interactive_type && el.is_enabled && window_accepts_input && !el.is_offscreen
                {
                    local_interactive.push(node);
                } else if el.is_scrollable && !el.is_offscreen {
                    local_scrollable.push(node);
                } else if (collect_text || dom_root.is_some())
                    && !el.is_offscreen
                    && !el.name.trim().is_empty()
                {
                    // Text the window is showing: a calculator's result, a
                    // dialog's message, a status line. This used to be
                    // gated on `dom_root.is_some()`, so it only ever
                    // applied to browser pages — every ordinary desktop app
                    // reported its buttons and none of its readable state.
                    local_informative.push(node);
                }
            }

            if use_dom
                && win.is_firefox()
                && dom_root.is_none()
                && let Ok(ia2_result) = ia2::walk_firefox(hwnd)
                && ia2_result.dom_bounds.as_ref().is_some_and(|bounds| {
                    selected_rect
                        .as_ref()
                        .is_none_or(|region| rects_intersect(bounds, region))
                })
            {
                dom_found = true;
                for element in ia2_result.elements {
                    if element.offscreen
                        || element.rect.right <= element.rect.left
                        || element.rect.bottom <= element.rect.top
                    {
                        continue;
                    }
                    let node = state::ElementNode {
                        element_id: 0,
                        parent_id: None,
                        owner_handle: win.handle,
                        runtime_id: Vec::new(),
                        automation_id: String::new(),
                        supported_actions: Vec::new(),
                        name: element.name,
                        control_type: element.control_type,
                        center: (
                            element.rect.left + (element.rect.right - element.rect.left) / 2,
                            element.rect.top + (element.rect.bottom - element.rect.top) / 2,
                        ),
                        bounding_box: (
                            element.rect.left,
                            element.rect.top,
                            element.rect.right,
                            element.rect.bottom,
                        ),
                        has_focus: element.focused,
                    };
                    let Some(node) = clip_node_to_rect(node, selected_rect.as_ref()) else {
                        continue;
                    };
                    if element.interactive && element.enabled {
                        local_interactive.push(node);
                    } else {
                        local_informative.push(node);
                    }
                }
            }

            dedupe_stacked_nodes(&mut local_interactive);
            local_informative.retain(|node| !node.name.trim().is_empty());
            dedupe_stacked_nodes(&mut local_informative);
            // Scrollable regions nest the same way interactive controls do —
            // Chromium stacks a legacy-bridge pane and its document at one
            // point — and an unnamed scroll region gives the model nothing to
            // aim at that its parent does not already offer.
            local_scrollable.retain(|node| !node.name.trim().is_empty());
            dedupe_stacked_nodes(&mut local_scrollable);

            if !local_interactive.is_empty()
                || !local_scrollable.is_empty()
                || !local_informative.is_empty()
            {
                let mut children = Vec::with_capacity(
                    local_interactive.len() + local_scrollable.len() + local_informative.len(),
                );
                children.extend(
                    local_interactive
                        .iter()
                        .map(|n| format_tree_line(n, "click")),
                );
                children.extend(local_informative.iter().map(format_informative_line));
                children.extend(
                    local_scrollable
                        .iter()
                        .map(|n| format_tree_line(n, "scroll")),
                );
                window_trees.push(WindowTree {
                    name: window_label,
                    children,
                });
            }

            interactive_nodes.extend(local_interactive);
            scrollable_nodes.extend(local_scrollable);
            informative_nodes.extend(local_informative);
            window_element_base += element_count;
        }
    }
    let uia_ms = uia_start.elapsed().as_secs_f64() * 1000.0;

    // --- Screenshot + annotation (only when use_vision) ---
    let image_start = Instant::now();
    let mut png_bytes: Option<Vec<u8>> = None;
    let mut screenshot_size_line: Option<String> = None;
    let mut backend_name: Option<&'static str> = None;
    let mut region_text: Option<(String, String)> = None;

    if use_vision {
        let (capture_rect, region) = match &display_indices {
            None => (capture::virtual_screen_rect(), None),
            Some(indices) => {
                let rect = selected_rect.expect("selected display rectangle validated above");
                let csv = indices
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let region_str = format!(
                    "({},{},{},{})",
                    rect.left, rect.top, rect.right, rect.bottom
                );
                (rect, Some((csv, region_str)))
            }
        };
        region_text = region;

        let backend = capture::resolve_backend();
        let (captured, backend) = capture::capture_rect_with_backend(capture_rect, backend)?;
        backend_name = Some(backend.name());

        let orig_width = captured.width();
        let orig_height = captured.height();
        let user_scale = screenshot::resolve_scale();
        let scale = screenshot::combined_scale(orig_width, orig_height, user_scale);

        let mut image = if scale != 1.0 {
            let (w, h) = screenshot::scaled_size(orig_width, orig_height, scale);
            image::imageops::resize(&captured, w.max(1), h.max(1), FilterType::Lanczos3)
        } else {
            captured
        };

        if use_annotation {
            draw_annotations(
                &mut image,
                &interactive_nodes,
                capture_rect.left,
                capture_rect.top,
                scale,
            );
        }

        if let (Some(w), Some(h)) = (params.width_reference_line, params.height_reference_line) {
            screenshot::draw_grid_lines(&mut image, w, h);
        }

        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("PNG encoding failed: {e}"))?;
        png_bytes = Some(bytes);

        screenshot_size_line = Some(if scale < 1.0 {
            let coord_scale = (1.0 / scale * 1_000_000.0).round() / 1_000_000.0;
            format!(
                "Screenshot Original Size: ({orig_width},{orig_height})\n{}\n{}",
                screenshot::coordinate_scale_text(coord_scale),
                screenshot::coordinate_transform_text(
                    coord_scale,
                    capture_rect.left,
                    capture_rect.top
                )
            )
        } else {
            let size = format!("Screenshot Size: ({},{})", image.width(), image.height());
            if capture_rect.left != 0 || capture_rect.top != 0 {
                format!(
                    "{size}\n{}",
                    screenshot::coordinate_transform_text(1.0, capture_rect.left, capture_rect.top)
                )
            } else {
                size
            }
        });
    }
    let image_ms = image_start.elapsed().as_secs_f64() * 1000.0;

    // --- Response text assembly ---
    // `WaitFor` polls this several times a second and reads only the node
    // lists and window titles, never the rendered text. Building the tables,
    // the UI tree and the virtual-desktop listing for it re-did that work on
    // every poll for output nobody read.
    if !needs_text {
        return Ok(SnapshotResult {
            generation,
            text: String::new(),
            png_bytes,
            interactive_nodes,
            scrollable_nodes,
            informative_nodes,
            dom_found,
            dom_scroll_percent,
            focused_window_title: foreground.as_ref().map(|w| w.title.clone()),
            window_titles: table_windows
                .iter()
                .map(|w| w.title.clone())
                .chain(foreground.as_ref().map(|w| w.title.clone()))
                .collect(),
        });
    }

    let (cx, cy) = screenshot::cursor_position();
    let mut text = format!("Cursor Position: ({cx}, {cy})\n");

    if let Some(line) = &screenshot_size_line {
        text += line;
        text += "\n";
    }

    let displays = display::get_displays();
    if !displays.is_empty() {
        text += &format!(
            "Visible Displays: {}\n",
            screenshot::display_list_text(&displays)
        );
    }

    if let Some((csv, region)) = &region_text {
        text += &format!("Selected Displays: {csv}\n");
        text += &format!("Screenshot Region: {region}\n");
        text += "Coordinate Space: Virtual desktop coordinates\n";
    }
    if let Some(name) = backend_name {
        text += &format!("Screenshot Backend: {name}\n");
    }

    let desktops = vdm::desktops();
    let active_desktops: Vec<_> = desktops
        .iter()
        .filter(|desktop| desktop.active)
        .cloned()
        .collect();
    text += "\nActive Desktop:\n";
    text += &desktop_table(&active_desktops);
    text += "\n\nAll Desktops:\n";
    text += &desktop_table(&desktops);

    let focused_window_title = foreground.as_ref().map(|w| w.title.clone());
    text += "\n\nFocused Window:\n";
    text += &focused_window_text(&foreground);
    text += "\n\nOpened Windows:\n";

    let mut window_titles = Vec::new();
    let opened_rows: Vec<Vec<String>> = table_windows
        .iter()
        .filter(|w| foreground.as_ref().map(|f| f.handle) != Some(w.handle))
        .filter(|w| {
            selected_rect.as_ref().is_none_or(|region| {
                window::get_window_rect(w.handle).is_some_and(|(x, y, width, height)| {
                    rects_intersect(
                        &windows::Win32::Foundation::RECT {
                            left: x,
                            top: y,
                            right: x + width,
                            bottom: y + height,
                        },
                        region,
                    )
                })
            })
        })
        .enumerate()
        .map(|(i, w)| {
            window_titles.push(w.title.clone());
            window_table_row(w, i)
        })
        .collect();
    if opened_rows.is_empty() {
        text += "No windows found";
    } else {
        text += &format_table(WINDOW_TABLE_HEADERS, &opened_rows);
    }
    if let Some(title) = &focused_window_title {
        window_titles.push(title.clone());
    }

    if use_ui_tree {
        text += "\n\nUI Tree:\n";
        text += &render_tree(&window_trees);
        if uia_truncated {
            text += &format!(
                "\nUI Tree Scan: truncated at timeout_ms={}",
                scan_options.timeout.as_millis()
            );
        }
        if !blocked_windows.is_empty() {
            blocked_windows.sort();
            blocked_windows.dedup();
            text += &format!(
                "\nElevated windows (not readable from this session): {}. \
                 Their contents are hidden by Windows UI privilege isolation; \
                 restart the server elevated to inspect them.",
                blocked_windows.join(", ")
            );
        }
    } else {
        text += "\n\nUI Tree: Skipped for fast screenshot-only capture. Call Snapshot when you need interactive or scrollable elements.\n";
    }

    if profile {
        eprintln!(
            "Snapshot tool profile: window_enum_ms={window_ms:.1} uia_scan_ms={uia_ms:.1} image_ms={image_ms:.1} total_ms={:.1}",
            total_start.elapsed().as_secs_f64() * 1000.0
        );
    }

    Ok(SnapshotResult {
        generation,
        text,
        png_bytes,
        interactive_nodes,
        scrollable_nodes,
        informative_nodes,
        dom_found,
        dom_scroll_percent,
        focused_window_title,
        window_titles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_window(handle: isize, title: &str) -> window::SnapshotWindow {
        window::SnapshotWindow::new(
            handle,
            title.to_string(),
            "TestWindow".to_string(),
            handle as u32,
        )
    }

    fn named_node(name: &str, control_type: &str, center: (i32, i32)) -> state::ElementNode {
        state::ElementNode {
            element_id: 0,
            parent_id: None,
            owner_handle: 0,
            runtime_id: Vec::new(),
            automation_id: String::new(),
            supported_actions: Vec::new(),
            name: name.to_string(),
            control_type: control_type.to_string(),
            center,
            bounding_box: (center.0, center.1, center.0 + 1, center.1 + 1),
            has_focus: false,
        }
    }

    #[test]
    fn a_control_wrapped_in_a_same_named_container_is_listed_once() {
        // Measured on a real page: the list item and the link inside it carry
        // the same name four pixels apart, and both passed the interactive
        // filter.
        let mut nodes = vec![
            named_node("Code", "listitem", (127, 152)),
            named_node("Code", "hyperlink", (127, 148)),
            named_node("Issues", "listitem", (210, 152)),
            named_node("Issues", "hyperlink", (210, 148)),
        ];
        dedupe_stacked_nodes(&mut nodes);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].control_type, "listitem");
        assert_eq!(nodes[1].name, "Issues");
    }

    #[test]
    fn the_same_name_far_away_is_a_different_control() {
        // "Code" also appears as a button elsewhere on the page; distance is
        // what separates a repeated label from a stacked duplicate.
        let mut nodes = vec![
            named_node("Code", "listitem", (127, 152)),
            named_node("Code", "button", (1230, 398)),
        ];
        dedupe_stacked_nodes(&mut nodes);
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn one_control_reported_twice_at_the_same_point_is_listed_once() {
        // Measured in Files: the address bar and the filename field each come
        // back as two `edit` nodes with identical names and identical centres.
        // Listing both gives the caller two ids for one control.
        let mut nodes = vec![
            named_node("移動先のパスを入力…", "edit", (1018, 67)),
            named_node("移動先のパスを入力…", "edit", (1018, 67)),
        ];
        dedupe_stacked_nodes(&mut nodes);
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn adjacent_same_typed_controls_sharing_a_name_are_all_kept() {
        // A toolbar packs same-named icon buttons closer together than the
        // stacking tolerance. They are separate targets, not one control the
        // provider reported twice — only a wrapper/child pair is that, and a
        // wrapper is always a different control type than what it wraps.
        let mut nodes = vec![
            named_node("More", "button", (100, 40)),
            named_node("More", "button", (118, 40)),
            named_node("More", "button", (136, 40)),
        ];
        dedupe_stacked_nodes(&mut nodes);
        assert_eq!(nodes.len(), 3, "same-typed neighbours are distinct controls");
    }

    #[test]
    fn unnamed_nodes_are_never_merged_together() {
        let mut nodes = vec![
            named_node("", "group", (100, 100)),
            named_node("", "group", (101, 101)),
        ];
        dedupe_stacked_nodes(&mut nodes);
        assert_eq!(nodes.len(), 2, "empty names carry no identity to match on");
    }

    #[test]
    fn a_prefix_of_a_long_title_selects_that_window() {
        // Real case: a 30-character verbatim prefix of a 64-character browser
        // title scored ~47 under whole-string matching and was reported as
        // "Window not found" even though the window was open.
        let windows = vec![
            snapshot_window(1, "ntaksh42/Windows-Operation-Cli および他 6 ページ - 個人 - Microsoft Edge"),
            snapshot_window(2, "Microsoft PC Manager"),
        ];
        let options = ScanOptions::resolve(
            None,
            Some("ntaksh42/Windows-Operation-Cli".to_string()),
            None,
        )
        .unwrap();
        let selected = select_scan_targets(&options, None, &windows).unwrap();
        assert_eq!(selected.first().map(|w| w.handle), Some(1));
    }

    #[test]
    fn equal_partial_matches_prefer_the_title_the_query_covers_most() {
        // Both titles contain the query, so both score 100 on partial match.
        // The shorter title is the better answer, not an ambiguity error.
        let windows = vec![
            snapshot_window(1, "Inbox - Gmail - Mozilla Firefox - Personal - Work Profile"),
            snapshot_window(2, "Gmail"),
        ];
        let options =
            ScanOptions::resolve(None, Some("Gmail".to_string()), None).unwrap();
        let selected = select_scan_targets(&options, None, &windows).unwrap();
        assert_eq!(selected.first().map(|w| w.handle), Some(2));
    }

    #[test]
    fn a_title_that_is_only_a_fragment_of_the_query_does_not_win() {
        // Partial matching slides the shorter string across the longer one, so
        // an unguarded partial score rates "Claude" a perfect match for the
        // query "Claude Settings" and the shorter title would take it.
        let windows = vec![
            snapshot_window(1, "Claude"),
            snapshot_window(2, "Claude Settings"),
        ];
        let options =
            ScanOptions::resolve(None, Some("Claude Settings".to_string()), None).unwrap();
        let selected = select_scan_targets(&options, None, &windows).unwrap();
        assert_eq!(selected.first().map(|w| w.handle), Some(2));
    }

    #[test]
    fn identical_titles_are_still_reported_as_ambiguous() {
        let windows = vec![
            snapshot_window(1, "Settings"),
            snapshot_window(2, "Settings"),
        ];
        let options =
            ScanOptions::resolve(None, Some("Settings".to_string()), None).unwrap();
        let error = select_scan_targets(&options, None, &windows).unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn scan_options_default_to_foreground_and_two_seconds() {
        let options = ScanOptions::resolve(None, None, None).unwrap();
        assert_eq!(options.scope, SnapshotScope::Foreground);
        assert_eq!(options.timeout, Duration::from_millis(2_000));
    }

    #[test]
    fn scan_options_reject_all_scope_with_window_query() {
        let error =
            ScanOptions::resolve(Some(SnapshotScope::All), Some("Claude".to_string()), None)
                .unwrap_err();
        assert_eq!(error, "window cannot be combined with scope=all");
    }

    #[test]
    fn scan_options_reject_timeout_outside_range() {
        assert!(ScanOptions::resolve(None, None, Some(99)).is_err());
        assert!(ScanOptions::resolve(None, None, Some(30_001)).is_err());
    }

    #[test]
    fn a_whole_desktop_sweep_gets_a_larger_default_budget() {
        // One window finishes well inside 2s; every window does not. Measured
        // at ~1.8s of work, which the foreground default clipped almost every
        // time.
        let foreground = ScanOptions::resolve(None, None, None).unwrap();
        let sweep = ScanOptions::resolve(Some(SnapshotScope::All), None, None).unwrap();
        assert_eq!(foreground.timeout.as_millis(), 2_000);
        assert!(
            sweep.timeout > foreground.timeout,
            "scope=all needs more than the foreground budget, got {:?}",
            sweep.timeout
        );
    }

    #[test]
    fn an_explicit_timeout_still_wins_for_either_scope() {
        for scope in [None, Some(SnapshotScope::All)] {
            let options = ScanOptions::resolve(scope, None, Some(500)).unwrap();
            assert_eq!(options.timeout.as_millis(), 500);
        }
    }

    #[test]
    fn retry_delay_never_exceeds_the_global_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        assert_eq!(
            bounded_retry_delay(now, deadline, Duration::from_millis(500)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            bounded_retry_delay(deadline, deadline, Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn foreground_scope_selects_only_the_focused_window() {
        let windows = vec![snapshot_window(1, "Claude"), snapshot_window(2, "Edge")];
        let foreground = window::WindowInfo {
            handle: 1,
            title: "Claude".to_string(),
            pid: 1,
        };
        let selected = select_scan_targets(
            &ScanOptions::resolve(None, None, None).unwrap(),
            Some(&foreground),
            &windows,
        )
        .unwrap();
        assert_eq!(
            selected.iter().map(|w| w.handle).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn all_scope_keeps_foreground_first() {
        let windows = vec![snapshot_window(2, "Edge"), snapshot_window(1, "Claude")];
        let foreground = window::WindowInfo {
            handle: 1,
            title: "Claude".to_string(),
            pid: 1,
        };
        let options = ScanOptions::resolve(Some(SnapshotScope::All), None, None).unwrap();
        let selected = select_scan_targets(&options, Some(&foreground), &windows).unwrap();
        assert_eq!(
            selected.iter().map(|w| w.handle).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn window_query_selects_the_best_title_match() {
        let windows = vec![
            snapshot_window(1, "Claude"),
            snapshot_window(2, "Claude Settings"),
            snapshot_window(3, "Edge"),
        ];
        let options =
            ScanOptions::resolve(None, Some("Claude Settings".to_string()), None).unwrap();
        let selected = select_scan_targets(&options, None, &windows).unwrap();
        assert_eq!(
            selected.iter().map(|w| w.handle).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn explicit_window_includes_related_popup_from_the_same_process() {
        let windows = vec![
            snapshot_window(1, "Claude"),
            window::SnapshotWindow::new(2, String::new(), "Popup".to_string(), 1),
            snapshot_window(3, "Edge"),
        ];
        let options = ScanOptions::resolve(None, Some("Claude".to_string()), None).unwrap();
        let selected = select_scan_targets(&options, None, &windows).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|window| window.handle)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn table_renders_header_rule_and_rows() {
        let table = format_table(
            &["Name", "Depth"],
            &[vec!["Notepad".to_string(), "0".to_string()]],
        );
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("Name"));
        assert!(lines[1].starts_with("----"));
        assert!(lines[2].starts_with("Notepad"));
    }

    #[test]
    fn tree_renders_desktop_root_and_indentation() {
        let trees = vec![WindowTree {
            name: "Notepad".to_string(),
            children: vec!["(1,2) button \"OK\"  [action: click]".to_string()],
        }];
        let rendered = render_tree(&trees);
        assert!(rendered.starts_with("desktop\n"));
        assert!(rendered.contains("window \"Notepad\""));
        assert!(rendered.contains("(1,2) button \"OK\"  [action: click]"));
    }

    #[test]
    fn tree_falls_back_when_empty() {
        assert_eq!(render_tree(&[]), "No elements found.");
    }

    #[test]
    fn format_tree_line_uses_action_verb() {
        let node = state::ElementNode {
            element_id: state::element_id(7, 3),
            parent_id: Some(state::element_id(7, 1)),
            owner_handle: 0,
            runtime_id: Vec::new(),
            automation_id: String::new(),
            supported_actions: vec![state::SupportedAction::Invoke],
            name: "Submit".to_string(),
            control_type: "button".to_string(),
            center: (10, 20),
            bounding_box: (0, 0, 20, 40),
            has_focus: false,
        };
        assert_eq!(
            format_tree_line(&node, "click"),
            format!(
                "(10,20) button \"Submit\"  [id={}, parent={}, actions=invoke, action: click]",
                state::element_id(7, 3),
                state::element_id(7, 1)
            )
        );
    }

    fn raw_element(rect: (i32, i32, i32, i32), clickable: Option<(i32, i32)>) -> uia::RawElement {
        uia::RawElement {
            parent_index: None,
            runtime_id: Vec::new(),
            supported_actions: Vec::new(),
            control_type: 0,
            name: "n".to_string(),
            automation_id: String::new(),
            rect: windows::Win32::Foundation::RECT {
                left: rect.0,
                top: rect.1,
                right: rect.2,
                bottom: rect.3,
            },
            is_enabled: true,
            is_offscreen: false,
            has_keyboard_focus: false,
            is_modal: false,
            is_scrollable: false,
            vertical_scroll_percent: 0.0,
            clickable_point: clickable,
        }
    }

    #[test]
    fn clickable_point_is_preferred_over_the_geometric_center() {
        let bounds = (0, 0, 100, 40);
        // No provider point: the geometric center stands.
        assert_eq!(
            element_center(&raw_element(bounds, None), bounds),
            (50, 20)
        );
        // A provider point inside the element wins — this is the case where
        // the center lands on a child or on padding.
        assert_eq!(
            element_center(&raw_element(bounds, Some((12, 30))), bounds),
            (12, 30)
        );
    }

    #[test]
    fn an_out_of_bounds_clickable_point_falls_back_to_the_center() {
        let bounds = (0, 0, 100, 40);
        for stale in [(200, 20), (-5, 20), (50, 400)] {
            assert_eq!(
                element_center(&raw_element(bounds, Some(stale)), bounds),
                (50, 20),
                "stale point {stale:?} must not be trusted"
            );
        }
    }

    #[test]
    fn display_filter_clips_nodes_and_recomputes_center() {
        let node = state::ElementNode {
            element_id: 0,
            parent_id: None,
            owner_handle: 0,
            runtime_id: Vec::new(),
            automation_id: String::new(),
            supported_actions: Vec::new(),
            name: "partly visible".to_string(),
            control_type: "button".to_string(),
            center: (100, 100),
            bounding_box: (0, 0, 200, 200),
            has_focus: false,
        };
        let region = windows::Win32::Foundation::RECT {
            left: 100,
            top: 50,
            right: 150,
            bottom: 120,
        };
        let clipped = clip_node_to_rect(node, Some(&region)).unwrap();
        assert_eq!(clipped.bounding_box, (100, 50, 150, 120));
        assert_eq!(clipped.center, (125, 85));
    }
}
