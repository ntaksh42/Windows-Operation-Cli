//! `Clipboard` tool: read/write the Windows text clipboard.

use rmcp::schemars;
use serde::Deserialize;

/// Parameters for the `Clipboard` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ClipboardParams {
    /// "get" to read the current clipboard text, "set" to write it.
    #[schemars(description = "Clipboard operation mode: \"get\" or \"set\".")]
    pub mode: ClipboardMode,
    /// Text to place on the clipboard. Required for `mode: "set"`.
    #[schemars(description = "Text to place on the clipboard (required for set mode).")]
    pub text: Option<String>,
}

/// `Clipboard` operation mode.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardMode {
    Get,
    Set,
}

/// Runs the `Clipboard` tool. Always returns a caller-facing text response.
pub fn clipboard(mode: ClipboardMode, text: Option<String>) -> String {
    match mode {
        ClipboardMode::Get => get(),
        ClipboardMode::Set => set(text),
    }
}

/// Retries an operation that failed only because another process held the
/// clipboard.
///
/// Windows lets one process own the clipboard at a time, and Office, browsers
/// and clipboard-history tools all take it briefly and often. Treating that as
/// a hard failure made the tool unreliable for a condition that clears in
/// milliseconds.
fn with_retry<T>(
    mut attempt: impl FnMut() -> Result<T, arboard::Error>,
) -> Result<T, arboard::Error> {
    const ATTEMPTS: u32 = 25;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(40);

    let mut last = attempt();
    for _ in 1..ATTEMPTS {
        match last {
            Err(arboard::Error::ClipboardOccupied) => {}
            other => return other,
        }
        std::thread::sleep(DELAY);
        last = attempt();
    }
    last
}

/// Turns a clipboard failure into something the caller can act on.
fn describe(error: arboard::Error, what: &str) -> String {
    match error {
        arboard::Error::ClipboardOccupied => format!(
            "Error: Could not {what} the clipboard: another process is holding it. \
             Clipboard history (cbdhsvc) and remote-desktop clipboard sync are the \
             usual causes; retrying shortly, or restarting that service, clears it."
        ),
        other => format!("Error: Failed to {what} clipboard: {other}"),
    }
}

fn get() -> String {
    match with_retry(|| arboard::Clipboard::new()?.get_text()) {
        Ok(data) => format!("Clipboard content:\n{data}"),
        Err(arboard::Error::ContentNotAvailable) => {
            "Clipboard is empty or contains non-text data.".to_string()
        }
        Err(e) => describe(e, "read"),
    }
}

fn set(text: Option<String>) -> String {
    let Some(text) = text else {
        return "Error: text parameter required for set mode.".to_string();
    };
    match with_retry(|| arboard::Clipboard::new()?.set_text(text.clone())) {
        Ok(()) => {
            let preview: String = text.chars().take(100).collect();
            let ellipsis = if text.chars().count() > 100 {
                "..."
            } else {
                ""
            };
            format!("Clipboard set to: {preview}{ellipsis}")
        }
        Err(e) => describe(e, "write"),
    }
}
