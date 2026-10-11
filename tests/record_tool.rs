//! `Record` writes a GIF that actually shows the desktop, with real frames,
//! and refuses to clobber an existing file.
//!
//! Like the capture tests, these assert on decoded pixels: a recording of the
//! right size that is all black is the failure this repository has shipped
//! before.

#![cfg(target_os = "windows")]

mod harness;

use std::time::Duration;

use harness::{TestApp, desktop_lock};
use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;
use windows_operation_cli::tools::record::{RecordParams, record};

fn params(action: &str, path: Option<&std::path::Path>) -> RecordParams {
    RecordParams {
        action: action.to_string(),
        fps: Some(8),
        max_duration: Some(10),
        display: None,
        window: None,
        path: path.map(|p| p.to_string_lossy().into_owned()),
    }
}

fn temp_gif(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("woc-{name}-{}.gif", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn decode(bytes: Vec<u8>) -> Vec<image::Frame> {
    GifDecoder::new(std::io::Cursor::new(bytes))
        .expect("GIF does not decode")
        .into_frames()
        .collect_frames()
        .expect("frames do not decode")
}

fn lit_fraction(image: &image::RgbaImage) -> f64 {
    let lit = image
        .pixels()
        .filter(|p| p.0[0] as u32 + p.0[1] as u32 + p.0[2] as u32 > 0)
        .count();
    lit as f64 / (image.width() * image.height()) as f64
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_recording_is_an_animated_gif_of_the_desktop() {
    let _desktop = desktop_lock();
    let Some(app) = TestApp::launch("Record Desktop Mover") else {
        return;
    };
    let path = temp_gif("record");

    record(params("start", Some(&path))).expect("start failed");
    // A still desktop would be (correctly) merged into one frame; give it
    // something to change.
    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(500));
        app.move_by(60, 0);
    }
    let text = record(params("stop", None)).expect("stop failed");

    let bytes = std::fs::read(&path).expect("no GIF was written");
    assert!(bytes.starts_with(b"GIF89a"), "not a GIF:\n{text}");
    assert_eq!(bytes.last(), Some(&0x3B), "the GIF has no trailer");

    eprintln!("{text}");
    let frames = decode(bytes);
    // Unoptimized builds resize and quantize several times slower, so the
    // frame count is only a floor; `--release` reaches the configured rate.
    assert!(
        frames.len() >= 2,
        "1.5s of recording should hold more than one frame, got {}:\n{text}",
        frames.len()
    );
    assert!(
        lit_fraction(frames[0].buffer()) > 0.5,
        "the first frame is mostly black:\n{text}"
    );
    assert!(!text.contains("Recording Warning"), "{text}");

    let _ = std::fs::remove_file(path);
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn an_existing_file_is_not_overwritten() {
    let _desktop = desktop_lock();
    let path = temp_gif("exists");
    std::fs::write(&path, b"keep me").unwrap();

    let error = record(params("start", Some(&path))).unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    // Nothing is left running to block the next recording.
    assert!(record(params("stop", None)).is_err());

    let _ = std::fs::remove_file(path);
}

#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_second_start_is_refused_until_the_first_stops() {
    let _desktop = desktop_lock();
    let first = temp_gif("first");
    let second = temp_gif("second");

    record(params("start", Some(&first))).expect("start failed");
    let error = record(params("start", Some(&second))).unwrap_err();
    assert!(error.contains("already in progress"), "{error}");
    record(params("stop", None)).expect("stop failed");
    assert!(!second.exists());

    let _ = std::fs::remove_file(first);
}

/// A window is recorded as itself, at its own size, and named in the result.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_window_recording_shows_that_window() {
    let _desktop = desktop_lock();
    let Some(_app) = TestApp::launch("Record Window Target") else {
        return;
    };
    let path = temp_gif("window");

    let mut start = params("start", Some(&path));
    start.window = Some("Record Window Target".to_string());
    let started = record(start).expect("start failed");
    assert!(
        started.contains("Window: Record Window Target"),
        "{started}"
    );
    std::thread::sleep(Duration::from_millis(1000));
    record(params("stop", None)).expect("stop failed");

    let frames = decode(std::fs::read(&path).expect("no GIF was written"));
    assert!(
        lit_fraction(frames[0].buffer()) > 0.5,
        "the window frame is mostly black"
    );
    // The window does not change, so 8 captures a second for a second must
    // collapse to a few frames that still add up to the time it took.
    assert!(
        frames.len() <= 4,
        "an unchanged window was stored {} times",
        frames.len()
    );
    let shown_ms: u32 = frames
        .iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            numer / denom
        })
        .sum();
    assert!(
        shown_ms >= 800,
        "the merged frames lost their length: {shown_ms}ms"
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn window_and_display_cannot_be_combined() {
    let mut start = params("start", None);
    start.window = Some("anything".to_string());
    start.display = Some(windows_operation_cli::params::ListOrString::List(vec![0]));
    let error = record(start).unwrap_err();
    assert!(error.contains("display"), "{error}");
}
