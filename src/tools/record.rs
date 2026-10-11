//! `Record` tool: records the screen, some displays, or one window to an
//! animated GIF.
//!
//! A tool call is synchronous, so a recording that lasted one call could not
//! show what the caller's other tool calls do. `start` therefore hands the
//! capture loop to a background thread and returns at once; `stop` ends it and
//! reports the file. Frames are encoded as they arrive, so memory stays flat
//! however long the recording runs.

use std::fs::OpenOptions;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame};
use rmcp::schemars;
use serde::Deserialize;
use windows::Win32::Foundation::RECT;

use crate::params::ListOrString;
use crate::tools::screenshot;
use crate::tools::snapshot;
use crate::{capture, display, window};

const DEFAULT_FPS: i64 = 5;
const MAX_FPS: i64 = 10;
const DEFAULT_MAX_DURATION: i64 = 30;
const MAX_MAX_DURATION: i64 = 60;

/// Frames are downscaled to fit within this size: a GIF carries 256 colours
/// per frame and no inter-frame compression to speak of, so file size grows
/// with every pixel.
const MAX_WIDTH: u32 = 1280;
const MAX_HEIGHT: u32 = 720;

/// NeuQuant sampling factor, 1 (best) to 30 (fastest). A UI is flat colour, so
/// the fast end costs little quality and keeps encoding well under a frame.
const QUANTIZE_SPEED: i32 = 20;

/// Consecutive failed captures (a minimized window, a locked screen) that end
/// a recording. A single missed frame is skipped.
const MAX_CONSECUTIVE_FAILURES: u32 = 5;

/// Longest the loop sleeps before looking at the stop flag again.
const STOP_POLL: Duration = Duration::from_millis(20);

const BLANK_WARNING: &str = "Recording Warning: every frame was almost entirely black. Windows \
handed back no desktop content (locked workstation, secure desktop, disconnected remote session). \
Run Doctor to check the capture backends.\n";

/// Parameters for the `Record` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecordParams {
    #[schemars(
        description = "\"start\" begins recording in the background and returns at once; \"stop\" ends the recording and returns the saved GIF's path."
    )]
    pub action: String,
    #[schemars(description = "start only: frames per second, 1-10. Default 5.")]
    pub fps: Option<i64>,
    #[schemars(
        description = "start only: the recording ends by itself after this many seconds, 1-60. Default 30."
    )]
    pub max_duration: Option<i64>,
    #[schemars(
        description = "start only: zero-based active display indices to record; omit for the full virtual desktop."
    )]
    pub display: Option<ListOrString<i32>>,
    #[schemars(
        description = "start only: fuzzy title query for recording one window, even while other windows cover it. Incompatible with display."
    )]
    pub window: Option<String>,
    #[schemars(
        description = "start only: absolute path of the GIF to write. Must not exist yet. Default: a timestamped file under %TEMP%\\windows-operation-cli."
    )]
    pub path: Option<String>,
}

#[derive(Clone, Copy)]
enum Source {
    Region(RECT),
    Window(isize),
}

impl Source {
    fn grab(self) -> Result<image::RgbaImage, String> {
        match self {
            Source::Region(rect) => {
                capture::capture_rect_with_backend(rect, capture::resolve_backend())
                    .map(|(image, _)| image)
            }
            Source::Window(handle) => capture::capture_window(handle).map(|(image, _, _)| image),
        }
    }
}

#[derive(Default)]
struct Summary {
    frames: u32,
    skipped: u32,
    blank: u32,
    elapsed: Duration,
    /// Why the loop ended early, if it did not end by `stop` or the time limit.
    error: Option<String>,
    /// The `max_duration` limit ended it before `stop` was called.
    timed_out: bool,
}

struct Recording {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Summary>,
    path: PathBuf,
    size: (u32, u32),
    target: String,
}

static ACTIVE: Mutex<Option<Recording>> = Mutex::new(None);

pub fn record(params: RecordParams) -> Result<String, String> {
    match params.action.trim().to_ascii_lowercase().as_str() {
        "start" => start(params),
        "stop" => stop(),
        other => Err(format!(
            "Unknown action {other:?}; expected \"start\" or \"stop\""
        )),
    }
}

fn ranged(value: Option<i64>, default: i64, max: i64, name: &str) -> Result<u64, String> {
    let value = value.unwrap_or(default);
    if !(1..=max).contains(&value) {
        return Err(format!("{name} must be between 1 and {max}, got {value}"));
    }
    Ok(value as u64)
}

/// The size a `width`x`height` capture is written at.
fn output_size(width: u32, height: u32, user_scale: f64) -> (u32, u32) {
    let scale = (MAX_WIDTH as f64 / width as f64)
        .min(MAX_HEIGHT as f64 / height as f64)
        .min(user_scale)
        .min(1.0);
    (
        ((width as f64 * scale) as u32).max(1),
        ((height as f64 * scale) as u32).max(1),
    )
}

fn default_path() -> PathBuf {
    std::env::temp_dir()
        .join("windows-operation-cli")
        .join(format!(
            "record-{}.gif",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
}

fn start(params: RecordParams) -> Result<String, String> {
    let fps = ranged(params.fps, DEFAULT_FPS, MAX_FPS, "fps")?;
    let max_duration = ranged(
        params.max_duration,
        DEFAULT_MAX_DURATION,
        MAX_MAX_DURATION,
        "max_duration",
    )?;
    if params.window.is_some() && params.display.is_some() {
        return Err("window cannot be combined with display".to_string());
    }

    let path = match params.path {
        Some(path) => PathBuf::from(path),
        None => default_path(),
    };
    if !path.is_absolute() {
        return Err(format!("path must be absolute, got {}", path.display()));
    }

    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(running) = active.as_ref() {
        return Err(format!(
            "A recording to {} is already in progress; call Record with action=\"stop\" first",
            running.path.display()
        ));
    }

    let (source, target) = if let Some(query) = &params.window {
        let windows = window::list_snapshot_windows();
        let found = snapshot::find_window(query, &windows)?;
        (
            Source::Window(found.handle),
            format!("Window: {}", found.title),
        )
    } else if let Some(list) = params.display {
        let indices = list
            .into_list()?
            .into_iter()
            .map(|index| {
                usize::try_from(index).map_err(|_| format!("Invalid display index {index}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let rect = display::get_display_union_rect(&indices)?;
        (
            Source::Region(rect),
            format!(
                "Region: ({},{},{},{})",
                rect.left, rect.top, rect.right, rect.bottom
            ),
        )
    } else {
        let rect = capture::virtual_screen_rect();
        (
            Source::Region(rect),
            format!(
                "Region: ({},{},{},{})",
                rect.left, rect.top, rect.right, rect.bottom
            ),
        )
    };

    // The first frame is taken here so a window that cannot be captured, or a
    // blank desktop, fails the call instead of producing an empty file later.
    let first = source.grab()?;
    let size = output_size(first.width(), first.height(), screenshot::resolve_scale());

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                format!("{} already exists; choose another path", path.display())
            }
            _ => format!("Cannot create {}: {e}", path.display()),
        })?;
    let mut encoder = GifEncoder::new_with_speed(BufWriter::new(file), QUANTIZE_SPEED);
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| format!("GIF setup failed: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let interval = Duration::from_millis(1000 / fps);
        let max = Duration::from_secs(max_duration);
        std::thread::Builder::new()
            .name("record".to_string())
            .spawn(move || run(encoder, source, first, size, interval, max, &stop))
            .map_err(|e| format!("Cannot start the recording thread: {e}"))?
    };

    let text = format!(
        "Recording started: {}\n{target}\nGIF Size: ({},{}) at {fps} fps; ends by itself after \
         {max_duration}s. Call Record with action=\"stop\" to finish and get the file.",
        path.display(),
        size.0,
        size.1,
    );
    *active = Some(Recording {
        stop,
        worker,
        path,
        size,
        target,
    });
    Ok(text)
}

fn stop() -> Result<String, String> {
    let recording = ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("No recording is in progress; call Record with action=\"start\" first")?;
    recording.stop.store(true, Ordering::Relaxed);
    let summary = recording
        .worker
        .join()
        .map_err(|_| "The recording thread panicked".to_string())?;

    let bytes = std::fs::metadata(&recording.path)
        .map(|m| m.len())
        .unwrap_or(0);
    let mut text = format!(
        "Recording saved: {}\n{}\nGIF Size: ({},{})\nFrames: {} (unchanged frames are merged) over {:.1}s ({:.1} KB)\n",
        recording.path.display(),
        recording.target,
        recording.size.0,
        recording.size.1,
        summary.frames,
        summary.elapsed.as_secs_f64(),
        bytes as f64 / 1024.0,
    );
    if summary.skipped > 0 {
        text += &format!(
            "Frames skipped after a failed capture: {}\n",
            summary.skipped
        );
    }
    if summary.timed_out {
        text += "The recording had already reached max_duration and ended by itself.\n";
    }
    if let Some(error) = summary.error {
        text += &format!("The recording ended early: {error}\n");
    }
    if summary.frames > 0 && summary.blank == summary.frames {
        text += BLANK_WARNING;
    }
    Ok(text)
}

/// The capture loop. Each frame is held back until the next one arrives, so
/// its delay is the time it was actually on screen — a slow capture then plays
/// back at real time instead of fast.
fn run(
    mut encoder: GifEncoder<BufWriter<std::fs::File>>,
    source: Source,
    first: image::RgbaImage,
    size: (u32, u32),
    interval: Duration,
    max: Duration,
    stop: &AtomicBool,
) -> Summary {
    let mut summary = Summary::default();
    let started = Instant::now();
    let result = capture_loop(
        &mut encoder,
        source,
        first,
        size,
        interval,
        max,
        stop,
        started,
        &mut summary,
    );
    summary.elapsed = started.elapsed();
    summary.timed_out = !stop.load(Ordering::Relaxed) && result.is_ok();
    summary.error = result.err();
    // Dropping the encoder writes the GIF trailer and flushes the file.
    drop(encoder);
    summary
}

#[allow(clippy::too_many_arguments)]
fn capture_loop(
    encoder: &mut GifEncoder<BufWriter<std::fs::File>>,
    source: Source,
    first: image::RgbaImage,
    size: (u32, u32),
    interval: Duration,
    max: Duration,
    stop: &AtomicBool,
    started: Instant,
    summary: &mut Summary,
) -> Result<(), String> {
    let mut write = |image: image::RgbaImage, shown_for: Duration| -> Result<(), String> {
        let delay = Delay::from_numer_denom_ms(shown_for.as_millis().max(20) as u32, 1);
        encoder
            .encode_frame(Frame::from_parts(image, 0, 0, delay))
            .map_err(|e| format!("GIF encoding failed: {e}"))
    };

    let mut pending: Option<(image::RgbaImage, Instant)> = None;
    let mut next_image = Some(first);
    let mut next_due = started;
    let mut failures = 0u32;
    let outcome = loop {
        let captured_at = Instant::now();
        let grabbed = match next_image.take() {
            Some(image) => Ok(image),
            None => source.grab(),
        };
        match grabbed {
            Ok(image) => {
                failures = 0;
                let image = if image.dimensions() == size {
                    image
                } else {
                    // A window that was resized mid-recording.
                    screenshot::resize_lanczos3(&image, size.0, size.1)?
                };
                // An unchanged screen extends the frame already held back, so
                // an idle stretch keeps its length without storing the same
                // pixels again: a GIF frame here is a quarter of a megabyte.
                let unchanged = pending.as_ref().is_some_and(|(held, _)| *held == image);
                if !unchanged {
                    if capture::is_blank(&image) {
                        summary.blank += 1;
                    }
                    if let Some((previous, at)) = pending.replace((image, captured_at)) {
                        write(previous, captured_at - at)?;
                        summary.frames += 1;
                    }
                }
            }
            Err(error) => {
                summary.skipped += 1;
                failures += 1;
                if failures >= MAX_CONSECUTIVE_FAILURES {
                    break Err(error);
                }
            }
        }

        // A capture slower than the interval must not be followed by a burst
        // of catch-up frames.
        next_due = (next_due + interval).max(Instant::now());
        while Instant::now() < next_due && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(STOP_POLL.min(next_due - Instant::now()));
        }
        if stop.load(Ordering::Relaxed) || started.elapsed() >= max {
            break Ok(());
        }
    };

    // Flush the held-back frame even when the loop ended on an error, so what
    // was recorded is not lost with it.
    if let Some((last, at)) = pending {
        write(last, at.elapsed())?;
        summary.frames += 1;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_outside_the_range_are_rejected() {
        assert!(ranged(Some(0), 5, 10, "fps").is_err());
        assert!(ranged(Some(11), 5, 10, "fps").is_err());
        assert_eq!(ranged(None, 5, 10, "fps").unwrap(), 5);
    }

    #[test]
    fn output_is_capped_but_never_enlarged() {
        assert_eq!(output_size(3840, 2160, 1.0), (1280, 720));
        assert_eq!(output_size(800, 600, 1.0), (800, 600));
        assert_eq!(output_size(1920, 1080, 0.5), (960, 540));
    }

    #[test]
    fn an_unknown_action_is_rejected() {
        let error = record(RecordParams {
            action: "pause".to_string(),
            fps: None,
            max_duration: None,
            display: None,
            window: None,
            path: None,
        })
        .unwrap_err();
        assert!(error.contains("Unknown action"), "{error}");
    }

    #[test]
    fn a_relative_path_is_rejected_before_anything_is_written() {
        let error = start(RecordParams {
            action: "start".to_string(),
            fps: None,
            max_duration: None,
            display: None,
            window: None,
            path: Some("clip.gif".to_string()),
        });
        assert!(error.unwrap_err().contains("must be absolute"));
        assert!(!std::path::Path::new("clip.gif").exists());
    }
}
