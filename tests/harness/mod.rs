//! A real Win32 application used as the target for input and accessibility
//! integration tests.
//!
//! The tests in `tests/` drive the production tool code (`Click`, `Type`,
//! `Snapshot`, `InvokeElement`, ...) against this window and then assert on
//! what the window *actually received*, rather than on the confirmation
//! string the tool returns — a tool that reports `"Typed abc"` proves nothing
//! about what reached the control.
//!
//! The child controls are the stock comctl classes (`BUTTON`, `EDIT`,
//! `LISTBOX`) on purpose: Windows ships UI Automation providers for them, so
//! `Snapshot` sees Invoke/Toggle/SelectionItem patterns coming from a
//! provider this repository does not implement — the same situation as a real
//! application. A custom-drawn window would expose nothing and the UIA tests
//! would be vacuous.

#![allow(dead_code)] // each test binary uses a different part of the harness

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, BM_GETCHECK, BM_SETCHECK, BringWindowToTop, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetForegroundWindow, GetMessageW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, HMENU, HWND_TOP, LB_ADDSTRING, LB_GETCURSEL, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_SHOW, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    SendMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE,
    WM_COMMAND, WM_DESTROY, WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_USER, WNDCLASSW,
    WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WindowFromPoint,
};
use windows::core::{PCWSTR, w};

/// Control ids. These become the `AutomationId` UIA reports for each control,
/// so `uia::find_matching_element`'s AutomationId fast path is exercised with
/// values the tests can predict.
pub const ID_BUTTON: i32 = 1001;
pub const ID_EDIT: i32 = 1002;
pub const ID_CHECKBOX: i32 = 1003;
pub const ID_LISTBOX: i32 = 1004;

/// Control captions, used by tests to find the element in a Snapshot tree.
pub const BUTTON_TEXT: &str = "Run Task";
pub const CHECKBOX_TEXT: &str = "Enable Feature";
pub const LIST_ITEMS: [&str; 3] = ["Alpha", "Bravo", "Charlie"];

/// Something the window observed. Tests assert on these instead of on tool
/// return strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A left click reached the window's client area at these client-relative
    /// coordinates.
    LeftClick(i32, i32),
    RightClick(i32, i32),
    /// The push button was activated (by a real click or by UIA Invoke).
    ButtonClicked,
    /// The checkbox changed state; payload is the new checked state.
    CheckboxToggled(bool),
    /// The listbox selection changed; payload is the selected index.
    ListSelectionChanged(i32),
}

type EventSender = Sender<Event>;

static EVENT_SENDER: OnceLock<Mutex<Option<EventSender>>> = OnceLock::new();

fn emit(event: Event) {
    if let Ok(sender) = EVENT_SENDER.get_or_init(|| Mutex::new(None)).lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.send(event);
    }
}

/// Posted to the window's thread to ask it to shut its message loop down.
const WM_HARNESS_CLOSE: u32 = WM_USER + 1;

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let packed = lparam.0 as u32;
                let x = (packed as u16 as i16) as i32;
                let y = ((packed >> 16) as u16 as i16) as i32;
                emit(if message == WM_LBUTTONDOWN {
                    Event::LeftClick(x, y)
                } else {
                    Event::RightClick(x, y)
                });
                LRESULT(0)
            }
            WM_COMMAND => {
                let control_id = (wparam.0 & 0xFFFF) as i32;
                let control = HWND(lparam.0 as *mut _);
                match control_id {
                    ID_BUTTON => emit(Event::ButtonClicked),
                    ID_CHECKBOX => {
                        let checked =
                            SendMessageW(control, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize;
                        emit(Event::CheckboxToggled(checked));
                    }
                    ID_LISTBOX => {
                        let index = SendMessageW(control, LB_GETCURSEL, None, None).0 as i32;
                        emit(Event::ListSelectionChanged(index));
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HARNESS_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Brings `hwnd` to the foreground.
///
/// A bare `SetForegroundWindow` is refused whenever another process owns the
/// foreground, which is the normal case when a test runs from a terminal: the
/// window stays behind and injected input lands on the terminal instead. This
/// mirrors `window::switch_to`'s `AttachThreadInput` handoff.
unsafe fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        let current_tid = GetCurrentThreadId();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let target_thread = GetWindowThreadProcessId(hwnd, None);

        let _ = AllowSetForegroundWindow(ASFW_ANY);
        let mut attached = Vec::new();
        for thread in [foreground_thread, target_thread] {
            if thread != 0
                && thread != current_tid
                && AttachThreadInput(current_tid, thread, true).as_bool()
            {
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

/// A running test application: a top-level window with child controls, owned
/// by its own thread running a message pump.
pub struct TestApp {
    hwnd: isize,
    button: isize,
    edit: isize,
    checkbox: isize,
    listbox: isize,
    client_origin: POINT,
    events: Receiver<Event>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Layout of the child controls, in client coordinates. Kept here so tests can
/// compute an expected point without duplicating magic numbers.
pub mod layout {
    pub const BUTTON: (i32, i32, i32, i32) = (20, 20, 120, 30);
    pub const EDIT: (i32, i32, i32, i32) = (20, 70, 240, 26);
    pub const CHECKBOX: (i32, i32, i32, i32) = (20, 110, 160, 24);
    pub const LISTBOX: (i32, i32, i32, i32) = (20, 145, 160, 80);
}

impl TestApp {
    /// Creates the window on a dedicated thread and waits until it is visible
    /// and in the foreground.
    ///
    /// Returns `None` when there is no interactive desktop to put a window on
    /// (a headless CI session), so callers can skip rather than fail.
    pub fn launch(title: &str) -> Option<Self> {
        let (window_tx, window_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        *EVENT_SENDER
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(event_tx);

        let title = title.to_string();
        let thread = std::thread::spawn(move || unsafe {
            let module = match GetModuleHandleW(None) {
                Ok(module) => module,
                Err(_) => return,
            };
            let instance = HINSTANCE(module.0);
            let class_name = w!("WindowsOperationCliTestApp");
            // The class persists for the process; a second registration is a
            // harmless failure that must not abort the thread.
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&class);

            let title_wide = wide(&title);
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                PCWSTR(title_wide.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                420,
                300,
                None,
                None,
                Some(instance),
                None,
            ) {
                Ok(hwnd) => hwnd,
                Err(_) => return,
            };

            let button = create_child(
                instance,
                hwnd,
                w!("BUTTON"),
                BUTTON_TEXT,
                // BS_PUSHBUTTON == 0
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                layout::BUTTON,
                ID_BUTTON,
            );
            let edit = create_child(
                instance,
                hwnd,
                w!("EDIT"),
                "",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER,
                layout::EDIT,
                ID_EDIT,
            );
            let checkbox = create_child(
                instance,
                hwnd,
                w!("BUTTON"),
                CHECKBOX_TEXT,
                // BS_AUTOCHECKBOX (0x03) keeps the checked state without the
                // window proc having to maintain it.
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0x0000_0003),
                layout::CHECKBOX,
                ID_CHECKBOX,
            );
            let listbox = create_child(
                instance,
                hwnd,
                w!("LISTBOX"),
                "",
                // LBS_NOTIFY (0x0001) makes the listbox report selection
                // changes through WM_COMMAND.
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WS_BORDER
                    | windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0x0000_0001),
                layout::LISTBOX,
                ID_LISTBOX,
            );
            for item in LIST_ITEMS {
                let text = wide(item);
                SendMessageW(
                    listbox,
                    LB_ADDSTRING,
                    None,
                    Some(LPARAM(text.as_ptr() as isize)),
                );
            }

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
            force_foreground(hwnd);

            let mut origin = POINT::default();
            let _ = ClientToScreen(hwnd, &mut origin);
            if window_tx
                .send((
                    hwnd.0 as isize,
                    button.0 as isize,
                    edit.0 as isize,
                    checkbox.0 as isize,
                    listbox.0 as isize,
                    origin,
                ))
                .is_err()
            {
                let _ = DestroyWindow(hwnd);
                return;
            }

            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });

        let (hwnd, button, edit, checkbox, listbox, client_origin) =
            window_rx.recv_timeout(Duration::from_secs(5)).ok()?;

        let app = Self {
            hwnd,
            button,
            edit,
            checkbox,
            listbox,
            client_origin,
            events: event_rx,
            thread: Some(thread),
        };
        // Activation is asynchronous: the window manager raises the window
        // after the creating thread has already reported its origin. Input
        // sent before it is actually in front lands on whatever still covers
        // it.
        app.wait_until_on_top(Duration::from_secs(3))?;
        Some(app)
    }

    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn button_hwnd(&self) -> isize {
        self.button
    }

    pub fn edit_hwnd(&self) -> isize {
        self.edit
    }

    pub fn checkbox_hwnd(&self) -> isize {
        self.checkbox
    }

    pub fn listbox_hwnd(&self) -> isize {
        self.listbox
    }

    /// Converts a client-area point to screen coordinates — what the input
    /// tools take.
    pub fn client_to_screen(&self, x: i32, y: i32) -> (i32, i32) {
        (self.client_origin.x + x, self.client_origin.y + y)
    }

    /// The screen-coordinate center of one of the `layout` rectangles.
    pub fn center_of(&self, rect: (i32, i32, i32, i32)) -> (i32, i32) {
        let (x, y, width, height) = rect;
        self.client_to_screen(x + width / 2, y + height / 2)
    }

    /// Waits until this window both owns its own pixels and is visible to the
    /// enumeration `Snapshot` walks.
    ///
    /// These settle at different times. `WindowFromPoint` reports the window
    /// as soon as it is drawn, but `EnumWindows` and the virtual-desktop
    /// manager only pick it up a moment later — measured at roughly 200ms
    /// after creation. Waiting on the hit test alone lets a test call
    /// `Snapshot` while the window is still missing from
    /// `list_snapshot_windows()`, which fails with "No foreground window is
    /// available for UI tree scanning".
    pub fn wait_until_on_top(&self, timeout: Duration) -> Option<()> {
        let (x, y) = self.center_of(layout::BUTTON);
        let deadline = Instant::now() + timeout;
        loop {
            let hit = unsafe { WindowFromPoint(POINT { x, y }) };
            let owns_pixels = hit.0 as isize == self.button || hit.0 as isize == self.hwnd;
            if owns_pixels && self.is_enumerable() {
                return Some(());
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether this window appears in the enumeration `Snapshot` scans.
    pub fn is_enumerable(&self) -> bool {
        windows_operation_cli::window::list_snapshot_windows()
            .iter()
            .any(|candidate| candidate.handle == self.hwnd)
    }

    /// Current text of the edit control, read straight from the control.
    pub fn edit_text(&self) -> String {
        let hwnd = HWND(self.edit as *mut _);
        unsafe {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return String::new();
            }
            let mut buffer = vec![0u16; len as usize + 1];
            let copied = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        }
    }

    /// Whether the checkbox is currently checked, read from the control.
    pub fn is_checked(&self) -> bool {
        unsafe {
            SendMessageW(HWND(self.checkbox as *mut _), BM_GETCHECK, None, None).0
                == BST_CHECKED.0 as isize
        }
    }

    pub fn set_checked(&self, checked: bool) {
        let state = if checked { BST_CHECKED.0 } else { BST_UNCHECKED.0 };
        unsafe {
            SendMessageW(
                HWND(self.checkbox as *mut _),
                BM_SETCHECK,
                Some(WPARAM(state as usize)),
                None,
            );
        }
    }

    /// Currently selected listbox index, or -1 when nothing is selected.
    pub fn list_selection(&self) -> i32 {
        unsafe { SendMessageW(HWND(self.listbox as *mut _), LB_GETCURSEL, None, None).0 as i32 }
    }

    /// The window's screen rectangle.
    pub fn window_rect(&self) -> RECT {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(HWND(self.hwnd as *mut _), &mut rect);
        }
        rect
    }

    /// The window's client rectangle.
    pub fn client_rect(&self) -> RECT {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(HWND(self.hwnd as *mut _), &mut rect);
        }
        rect
    }

    /// Waits for the next event the window observes, up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Waits up to `timeout` for an event matching `predicate`, discarding
    /// events that do not match. Input injection produces incidental events
    /// (a click that focuses a control before activating it), so tests that
    /// care about one specific event use this instead of `next_event`.
    pub fn wait_for_event(
        &self,
        timeout: Duration,
        predicate: impl Fn(&Event) -> bool,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let event = self.events.recv_timeout(remaining).ok()?;
            if predicate(&event) {
                return Some(event);
            }
        }
    }

    /// Drains every event received so far, so a later `wait_for_event` only
    /// sees what happens after this point.
    pub fn drain_events(&self) {
        while self.events.try_recv().is_ok() {}
    }

    /// Blocks until `condition` holds or `timeout` elapses. Used for state
    /// the window mutates without sending an event (an edit control's text).
    pub fn wait_until(&self, timeout: Duration, condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

unsafe fn create_child(
    instance: HINSTANCE,
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE,
    rect: (i32, i32, i32, i32),
    id: i32,
) -> HWND {
    let text_wide = wide(text);
    let (x, y, width, height) = rect;
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            PCWSTR(text_wide.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            Some(HMENU(id as *mut _)),
            Some(instance),
            None,
        )
        .unwrap_or_default()
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_HARNESS_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut sender) = EVENT_SENDER.get_or_init(|| Mutex::new(None)).lock() {
            *sender = None;
        }
    }
}

/// Serializes tests that need the foreground window and inject input, which is
/// a process-wide resource: two tests clicking at once would steal each
/// other's focus.
pub fn desktop_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
