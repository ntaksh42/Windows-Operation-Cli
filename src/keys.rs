//! Key-name resolution for the `Shortcut` tool: maps a `+`-separated token
//! like `"ctrl"`, `"esc"`, or `"r"` to a virtual-key code.
//!
//! Ports the alias table and `SpecialKeyNames` lookup from the Python
//! reference (`windows_mcp.uia.enums.SpecialKeyNames`, `_KEY_ALIASES` in
//! `windows_mcp.desktop.service`), trimmed to the simultaneous-chord model
//! described in docs/SPEC.md §12 rather than the full `SendKeys` escape
//! parser.

use windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW;

/// Aliases applied (case-insensitively) before the named-key lookup.
/// Mirrors `_KEY_ALIASES` in the Python reference exactly.
fn alias(name: &str) -> &str {
    match name {
        "backspace" => "back",
        "capslock" => "capital",
        "scrolllock" => "scroll",
        "windows" | "command" => "win",
        "option" => "alt",
        // `+` is the separator between keys, so a shortcut that *contains*
        // the plus key ("ctrl++" for zoom in) cannot be written literally.
        // These names give it a spelling: `ctrl+plus`, `ctrl+minus`.
        "plus" => "add",
        "minus" => "subtract",
        other => other,
    }
}

/// Named virtual keys, keyed by uppercase name (post-alias). Covers the
/// modifiers, navigation, editing, and function keys documented in
/// docs/SPEC.md's Shortcut examples plus their common synonyms.
fn named_vk(name: &str) -> Option<u16> {
    let vk = match name {
        "SHIFT" => 0x10,
        "CTRL" | "CONTROL" => 0x11,
        "ALT" | "MENU" => 0x12,
        "PAUSE" => 0x13,
        "CAPITAL" => 0x14,
        "ESC" | "ESCAPE" => 0x1B,
        "SPACE" => 0x20,
        "PRIOR" | "PAGEUP" => 0x21,
        "NEXT" | "PAGEDOWN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "PRINTSCREEN" | "SNAPSHOT" => 0x2C,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "BACK" => 0x08,
        "TAB" => 0x09,
        "CLEAR" => 0x0C,
        "RETURN" | "ENTER" => 0x0D,
        "APPS" | "MENUKEY" => 0x5D,
        "WIN" | "LWIN" => 0x5B,
        "RWIN" => 0x5C,
        "SLEEP" => 0x5F,
        "NUMPAD0" => 0x60,
        "NUMPAD1" => 0x61,
        "NUMPAD2" => 0x62,
        "NUMPAD3" => 0x63,
        "NUMPAD4" => 0x64,
        "NUMPAD5" => 0x65,
        "NUMPAD6" => 0x66,
        "NUMPAD7" => 0x67,
        "NUMPAD8" => 0x68,
        "NUMPAD9" => 0x69,
        "MULTIPLY" => 0x6A,
        "ADD" => 0x6B,
        "SEPARATOR" => 0x6C,
        "SUBTRACT" => 0x6D,
        "DECIMAL" => 0x6E,
        "DIVIDE" => 0x6F,
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        "F13" => 0x7C,
        "F14" => 0x7D,
        "F15" => 0x7E,
        "F16" => 0x7F,
        "F17" => 0x80,
        "F18" => 0x81,
        "F19" => 0x82,
        "F20" => 0x83,
        "F21" => 0x84,
        "F22" => 0x85,
        "F23" => 0x86,
        "F24" => 0x87,
        "NUMLOCK" => 0x90,
        "SCROLL" => 0x91,
        "LSHIFT" => 0xA0,
        "RSHIFT" => 0xA1,
        "LCONTROL" | "LCTRL" => 0xA2,
        "RCONTROL" | "RCTRL" => 0xA3,
        "LALT" | "LMENU" => 0xA4,
        "RALT" | "RMENU" => 0xA5,
        _ => return None,
    };
    Some(vk)
}

/// Resolves one `+`-separated shortcut token (already trimmed) to a
/// virtual-key code.
///
/// Single alphanumeric characters map directly to their VK code (which for
/// `'A'..='Z'`/`'0'..='9'` equals the uppercase ASCII value). Other single
/// characters fall back to `VkKeyScanW` to find the key that types them on
/// the active keyboard layout. Everything else goes through the alias table
/// and then the named-key table.
pub fn resolve_key(token: &str) -> Result<u16, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("Shortcut key must not be empty".to_string());
    }

    let mut chars = trimmed.chars();
    let first = chars.next().unwrap();
    if chars.next().is_none() {
        if first.is_ascii_alphanumeric() {
            return Ok(first.to_ascii_uppercase() as u16);
        }
        let scan = unsafe { VkKeyScanW(first as u16) };
        if scan != -1 {
            return Ok((scan as u16) & 0xFF);
        }
        return Err(format!("Unknown shortcut key: {token:?}"));
    }

    let lower = trimmed.to_ascii_lowercase();
    let upper = alias(&lower).to_ascii_uppercase();
    named_vk(&upper).ok_or_else(|| format!("Unknown shortcut key: {token:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_single_char_letters_and_digits() {
        assert_eq!(resolve_key("c").unwrap(), b'C' as u16);
        assert_eq!(resolve_key("R").unwrap(), b'R' as u16);
        assert_eq!(resolve_key("5").unwrap(), b'5' as u16);
    }

    #[test]
    fn resolves_aliases() {
        assert_eq!(resolve_key("backspace").unwrap(), 0x08);
        assert_eq!(resolve_key("capslock").unwrap(), 0x14);
        assert_eq!(resolve_key("scrolllock").unwrap(), 0x91);
        assert_eq!(resolve_key("windows").unwrap(), 0x5B);
        assert_eq!(resolve_key("command").unwrap(), 0x5B);
        assert_eq!(resolve_key("option").unwrap(), 0x12);
    }

    #[test]
    fn resolves_named_keys_case_insensitively() {
        assert_eq!(resolve_key("ctrl").unwrap(), 0x11);
        assert_eq!(resolve_key("Ctrl").unwrap(), 0x11);
        assert_eq!(resolve_key("ESC").unwrap(), 0x1B);
        assert_eq!(resolve_key("win").unwrap(), 0x5B);
        assert_eq!(resolve_key("F5").unwrap(), 0x74);
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(resolve_key("notakey").is_err());
    }

    #[test]
    fn the_plus_key_has_a_name_because_the_separator_takes_the_symbol() {
        // "ctrl++" cannot be written: the second `+` is read as a separator
        // and leaves an empty token. `plus` and `minus` are the way to say it.
        assert_eq!(resolve_key("plus").unwrap(), 0x6B);
        assert_eq!(resolve_key("minus").unwrap(), 0x6D);
        assert_eq!(resolve_key("add").unwrap(), 0x6B);

        let chord: Result<Vec<u16>, String> = "ctrl+plus".split('+').map(resolve_key).collect();
        assert_eq!(chord.unwrap(), vec![0x11, 0x6B]);
    }

    #[test]
    fn a_missing_key_between_separators_is_rejected() {
        for shortcut in ["ctrl+", "+c", "ctrl++", ""] {
            let parsed: Result<Vec<u16>, String> =
                shortcut.split('+').map(resolve_key).collect();
            assert!(
                parsed.is_err(),
                "{shortcut:?} should be rejected, got {parsed:?}"
            );
        }
    }
}
