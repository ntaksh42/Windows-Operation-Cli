//! Browser page extraction through UI Automation.
//!
//! `use_dom=true` is the path that reads a web page's contents rather than the
//! browser's own chrome. It has its own Document-root discovery, its own
//! filtering, and an IAccessible2 fallback for Firefox — none of which the
//! desktop-app tests exercise.
//!
//! The page under test is a `data:` URL, so the test does not depend on the
//! network or on any particular site staying the same.

#![cfg(target_os = "windows")]

use std::process::Command;
use std::time::{Duration, Instant};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::snapshot::{SnapshotParams, SnapshotResult, capture};
use windows_operation_cli::window;

fn desktop_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marker text placed in the page, distinctive enough that finding it proves
/// the page body was read rather than the browser's chrome.
const HEADING: &str = "DomProbeHeading";
const PARAGRAPH: &str = "DomProbeParagraph";
const BUTTON_LABEL: &str = "DomProbeButton";
const LINK_LABEL: &str = "DomProbeLink";

fn probe_page() -> String {
    // Deliberately minimal: no CSS, no scripts, nothing that needs the network.
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>DomProbe</title></head><body>\
         <h1>{HEADING}</h1><p>{PARAGRAPH}</p>\
         <button>{BUTTON_LABEL}</button>\
         <a href=\"#x\">{LINK_LABEL}</a>\
         </body></html>"
    );
    format!("data:text/html;charset=utf-8,{}", urlencode(&html))
}

fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// An Edge window showing the probe page, closed when the test ends.
struct Browser {
    handle: isize,
    profile: std::path::PathBuf,
}

/// Edge's install path. It is not on `PATH`, so look where the installer puts
/// it rather than relying on the shell to find it.
fn edge_executable() -> Option<std::path::PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|base| {
            std::path::Path::new(&base)
                .join("Microsoft")
                .join("Edge")
                .join("Application")
                .join("msedge.exe")
        })
        .find(|path| path.exists())
}

impl Browser {
    fn open() -> Option<Self> {
        let executable = edge_executable()?;
        // A dedicated profile directory keeps this out of the user's own
        // browser session: no tabs added to their window, no state touched.
        // It is per-call because Chromium refuses to start a second instance
        // against a profile another one still holds — the previous test's
        // browser may not have finished exiting.
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let profile = std::env::temp_dir().join(format!(
            "wocli-dom-probe-{}-{unique}",
            std::process::id()
        ));
        let child = Command::new(executable)
            .args([
                "--new-window",
                "--no-first-run",
                "--no-default-browser-check",
                &format!("--user-data-dir={}", profile.display()),
                &probe_page(),
            ])
            .spawn()
            .ok()?;
        let spawned = child.id();

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(found) = window::list_current_windows()
                .into_iter()
                .find(|candidate| candidate.title.contains("DomProbe"))
            {
                window::switch_to(found.handle);
                // Wait for the page to be readable, not merely for the window
                // to exist: a renderer that has not painted yet exposes an
                // empty Document.
                let deadline = Instant::now() + Duration::from_secs(20);
                while Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(400));
                    if capture_dom().informative_nodes.iter().any(|node| {
                        node.name.contains(HEADING) || node.name.contains(PARAGRAPH)
                    }) {
                        break;
                    }
                }
                return Some(Self {
                    handle: found.handle,
                    profile,
                });
            }
            if Instant::now() >= deadline {
                let _ = Command::new("taskkill")
                    .args(["/PID", &spawned.to_string(), "/F", "/T"])
                    .output();
                return None;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        // Chromium spreads itself over a browser process, a GPU process, and
        // one renderer per tab, and the window's own process is rarely the
        // root of that tree — so `taskkill /T` on it leaves the siblings
        // running. Select by the profile directory instead, which is unique
        // to this browser and appears in every one of its command lines.
        let profile = self.profile.display().to_string();
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Get-CimInstance Win32_Process -Filter \"Name='msedge.exe'\" | \
                     Where-Object {{ $_.CommandLine -like '*{profile}*' }} | \
                     ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force \
                     -ErrorAction SilentlyContinue }}"
                ),
            ])
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
        // The throwaway profile is several megabytes, and the processes above
        // have to be gone before it can be removed.
        for _ in 0..10 {
            if std::fs::remove_dir_all(&self.profile).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn capture_dom() -> SnapshotResult {
    capture(&SnapshotParams {
        window: Some("DomProbe".to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        use_dom: Some(BoolOrString::Bool(true)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("DOM capture failed")
}

fn capture_plain() -> SnapshotResult {
    capture(&SnapshotParams {
        window: Some("DomProbe".to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        timeout_ms: Some(20_000),
        ..Default::default()
    })
    .expect("capture failed")
}

/// The page's own text and controls have to come through, not the browser's
/// toolbar.
#[test]
#[ignore = "requires an interactive Windows desktop session and Microsoft Edge"]
fn dom_capture_reads_the_page_body() {
    let _desktop = desktop_lock();
    let Some(_browser) = Browser::open() else {
        eprintln!("skipped: Edge did not open the probe page");
        return;
    };
    let result = capture_dom();

    let text: Vec<&str> = result
        .informative_nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert!(
        text.iter().any(|name| name.contains(HEADING)),
        "the heading was not read; informative nodes: {text:?}"
    );
    assert!(
        text.iter().any(|name| name.contains(PARAGRAPH)),
        "the paragraph was not read; informative nodes: {text:?}"
    );

    let controls: Vec<&str> = result
        .interactive_nodes
        .iter()
        .map(|node| node.name.as_str())
        .collect();
    assert!(
        controls.iter().any(|name| name.contains(BUTTON_LABEL)),
        "the page button was not found; controls: {controls:?}"
    );
    assert!(
        controls.iter().any(|name| name.contains(LINK_LABEL)),
        "the page link was not found; controls: {controls:?}"
    );

    assert!(
        result.dom_found,
        "the capture did not report finding a DOM root"
    );
}

/// Without `use_dom`, the same window should still be usable — the browser's
/// own controls — rather than coming back empty.
#[test]
#[ignore = "requires an interactive Windows desktop session and Microsoft Edge"]
fn a_browser_without_use_dom_still_reports_its_chrome() {
    let _desktop = desktop_lock();
    let Some(browser) = Browser::open() else {
        eprintln!("skipped: Edge did not open the probe page");
        return;
    };
    let result = capture_plain();
    let owned = result
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == browser.handle)
        .count();
    assert!(
        owned > 0,
        "a browser window with use_dom=false reported no controls at all"
    );
}

/// Page controls must be clickable where they are reported: a coordinate
/// outside the browser window would be a capture bug, not a page quirk.
#[test]
#[ignore = "requires an interactive Windows desktop session and Microsoft Edge"]
fn page_control_coordinates_fall_inside_the_browser_window() {
    let _desktop = desktop_lock();
    let Some(browser) = Browser::open() else {
        eprintln!("skipped: Edge did not open the probe page");
        return;
    };
    let result = capture_dom();
    let (x, y, width, height) =
        window::get_window_rect(browser.handle).expect("the browser window has no bounds");

    let page_controls: Vec<_> = result
        .interactive_nodes
        .iter()
        .filter(|node| node.name.contains(BUTTON_LABEL) || node.name.contains(LINK_LABEL))
        .collect();
    assert!(!page_controls.is_empty(), "no page controls to check");

    for node in page_controls {
        let (cx, cy) = node.center;
        assert!(
            cx >= x && cx < x + width && cy >= y && cy < y + height,
            "{:?} centres at ({cx},{cy}), outside the browser window ({x},{y},{width}x{height})",
            node.name
        );
    }
}
