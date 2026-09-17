//! Capture and accessibility against a real third-party application.
//!
//! The rest of the suite drives `tests/harness`, a window this repository
//! creates with stock comctl controls. That proves the code paths work, but
//! not that they work on an application nobody here wrote: a packaged WinUI
//! app with its own provider, its own compositor surface, and its own idea of
//! what it exposes. Microsoft PC Manager is such an app and ships with
//! Windows, so it is available without installing anything.
//!
//! These tests skip themselves when the app is not running, rather than
//! launching it: a test that starts and leaves applications around is worse
//! than one that asks for a running target.

#![cfg(target_os = "windows")]

use std::time::Duration;

use windows_operation_cli::capture::{Backend, capture_rect_with_backend};
use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::state;
use windows_operation_cli::tools::snapshot::{SnapshotParams, snapshot};
use windows_operation_cli::window;

const APP_TITLE: &str = "Microsoft PC Manager";

/// The target window, or `None` when the app is not running.
fn find_app() -> Option<window::WindowInfo> {
    window::list_current_windows()
        .into_iter()
        .find(|candidate| candidate.title.contains(APP_TITLE))
}

fn window_rect(handle: isize) -> windows::Win32::Foundation::RECT {
    let (x, y, width, height) =
        window::get_window_rect(handle).expect("the app window has no bounds");
    windows::Win32::Foundation::RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

/// Share of pixels carrying any colour.
fn lit_fraction(image: &image::RgbaImage) -> f64 {
    let lit = image
        .pixels()
        .filter(|pixel| pixel.0[0] as u32 + pixel.0[1] as u32 + pixel.0[2] as u32 > 0)
        .count();
    lit as f64 / (image.width() as f64 * image.height() as f64).max(1.0)
}

/// Both backends must return the app's actual pixels.
///
/// This is the test that would have caught the blank-frame bug: the window is
/// a WinUI surface, and a Desktop Duplication frame that came back empty
/// looked like a valid capture of the right size.
#[test]
#[ignore = "requires Microsoft PC Manager to be running; run with --ignored"]
fn both_backends_capture_a_third_party_window() {
    let Some(app) = find_app() else {
        eprintln!("skipped: {APP_TITLE} is not running");
        return;
    };
    window::switch_to(app.handle);
    std::thread::sleep(Duration::from_millis(400));
    let rect = window_rect(app.handle);

    let (gdi, gdi_backend) =
        capture_rect_with_backend(rect, Backend::Gdi).expect("GDI capture failed");
    let (dxgi, dxgi_backend) =
        capture_rect_with_backend(rect, Backend::Dxgi).expect("DXGI capture failed");
    assert_eq!(gdi_backend, Backend::Gdi);
    assert_eq!(dxgi_backend, Backend::Dxgi);

    for (name, image) in [("gdi", &gdi), ("dxgi", &dxgi)] {
        let lit = lit_fraction(image);
        eprintln!(
            "{name}: {}x{} lit={:.1}%",
            image.width(),
            image.height(),
            lit * 100.0
        );
        assert!(
            lit > 0.5,
            "{name} captured a blank frame of {APP_TITLE}: only {:.1}% of pixels lit",
            lit * 100.0
        );
    }
}

/// `auto` must produce a usable image whatever the display stack does.
#[test]
#[ignore = "requires Microsoft PC Manager to be running; run with --ignored"]
fn auto_returns_a_usable_capture_of_a_third_party_window() {
    let Some(app) = find_app() else {
        eprintln!("skipped: {APP_TITLE} is not running");
        return;
    };
    window::switch_to(app.handle);
    std::thread::sleep(Duration::from_millis(400));

    let (image, backend) = capture_rect_with_backend(window_rect(app.handle), Backend::Auto)
        .expect("auto capture failed");
    let lit = lit_fraction(&image);
    eprintln!("auto chose {} lit={:.1}%", backend.name(), lit * 100.0);
    assert!(
        lit > 0.5,
        "auto returned a blank frame via {}: only {:.1}% of pixels lit",
        backend.name(),
        lit * 100.0
    );
}

/// Snapshot must find real, actionable controls in an application whose
/// provider this repository does not implement.
#[test]
#[ignore = "requires Microsoft PC Manager to be running; run with --ignored"]
fn snapshot_finds_actionable_controls_in_a_third_party_app() {
    let Some(app) = find_app() else {
        eprintln!("skipped: {APP_TITLE} is not running");
        return;
    };
    window::switch_to(app.handle);
    std::thread::sleep(Duration::from_millis(400));

    snapshot(&SnapshotParams {
        window: Some(APP_TITLE.to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("Snapshot failed");

    let state = state::current_state().expect("Snapshot published no state");
    let owned: Vec<_> = state
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == app.handle)
        .collect();

    eprintln!(
        "found {} interactive elements: {:?}",
        owned.len(),
        owned.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(
        !owned.is_empty(),
        "Snapshot found no interactive elements in {APP_TITLE}"
    );

    // Whatever the app is showing, the elements it reports have to be usable:
    // a name to match on, a control type, and a centre inside the window.
    let rect = window_rect(app.handle);
    for node in &owned {
        assert!(
            !node.control_type.is_empty(),
            "element {:?} has no control type",
            node.name
        );
        let (x, y) = node.center;
        assert!(
            x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom,
            "element {:?} centres at ({x},{y}), outside the window {rect:?}",
            node.name
        );
    }

    // At least one element must carry the identity InvokeElement needs; an
    // element with an empty RuntimeId can only ever be clicked by coordinate.
    assert!(
        owned.iter().any(|node| !node.runtime_id.is_empty()),
        "no element in {APP_TITLE} carries a RuntimeId"
    );
}

/// The identity probe added for stale coordinates must not reject elements of
/// a third-party app that have not moved — that would block every click.
#[test]
#[ignore = "requires Microsoft PC Manager to be running; run with --ignored"]
fn element_identity_resolves_in_a_third_party_app() {
    let Some(app) = find_app() else {
        eprintln!("skipped: {APP_TITLE} is not running");
        return;
    };
    window::switch_to(app.handle);
    std::thread::sleep(Duration::from_millis(400));

    snapshot(&SnapshotParams {
        window: Some(APP_TITLE.to_string()),
        use_vision: Some(BoolOrString::Bool(false)),
        ..Default::default()
    })
    .expect("Snapshot failed");

    let state = state::current_state().expect("Snapshot published no state");
    let owned: Vec<_> = state
        .interactive_nodes
        .iter()
        .filter(|node| node.owner_handle == app.handle && !node.runtime_id.is_empty())
        .cloned()
        .collect();
    assert!(!owned.is_empty(), "no identifiable elements to probe");

    // Nothing has moved since the capture, so the provider must still report
    // each element at its own centre. `Unknown` is tolerated (a provider that
    // declines to answer), `Differs` is the failure this guards against.
    let mut matched = 0;
    for node in &owned {
        let (x, y) = node.center;
        let identity = windows_operation_cli::uia::identify_point(node, x, y);
        assert_ne!(
            identity,
            windows_operation_cli::uia::PointIdentity::Differs,
            "element {:?} was reported as moved although nothing changed",
            node.name
        );
        if identity == windows_operation_cli::uia::PointIdentity::Matches {
            matched += 1;
        }
    }
    eprintln!("{matched}/{} elements confirmed in place", owned.len());
    assert!(
        matched > 0,
        "the identity probe confirmed none of {APP_TITLE}'s elements; it would be dead weight here"
    );
}
