//! Background mode: drive windows without the mouse, the keyboard, or focus.
//!
//! Two things turn it on. `WINDOWS_MCP_BACKGROUND=1` asks for it, so the
//! server never competes with a person working on the same desktop. A locked
//! session forces it: right after locking, injected keys and clicks land on
//! the lock screen, and seconds later Windows rejects them outright. UI
//! Automation patterns and `PrintWindow` keep reaching the user's windows in
//! both cases (checked on Windows 11 against Notepad), so the tools that can
//! switch to those routes do, and the rest refuse instead of sending input.

use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTSFreeMemory,
    WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, OpenInputDesktop,
};
use windows::core::PWSTR;

/// Why background mode is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// `WINDOWS_MCP_BACKGROUND` asked for it.
    Requested,
    /// The session is locked, or a secure desktop (UAC) holds the input.
    Locked,
}

impl Reason {
    fn describe(self) -> &'static str {
        match self {
            Reason::Requested => "WINDOWS_MCP_BACKGROUND is set",
            Reason::Locked => "the session is locked",
        }
    }
}

/// Whether background mode is on, and why. A lock takes precedence because it
/// also rules out whole-desktop capture.
pub fn reason() -> Option<Reason> {
    if session_locked() {
        Some(Reason::Locked)
    } else if requested() {
        Some(Reason::Requested)
    } else {
        None
    }
}

fn requested() -> bool {
    std::env::var("WINDOWS_MCP_BACKGROUND").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on"
        )
    })
}

/// Whether the interactive session cannot take input or show its desktop.
///
/// The WTS lock flag is set the moment the session locks. The input-desktop
/// check catches what it does not report: a UAC prompt, and the point a few
/// seconds into a lock where the input desktop switches to Winlogon.
pub fn session_locked() -> bool {
    wts_locked() || !input_desktop_is_ours()
}

fn wts_locked() -> bool {
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    unsafe {
        if WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
        .is_err()
            || buffer.is_null()
        {
            return false;
        }
        let info = &*(buffer.0 as *const WTSINFOEXW);
        let locked = info.Level == 1
            && info.Data.WTSInfoExLevel1.SessionFlags == WTS_SESSIONSTATE_LOCK as i32;
        WTSFreeMemory(buffer.0 as _);
        locked
    }
}

fn input_desktop_is_ours() -> bool {
    unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) {
            Ok(desktop) => {
                let _ = CloseDesktop(desktop);
                true
            }
            Err(_) => false,
        }
    }
}

const ALTERNATIVES: &str = "Use Snapshot with window=<title>, then InvokeElement, or Type/Scroll \
with a label, which work through UI Automation without input.";

/// Fails when background mode is on, for a tool that can only work by sending
/// mouse or keyboard input or by taking focus.
pub fn ensure_input_allowed(tool: &str) -> Result<(), String> {
    match reason() {
        None => Ok(()),
        Some(reason) => Err(format!(
            "{tool} needs mouse or keyboard input, which background mode does not send ({}). {ALTERNATIVES}",
            reason.describe()
        )),
    }
}

/// Fails while the session is locked, for a capture or scan that needs the
/// visible desktop rather than one named window.
pub fn ensure_desktop_visible(what: &str) -> Result<(), String> {
    if session_locked() {
        return Err(format!(
            "{what} needs the visible desktop, which is unavailable because the session is \
             locked. Pass window=<title> to work with one window instead"
        ));
    }
    Ok(())
}

/// Extra line for responses while the session is locked: Store apps can be
/// suspended and stop exposing their controls until it is unlocked.
pub fn locked_note() -> Option<&'static str> {
    session_locked().then_some(
        "Background mode: the session is locked. Store (UWP) apps such as Calculator may be \
         suspended and expose no controls until it is unlocked.",
    )
}
