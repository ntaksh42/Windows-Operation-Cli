//! The `App` tool: launching, switching, resizing, and the parameter
//! combinations it has to refuse.
//!
//! The window-manipulating modes run against the test harness rather than a
//! real application, so nothing the user has open is moved or resized.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock};

use windows_operation_cli::params::ListOrString;
use windows_operation_cli::tools::app::{AppMode, AppParams, app};
use windows_operation_cli::window;

fn params(mode: AppMode) -> AppParams {
    AppParams {
        mode,
        name: None,
        window_loc: None,
        window_size: None,
        executable: None,
        args: None,
        cwd: None,
    }
}

fn launch_app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Resize has to move and size the window it names.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn resize_moves_and_sizes_the_named_window() {
    let _desktop = desktop_lock();
    let title = "App Resize";
    let Some(harness) = launch_app(title) else {
        return;
    };

    let response = app(AppParams {
        name: Some(title.to_string()),
        window_loc: Some(ListOrString::List(vec![120, 90])),
        window_size: Some(ListOrString::List(vec![500, 380])),
        ..params(AppMode::Resize)
    })
    .expect("resize failed");
    assert!(
        !response.contains("not found"),
        "the window was not found: {response}"
    );

    assert!(
        harness.wait_until(Duration::from_secs(5), || {
            window::get_window_rect(harness.hwnd())
                .is_some_and(|(x, y, width, height)| {
                    (x, y, width, height) == (120, 90, 500, 380)
                })
        }),
        "the window is at {:?}, not (120,90,500x380)",
        window::get_window_rect(harness.hwnd())
    );
}

/// Resize with only a size keeps the position, and vice versa — otherwise a
/// caller adjusting one would silently move the window.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn resize_leaves_unspecified_bounds_alone() {
    let _desktop = desktop_lock();
    let title = "App Partial Resize";
    let Some(harness) = launch_app(title) else {
        return;
    };

    app(AppParams {
        name: Some(title.to_string()),
        window_loc: Some(ListOrString::List(vec![200, 150])),
        window_size: Some(ListOrString::List(vec![420, 340])),
        ..params(AppMode::Resize)
    })
    .expect("initial resize failed");
    assert!(harness.wait_until(Duration::from_secs(5), || {
        window::get_window_rect(harness.hwnd()) == Some((200, 150, 420, 340))
    }));

    // Size only: the position must not move.
    app(AppParams {
        name: Some(title.to_string()),
        window_size: Some(ListOrString::List(vec![460, 300])),
        ..params(AppMode::Resize)
    })
    .expect("size-only resize failed");
    assert!(
        harness.wait_until(Duration::from_secs(5), || {
            window::get_window_rect(harness.hwnd()) == Some((200, 150, 460, 300))
        }),
        "a size-only resize moved the window to {:?}",
        window::get_window_rect(harness.hwnd())
    );

    // Position only: the size must not change.
    app(AppParams {
        name: Some(title.to_string()),
        window_loc: Some(ListOrString::List(vec![240, 180])),
        ..params(AppMode::Resize)
    })
    .expect("position-only resize failed");
    assert!(
        harness.wait_until(Duration::from_secs(5), || {
            window::get_window_rect(harness.hwnd()) == Some((240, 180, 460, 300))
        }),
        "a position-only resize changed the size to {:?}",
        window::get_window_rect(harness.hwnd())
    );
}

/// Switch has to bring the named window to the front.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn switch_brings_the_named_window_forward() {
    let _desktop = desktop_lock();
    let first = launch_app("App Switch First");
    let Some(first) = first else { return };
    let Some(second) = launch_app("App Switch Second") else {
        return;
    };
    // The second window is in front now; ask for the first one back.
    assert!(
        second.wait_until(Duration::from_secs(3), || {
            window::foreground_window().is_some_and(|w| w.handle == second.hwnd())
        }),
        "the second window never took the foreground"
    );

    let response = app(AppParams {
        name: Some("App Switch First".to_string()),
        ..params(AppMode::Switch)
    })
    .expect("switch failed");
    assert!(
        !response.contains("not found"),
        "the window was not found: {response}"
    );

    assert!(
        first.wait_until(Duration::from_secs(5), || {
            window::foreground_window().is_some_and(|w| w.handle == first.hwnd())
        }),
        "switch did not bring the first window forward"
    );
}

/// A name nothing answers to is reported rather than acted on.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_unknown_window_name_is_reported() {
    let response = app(AppParams {
        name: Some("NoSuchWindowAnywhereOnThisDesktop".to_string()),
        ..params(AppMode::Switch)
    })
    .expect("switch returned an unexpected error");
    assert!(
        response.contains("not found"),
        "an unknown window should be reported: {response}"
    );
}

/// `window_loc`/`window_size` belong to resize; passing them elsewhere is a
/// caller mistake worth naming rather than ignoring.
#[test]
fn window_bounds_outside_resize_are_rejected() {
    let error = app(AppParams {
        name: Some("anything".to_string()),
        window_size: Some(ListOrString::List(vec![100, 100])),
        ..params(AppMode::Switch)
    })
    .expect_err("window_size on switch should be rejected");
    assert!(error.contains("resize"), "unexpected error: {error}");
}

/// Likewise the launch_executable inputs.
#[test]
fn executable_inputs_outside_launch_executable_are_rejected() {
    let error = app(AppParams {
        executable: Some("notepad.exe".to_string()),
        ..params(AppMode::Launch)
    })
    .expect_err("executable on launch should be rejected");
    assert!(
        error.contains("launch_executable"),
        "unexpected error: {error}"
    );
}

/// `launch_executable` needs the executable it is named for.
#[test]
fn launch_executable_without_an_executable_is_rejected() {
    let error = app(params(AppMode::LaunchExecutable))
        .expect_err("launch_executable with no executable should be rejected");
    assert!(error.contains("executable"), "unexpected error: {error}");
}

/// A malformed coordinate pair is rejected before anything is moved.
#[test]
fn a_bounds_pair_of_the_wrong_length_is_rejected() {
    let error = app(AppParams {
        name: Some("anything".to_string()),
        window_loc: Some(ListOrString::List(vec![1, 2, 3])),
        ..params(AppMode::Resize)
    })
    .expect_err("a three-element location should be rejected");
    assert!(
        error.contains("exactly 2 elements"),
        "unexpected error: {error}"
    );
}

/// Each mode that needs a name has to say so rather than acting on whatever
/// happens to be in front.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn launch_without_a_name_is_reported() {
    let response = app(params(AppMode::Launch)).expect("launch returned an unexpected error");
    assert!(
        response.contains("name is required"),
        "unexpected response: {response}"
    );
}
