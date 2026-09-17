//! A multi-step sequence against a real application.
//!
//! `tests/workflow.rs` runs the same loop against the harness, whose controls
//! this repository creates. This runs it against Notepad, whose UI Automation
//! provider, focus behaviour and redraw timing are nobody's to adjust — which
//! is where the assumptions that hold for the harness tend to break.

#![cfg(target_os = "windows")]

use std::process::Command;
use std::time::{Duration, Instant};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::state::{self, ElementNode};
use windows_operation_cli::tools::snapshot::{SnapshotParams, snapshot};
use windows_operation_cli::tools::typing::{TypeParams, type_text};
use windows_operation_cli::tools::wait_for::{WaitForParams, wait_for};
use windows_operation_cli::window;

fn desktop_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A Notepad editing a scratch file, closed when the test ends.
struct Notepad {
    pid: u32,
    handle: isize,
    file: std::path::PathBuf,
}

impl Notepad {
    /// Opens Notepad on a file the test owns, so nothing the user has open is
    /// touched and no save dialog is ever needed.
    fn open(name: &str) -> Option<Self> {
        let file = std::env::temp_dir().join(format!(
            "wocli-notepad-{}-{name}.txt",
            std::process::id()
        ));
        std::fs::write(&file, "").ok()?;

        let child = Command::new("notepad.exe").arg(&file).spawn().ok()?;
        let spawned = child.id();
        let stem = file.file_stem()?.to_string_lossy().into_owned();

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(found) = window::list_current_windows()
                .into_iter()
                .find(|candidate| candidate.title.contains(&stem))
            {
                window::switch_to(found.handle);
                std::thread::sleep(Duration::from_millis(1200));
                return Some(Self {
                    pid: found.pid,
                    handle: found.handle,
                    file,
                });
            }
            if Instant::now() >= deadline {
                let _ = Command::new("taskkill")
                    .args(["/PID", &spawned.to_string(), "/F", "/T"])
                    .output();
                let _ = std::fs::remove_file(&file);
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn title(&self) -> String {
        window::list_current_windows()
            .into_iter()
            .find(|candidate| candidate.handle == self.handle)
            .map(|candidate| candidate.title)
            .unwrap_or_default()
    }
}

impl Drop for Notepad {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/PID", &self.pid.to_string(), "/F", "/T"])
            .output();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !window::list_current_windows()
                .iter()
                .any(|candidate| candidate.handle == self.handle)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_file(&self.file);
    }
}

fn capture(title: &str) -> Vec<ElementNode> {
    snapshot(&SnapshotParams {
        window: Some(title.to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("Snapshot failed");
    let state = state::current_state().expect("Snapshot published no state");
    state
        .interactive_nodes
        .iter()
        .chain(state.scrollable_nodes.iter())
        .cloned()
        .collect()
}

/// Capture, type into what the capture found, and confirm the application
/// registered it — against an editor this repository does not control.
#[test]
#[ignore = "requires an interactive Windows desktop session and Notepad; run with --ignored"]
fn text_typed_into_notepad_registers_as_an_edit() {
    let _desktop = desktop_lock();
    let Some(notepad) = Notepad::open("edit") else {
        eprintln!("skipped: Notepad did not open the scratch file");
        return;
    };
    let title = notepad.title();
    assert!(!title.is_empty(), "the window reported no title");

    // The editing surface is a scrollable document rather than an `edit`
    // control, which is exactly the shape a caller has to work with here.
    let nodes = capture(&title);
    let surface = nodes
        .iter()
        .find(|node| node.control_type == "document" || node.control_type == "edit")
        .unwrap_or_else(|| panic!("no editing surface in the capture: {nodes:#?}"));

    type_text(TypeParams {
        text: "workflow against a real editor".to_string(),
        loc: None,
        label: Some(surface.element_id as i64),
        clear: Some(BoolOrString::Bool(false)),
        caret_position: None,
        press_enter: None,
    })
    .expect("typing failed");

    // Notepad marks unsaved changes in its title, which is the application's
    // own signal that the text arrived — not something this test computed.
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut modified = false;
    while Instant::now() < deadline {
        if notepad.title().starts_with('*') || notepad.title().contains('*') {
            modified = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        modified,
        "Notepad does not report an unsaved change; the text did not arrive. Title: {:?}",
        notepad.title()
    );
}

/// `WaitFor` has to see a real application's state, not only the harness's.
#[test]
#[ignore = "requires an interactive Windows desktop session and Notepad; run with --ignored"]
fn wait_for_sees_a_real_applications_window() {
    let _desktop = desktop_lock();
    let Some(notepad) = Notepad::open("waitfor") else {
        eprintln!("skipped: Notepad did not open the scratch file");
        return;
    };
    let title = notepad.title();

    let message = wait_for(WaitForParams {
        condition: "active_window".to_string(),
        text: None,
        window_name: Some(title.clone()),
        timeout: Some(8.0),
        interval: Some(0.25),
        use_dom: None,
    })
    .unwrap_or_else(|error| panic!("WaitFor did not match {title:?}: {error}"));
    assert!(message.contains("satisfied"), "unexpected: {message}");
}

/// Two captures of the same unchanged window have to agree on what is in it.
/// A capture that races the application's redraw would drift between them.
#[test]
#[ignore = "requires an interactive Windows desktop session and Notepad; run with --ignored"]
fn repeated_captures_of_an_idle_window_agree() {
    let _desktop = desktop_lock();
    let Some(notepad) = Notepad::open("stable") else {
        eprintln!("skipped: Notepad did not open the scratch file");
        return;
    };
    let title = notepad.title();

    let names = |nodes: &[ElementNode]| {
        let mut names: Vec<String> = nodes
            .iter()
            .filter(|node| !node.name.trim().is_empty())
            .map(|node| node.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    };

    let first = names(&capture(&title));
    std::thread::sleep(Duration::from_millis(600));
    let second = names(&capture(&title));

    assert!(!first.is_empty(), "the first capture found nothing");
    assert_eq!(
        first, second,
        "two captures of an idle window disagreed about its controls"
    );
}
