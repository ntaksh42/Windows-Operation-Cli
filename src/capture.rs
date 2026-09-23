//! Screen capture backends.
//!
use std::cell::RefCell;
use std::env;
use std::ffi::c_void;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HDC, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowRect, IsIconic, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::core::Interface;

const DXGI_ACQUIRE_TIMEOUT_MS: u32 = 100;

/// How long to keep asking Desktop Duplication for a frame that actually
/// carries the desktop, before giving up and letting the caller fall back.
/// A new duplication always yields one contentless frame first, and an idle
/// desktop may present nothing for a while after that.
const DXGI_FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_millis(350);

type AdapterKey = (u32, i32);

/// A cached D3D device and its immediate context.
///
/// `ManuallyDrop` is the point: these live in thread-local storage, whose
/// destructor runs while the thread is being torn down. Releasing a D3D
/// device there crashed the process — the graphics runtime is already
/// unwinding by then, and the reference drop faulted inside it. Screenshot
/// requests run on Tokio's blocking pool, whose threads are retired routinely,
/// so every retirement took the server down with it.
///
/// Leaking one device per thread is the documented trade for caching a COM
/// object in TLS; the pool is small and bounded, and the OS reclaims
/// everything at process exit.
struct CachedDevice {
    device: ManuallyDrop<ID3D11Device>,
    context: ManuallyDrop<ID3D11DeviceContext>,
}

thread_local! {
    // Screenshot requests run on Tokio's blocking threads. Reusing the D3D
    // device per such thread avoids recreating an expensive device for every
    // output on every request, while keeping COM objects thread-affine.
    static DXGI_DEVICES: RefCell<Vec<(AdapterKey, CachedDevice)>> = const {
        RefCell::new(Vec::new())
    };
}

/// Screen capture backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Gdi,
    Dxgi,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Gdi => "gdi",
            Backend::Dxgi => "dxgi",
        }
    }
}

/// Resolves the capture backend from `WINDOWS_MCP_SCREENSHOT_BACKEND`
/// (`auto`/`dxgi`/`gdi`; unrecognized or unset values are treated as `auto`).
pub fn resolve_backend() -> Backend {
    match env::var("WINDOWS_MCP_SCREENSHOT_BACKEND")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "gdi" | "mss" | "pillow" => Backend::Gdi,
        "dxgi" | "dxcam" => Backend::Dxgi,
        _ => Backend::Auto,
    }
}

/// Returns the bounding rectangle of the full virtual desktop (all monitors).
pub fn virtual_screen_rect() -> RECT {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let cx = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let cy = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        RECT {
            left: x,
            top: y,
            right: x + cx,
            bottom: y + cy,
        }
    }
}

/// Captures a rectangle and reports the backend that actually succeeded.
/// `auto` prefers GDI and falls back to Desktop Duplication only when GDI
/// fails or hands back a blank frame.
pub fn capture_rect_with_backend(
    rect: RECT,
    backend: Backend,
) -> Result<(image::RgbaImage, Backend), String> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return Err(format!("Invalid capture region: {width}x{height}"));
    }

    match backend {
        // GDI reads the DWM-composed desktop on every call, so it has none of
        // Desktop Duplication's black-frame modes: a fresh duplication's empty
        // first frame, an idle desktop presenting nothing, duplication lost to
        // manual input or a mode change. Duplication stays as the fallback for
        // the rare session where GDI fails or reads back nothing.
        Backend::Auto => match capture_gdi_image(rect, width, height) {
            Ok(image) if !is_blank(&image) => Ok((image, Backend::Gdi)),
            gdi => match unsafe { capture_rect_dxgi(rect) } {
                Ok(image) if !is_blank(&image) => Ok((image, Backend::Dxgi)),
                // Neither produced content. A genuinely dark screen is still a
                // screenshot, so prefer GDI's frame over an error.
                dxgi => gdi
                    .map(|image| (image, Backend::Gdi))
                    .or_else(|_| dxgi.map(|image| (image, Backend::Dxgi))),
            },
        },
        Backend::Gdi => capture_gdi_image(rect, width, height).map(|image| (image, Backend::Gdi)),
        Backend::Dxgi => unsafe { capture_rect_dxgi(rect) }.map(|image| (image, Backend::Dxgi)),
    }
}

/// Whether a capture came back with essentially nothing in it.
///
/// A real desktop always lights up most of its pixels — even a dark theme
/// sits well above zero. Sampling every 64th pixel keeps this at a fraction
/// of a millisecond on a 4K frame while still being decisive.
fn is_blank(image: &image::RgbaImage) -> bool {
    const STRIDE: usize = 64;
    let raw = image.as_raw();
    let mut sampled = 0u32;
    let mut lit = 0u32;
    for pixel in raw.chunks_exact(4).step_by(STRIDE) {
        sampled += 1;
        if pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32 > 0 {
            lit += 1;
        }
    }
    sampled > 0 && lit * 2 < sampled
}

fn capture_gdi_image(rect: RECT, width: i32, height: i32) -> Result<image::RgbaImage, String> {
    let pixels = unsafe { capture_rect_gdi(rect, width, height)? };
    image::RgbaImage::from_raw(width as u32, height as u32, pixels)
        .ok_or_else(|| "Failed to build image buffer from captured pixels".to_string())
}

unsafe fn create_device(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice failed: {e}"))?;
        Ok((
            device.ok_or("D3D11CreateDevice returned no device")?,
            context.ok_or("D3D11CreateDevice returned no context")?,
        ))
    }
}

fn device_for_adapter(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let desc = unsafe { adapter.GetDesc1() }.map_err(|error| error.to_string())?;
    let key = (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart);
    DXGI_DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        if let Some((_, cached)) = devices.iter().find(|(stored_key, _)| *stored_key == key) {
            return Ok(((*cached.device).clone(), (*cached.context).clone()));
        }
        let (device, context) = unsafe { create_device(adapter) }?;
        devices.push((
            key,
            CachedDevice {
                device: ManuallyDrop::new(device.clone()),
                context: ManuallyDrop::new(context.clone()),
            },
        ));
        Ok((device, context))
    })
}

unsafe fn capture_output(
    adapter: &IDXGIAdapter1,
    output: &IDXGIOutput1,
) -> Result<(RECT, image::RgbaImage), String> {
    unsafe {
        let output_desc = output
            .GetDesc()
            .map_err(|e| format!("GetDesc failed: {e}"))?;
        let (device, context) = device_for_adapter(adapter)?;
        let duplication = output
            .DuplicateOutput(&device)
            .map_err(|e| format!("DuplicateOutput failed: {e}"))?;

        // A freshly created duplication hands back a frame with no desktop
        // image in it: `AcquireNextFrame` succeeds, `LastPresentTime` is zero,
        // and the texture holds whatever the accumulator started as — black.
        // Only a frame the compositor has actually presented carries pixels.
        //
        // Nothing forces the desktop to present, either, so a still screen can
        // legitimately have nothing new for a while. Release each contentless
        // frame and ask again until one arrives or the budget runs out. This
        // is why full-screen captures looked fine while a capture taken right
        // after another one came back blank: the second duplication was new.
        let deadline = std::time::Instant::now() + DXGI_FRAME_DEADLINE;
        let resource = loop {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut next = None;
            duplication
                .AcquireNextFrame(DXGI_ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut next)
                .map_err(|e| format!("AcquireNextFrame failed: {e}"))?;
            if frame_info.LastPresentTime != 0 {
                break next;
            }
            // This frame carries only cursor or metadata changes. Give it back
            // before asking for the next one — holding it blocks the queue.
            drop(next);
            let _ = duplication.ReleaseFrame();
            if std::time::Instant::now() >= deadline {
                return Err(
                    "DXGI produced no presented frame before the deadline; the desktop may be idle"
                        .to_string(),
                );
            }
        };

        let result = (|| {
            let texture: ID3D11Texture2D = resource
                .ok_or("AcquireNextFrame returned no resource")?
                .cast()
                .map_err(|e| format!("Frame is not a D3D11 texture: {e}"))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..desc
            };
            let mut staging = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| format!("CreateTexture2D staging failed: {e}"))?;
            let staging = staging.ok_or("CreateTexture2D returned no staging texture")?;
            context.CopyResource(&staging, &texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Map staging texture failed: {e}"))?;
            let mut pixels = vec![0u8; desc.Width as usize * desc.Height as usize * 4];
            for y in 0..desc.Height as usize {
                let source = std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    desc.Width as usize * 4,
                );
                let target =
                    &mut pixels[y * desc.Width as usize * 4..(y + 1) * desc.Width as usize * 4];
                target.copy_from_slice(source);
                for pixel in target.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                    pixel[3] = 255;
                }
            }
            context.Unmap(&staging, 0);
            let image = image::RgbaImage::from_raw(desc.Width, desc.Height, pixels)
                .ok_or_else(|| "Failed to build DXGI image buffer".to_string())?;
            let image = match output_desc.Rotation {
                DXGI_MODE_ROTATION_ROTATE90 => image::imageops::rotate90(&image),
                DXGI_MODE_ROTATION_ROTATE180 => image::imageops::rotate180(&image),
                DXGI_MODE_ROTATION_ROTATE270 => image::imageops::rotate270(&image),
                _ => image,
            };
            Ok((output_desc.DesktopCoordinates, image))
        })();
        let _ = duplication.ReleaseFrame();
        result
    }
}

unsafe fn capture_rect_dxgi(rect: RECT) -> Result<image::RgbaImage, String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;
        let mut result = image::RgbaImage::new(width, height);
        let mut captured_any = false;
        let mut last_error = None;

        for adapter_index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(adapter_index) else {
                break;
            };
            for output_index in 0.. {
                let Ok(output) = adapter.EnumOutputs(output_index) else {
                    break;
                };
                let output: IDXGIOutput1 = match output.cast() {
                    Ok(output) => output,
                    Err(_) => continue,
                };
                let desc = output
                    .GetDesc()
                    .map_err(|e| format!("GetDesc failed: {e}"))?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let bounds = desc.DesktopCoordinates;
                let left = rect.left.max(bounds.left);
                let top = rect.top.max(bounds.top);
                let right = rect.right.min(bounds.right);
                let bottom = rect.bottom.min(bounds.bottom);
                if right <= left || bottom <= top {
                    continue;
                }

                let (output_bounds, output_image) = match capture_output(&adapter, &output) {
                    Ok(capture) => capture,
                    Err(error) => {
                        last_error = Some(error);
                        continue;
                    }
                };
                copy_rgba_region(
                    &output_image,
                    &mut result,
                    (
                        (left - output_bounds.left) as u32,
                        (top - output_bounds.top) as u32,
                    ),
                    ((left - rect.left) as u32, (top - rect.top) as u32),
                    ((right - left) as u32, (bottom - top) as u32),
                );
                captured_any = true;
            }
        }
        captured_any.then_some(result).ok_or_else(|| {
            last_error.unwrap_or_else(|| {
                "DXGI found no attached display intersecting the capture region".to_string()
            })
        })
    }
}

/// Copies an RGBA sub-rectangle row-by-row. This avoids a bounds check and a
/// method call for every pixel while composing multi-monitor screenshots.
fn copy_rgba_region(
    source: &image::RgbaImage,
    destination: &mut image::RgbaImage,
    (source_x, source_y): (u32, u32),
    (destination_x, destination_y): (u32, u32),
    (width, height): (u32, u32),
) {
    let copy_width = width
        .min(source.width().saturating_sub(source_x))
        .min(destination.width().saturating_sub(destination_x));
    let copy_height = height
        .min(source.height().saturating_sub(source_y))
        .min(destination.height().saturating_sub(destination_y));
    let source_stride = source.width() as usize * 4;
    let destination_stride = destination.width() as usize * 4;
    let row_bytes = copy_width as usize * 4;
    for row in 0..copy_height as usize {
        let source_start = (source_y as usize + row) * source_stride + source_x as usize * 4;
        let destination_start =
            (destination_y as usize + row) * destination_stride + destination_x as usize * 4;
        destination.as_mut()[destination_start..destination_start + row_bytes]
            .copy_from_slice(&source.as_raw()[source_start..source_start + row_bytes]);
    }
}

/// Verifies Desktop Duplication by actually capturing a frame.
///
/// An earlier version stopped at `DuplicateOutput` succeeding, which reports
/// only that duplication can be *set up*. That is not what callers of a
/// diagnostic want to know: a session where duplication initializes but every
/// frame comes back empty passed the check while `use_vision` returned a black
/// image. Take a real frame and look at it.
pub fn dxgi_available() -> Result<(), String> {
    let (image, _) = capture_rect_with_backend(virtual_screen_rect(), Backend::Dxgi)?;
    let mut lit = 0u64;
    for pixel in image.pixels() {
        if pixel.0[0] as u32 + pixel.0[1] as u32 + pixel.0[2] as u32 > 0 {
            lit += 1;
        }
    }
    let total = (image.width() as u64 * image.height() as u64).max(1);
    if lit * 2 < total {
        return Err(format!(
            "DXGI captured a mostly blank frame ({}% of pixels lit); \
             the desktop may be locked or the session may not allow duplication",
            lit * 100 / total
        ));
    }
    Ok(())
}

/// Captures `rect` via GDI `BitBlt` + `GetDIBits`, returning RGBA bytes.
unsafe fn capture_rect_gdi(rect: RECT, width: i32, height: i32) -> Result<Vec<u8>, String> {
    unsafe {
        render_gdi(width, height, |hdc_mem, hdc_screen| {
            BitBlt(
                hdc_mem,
                0,
                0,
                width,
                height,
                Some(hdc_screen),
                rect.left,
                rect.top,
                SRCCOPY,
            )
            .map_err(|e| format!("BitBlt failed: {e}"))
        })
    }
}

/// Has `draw` paint a `width`x`height` memory bitmap, then reads it back as
/// RGBA bytes. `draw` receives the memory DC and the screen DC.
unsafe fn render_gdi(
    width: i32,
    height: i32,
    draw: impl FnOnce(HDC, HDC) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err("GetDC returned a null device context".to_string());
        }

        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return Err("CreateCompatibleDC failed".to_string());
        }

        let hbitmap = CreateCompatibleBitmap(hdc_screen, width, height);
        if hbitmap.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        let old_obj = SelectObject(hdc_mem, hbitmap.into());
        let draw_result = draw(hdc_mem, hdc_screen);

        let mut buffer = vec![0u8; width as usize * height as usize * 4];
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height; // negative height requests a top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let lines_copied = if draw_result.is_ok() {
            GetDIBits(
                hdc_mem,
                hbitmap,
                0,
                height as u32,
                Some(buffer.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbitmap.into());
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        draw_result?;
        if lines_copied == 0 {
            return Err("GetDIBits failed to read captured pixels".to_string());
        }

        // GDI writes 32bpp pixels as BGRA (with a meaningless alpha byte for
        // an opaque desktop capture); swap to RGBA and force full opacity.
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Ok(buffer)
    }
}

/// `PrintWindow` flag that has DWM render the window's full content, including
/// DirectComposition surfaces (Chromium, WinUI) that a plain `WM_PRINT` leaves
/// black. Missing from the `windows` crate's metadata.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

/// Which path produced a window capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCapture {
    /// The window rendered itself; windows in front of it do not show.
    PrintWindow,
    /// The window's region of the screen, taken with this backend; anything
    /// in front of the window shows in the image.
    ScreenRegion(Backend),
}

/// Captures one top-level window and returns it with its visible bounds in
/// virtual desktop coordinates.
///
/// `PrintWindow` asks the window to render itself, so the capture shows the
/// window even while other windows cover it. A few GPU-rendered applications
/// hand back nothing that way; for those, the window's region of the screen
/// is captured instead, provided the window is on the current desktop.
pub fn capture_window(handle: isize) -> Result<(image::RgbaImage, RECT, WindowCapture), String> {
    let hwnd = HWND(handle as *mut c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            return Err("The window is minimized; restore it before capturing".to_string());
        }
        let mut window_rect = RECT::default();
        GetWindowRect(hwnd, &mut window_rect).map_err(|e| format!("GetWindowRect failed: {e}"))?;
        // Since Windows 10 the window rectangle includes invisible resize
        // borders. The extended frame bounds are what is actually drawn.
        let mut bounds = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut bounds as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            bounds = window_rect;
        }
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        if width <= 0 || height <= 0 {
            return Err(format!("The window has no visible area: {width}x{height}"));
        }

        let full_width = window_rect.right - window_rect.left;
        let full_height = window_rect.bottom - window_rect.top;
        let printed = render_gdi(full_width, full_height, |hdc_mem, _| {
            PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT)
                .ok()
                .map_err(|e| format!("PrintWindow failed: {e}"))
        })
        .and_then(|pixels| {
            let full = image::RgbaImage::from_raw(full_width as u32, full_height as u32, pixels)
                .ok_or("Failed to build image buffer from captured pixels")?;
            let mut image = image::RgbaImage::new(width as u32, height as u32);
            copy_rgba_region(
                &full,
                &mut image,
                (
                    (bounds.left - window_rect.left) as u32,
                    (bounds.top - window_rect.top) as u32,
                ),
                (0, 0),
                (width as u32, height as u32),
            );
            Ok(image)
        });
        let print_error = match printed {
            Ok(image) if !is_blank(&image) => {
                return Ok((image, bounds, WindowCapture::PrintWindow));
            }
            Ok(_) => "PrintWindow returned a blank image".to_string(),
            Err(error) => error,
        };

        // The screen only shows the window when it is on the current virtual
        // desktop; a cloaked window's region belongs to whatever is there.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return Err(format!(
                "{print_error}, and the window is not on the current desktop"
            ));
        }
        let (image, backend) = capture_rect_with_backend(bounds, Backend::Auto)?;
        Ok((image, bounds, WindowCapture::ScreenRegion(backend)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_stable() {
        assert_eq!(Backend::Auto.name(), "auto");
        assert_eq!(Backend::Dxgi.name(), "dxgi");
        assert_eq!(Backend::Gdi.name(), "gdi");
    }

    #[test]
    fn capture_rect_rejects_empty_region() {
        let rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 100,
        };
        let err = capture_rect_with_backend(rect, Backend::Gdi).unwrap_err();
        assert!(err.contains("Invalid capture region"));
    }

    #[test]
    fn a_black_frame_is_recognized_as_blank() {
        // What a failed Desktop Duplication hands back: right size, no pixels.
        let black = image::RgbaImage::from_pixel(256, 256, image::Rgba([0, 0, 0, 255]));
        assert!(is_blank(&black));
    }

    #[test]
    fn a_dark_desktop_is_not_blank() {
        // A dark theme is dim, not empty; it must not trigger the fallback.
        let dark = image::RgbaImage::from_pixel(256, 256, image::Rgba([13, 17, 23, 255]));
        assert!(!is_blank(&dark));
    }

    #[test]
    fn a_mostly_black_frame_with_some_content_is_not_blank() {
        // A window on an unlit desktop still counts as a real capture once
        // more than half the sampled pixels carry colour.
        let mut image = image::RgbaImage::from_pixel(256, 256, image::Rgba([0, 0, 0, 255]));
        for (_, _, pixel) in image.enumerate_pixels_mut().filter(|(_, y, _)| *y > 100) {
            *pixel = image::Rgba([40, 40, 40, 255]);
        }
        assert!(!is_blank(&image));
    }

    #[test]
    fn rgba_region_copy_clips_to_both_images() {
        let source = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4]));
        let mut destination = image::RgbaImage::new(2, 2);
        copy_rgba_region(&source, &mut destination, (0, 0), (1, 1), (2, 2));
        assert_eq!(destination.get_pixel(1, 1), &image::Rgba([1, 2, 3, 4]));
        assert_eq!(destination.get_pixel(0, 1), &image::Rgba([0, 0, 0, 0]));
    }

    /// Share of pixels with any colour in them, and the mean channel sum.
    /// A capture that silently produced an empty frame scores zero on both
    /// while still being the right size, which a dimension check misses.
    fn ink(image: &image::RgbaImage) -> (f64, f64) {
        let mut lit = 0u64;
        let mut total = 0u64;
        for pixel in image.pixels() {
            let sum = pixel.0[0] as u64 + pixel.0[1] as u64 + pixel.0[2] as u64;
            total += sum;
            if sum > 0 {
                lit += 1;
            }
        }
        let count = (image.width() as u64 * image.height() as u64).max(1);
        (lit as f64 / count as f64, total as f64 / count as f64)
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn dxgi_captures_the_live_virtual_screen() {
        let (image, backend) =
            capture_rect_with_backend(virtual_screen_rect(), Backend::Dxgi).unwrap();
        assert_eq!(backend, Backend::Dxgi);
        assert!(image.width() > 0 && image.height() > 0);

        // A desktop is never uniformly black, so an all-zero frame means the
        // duplication handed back an empty surface.
        let (lit_fraction, mean) = ink(&image);
        assert!(
            lit_fraction > 0.5 && mean > 1.0,
            "DXGI returned a blank frame: {:.1}% of pixels lit, mean channel sum {mean:.1}",
            lit_fraction * 100.0
        );
    }

    /// `auto` must not pay for Desktop Duplication on a desktop GDI can read.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn auto_uses_gdi_on_a_live_desktop() {
        let (image, backend) =
            capture_rect_with_backend(virtual_screen_rect(), Backend::Auto).unwrap();
        assert_eq!(backend, Backend::Gdi);
        assert!(!is_blank(&image));
    }

    /// The two backends look at the same desktop, so their captures must agree
    /// on roughly how much light is in it. This is what catches one backend
    /// going blank while the other still works.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn both_backends_agree_on_what_is_on_screen() {
        let rect = virtual_screen_rect();
        let (gdi, _) = capture_rect_with_backend(rect, Backend::Gdi).unwrap();
        let (dxgi, _) = capture_rect_with_backend(rect, Backend::Dxgi).unwrap();
        assert_eq!((gdi.width(), gdi.height()), (dxgi.width(), dxgi.height()));

        let (_, gdi_mean) = ink(&gdi);
        let (_, dxgi_mean) = ink(&dxgi);
        let ratio = gdi_mean.max(dxgi_mean) / gdi_mean.min(dxgi_mean).max(f64::EPSILON);
        assert!(
            ratio < 1.5,
            "the backends disagree about the screen: gdi mean {gdi_mean:.1}, dxgi mean {dxgi_mean:.1}"
        );
    }

    /// Capturing on a worker thread and joining it must not take the process
    /// down.
    ///
    /// The cached D3D device lives in thread-local storage, and releasing it
    /// from the TLS destructor — which runs during thread teardown — faulted
    /// inside the graphics runtime, killing the process with a DXGI status
    /// code. Screenshot work runs on Tokio's blocking pool, so every retired
    /// thread crashed the server. `capture_rect_with_backend` is the whole
    /// body here because the crash was in the teardown, not the capture.
    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn a_worker_thread_that_captured_can_exit() {
        std::thread::spawn(|| {
            capture_rect_with_backend(virtual_screen_rect(), Backend::Dxgi)
                .expect("DXGI capture failed on the worker thread");
        })
        .join()
        .expect("the capturing thread did not exit cleanly");
    }
}
