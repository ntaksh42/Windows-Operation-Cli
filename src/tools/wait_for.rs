//! `WaitFor` tool: polls a Snapshot-equivalent capture (no vision) until a
//! condition is satisfied or `timeout` elapses (docs/SPEC.md §14).

use std::time::{Duration, Instant};

use rmcp::schemars;
use serde::Deserialize;

use crate::params::{BoolOrString, opt_bool};
use crate::tools::snapshot::{self, SnapshotParams};

const MIN_TIMEOUT: f64 = 0.0;
const MAX_TIMEOUT: f64 = 120.0;
const DEFAULT_TIMEOUT: f64 = 10.0;
const MIN_INTERVAL: f64 = 0.0;
const MAX_INTERVAL: f64 = 5.0;
const DEFAULT_INTERVAL: f64 = 0.25;

/// Parameters for the `WaitFor` tool (docs/SPEC.md §14).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForParams {
    #[schemars(
        description = "Condition to wait for: text_exists, active_window, element_exists, element_enabled, or focused_element (aliases: text, window, element, enabled, focused)."
    )]
    pub condition: String,
    #[schemars(
        description = "Text to match (casefold substring). Required for text_exists/element_exists/element_enabled; optional for active_window/focused_element."
    )]
    pub text: Option<String>,
    #[schemars(description = "Window title to match for the active_window condition.")]
    pub window_name: Option<String>,
    #[schemars(description = "Maximum seconds to wait (0 < timeout <= 120). Defaults to 10.")]
    pub timeout: Option<f64>,
    #[schemars(
        description = "Seconds between polling attempts (0 < interval <= 5). Defaults to 0.25."
    )]
    pub interval: Option<f64>,
    #[schemars(description = "Use browser DOM content while polling text and element conditions.")]
    pub use_dom: Option<BoolOrString>,
}

/// The normalized condition kind, after alias resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Condition {
    TextExists,
    ActiveWindow,
    ElementExists,
    ElementEnabled,
    FocusedElement,
}

/// Normalizes `raw` (lowercased, `-` -> `_`, aliases resolved) into a
/// [`Condition`]. Unknown conditions are a caller-facing error.
fn normalize_condition(raw: &str) -> Result<Condition, String> {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "text_exists" | "text" => Ok(Condition::TextExists),
        "active_window" | "window" => Ok(Condition::ActiveWindow),
        "element_exists" | "element" => Ok(Condition::ElementExists),
        "element_enabled" | "enabled" => Ok(Condition::ElementEnabled),
        "focused_element" | "focused" => Ok(Condition::FocusedElement),
        other => Err(format!(
            "Unknown condition '{other}'. Expected one of: text_exists, active_window, element_exists, element_enabled, focused_element."
        )),
    }
}

fn casefold_contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Evaluates `condition` against one poll of `snapshot`. Returns `Some(detail)`
/// on success (the text to report in the success message).
fn evaluate_condition(
    condition: Condition,
    params: &WaitForParams,
    result: &snapshot::SnapshotResult,
) -> Option<String> {
    match condition {
        Condition::TextExists => {
            let needle = params.text.as_deref().unwrap_or("");
            if let Some(title) = result.focused_window_title.as_deref()
                && casefold_contains(title, needle)
            {
                return Some(format!("text '{needle}' found in focused window '{title}'"));
            }
            for title in &result.window_titles {
                if casefold_contains(title, needle) {
                    return Some(format!("text '{needle}' found in window '{title}'"));
                }
            }
            for node in result
                .interactive_nodes
                .iter()
                .chain(result.scrollable_nodes.iter())
                .chain(result.informative_nodes.iter())
            {
                if casefold_contains(&node.name, needle) {
                    return Some(format!("text '{needle}' found in element '{}'", node.name));
                }
            }
            None
        }
        Condition::ActiveWindow => {
            let needle = params
                .window_name
                .as_deref()
                .or(params.text.as_deref())
                .unwrap_or("");
            let title = result.focused_window_title.as_deref()?;
            if casefold_contains(title, needle) {
                Some(format!("active window '{title}' matches '{needle}'"))
            } else {
                None
            }
        }
        Condition::ElementExists => {
            let needle = params.text.as_deref().unwrap_or("");
            result
                .interactive_nodes
                .iter()
                .chain(result.scrollable_nodes.iter())
                .find(|n| casefold_contains(&n.name, needle))
                .map(|n| format!("element '{}' found", n.name))
        }
        Condition::ElementEnabled => {
            // interactive_nodes only ever contains IsEnabled elements
            // (docs/SPEC.md §6 item 4), so membership implies "enabled".
            let needle = params.text.as_deref().unwrap_or("");
            result
                .interactive_nodes
                .iter()
                .find(|n| casefold_contains(&n.name, needle))
                .map(|n| format!("enabled element '{}' found", n.name))
        }
        Condition::FocusedElement => {
            let needle = params.text.as_deref();
            result
                .interactive_nodes
                .iter()
                .chain(result.scrollable_nodes.iter())
                .find(|n| n.has_focus && needle.is_none_or(|t| casefold_contains(&n.name, t)))
                .map(|n| format!("focused element '{}' found", n.name))
        }
    }
}

fn timeout_hint(condition: Condition, params: &WaitForParams) -> String {
    match condition {
        Condition::TextExists => {
            format!("text '{}' not found", params.text.as_deref().unwrap_or(""))
        }
        Condition::ActiveWindow => {
            let needle = params
                .window_name
                .as_deref()
                .or(params.text.as_deref())
                .unwrap_or("");
            format!("active window matching '{needle}' not found")
        }
        Condition::ElementExists => {
            format!(
                "element '{}' not found",
                params.text.as_deref().unwrap_or("")
            )
        }
        Condition::ElementEnabled => {
            format!(
                "enabled element '{}' not found",
                params.text.as_deref().unwrap_or("")
            )
        }
        Condition::FocusedElement => match params.text.as_deref() {
            Some(text) => format!("focused element matching '{text}' not found"),
            None => "no focused element found".to_string(),
        },
    }
}

fn active_window_match(params: &WaitForParams, title: &str) -> Option<String> {
    let needle = params
        .window_name
        .as_deref()
        .or(params.text.as_deref())
        .unwrap_or("");
    casefold_contains(title, needle).then(|| format!("active window '{title}' matches '{needle}'"))
}

fn snapshot_timeout_ms(remaining: Duration) -> Option<u64> {
    (remaining >= Duration::from_millis(100))
        .then(|| remaining.as_millis().clamp(100, 30_000) as u64)
}
fn wait_for_active_window(
    params: &WaitForParams,
    timeout: f64,
    interval: f64,
) -> Result<String, String> {
    let started = Instant::now();
    let timeout_duration = Duration::from_secs_f64(timeout);
    let interval_duration = Duration::from_secs_f64(interval);
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        if let Some(window) = crate::window::foreground_window()
            && let Some(detail) = active_window_match(params, &window.title)
        {
            let elapsed = started.elapsed().as_secs_f64();
            return Ok(format!(
                "WaitFor condition '{}' satisfied after {elapsed:.2}s and {attempt} attempt(s): {detail}.",
                params.condition
            ));
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout_duration {
            return Err(format!(
                "Timed out after {:.2}s waiting for '{}': {}.",
                elapsed.as_secs_f64(),
                params.condition,
                timeout_hint(Condition::ActiveWindow, params)
            ));
        }
        std::thread::sleep(interval_duration.min(timeout_duration - elapsed));
    }
}

/// Executes the `WaitFor` tool: polls a Snapshot-equivalent capture (no
/// vision, no annotation) every `interval` seconds until `condition` is
/// satisfied or `timeout` elapses.
pub fn wait_for(params: WaitForParams) -> Result<String, String> {
    let use_dom = opt_bool(&params.use_dom, false)?;

    let condition = normalize_condition(&params.condition)?;

    let timeout = params.timeout.unwrap_or(DEFAULT_TIMEOUT);
    if !(timeout > MIN_TIMEOUT && timeout <= MAX_TIMEOUT) {
        return Err(format!(
            "timeout must be between {MIN_TIMEOUT} (exclusive) and {MAX_TIMEOUT} seconds, got {timeout}"
        ));
    }
    let interval = params.interval.unwrap_or(DEFAULT_INTERVAL);
    if !(interval > MIN_INTERVAL && interval <= MAX_INTERVAL) {
        return Err(format!(
            "interval must be between {MIN_INTERVAL} (exclusive) and {MAX_INTERVAL} seconds, got {interval}"
        ));
    }

    let text_required = matches!(
        condition,
        Condition::TextExists | Condition::ElementExists | Condition::ElementEnabled
    );
    if text_required && params.text.as_deref().unwrap_or("").is_empty() {
        return Err(format!(
            "text is required for condition '{}'",
            params.condition
        ));
    }
    if condition == Condition::ActiveWindow
        && params.window_name.as_deref().unwrap_or("").is_empty()
        && params.text.as_deref().unwrap_or("").is_empty()
    {
        return Err("window_name or text is required for condition 'active_window'".to_string());
    }
    if condition == Condition::ActiveWindow {
        return wait_for_active_window(&params, timeout, interval);
    }

    let mut snapshot_params = SnapshotParams {
        scope: None,
        window: None,
        timeout_ms: Some(100),
        use_vision: Some(BoolOrString::Bool(false)),
        use_dom: Some(BoolOrString::Bool(use_dom)),
        use_annotation: Some(BoolOrString::Bool(false)),
        use_ui_tree: Some(BoolOrString::Bool(true)),
        width_reference_line: None,
        height_reference_line: None,
        display: None,
    };

    let started = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let remaining = Duration::from_secs_f64(timeout).saturating_sub(started.elapsed());
        let Some(snapshot_timeout_ms) = snapshot_timeout_ms(remaining) else {
            let elapsed = started.elapsed().as_secs_f64();
            return Err(format!(
                "Timed out after {elapsed:.2}s waiting for '{}': {}.",
                params.condition,
                timeout_hint(condition, &params)
            ));
        };
        snapshot_params.timeout_ms = Some(snapshot_timeout_ms);
        if let Ok(result) = snapshot::capture(&snapshot_params) {
            let matched = evaluate_condition(condition, &params, &result);
            if let Some(detail) = matched {
                let elapsed = started.elapsed().as_secs_f64();
                return Ok(format!(
                    "WaitFor condition '{}' satisfied after {elapsed:.2}s and {attempt} attempt(s): {detail}.",
                    params.condition
                ));
            }
        }

        if started.elapsed().as_secs_f64() >= timeout {
            let elapsed = started.elapsed().as_secs_f64();
            return Err(format!(
                "Timed out after {elapsed:.2}s waiting for '{}': {}.",
                params.condition,
                timeout_hint(condition, &params)
            ));
        }
        let remaining = Duration::from_secs_f64(timeout).saturating_sub(started.elapsed());
        std::thread::sleep(Duration::from_secs_f64(interval).min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_canonical_and_alias_names() {
        assert_eq!(
            normalize_condition("text_exists").unwrap(),
            Condition::TextExists
        );
        assert_eq!(normalize_condition("text").unwrap(), Condition::TextExists);
        assert_eq!(
            normalize_condition("Active-Window").unwrap(),
            Condition::ActiveWindow
        );
        assert_eq!(
            normalize_condition("window").unwrap(),
            Condition::ActiveWindow
        );
        assert_eq!(
            normalize_condition("ELEMENT").unwrap(),
            Condition::ElementExists
        );
        assert_eq!(
            normalize_condition("enabled").unwrap(),
            Condition::ElementEnabled
        );
        assert_eq!(
            normalize_condition("focused").unwrap(),
            Condition::FocusedElement
        );
    }

    #[test]
    fn rejects_unknown_condition() {
        assert!(normalize_condition("bogus").is_err());
    }

    fn base_params(condition: &str) -> WaitForParams {
        WaitForParams {
            condition: condition.to_string(),
            text: None,
            window_name: None,
            timeout: None,
            interval: None,
            use_dom: None,
        }
    }

    #[test]
    fn rejects_timeout_out_of_range() {
        let mut params = base_params("text_exists");
        params.text = Some("x".to_string());
        params.timeout = Some(0.0);
        assert!(wait_for(params).unwrap_err().contains("timeout"));

        let mut params = base_params("text_exists");
        params.text = Some("x".to_string());
        params.timeout = Some(121.0);
        assert!(wait_for(params).unwrap_err().contains("timeout"));
    }

    #[test]
    fn rejects_interval_out_of_range() {
        let mut params = base_params("text_exists");
        params.text = Some("x".to_string());
        params.interval = Some(5.5);
        assert!(wait_for(params).unwrap_err().contains("interval"));
    }

    #[test]
    fn rejects_missing_text_for_text_exists() {
        let params = base_params("text_exists");
        assert!(wait_for(params).unwrap_err().contains("text is required"));
    }

    #[test]
    fn rejects_missing_target_for_active_window() {
        let params = base_params("active_window");
        assert!(
            wait_for(params)
                .unwrap_err()
                .contains("window_name or text")
        );
    }

    #[test]
    fn active_window_matching_does_not_require_a_snapshot() {
        let mut params = base_params("active_window");
        params.window_name = Some("SETTINGS".to_string());
        assert_eq!(
            active_window_match(&params, "Windows Settings"),
            Some("active window 'Windows Settings' matches 'SETTINGS'".to_string())
        );
        assert_eq!(active_window_match(&params, "File Explorer"), None);
    }
    #[test]
    fn snapshot_poll_budget_never_exceeds_remaining_timeout() {
        assert_eq!(snapshot_timeout_ms(Duration::from_millis(99)), None);
        assert_eq!(snapshot_timeout_ms(Duration::from_millis(100)), Some(100));
        assert_eq!(snapshot_timeout_ms(Duration::from_secs(45)), Some(30_000));
    }
}
