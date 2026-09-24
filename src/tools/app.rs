//! `App` tool: launch/resize/switch applications and windows (docs/SPEC.md §1).

use std::path::Path;
use std::time::Duration;

use rmcp::schemars;
use serde::Deserialize;

use crate::apps::{self, title_case};
use crate::params::{BoolOrString, ListOrString, opt_bool};
use crate::tools::snapshot::SnapshotParams;
use crate::window;

/// `App` tool modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppMode {
    Launch,
    LaunchExecutable,
    Resize,
    Switch,
}

fn default_mode() -> AppMode {
    AppMode::Launch
}

/// Parameters for the `App` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AppParams {
    #[serde(default = "default_mode")]
    pub mode: AppMode,
    /// Application/window name to fuzzy-match against the Start Menu (launch)
    /// or currently open windows (resize/switch).
    pub name: Option<String>,
    /// `[x, y]` target position, `resize` mode only.
    pub window_loc: Option<ListOrString<i32>>,
    /// `[width, height]` target size, `resize` mode only.
    pub window_size: Option<ListOrString<i32>>,
    /// Executable path, `launch_executable` mode only (required).
    pub executable: Option<String>,
    /// Argv for the executable, `launch_executable` mode only.
    pub args: Option<ListOrString<String>>,
    /// Working directory, `launch_executable` mode only.
    pub cwd: Option<String>,
    /// Append a Snapshot of the foreground window (element ids included) to
    /// the response, so the caller can act without a separate Snapshot call.
    /// Not supported for `launch_executable`. Defaults to false.
    pub snapshot: Option<BoolOrString>,
}

/// Dispatches to the mode-specific handler. `Err` results are structural/
/// validation failures that surface as MCP tool errors (isError); `Ok`
/// results (including "application not found"-style messages) are the
/// tool's normal text response, matching the Python reference.
pub fn app(params: AppParams) -> Result<String, String> {
    let AppParams {
        mode,
        name,
        window_loc,
        window_size,
        executable,
        args,
        cwd,
        snapshot,
    } = params;
    let snapshot = opt_bool(&snapshot, false)?;

    let window_loc = to_pair(window_loc, "window_loc")?;
    let window_size = to_pair(window_size, "window_size")?;
    let args = args.map(ListOrString::into_list).transpose()?;

    let has_exact_launch_inputs = executable.is_some() || args.is_some() || cwd.is_some();
    if mode != AppMode::LaunchExecutable && has_exact_launch_inputs {
        return Err(r#"executable, args, and cwd require mode="launch_executable""#.to_string());
    }
    if mode != AppMode::Resize && (window_loc.is_some() || window_size.is_some()) {
        return Err(r#"window_loc and window_size require mode="resize""#.to_string());
    }

    if mode == AppMode::LaunchExecutable {
        let Some(executable) = executable else {
            return Err(r#"executable is required for mode="launch_executable""#.to_string());
        };
        if snapshot {
            return Err(r#"snapshot is not supported for mode="launch_executable""#.to_string());
        }
        if name.is_some() || window_loc.is_some() || window_size.is_some() {
            return Err(
                "name, window_loc, and window_size are not supported for mode=\"launch_executable\"".to_string(),
            );
        }
        return launch_executable(&executable, args.unwrap_or_default(), cwd.as_deref());
    }

    let (message, handle) = match mode {
        AppMode::Launch => launch(name.as_deref()),
        AppMode::Resize => resize(name.as_deref(), window_loc, window_size),
        AppMode::Switch => switch(name.as_deref()),
        AppMode::LaunchExecutable => unreachable!("handled above"),
    };
    if !snapshot {
        return Ok(message);
    }
    // Capture the window this call acted on, not whatever is in front: a
    // launcher, a toast or a focus-stealing popup can hold the foreground.
    let captured = match handle {
        Some(handle) => snapshot_when_populated(handle),
        None => crate::tools::snapshot::snapshot(&SnapshotParams::default()),
    };
    let captured = match captured {
        Ok(output) => output.text,
        Err(error) => error,
    };
    Ok(format!("{message}\n\n{captured}"))
}

/// How long a just-launched window gets to put up its controls. A packaged
/// app's frame appears well before its content does, and a Snapshot taken in
/// between reports no elements at all.
const CONTENT_TIMEOUT: Duration = Duration::from_secs(3);
const CONTENT_INTERVAL: Duration = Duration::from_millis(100);

/// Snapshots `handle`'s application once its content has settled: it shows
/// a control, and two captures in a row found the same number of elements.
/// An app that restores a session fills in for a while after its first
/// control appears, and a capture taken partway through would leave the rest
/// to be reported as a change by the next action.
fn snapshot_when_populated(
    handle: isize,
) -> Result<crate::tools::snapshot::SnapshotOutput, String> {
    let deadline = std::time::Instant::now() + CONTENT_TIMEOUT;
    let mut previous_count = None;
    loop {
        let output = crate::tools::snapshot::snapshot_window(handle);
        let count = crate::state::current_state().map_or(0, |state| {
            state.interactive_nodes.len()
                + state.scrollable_nodes.len()
                + state.informative_nodes.len()
        });
        let settled = count > 0 && previous_count == Some(count);
        if settled || output.is_err() || std::time::Instant::now() >= deadline {
            return output;
        }
        previous_count = Some(count);
        std::thread::sleep(CONTENT_INTERVAL);
    }
}

fn to_pair(value: Option<ListOrString<i32>>, field: &str) -> Result<Option<(i32, i32)>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let list = value.into_list()?;
    match list.as_slice() {
        [x, y] => Ok(Some((*x, *y))),
        _ => Err(format!(
            "{field} must have exactly 2 elements [x, y], got {}",
            list.len()
        )),
    }
}

/// The response text, and the window it concerns when there is one.
type Outcome = (String, Option<isize>);

fn launch(name: Option<&str>) -> Outcome {
    let Some(name) = name else {
        return (r#"name is required for mode="launch""#.to_string(), None);
    };
    let existing: Vec<isize> = window::list_windows().iter().map(|w| w.handle).collect();
    let (response, status, pid, matched_name) = apps::launch_app(name);
    if status != 0 {
        return (response, None);
    }
    match window::wait_for_window(pid, &matched_name, Duration::from_secs(10), &existing) {
        Some(handle) => (
            format!("{} launched.", title_case(&matched_name)),
            Some(handle),
        ),
        None => (
            format!(
                "Launching {} sent, but window not detected yet.",
                title_case(&matched_name)
            ),
            None,
        ),
    }
}

fn resize(name: Option<&str>, loc: Option<(i32, i32)>, size: Option<(i32, i32)>) -> Outcome {
    let target = match name {
        Some(name) => match window::find_by_name(name) {
            Some(w) => w,
            None => return (format!("Application {} not found.", title_case(name)), None),
        },
        None => match window::foreground_window() {
            Some(w) => w,
            None => return ("No active window found".to_string(), None),
        },
    };
    let handle = Some(target.handle);

    if window::is_minimized(target.handle) {
        return (format!("{} is minimized", target.title), handle);
    }
    if window::is_maximized(target.handle) {
        return (format!("{} is maximized", target.title), handle);
    }

    let message = match window::resize_window(target.handle, loc, size) {
        Ok((x, y, w, h)) => format!("{} resized to {w}x{h} at {x},{y}.", target.title),
        Err(e) => format!("Failed to resize {}: {e}", target.title),
    };
    (message, handle)
}

fn switch(name: Option<&str>) -> Outcome {
    let Some(name) = name else {
        return (r#"name is required for mode="switch""#.to_string(), None);
    };
    let Some(target) = window::find_by_name(name) else {
        return (format!("Application {} not found.", title_case(name)), None);
    };

    let was_minimized = window::is_minimized(target.handle);
    window::switch_to(target.handle);
    let message = if was_minimized {
        format!(
            "Restored {} from minimized and switched to it.",
            title_case(&target.title)
        )
    } else {
        format!("Switched to {} window.", title_case(&target.title))
    };
    (message, Some(target.handle))
}

fn launch_executable(
    executable: &str,
    args: Vec<String>,
    cwd: Option<&str>,
) -> Result<String, String> {
    let resolved_executable = resolve_executable(executable)?;
    let resolved_cwd = cwd.map(resolve_cwd).transpose()?;

    let mut command = std::process::Command::new(&resolved_executable);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = &resolved_cwd {
        command.current_dir(dir);
    }

    let child = command
        .spawn()
        .map_err(|e| format!("Failed to launch executable: {e}"))?;

    let payload = serde_json::json!({
        "pid": child.id(),
        "executable": resolved_executable,
        "args": args,
        "cwd": resolved_cwd,
    });
    Ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
}

fn expand_user(path: &str) -> String {
    match path.strip_prefix('~') {
        Some("") => crate::win::home_dir(),
        Some(rest) => match rest.strip_prefix(['/', '\\']) {
            Some(tail) => format!("{}\\{tail}", crate::win::home_dir()),
            None => path.to_string(),
        },
        None => path.to_string(),
    }
}

fn resolve_executable(executable: &str) -> Result<String, String> {
    let expanded = expand_user(executable);
    let absolute =
        std::path::absolute(&expanded).unwrap_or_else(|_| Path::new(&expanded).to_path_buf());
    if !absolute.is_file() {
        return Err(format!("Executable does not exist: {}", absolute.display()));
    }
    Ok(std::fs::canonicalize(&absolute)
        .unwrap_or(absolute)
        .display()
        .to_string())
}

fn resolve_cwd(cwd: &str) -> Result<String, String> {
    let expanded = expand_user(cwd);
    let absolute =
        std::path::absolute(&expanded).unwrap_or_else(|_| Path::new(&expanded).to_path_buf());
    if !absolute.is_dir() {
        return Err(format!(
            "Working directory does not exist: {}",
            absolute.display()
        ));
    }
    Ok(std::fs::canonicalize(&absolute)
        .unwrap_or(absolute)
        .display()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_resize_parameters_in_launch_mode() {
        let result = app(AppParams {
            mode: AppMode::Launch,
            name: Some("anything".to_string()),
            window_loc: Some(ListOrString::List(vec![0, 0])),
            window_size: None,
            executable: None,
            args: None,
            cwd: None,
            snapshot: None,
        });
        assert!(result.unwrap_err().contains("require mode=\"resize\""));
    }
}
