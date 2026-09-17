//! Clipboard behaviour, and the agreement between the two implementations
//! that touch it.
//!
//! The `Clipboard` tool goes through `arboard`, while `Type`'s paste path uses
//! its own Win32 `OpenClipboard`/`SetClipboardData` code in `input_sim`. They
//! have to agree: `Type` snapshots the clipboard, pastes, and restores it, so
//! a mismatch between the two would silently corrupt whatever the user had
//! copied.

#![cfg(target_os = "windows")]

use windows_operation_cli::input_sim;
use windows_operation_cli::tools::clipboard::{ClipboardMode, clipboard};

/// The clipboard is process-global; these tests must not interleave.
fn clipboard_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the clipboard can be taken at all right now.
///
/// Windows hands the clipboard to one process at a time, and clipboard
/// history (`cbdhsvc`) or a remote-desktop session's clipboard sync can hold
/// it indefinitely — `GetOpenClipboardWindow` reports no owner while every
/// open still fails. That is the machine's state, not a defect in the code
/// under test, so these tests skip rather than fail.
fn clipboard_is_available() -> bool {
    input_sim::set_clipboard_text("availability probe")
}

/// Restores whatever the clipboard held before a test ran.
struct Preserved(Option<String>);

impl Preserved {
    fn capture() -> Self {
        Self(input_sim::get_clipboard_text())
    }
}

impl Drop for Preserved {
    fn drop(&mut self) {
        match self.0.take() {
            Some(text) => {
                input_sim::set_clipboard_text(&text);
            }
            None => input_sim::clear_clipboard(),
        }
    }
}

#[test]
#[ignore = "touches the shared clipboard; run with --ignored"]
fn text_written_by_the_tool_is_read_back_by_the_tool() {
    let _lock = clipboard_lock();
    if !clipboard_is_available() {
        eprintln!("skipped: the clipboard is held by another process");
        return;
    }
    let _preserved = Preserved::capture();

    let text = "clipboard round trip 日本語 {braces} \"quotes\"";
    let set = clipboard(ClipboardMode::Set, Some(text.to_string()));
    assert!(set.starts_with("Clipboard set to:"), "unexpected: {set}");

    let got = clipboard(ClipboardMode::Get, None);
    assert!(
        got.contains(text),
        "the clipboard did not read back what was written: {got}"
    );
}

/// `Type`'s paste path reads and restores the clipboard with its own Win32
/// code. What the `Clipboard` tool wrote must be visible to it, or `Type`
/// would restore something else after pasting.
#[test]
#[ignore = "touches the shared clipboard; run with --ignored"]
fn the_two_clipboard_implementations_agree() {
    let _lock = clipboard_lock();
    if !clipboard_is_available() {
        eprintln!("skipped: the clipboard is held by another process");
        return;
    }
    let _preserved = Preserved::capture();

    let text = "cross-implementation 日本語 probe";
    clipboard(ClipboardMode::Set, Some(text.to_string()));
    assert_eq!(
        input_sim::get_clipboard_text().as_deref(),
        Some(text),
        "input_sim could not read what the Clipboard tool wrote"
    );

    let other = "written by input_sim 日本語";
    assert!(
        input_sim::set_clipboard_text(other),
        "input_sim failed to write the clipboard"
    );
    let got = clipboard(ClipboardMode::Get, None);
    assert!(
        got.contains(other),
        "the Clipboard tool could not read what input_sim wrote: {got}"
    );
}

/// An empty clipboard has to be reported, not mistaken for an error.
#[test]
#[ignore = "touches the shared clipboard; run with --ignored"]
fn an_empty_clipboard_is_reported_plainly() {
    let _lock = clipboard_lock();
    if !clipboard_is_available() {
        eprintln!("skipped: the clipboard is held by another process");
        return;
    }
    let _preserved = Preserved::capture();

    input_sim::clear_clipboard();
    let got = clipboard(ClipboardMode::Get, None);
    assert!(
        got.contains("empty") || got.contains("Clipboard content:"),
        "an empty clipboard produced an unexpected message: {got}"
    );
    assert!(
        !got.starts_with("Error:"),
        "an empty clipboard should not be an error: {got}"
    );
}

/// Set mode without text is a caller mistake and must say so rather than
/// clearing the clipboard.
#[test]
#[ignore = "touches the shared clipboard; run with --ignored"]
fn set_without_text_is_rejected_without_touching_the_clipboard() {
    let _lock = clipboard_lock();
    if !clipboard_is_available() {
        eprintln!("skipped: the clipboard is held by another process");
        return;
    }
    let _preserved = Preserved::capture();

    let sentinel = "sentinel value that must survive";
    input_sim::set_clipboard_text(sentinel);

    let response = clipboard(ClipboardMode::Set, None);
    assert!(
        response.starts_with("Error:"),
        "set without text should be an error: {response}"
    );
    assert_eq!(
        input_sim::get_clipboard_text().as_deref(),
        Some(sentinel),
        "a rejected set must leave the clipboard alone"
    );
}
