//! Text outside the Basic Multilingual Plane, and other characters the
//! encoding path has to get right.
//!
//! `KEYEVENTF_UNICODE` carries one UTF-16 code unit per event, so anything
//! above U+FFFF has to go out as a surrogate pair — two events for one
//! character. Nothing had checked that, and a half-sent pair produces a
//! replacement character rather than a visible failure.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock, layout};

use windows_operation_cli::tools::typing::{CaretPosition, type_at};

const SETTLE: Duration = Duration::from_secs(8);

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Types `text` and asserts the field ends up holding exactly it.
fn round_trip(app: &TestApp, text: &str, what: &str) {
    let (x, y) = app.center_of(layout::EDIT);
    type_at(x, y, text, CaretPosition::Idle, true, false).expect("typing failed");
    assert!(
        app.wait_until(SETTLE, || app.edit_text() == text),
        "{what}: the field holds {:?}, expected {:?}",
        app.edit_text(),
        text
    );
}

/// An emoji is a single `char` that encodes to two UTF-16 units. Both have to
/// arrive, in order, for the character to exist at all.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_surrogate_pair_arrives_as_one_character() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Emoji") else {
        return;
    };

    // U+1F600, above the BMP: one char, two code units.
    let text = "ok 😀";
    assert_eq!(text.chars().count(), 4);
    assert_eq!(text.encode_utf16().count(), 5, "the emoji is a surrogate pair");

    round_trip(&app, text, "emoji");
}

/// Several in a row must not run their halves together.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn consecutive_surrogate_pairs_stay_separate() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Emoji Run") else {
        return;
    };
    round_trip(&app, "😀😁😂", "three emoji");
}

/// Mixed scripts in one string: each has its own encoding width, and the
/// short ones must not be dropped between the long ones.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn mixed_scripts_survive_together() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Mixed") else {
        return;
    };
    round_trip(&app, "abc 日本語 Ελληνικά Кириллица 😀 end", "mixed scripts");
}

/// A combining mark is a separate `char` that renders onto the one before it.
/// It has to be sent, not skipped as though it were formatting.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn combining_marks_are_sent() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Combining") else {
        return;
    };
    // "e" followed by U+0301 COMBINING ACUTE ACCENT — two chars, one glyph.
    let text = "cafe\u{0301}";
    assert_eq!(text.chars().count(), 5);
    round_trip(&app, text, "combining mark");
}

/// Full-width and half-width forms are different characters, and a field that
/// receives one must not end up with the other.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn full_width_characters_are_not_folded() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Width") else {
        return;
    };
    round_trip(&app, "ＡＢＣ１２３ ABC123", "full-width and half-width");
}

/// The same text has to arrive identically whether it takes the per-character
/// route or the clipboard one, since the threshold between them is a length
/// the caller does not control.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn both_delivery_routes_produce_the_same_text() {
    let _desktop = desktop_lock();
    let Some(app) = app("Unicode Routes") else {
        return;
    };

    // Under the 20-byte threshold: per-character.
    let short = "日本😀";
    assert!(short.len() < 20, "this should take the per-character route");
    round_trip(&app, short, "short (per-character)");

    // Over it: clipboard.
    let long = "日本語と絵文字 😀😁 のまじった長めのテキスト";
    assert!(long.len() >= 20, "this should take the clipboard route");
    round_trip(&app, long, "long (clipboard)");
}
