//! Minimal top-level window enumeration and manipulation used by the App
//! tool's `resize`/`switch` modes (docs/SPEC.md §1).
//!
//! This works directly against Win32 top-level windows via `EnumWindows`
//! rather than the UIA-based Snapshot tree service.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, BringWindowToTop, EnumWindows, FindWindowW, GA_ROOT,
    GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, HWND_TOP, IsIconic, IsWindow, IsWindowVisible,
    IsZoomed, MoveWindow, SW_RESTORE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetForegroundWindow,
    SetWindowPos, ShowWindow, WindowFromPoint,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

/// A visible, titled top-level window.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub handle: isize,
    pub title: String,
    pub pid: u32,
}

/// Classes belonging to shell chrome (desktop, taskbar), excluded from enumeration.
const EXCLUDED_CLASSES: &[&str] = &["Progman", "Shell_TrayWnd"];

/// Enumerates visible, titled top-level windows, skipping desktop/taskbar shell windows.
pub fn list_windows() -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
        );
    }
    windows
}

/// Visible titled windows belonging to the active virtual desktop.
pub fn list_current_windows() -> Vec<WindowInfo> {
    windows_for_table(&list_snapshot_windows())
}

/// Narrows a Snapshot walk list down to the windows the response table shows.
///
/// [`list_snapshot_windows`] is a superset of this: it additionally keeps
/// empty-titled windows and the shell's own taskbar/desktop windows, which
/// the UIA walk needs but the table does not. Snapshot used to build the two
/// lists with independent `EnumWindows` passes; deriving one from the other
/// halves the enumeration work per capture.
pub fn windows_for_table(scanned: &[SnapshotWindow]) -> Vec<WindowInfo> {
    scanned
        .iter()
        .filter(|window| !window.title.is_empty())
        .filter(|window| !EXCLUDED_CLASSES.contains(&window.class_name.as_str()))
        .filter(|window| !window.title.trim().contains("Overlay"))
        .map(|window| WindowInfo {
            handle: window.handle,
            title: window.title.clone(),
            pid: window.pid,
        })
        .collect()
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        if EXCLUDED_CLASSES.contains(&window_class_name(hwnd).as_str()) {
            return true.into();
        }
        let title = window_title(hwnd);
        if title.is_empty() {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        windows.push(WindowInfo {
            handle: hwnd.0 as isize,
            title,
            pid,
        });
        true.into()
    }
}

fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..copied])
    }
}

fn window_class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = vec![0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..len])
    }
}

/// A window candidate for Snapshot's accessibility-tree walk. Broader than
/// [`list_windows`] (used by the App tool): includes empty-titled windows
/// plus the taskbar (`Shell_TrayWnd`) and desktop (`Progman`), since Snapshot
/// needs to walk their UI trees too (docs/SPEC.md §6 item 3).
#[derive(Debug, Clone)]
pub struct SnapshotWindow {
    pub handle: isize,
    pub title: String,
    pub class_name: String,
    pub pid: u32,
    /// The owning executable's file name, resolved once on first use.
    ///
    /// `is_browser` and `is_firefox` are called several times per window
    /// inside Snapshot's scan loop; resolving the name on every call meant an
    /// `OpenProcess` and a 32KiB allocation each time. The inner `None` is a
    /// resolved "the process could not be queried", distinct from the outer
    /// "not looked up yet".
    executable: OnceCell<Option<String>>,
}

impl SnapshotWindow {
    pub fn new(handle: isize, title: String, class_name: String, pid: u32) -> Self {
        Self {
            handle,
            title,
            class_name,
            pid,
            executable: OnceCell::new(),
        }
    }

    fn executable_name(&self) -> Option<&str> {
        self.executable
            .get_or_init(|| process_executable_name(self.pid))
            .as_deref()
    }

    /// Whether this window hosts web content whose DOM can be walked.
    ///
    /// Chromium hands every renderer window the `Chrome_WidgetWin_1` class,
    /// so the class covers Chrome and Edge along with the Electron apps built
    /// on the same engine (VS Code, Slack, Discord) — all of which expose
    /// their page through the same Document root. Matching on the class
    /// instead of an executable allowlist keeps Electron working without
    /// naming each app. Firefox uses its own class and is listed explicitly.
    pub fn is_browser(&self) -> bool {
        if matches!(
            self.class_name.as_str(),
            "Chrome_WidgetWin_1" | "MozillaWindowClass"
        ) {
            return true;
        }
        self.executable_name()
            .is_some_and(|name| matches!(name, "chrome.exe" | "msedge.exe" | "firefox.exe"))
    }

    pub fn is_firefox(&self) -> bool {
        self.executable_name() == Some("firefox.exe")
    }
}

fn process_executable_name(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // `QueryFullProcessImageNameW` reports the length it needs, so start
        // at MAX_PATH and grow only for the rare longer path rather than
        // allocating 32KiB for every window.
        let mut buffer = vec![0u16; 260];
        let name = loop {
            let mut length = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                process,
                Default::default(),
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            );
            if result.is_ok() {
                break Some(String::from_utf16_lossy(&buffer[..length as usize]));
            }
            if buffer.len() >= 32_768 {
                break None;
            }
            buffer.resize(buffer.len() * 4, 0);
        };
        let _ = CloseHandle(process);
        std::path::Path::new(&name?)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase())
    }
}

unsafe extern "system" fn enum_snapshot_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let windows = &mut *(lparam.0 as *mut Vec<SnapshotWindow>);
        if !IsWindowVisible(hwnd).as_bool()
            || !crate::vdm::is_window_on_current_desktop(hwnd.0 as isize)
        {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        windows.push(SnapshotWindow::new(
            hwnd.0 as isize,
            window_title(hwnd),
            window_class_name(hwnd),
            pid,
        ));
        true.into()
    }
}

fn find_window_by_class(class_name: &str) -> Option<HWND> {
    let mut wide: Vec<u16> = class_name.encode_utf16().collect();
    wide.push(0);
    unsafe {
        let hwnd = FindWindowW(PCWSTR(wide.as_ptr()), PCWSTR::null()).ok()?;
        if hwnd.is_invalid() { None } else { Some(hwnd) }
    }
}

/// Enumerates visible top-level windows (including empty-titled ones), plus
/// the taskbar and desktop shell windows, for the Snapshot tool's UIA walk.
pub fn list_snapshot_windows() -> Vec<SnapshotWindow> {
    let mut windows: Vec<SnapshotWindow> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(enum_snapshot_proc),
            LPARAM(&mut windows as *mut Vec<SnapshotWindow> as isize),
        );
    }
    for class_name in ["Shell_TrayWnd", "Shell_SecondaryTrayWnd", "Progman"] {
        if let Some(hwnd) = find_window_by_class(class_name)
            && !windows.iter().any(|w| w.handle == hwnd.0 as isize)
        {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            windows.push(SnapshotWindow::new(
                hwnd.0 as isize,
                window_title(hwnd),
                class_name.to_string(),
                pid,
            ));
        }
    }
    windows
}

/// Reads a window's current screen bounds as `(x, y, width, height)`.
pub fn get_window_rect(handle: isize) -> Option<(i32, i32, i32, i32)> {
    window_rect(handle)
}

/// Whether `(x, y)` is covered by a window belonging to a different top-level
/// window than `owner_handle`.
///
/// UI Automation reports an element's bounds regardless of what is drawn on
/// top of it, so a control behind another window still looks clickable. A
/// click there lands on whatever is in front. `WindowFromPoint` returns the
/// window that would actually receive the click; anything in `owner_handle`'s
/// own top-level chain (its child controls) counts as the owner itself.
pub fn is_point_occluded(x: i32, y: i32, owner_handle: isize) -> bool {
    unsafe {
        let hit = WindowFromPoint(POINT { x, y });
        if hit.0.is_null() {
            // Nothing there to intercept the click; treat it as reachable and
            // let the existing bounds checks decide.
            return false;
        }
        let owner = HWND(owner_handle as *mut _);
        if hit == owner {
            return false;
        }
        // A hit on one of the owner's own controls resolves to the same
        // top-level window.
        let hit_root = GetAncestor(hit, GA_ROOT);
        let owner_root = GetAncestor(owner, GA_ROOT);
        if !hit_root.0.is_null() && hit_root == owner_root {
            return false;
        }
        true
    }
}

/// Finds the best fuzzy-name match (score_cutoff 70) among currently open windows.
pub fn find_by_name(name: &str) -> Option<WindowInfo> {
    let windows = list_windows();
    let titles: Vec<&str> = windows.iter().map(|w| w.title.as_str()).collect();
    let (matched_title, _) = crate::fuzzy::extract_one(name, titles, 70.0)?;
    let matched_title = matched_title.to_string();
    windows.into_iter().find(|w| w.title == matched_title)
}

/// Returns the current foreground (active) top-level window, if any.
/// Used by `App` `resize` mode when no `name` is given.
pub fn foreground_window() -> Option<WindowInfo> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let title = window_title(hwnd);
        if title.is_empty() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        Some(WindowInfo {
            handle: hwnd.0 as isize,
            title,
            pid,
        })
    }
}

pub fn is_minimized(handle: isize) -> bool {
    unsafe { IsIconic(HWND(handle as *mut _)).as_bool() }
}

/// Whether a window can currently receive input.
///
/// A window that put up a modal dialog disables itself, and every click on it
/// is discarded. UI Automation does not pass that on: the disabled window's
/// child controls keep reporting `IsEnabled = true`, so a capture offers
/// buttons that do nothing when clicked. Ask Win32 about the top-level window
/// instead.
pub fn is_enabled(handle: isize) -> bool {
    unsafe { IsWindowEnabled(HWND(handle as *mut _)).as_bool() }
}

pub fn is_maximized(handle: isize) -> bool {
    unsafe { IsZoomed(HWND(handle as *mut _)).as_bool() }
}

fn window_rect(handle: isize) -> Option<(i32, i32, i32, i32)> {
    let hwnd = HWND(handle as *mut _);
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect).ok()?;
    }
    Some((
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}

/// Resizes/moves a window, defaulting unset `loc`/`size` to its current bounds.
/// Returns the applied `(x, y, width, height)`.
pub fn resize_window(
    handle: isize,
    loc: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
) -> Result<(i32, i32, i32, i32), String> {
    let (cur_x, cur_y, cur_w, cur_h) =
        window_rect(handle).ok_or("Failed to read window bounds.")?;
    let (x, y) = loc.unwrap_or((cur_x, cur_y));
    let (w, h) = size.unwrap_or((cur_w, cur_h));
    unsafe {
        MoveWindow(HWND(handle as *mut _), x, y, w, h, true).map_err(|e| e.to_string())?;
    }
    Ok((x, y, w, h))
}

/// Brings `handle` to the foreground, matching the Python reference's
/// `bring_window_to_top` (AttachThreadInput dance for cross-thread focus transfer).
pub fn switch_to(handle: isize) {
    let hwnd = HWND(handle as *mut _);
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }

        let foreground = GetForegroundWindow();
        if !IsWindow(Some(foreground)).as_bool() {
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
            return;
        }

        let mut fg_pid = 0u32;
        let foreground_thread = GetWindowThreadProcessId(foreground, Some(&mut fg_pid));
        let mut tgt_pid = 0u32;
        let target_thread = GetWindowThreadProcessId(hwnd, Some(&mut tgt_pid));
        let current_tid = GetCurrentThreadId();

        if foreground_thread == 0 || target_thread == 0 || foreground_thread == target_thread {
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
            return;
        }

        let _ = AllowSetForegroundWindow(ASFW_ANY);

        let mut attached = Vec::new();
        for thread in [foreground_thread, target_thread] {
            if thread != current_tid && AttachThreadInput(current_tid, thread, true).as_bool() {
                attached.push(thread);
            }
        }

        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );

        for thread in attached.into_iter().rev() {
            let _ = AttachThreadInput(current_tid, thread, false);
        }
    }
}

/// Waits up to `timeout` for a window belonging to `pid` (if given) or whose
/// title contains `name` (case-insensitive) to appear.
pub fn wait_for_window(pid: Option<u32>, name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let name_lower = name.to_lowercase();
    loop {
        let windows = list_windows();
        if pid.is_some_and(|pid| windows.iter().any(|w| w.pid == pid)) {
            return true;
        }
        if windows
            .iter()
            .any(|w| w.title.to_lowercase().contains(&name_lower))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// Occlusion is covered by `tests/occlusion.rs`, which stacks two windows it
// owns. The tests that lived here derived their geometry from whatever window
// happened to be in the foreground and returned early when there was none, so
// they asserted nothing on a headless CI runner while still reporting success.
