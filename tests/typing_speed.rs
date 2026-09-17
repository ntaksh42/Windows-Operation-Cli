//! Delivery of non-Latin text.
//!
//! The paste threshold used to count characters, so 19 Japanese characters sat
//! under it and went out one `SendInput` per character at `TYPE_INTERVAL`
//! each. Counting UTF-8 bytes puts CJK text on the clipboard path at around
//! seven characters, which is where per-character delivery stops being the
//! cheaper option.

#![cfg(target_os = "windows")]

mod harness;

use std::time::{Duration, Instant};

use harness::{TestApp, desktop_lock, layout};

use windows_operation_cli::tools::typing::{CaretPosition, type_at};

const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

fn type_into_edit(app: &TestApp, text: &str) -> Duration {
    let (x, y) = app.center_of(layout::EDIT);
    let start = Instant::now();
    type_at(x, y, text, CaretPosition::Idle, true, false).expect("typing failed");
    let elapsed = start.elapsed();
    assert!(
        app.wait_until(EVENT_TIMEOUT, || app.edit_text() == text),
        "the control received {:?}, expected {text:?}",
        app.edit_text()
    );
    elapsed
}

/// Japanese text must arrive verbatim — the encoding path is what the
/// byte-length threshold changed.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn japanese_text_arrives_verbatim() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Japanese Typing") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    // 7 characters, 21 bytes: over the byte threshold, under the old
    // character one.
    let elapsed = type_into_edit(&app, "こんにちは世界");
    eprintln!("7 Japanese characters took {:.0}ms", elapsed.as_millis());

    // Per-character delivery would spend 7 * TYPE_INTERVAL (40ms) = 280ms on
    // the keystrokes alone, before the focus click and settle waits. The paste
    // path has no per-character cost, so give it room for the clipboard round
    // trip and still catch a regression to character-by-character.
    assert!(
        elapsed < Duration::from_millis(900),
        "delivery took {:.0}ms; it looks like the per-character path",
        elapsed.as_millis()
    );
}

/// Mixed scripts and punctuation must survive the clipboard round trip.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn mixed_script_text_arrives_verbatim() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Mixed Typing") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    type_into_edit(&app, "日本語 and English 123 — ünïcödé");
}

/// Braces are ordinary characters here: escaping them is a `SendKeys`
/// constraint, and this code uses `KEYEVENTF_UNICODE` and `Ctrl+V`. Excluding
/// them pushed JSON and code onto the slow path for no reason.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn json_text_arrives_verbatim_and_quickly() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("JSON Typing") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    let json = r#"{"name": "test", "count": 42}"#;
    let elapsed = type_into_edit(&app, json);
    eprintln!("{} characters of JSON took {:.0}ms", json.len(), elapsed.as_millis());
    assert!(
        elapsed < Duration::from_millis(900),
        "JSON delivery took {:.0}ms; braces should not force the slow path",
        elapsed.as_millis()
    );
}

/// Short text still goes out per character — the clipboard round trip is not
/// worth it, and this keeps the fast path honest.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn short_text_still_arrives_verbatim() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Short Typing") else {
        return;
    };
    app.wait_until_on_top(Duration::from_secs(3))
        .expect("the test window never reached the foreground");

    type_into_edit(&app, "ok");
}
