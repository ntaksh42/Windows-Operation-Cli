//! `Screenshot` tool: fast screenshot-first desktop capture.
//!
//! This is the vision-only, UI-tree-skipping counterpart to `Snapshot`: it
//! captures the desktop (or a subset of
//! displays), scales it to fit within 1920x1080, optionally overlays a
//! reference grid, and returns a text summary plus a PNG image.

use rmcp::schemars;
use serde::Deserialize;
use std::env;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use image::ImageEncoder;

use crate::params::{BoolOrString, ListOrString};
use crate::tools::snapshot::{self, SnapshotParams};
use crate::{capture, display, window};

/// Screenshots are downscaled to fit within this size (before applying
/// `WINDOWS_MCP_SCREENSHOT_SCALE`).
pub const MAX_IMAGE_WIDTH: u32 = 1920;
pub const MAX_IMAGE_HEIGHT: u32 = 1080;

/// Appended to a response whose image came back essentially black from every
/// backend. The image is still returned — a black desktop is possible — but a
/// caller reasoning about it must not mistake it for what is on the screen.
pub const BLANK_CAPTURE_WARNING: &str = "Screenshot Warning: the captured image is almost entirely \
black. Windows handed back no desktop content, which happens when the workstation is locked, a \
UAC or other secure desktop is up, the Remote Desktop window is minimized or disconnected, or the \
server runs outside the interactive session (a service or an SSH login). Do not act on this image; \
run Doctor to check the capture backends.\n";

/// Parameters for the `Screenshot` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScreenshotParams {
    /// Currently a no-op: this tool has no UI element/node information to
    /// annotate with, so the parameter is accepted (for interface
    /// compatibility) but ignored.
    #[schemars(description = "Reserved for future bounding-box annotation. Currently ignored.")]
    pub use_annotation: Option<BoolOrString>,
    #[schemars(
        description = "Number of vertical grid divisions to overlay; only takes effect when height_reference_line is also set."
    )]
    pub width_reference_line: Option<i64>,
    #[schemars(
        description = "Number of horizontal grid divisions to overlay; only takes effect when width_reference_line is also set."
    )]
    pub height_reference_line: Option<i64>,
    #[schemars(
        description = "Zero-based active display indices to restrict the capture to; omit for the full virtual desktop."
    )]
    pub display: Option<ListOrString<i32>>,
    #[schemars(
        description = "Fuzzy title query for capturing one window, even while other windows cover it. Incompatible with display."
    )]
    pub window: Option<String>,
}

/// Text + PNG bytes making up a successful `Screenshot` response.
pub struct ScreenshotOutput {
    pub text: String,
    pub png_bytes: Vec<u8>,
}

/// Resolves `WINDOWS_MCP_SCREENSHOT_SCALE`, clamped to `0.1..=1.0`. Falls
/// back to `1.0` when unset or unparsable.
pub fn resolve_scale() -> f64 {
    resolve_scale_from(env::var("WINDOWS_MCP_SCREENSHOT_SCALE").ok().as_deref())
}

fn resolve_scale_from(raw: Option<&str>) -> f64 {
    let scale = raw
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(1.0);
    scale.clamp(0.1, 1.0)
}

pub(crate) fn profiling_enabled() -> bool {
    matches!(
        env::var("WINDOWS_MCP_PROFILE_SNAPSHOT")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Combines the user-requested scale with the 1920x1080 cap into a single
/// downscale factor (never > 1.0, since neither input can exceed 1.0).
pub fn combined_scale(orig_width: u32, orig_height: u32, user_scale: f64) -> f64 {
    let scale_width = if orig_width > MAX_IMAGE_WIDTH {
        MAX_IMAGE_WIDTH as f64 / orig_width as f64
    } else {
        1.0
    };
    let scale_height = if orig_height > MAX_IMAGE_HEIGHT {
        MAX_IMAGE_HEIGHT as f64 / orig_height as f64
    } else {
        1.0
    };
    user_scale.min(scale_width).min(scale_height)
}

/// Applies `scale` to `(orig_width, orig_height)`, truncating towards zero
/// (matching Python's `int(width * scale)`).
pub fn scaled_size(orig_width: u32, orig_height: u32, scale: f64) -> (u32, u32) {
    (
        ((orig_width as f64) * scale) as u32,
        ((orig_height as f64) * scale) as u32,
    )
}

/// Downscales `image` with a Lanczos3 filter. `fast_image_resize` produces the
/// same pixels as `image::imageops::resize` (mean difference under 0.05 of a
/// level) in about a fifth of the time: 12ms against 55ms for a 1920x1280
/// desktop, on every downscaled capture.
pub(crate) fn resize_lanczos3(
    image: &image::RgbaImage,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage, String> {
    use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
    let mut resized = image::RgbaImage::new(width, height);
    // Captures are opaque, so skip the alpha premultiply/unpremultiply passes.
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .use_alpha(false);
    Resizer::new()
        .resize(image, &mut resized, &options)
        .map_err(|e| format!("Image resize failed: {e}"))?;
    Ok(resized)
}

/// Encodes `image` as PNG with the Paeth filter on every row. The default
/// adaptive filter tries all five per row: on a desktop capture that bought a
/// 4% smaller file for 60% more encoding time (8.5ms against 5.3ms).
pub(crate) fn encode_png(image: &image::RgbaImage) -> Result<Vec<u8>, String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Paeth)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(bytes)
}

/// Builds the "Screenshot Coordinate Scale" explanatory line shown when a
/// screenshot has been downscaled.
pub fn coordinate_scale_text(coord_scale: f64) -> String {
    let sample_x = (200.0 * coord_scale).round() as i64;
    let sample_y = (150.0 * coord_scale).round() as i64;
    format!(
        "Screenshot Coordinate Scale: {coord_scale} \u{2014} image pixels are downscaled; \
         multiply every image pixel coordinate by {coord_scale} before passing to Click, Move, \
         Scroll, or any loc= argument (e.g. image pixel (200, 150) \u{2192} screen coordinate \
         ({sample_x}, {sample_y}))"
    )
}

/// Explains how to convert a returned image coordinate into virtual desktop
/// screen coordinates when the capture begins at a non-zero desktop origin.
pub fn coordinate_transform_text(coord_scale: f64, origin_x: i32, origin_y: i32) -> String {
    format!(
        "Screenshot Coordinate Transform: screen = ({origin_x} + image_x × {coord_scale}, \
         {origin_y} + image_y × {coord_scale})"
    )
}

/// Overlays a light grid (`w_count` vertical, `h_count` horizontal
/// divisions) onto `image` for spatial reference.
pub(crate) fn draw_grid_lines(image: &mut image::RgbaImage, w_count: i64, h_count: i64) {
    if w_count <= 0 || h_count <= 0 {
        return;
    }
    let width = image.width() as i64;
    let height = image.height() as i64;
    let color = image::Rgba([200u8, 200, 200, 128]);
    for i in 1..w_count {
        let x = width * i / w_count;
        if (0..width).contains(&x) {
            for y in 0..height {
                image.put_pixel(x as u32, y as u32, color);
            }
        }
    }
    for i in 1..h_count {
        let y = height * i / h_count;
        if (0..height).contains(&y) {
            for x in 0..width {
                image.put_pixel(x as u32, y as u32, color);
            }
        }
    }
}

pub(crate) fn cursor_position() -> (i32, i32) {
    let mut point = POINT::default();
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok.is_ok() {
        (point.x, point.y)
    } else {
        (0, 0)
    }
}

pub(crate) fn display_list_text(displays: &[display::Display]) -> String {
    displays
        .iter()
        .map(|d| {
            let primary = if d.primary { " primary" } else { "" };
            format!(
                "{}:{} ({},{},{},{}){}",
                d.index,
                d.device,
                d.bounds.left,
                d.bounds.top,
                d.bounds.right,
                d.bounds.bottom,
                primary
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Executes the `Screenshot` tool: captures the desktop (or the union of the
/// selected displays), scales it to fit the size cap, optionally overlays a
/// reference grid, and builds the text + PNG response.
///
/// Any failure (invalid `display` selection, capture failure, encode
/// failure) is returned as `Err` with a caller-facing message; the caller is
/// expected to wrap it as `"Error capturing screenshot: {e}. Please try
/// again."` per the tool's error contract.
pub fn screenshot(params: &ScreenshotParams) -> Result<ScreenshotOutput, String> {
    if let Some(query) = &params.window {
        if params.display.is_some() {
            return Err("window cannot be combined with display".to_string());
        }
        return screenshot_window(query, params);
    }
    let result = snapshot::capture(&SnapshotParams {
        scope: None,
        window: None,
        timeout_ms: None,
        use_vision: Some(BoolOrString::Bool(true)),
        use_dom: Some(BoolOrString::Bool(false)),
        use_annotation: params.use_annotation.clone(),
        use_ui_tree: Some(BoolOrString::Bool(false)),
        width_reference_line: params.width_reference_line,
        height_reference_line: params.height_reference_line,
        display: params.display.clone(),
    })?;
    Ok(ScreenshotOutput {
        text: result.text,
        png_bytes: result
            .png_bytes
            .ok_or_else(|| "Screenshot capture returned no image".to_string())?,
    })
}

/// Captures the one window matching `query`. The image covers that window
/// only, so the response always states its origin for coordinate conversion.
fn screenshot_window(query: &str, params: &ScreenshotParams) -> Result<ScreenshotOutput, String> {
    let windows = window::list_snapshot_windows();
    let target = snapshot::find_window(query, &windows)?;
    let (captured, bounds, method) = capture::capture_window(target.handle)?;
    let blank = capture::is_blank(&captured);

    let (orig_width, orig_height) = (captured.width(), captured.height());
    let scale = combined_scale(orig_width, orig_height, resolve_scale());
    let mut image = if scale != 1.0 {
        let (w, h) = scaled_size(orig_width, orig_height, scale);
        resize_lanczos3(&captured, w.max(1), h.max(1))?
    } else {
        captured
    };
    if let (Some(w), Some(h)) = (params.width_reference_line, params.height_reference_line) {
        draw_grid_lines(&mut image, w, h);
    }
    let png_bytes = encode_png(&image)?;

    let (cx, cy) = cursor_position();
    let mut text = format!("Cursor Position: ({cx}, {cy})\n");
    let coord_scale = if scale < 1.0 {
        let coord_scale = (1.0 / scale * 1_000_000.0).round() / 1_000_000.0;
        text += &format!(
            "Screenshot Original Size: ({orig_width},{orig_height})\n{}\n",
            coordinate_scale_text(coord_scale)
        );
        coord_scale
    } else {
        text += &format!("Screenshot Size: ({},{})\n", image.width(), image.height());
        1.0
    };
    text += &coordinate_transform_text(coord_scale, bounds.left, bounds.top);
    text += &format!(
        "\nScreenshot Window: {}\nScreenshot Region: ({},{},{},{})\n\
         Coordinate Space: Virtual desktop coordinates\n",
        target.title, bounds.left, bounds.top, bounds.right, bounds.bottom
    );
    text += &match method {
        capture::WindowCapture::PrintWindow => "Screenshot Backend: printwindow\n".to_string(),
        capture::WindowCapture::ScreenRegion(backend) => format!(
            "Screenshot Backend: {} (the window could not render itself; this is its screen \
             region, so windows in front of it appear in the image)\n",
            backend.name()
        ),
    };
    if blank {
        text += BLANK_CAPTURE_WARNING;
    }
    Ok(ScreenshotOutput { text, png_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_clamps_below_minimum() {
        assert_eq!(resolve_scale_from(Some("0.01")), 0.1);
    }

    #[test]
    fn scale_clamps_above_maximum() {
        assert_eq!(resolve_scale_from(Some("2.5")), 1.0);
    }

    #[test]
    fn scale_falls_back_to_default_on_parse_failure() {
        assert_eq!(resolve_scale_from(Some("not-a-number")), 1.0);
    }

    #[test]
    fn scale_defaults_when_unset() {
        assert_eq!(resolve_scale_from(None), 1.0);
    }

    #[test]
    fn scale_within_range_is_unchanged() {
        assert_eq!(resolve_scale_from(Some("0.5")), 0.5);
    }

    #[test]
    fn cap_does_not_apply_below_the_limit() {
        let scale = combined_scale(800, 600, 1.0);
        assert_eq!(scale, 1.0);
        assert_eq!(scaled_size(800, 600, scale), (800, 600));
    }

    #[test]
    fn cap_shrinks_oversized_image_to_the_limit() {
        let scale = combined_scale(3840, 2160, 1.0);
        let (w, h) = scaled_size(3840, 2160, scale);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn cap_combines_with_user_scale() {
        let scale = combined_scale(1920, 1080, 0.5);
        let (w, h) = scaled_size(1920, 1080, scale);
        assert_eq!((w, h), (960, 540));
    }

    #[test]
    fn coordinate_scale_text_reports_multiplier_and_example() {
        let text = coordinate_scale_text(2.0);
        assert!(text.contains("Screenshot Coordinate Scale: 2"));
        assert!(text.contains("(400, 300)"));
    }

    #[test]
    fn coordinate_transform_includes_capture_origin() {
        let text = coordinate_transform_text(2.0, -1920, 100);
        assert!(text.contains("-1920 + image_x × 2"));
        assert!(text.contains("100 + image_y × 2"));
    }
}
