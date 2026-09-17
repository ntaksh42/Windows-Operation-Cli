//! A sweep across the applications that ship with Windows.
//!
//! `tests/harness` proves the code paths work against a window this repository
//! builds; `tests/third_party_app` proves one real application works. This
//! sweep is the breadth check: launch each stock app, capture it, and assert
//! the capture is *usable* — controls found, names present, centres inside the
//! window, text readable. Each app is a different UI stack (Win32, WinUI 3,
//! XAML islands, WebView), which is where capture assumptions break.
//!
//! Every test launches what it needs and closes it again, and skips itself
//! when the app is unavailable, so the suite stays runnable on a machine that
//! does not have all of them.

#![cfg(target_os = "windows")]

use std::process::Command;
use std::time::{Duration, Instant};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, SnapshotResult, capture};
use windows_operation_cli::window;

/// Serializes the sweep: these tests all drive the foreground window.
fn desktop_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A launched application, closed when the test ends.
struct App {
    pid: u32,
    handle: isize,
}

impl App {
    /// Starts `command` and waits for a visible window whose title contains
    /// `title_fragment`. Returns `None` when the app never appears, so a test
    /// can skip rather than fail on a machine without it.
    fn launch(command: &str, args: &[&str], title_fragment: &str) -> Option<Self> {
        let child = Command::new(command).args(args).spawn().ok()?;
        let spawned_pid = child.id();

        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            // The launcher process often is not the one that owns the window
            // (a shell stub hands off to a packaged app), so match on the
            // title and take whatever process owns it.
            if let Some(found) = window::list_current_windows()
                .into_iter()
                .find(|candidate| candidate.title.contains(title_fragment))
            {
                // An app that ends up running elevated cannot be terminated
                // from this unelevated process, and an elevated window left
                // in the foreground blocks input injection for every later
                // test. Refuse to adopt one rather than stranding it.
                if !windows_operation_cli::win::is_elevated()
                    && windows_operation_cli::win::runs_at_higher_integrity(found.pid)
                {
                    eprintln!(
                        "not adopting {:?}: it runs elevated and could not be closed again",
                        found.title
                    );
                    return None;
                }
                window::switch_to(found.handle);
                // A packaged (UWP/WinUI) app replaces its window while
                // starting: the `ApplicationFrameWindow` that first carries
                // the title is not yet the one hosting the controls, and a
                // capture taken in that gap finds the frame and nothing in
                // it. Wait until the handle behind the title stops changing.
                let handle = settle_window(title_fragment, found.handle);
                let pid = window::list_current_windows()
                    .into_iter()
                    .find(|candidate| candidate.handle == handle)
                    .map_or(found.pid, |candidate| candidate.pid);
                return Some(Self { pid, handle });
            }
            if Instant::now() >= deadline {
                let _ = Command::new("taskkill")
                    .args(["/PID", &spawned_pid.to_string(), "/F", "/T"])
                    .output();
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// Waits until the window behind `title_fragment` stops changing identity and
/// actually reports controls, returning the handle that settled.
fn settle_window(title_fragment: &str, initial: isize) -> isize {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut handle = initial;
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let current = window::list_current_windows()
            .into_iter()
            .find(|candidate| candidate.title.contains(title_fragment))
            .map(|candidate| candidate.handle);
        if let Some(current) = current {
            handle = current;
            let has_controls = capture(&SnapshotParams {
                window: Some(title_fragment.to_string()),
                use_vision: Some(BoolOrString::Bool(false)),
                timeout_ms: Some(5_000),
                ..Default::default()
            })
            .is_ok_and(|result| {
                result
                    .interactive_nodes
                    .iter()
                    .any(|node| node.owner_handle == current)
            });
            if has_controls {
                return handle;
            }
        }
        if Instant::now() >= deadline {
            return handle;
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = Command::new("taskkill")
            .args(["/PID", &self.pid.to_string(), "/F", "/T"])
            .output();
        // An elevated window in the foreground blocks input injection from
        // this unelevated process entirely — every later `SetCursorPos` fails
        // with ERROR_INVALID_HANDLE while the cursor stays put. Killing the
        // process is not enough on its own: wait for its window to actually
        // go away before the next test starts sending input.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let gone = !window::list_current_windows()
                .iter()
                .any(|candidate| candidate.handle == self.handle);
            if gone {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn capture_window(title: &str) -> SnapshotResult {
    capture(&SnapshotParams {
        window: Some(title.to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(15_000),
        ..Default::default()
    })
    .expect("capture failed")
}

/// What every capture has to satisfy, whatever stack drew the window.
///
/// These are the properties a caller depends on to act: something to click,
/// a name to pick it by, and coordinates that land inside the window.
fn assert_capture_is_usable(app: &App, label: &str, result: &SnapshotResult) {
    let owned: Vec<_> = result
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == app.handle)
        .collect();
    assert!(
        !owned.is_empty(),
        "{label}: no interactive elements found in the window"
    );

    let named = owned.iter().filter(|node| !node.name.trim().is_empty()).count();
    assert!(
        named * 2 >= owned.len(),
        "{label}: only {named} of {} controls have a name; the capture is not addressable",
        owned.len()
    );

    let (x, y, width, height) =
        window::get_window_rect(app.handle).expect("the window has no bounds");
    for node in &owned {
        let (cx, cy) = node.center;
        assert!(
            cx >= x && cx < x + width && cy >= y && cy < y + height,
            "{label}: {:?} centres at ({cx},{cy}), outside the window ({x},{y},{width}x{height})",
            node.name
        );
        assert!(
            !node.control_type.is_empty(),
            "{label}: {:?} has no control type",
            node.name
        );
    }

    // Identity is what InvokeElement and the staleness check need.
    assert!(
        owned.iter().any(|node| !node.runtime_id.is_empty()),
        "{label}: no control carries a RuntimeId"
    );

    eprintln!(
        "{label}: {} interactive, {} informative, {} scrollable",
        owned.len(),
        result
            .informative_nodes
            .iter()
            .filter(|n| n.owner_handle == app.handle)
            .count(),
        result
            .scrollable_nodes
            .iter()
            .filter(|n| n.owner_handle == app.handle)
            .count(),
    );
}

/// Notepad: WinUI 3 with a RichEdit surface.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn notepad_is_capturable() {
    let _desktop = desktop_lock();
    let Some(app) = App::launch("notepad.exe", &[], "メモ帳") else {
        eprintln!("skipped: Notepad did not appear");
        return;
    };
    let result = capture_window("メモ帳");
    assert_capture_is_usable(&app, "notepad", &result);
}

/// Calculator: WinUI, and its result is a read-only element — exactly the
/// text that used to be dropped for every non-browser window.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn calculator_exposes_its_buttons_and_its_readout() {
    let _desktop = desktop_lock();
    let Some(app) = App::launch("calc.exe", &[], "電卓") else {
        eprintln!("skipped: Calculator did not appear");
        return;
    };
    let result = capture_window("電卓");
    assert_capture_is_usable(&app, "calculator", &result);

    // The digit keys are what makes it drivable at all.
    for digit in ["1", "5", "9"] {
        assert!(
            result
                .interactive_nodes
                .iter()
                .any(|node| node.owner_handle == app.handle && node.name == digit),
            "calculator: the {digit} key was not found"
        );
    }
}

/// Registry Editor: a classic Win32 app with a tree and a list view.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn registry_editor_is_capturable() {
    let _desktop = desktop_lock();
    let Some(app) = App::launch("regedit.exe", &[], "レジストリ エディター") else {
        eprintln!("skipped: Registry Editor did not appear (it may need elevation)");
        return;
    };
    let result = capture_window("レジストリ エディター");
    assert_capture_is_usable(&app, "regedit", &result);
}

/// An elevated window reports its frame and nothing inside, because Windows'
/// privilege isolation hides its contents from an ordinary session. That is
/// not a capture failure, and the capture has to *say* so — otherwise the
/// caller sees an empty window and retries forever.
///
/// This uses a window that happens to be elevated rather than starting one.
/// An unelevated test cannot terminate an elevated process it starts
/// (`taskkill` is denied), so launching Task Manager here would leave it in
/// the foreground, where it blocks input injection for every later test.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_elevated_window_is_reported_as_blocked_not_empty() {
    let _desktop = desktop_lock();
    if windows_operation_cli::win::is_elevated() {
        eprintln!("skipped: this session is elevated, so nothing is hidden from it");
        return;
    }
    let Some(elevated) = window::list_current_windows()
        .into_iter()
        .find(|candidate| windows_operation_cli::win::runs_at_higher_integrity(candidate.pid))
    else {
        eprintln!("skipped: no elevated window is open to test against");
        return;
    };
    eprintln!("using elevated window {:?}", elevated.title);

    let result = capture_window(&elevated.title);
    let owned = result
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == elevated.handle)
        .count();
    assert_eq!(
        owned, 0,
        "an unelevated session should not see into an elevated window"
    );
    assert!(
        result.text.contains("Elevated windows"),
        "the capture did not explain why the window was empty:\n{}",
        result.text
    );
}

/// Paint: Win32 with a ribbon, a control surface unlike the others here.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn paint_is_capturable() {
    let _desktop = desktop_lock();
    let Some(app) = App::launch("mspaint.exe", &[], "ペイント") else {
        eprintln!("skipped: Paint did not appear");
        return;
    };
    let result = capture_window("ペイント");
    assert_capture_is_usable(&app, "paint", &result);
}
