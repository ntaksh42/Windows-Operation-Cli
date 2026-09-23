//! `Screenshot` and `DisplayInventory`: the image actually contains the
//! desktop, and the coordinates the response reports can be acted on.
//!
//! The blank-frame bug lived exactly here — a capture of the right size with
//! nothing in it — so these assert on the pixels, not only on the dimensions.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock};
use windows_operation_cli::capture::{Backend, capture_rect_with_backend};
use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::display_inventory::display_inventory;
use windows_operation_cli::tools::screenshot::{ScreenshotParams, screenshot};

fn params() -> ScreenshotParams {
    ScreenshotParams {
        use_annotation: Some(BoolOrString::Bool(false)),
        width_reference_line: None,
        height_reference_line: None,
        display: None,
        window: None,
    }
}

/// Decodes the PNG and reports the share of pixels carrying any colour.
fn lit_fraction(png: &[u8]) -> f64 {
    let image = image::load_from_memory(png)
        .expect("the response was not a decodable image")
        .to_rgba8();
    let lit = image
        .pixels()
        .filter(|pixel| pixel.0[0] as u32 + pixel.0[1] as u32 + pixel.0[2] as u32 > 0)
        .count();
    lit as f64 / (image.width() as f64 * image.height() as f64).max(1.0)
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_screenshot_contains_the_desktop() {
    let output = screenshot(&params()).expect("screenshot failed");
    assert!(!output.png_bytes.is_empty(), "the response carried no image");

    let lit = lit_fraction(&output.png_bytes);
    assert!(
        lit > 0.5,
        "the screenshot is {:.1}% lit; a desktop is never this dark",
        lit * 100.0
    );
}

/// The response has to say how big the image is, since every coordinate the
/// caller derives from it depends on that.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_response_reports_the_image_size() {
    let output = screenshot(&params()).expect("screenshot failed");
    assert!(
        output.text.contains("Screenshot Size:")
            || output.text.contains("Screenshot Original Size:"),
        "the response does not report a size:\n{}",
        output.text
    );
    assert!(
        output.text.contains("Cursor Position:"),
        "the response does not report the cursor:\n{}",
        output.text
    );
}

/// Two captures in a row must both be real: the blank-frame bug produced a
/// good first frame and an empty second one, because each capture rebuilt the
/// duplication and read its contentless first frame.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn consecutive_screenshots_are_all_real() {
    for attempt in 0..3 {
        let output = screenshot(&params()).expect("screenshot failed");
        let lit = lit_fraction(&output.png_bytes);
        assert!(
            lit > 0.5,
            "capture {attempt} is {:.1}% lit",
            lit * 100.0
        );
    }
}

/// Restricting to a display has to produce that display's region, and say so.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_single_display_capture_reports_its_region() {
    let output = screenshot(&ScreenshotParams {
        display: Some(windows_operation_cli::params::ListOrString::List(vec![0])),
        ..params()
    })
    .expect("screenshot failed");

    assert!(
        output.text.contains("Screenshot Region:"),
        "a display-restricted capture should report its region:\n{}",
        output.text
    );
    assert!(
        lit_fraction(&output.png_bytes) > 0.5,
        "the display capture came back blank"
    );
}

/// An index that names no display is a caller mistake.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_unknown_display_index_is_rejected() {
    let result = screenshot(&ScreenshotParams {
        display: Some(windows_operation_cli::params::ListOrString::List(vec![99])),
        ..params()
    });
    let Err(error) = result else {
        panic!("an out-of-range display should be rejected");
    };
    assert!(
        error.to_lowercase().contains("display"),
        "unexpected error: {error}"
    );
}

/// The inventory has to describe the displays a capture can be restricted to,
/// as parseable JSON rather than prose.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn the_display_inventory_lists_real_displays() {
    let json = display_inventory();
    let parsed: serde_json::Value =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("not JSON: {e}\n{json}"));
    let entries = parsed.as_array().expect("the inventory should be an array");
    assert!(!entries.is_empty(), "no displays were reported: {json}");

    for entry in entries {
        let width = entry["bounds"]["width"].as_i64().unwrap_or(0);
        let height = entry["bounds"]["height"].as_i64().unwrap_or(0);
        assert!(
            width > 0 && height > 0,
            "a display reported no area: {entry}"
        );
        // The work area is the part a window can occupy, so it must fit
        // inside the display it belongs to.
        assert!(
            entry["work_area"]["width"].as_i64().unwrap_or(0) <= width
                && entry["work_area"]["height"].as_i64().unwrap_or(0) <= height,
            "the work area is larger than its display: {entry}"
        );
    }
    assert_eq!(
        entries.iter().filter(|e| e["primary"] == true).count(),
        1,
        "there should be exactly one primary display: {json}"
    );
}

/// Reads the `Screenshot Region: (l,t,r,b)` line back as a rectangle.
fn reported_region(text: &str) -> windows::Win32::Foundation::RECT {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("Screenshot Region: "))
        .unwrap_or_else(|| panic!("the response reports no region:\n{text}"));
    let values: Vec<i32> = line
        .trim_matches(|c| c == '(' || c == ')')
        .split(',')
        .map(|value| value.parse().expect("region values are integers"))
        .collect();
    windows::Win32::Foundation::RECT {
        left: values[0],
        top: values[1],
        right: values[2],
        bottom: values[3],
    }
}

/// The point of `window`: a window covered by another is still captured as
/// itself, not as whatever sits in front of it.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_covered_window_is_captured_as_itself() {
    let _desktop = desktop_lock();
    let Some(back) = TestApp::launch("Window Capture Back") else {
        return;
    };
    let Some(front) = TestApp::launch("Window Capture Front") else {
        return;
    };
    let back_rect = back.window_rect();
    windows_operation_cli::window::resize_window(
        front.hwnd(),
        Some((back_rect.left, back_rect.top)),
        Some((
            back_rect.right - back_rect.left,
            back_rect.bottom - back_rect.top,
        )),
    )
    .expect("failed to move the front window over the back one");
    front
        .wait_until_on_top(Duration::from_secs(3))
        .expect("the front window never came to the top");

    let output = screenshot(&ScreenshotParams {
        window: Some("Window Capture Back".to_string()),
        ..params()
    })
    .expect("window screenshot failed");
    assert!(
        output
            .text
            .contains("Screenshot Window: Window Capture Back"),
        "the response names the wrong window:\n{}",
        output.text
    );
    assert!(
        output.text.contains("Screenshot Backend: printwindow"),
        "the covered window was not rendered by itself:\n{}",
        output.text
    );

    // What the screen shows there is the front window. The capture must not
    // be that.
    let region = reported_region(&output.text);
    let (on_screen, _) = capture_rect_with_backend(region, Backend::Gdi).expect("GDI failed");
    let captured = image::load_from_memory(&output.png_bytes)
        .expect("the response was not a decodable image")
        .to_rgba8();
    assert_eq!(
        (captured.width(), captured.height()),
        (on_screen.width(), on_screen.height()),
        "the window capture is not the size of the region it reports"
    );
    assert!(
        captured
            .pixels()
            .zip(on_screen.pixels())
            .any(|(a, b)| a != b),
        "the window capture is identical to the window covering it"
    );
    assert!(
        lit_fraction(&output.png_bytes) > 0.5,
        "the window capture is blank"
    );
}

/// A minimized window has nothing to render; saying so beats a blank image.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_minimized_window_is_refused() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Window Capture Minimized") else {
        return;
    };
    app.minimize();
    std::thread::sleep(Duration::from_millis(300));

    let error = screenshot(&ScreenshotParams {
        window: Some("Window Capture Minimized".to_string()),
        ..params()
    })
    .err()
    .expect("a minimized window should not be captured");
    assert!(error.contains("minimized"), "unexpected error: {error}");
}

#[test]
fn window_and_display_cannot_be_combined() {
    let error = screenshot(&ScreenshotParams {
        window: Some("anything".to_string()),
        display: Some(windows_operation_cli::params::ListOrString::List(vec![0])),
        ..params()
    })
    .err()
    .expect("window with display should be rejected");
    assert!(error.contains("display"), "unexpected error: {error}");
}
