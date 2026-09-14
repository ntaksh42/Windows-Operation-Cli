//! Screen capture backends.
//!
use std::cell::RefCell;
use std::env;
use std::ffi::c_void;

use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use windows::core::Interface;

const DXGI_ACQUIRE_TIMEOUT_MS: u32 = 100;

type AdapterKey = (u32, i32);

thread_local! {
    // Screenshot requests run on Tokio's blocking threads. Reusing the D3D
    // device per such thread avoids recreating an expensive device for every
    // output on every request, while keeping COM objects thread-affine.
    static DXGI_DEVICES: RefCell<Vec<(AdapterKey, ID3D11Device, ID3D11DeviceContext)>> = const {
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
/// `auto` prefers Desktop Duplication and falls back to GDI when unavailable
/// (for example on a locked desktop or an unsupported display adapter).
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
        Backend::Auto => match unsafe { capture_rect_dxgi(rect) } {
            Ok(image) => Ok((image, Backend::Dxgi)),
            Err(_) => capture_gdi_image(rect, width, height).map(|image| (image, Backend::Gdi)),
        },
        Backend::Gdi => capture_gdi_image(rect, width, height).map(|image| (image, Backend::Gdi)),
        Backend::Dxgi => unsafe { capture_rect_dxgi(rect) }.map(|image| (image, Backend::Dxgi)),
    }
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
        if let Some((_, device, context)) =
            devices.iter().find(|(stored_key, _, _)| *stored_key == key)
        {
            return Ok((device.clone(), context.clone()));
        }
        let (device, context) = unsafe { create_device(adapter) }?;
        devices.push((key, device.clone(), context.clone()));
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
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        duplication
            .AcquireNextFrame(DXGI_ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut resource)
            .map_err(|e| format!("AcquireNextFrame failed: {e}"))?;

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

/// Verifies that Desktop Duplication can be initialized for at least one
/// attached output without acquiring a frame.
pub fn dxgi_available() -> Result<(), String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1 failed: {e}"))?;
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
                    Err(error) => {
                        last_error = Some(error.to_string());
                        continue;
                    }
                };
                if !output
                    .GetDesc()
                    .map_err(|e| format!("GetDesc failed: {e}"))?
                    .AttachedToDesktop
                    .as_bool()
                {
                    continue;
                }
                let (device, _) = create_device(&adapter)?;
                match output.DuplicateOutput(&device) {
                    Ok(_) => return Ok(()),
                    Err(error) => last_error = Some(format!("DuplicateOutput failed: {error}")),
                }
            }
        }
        Err(last_error.unwrap_or_else(|| "DXGI found no attached output".to_string()))
    }
}

/// Captures `rect` via GDI `BitBlt` + `GetDIBits`, returning RGBA bytes.
unsafe fn capture_rect_gdi(rect: RECT, width: i32, height: i32) -> Result<Vec<u8>, String> {
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
        let blit_result = BitBlt(
            hdc_mem,
            0,
            0,
            width,
            height,
            Some(hdc_screen),
            rect.left,
            rect.top,
            SRCCOPY,
        );

        let mut buffer = vec![0u8; width as usize * height as usize * 4];
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = -height; // negative height requests a top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;

        let lines_copied = if blit_result.is_ok() {
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

        if let Err(e) = blit_result {
            return Err(format!("BitBlt failed: {e}"));
        }
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
    fn rgba_region_copy_clips_to_both_images() {
        let source = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4]));
        let mut destination = image::RgbaImage::new(2, 2);
        copy_rgba_region(&source, &mut destination, (0, 0), (1, 1), (2, 2));
        assert_eq!(destination.get_pixel(1, 1), &image::Rgba([1, 2, 3, 4]));
        assert_eq!(destination.get_pixel(0, 1), &image::Rgba([0, 0, 0, 0]));
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn dxgi_captures_the_live_virtual_screen() {
        let (image, backend) =
            capture_rect_with_backend(virtual_screen_rect(), Backend::Dxgi).unwrap();
        assert_eq!(backend, Backend::Dxgi);
        assert!(image.width() > 0 && image.height() > 0);
    }
}
