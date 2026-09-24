//! Start Menu application discovery and launch logic for the App tool's
//! `launch` mode (docs/SPEC.md §1).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use crate::{powershell, win};

/// Maps lowercase Start Menu app name -> AppID (for `shell:AppsFolder\<AppID>`)
/// or a filesystem path to a `.lnk`/executable.
pub fn get_apps_from_start_menu() -> HashMap<String, String> {
    let (output, status) =
        powershell::execute_command("Get-StartApps | ConvertTo-Csv -NoTypeInformation", 10, None);
    if status == 0 && !output.trim().is_empty() {
        let apps = parse_start_apps_csv(&output);
        if !apps.is_empty() {
            return apps;
        }
    }
    apps_from_shortcuts()
}

fn parse_start_apps_csv(csv_text: &str) -> HashMap<String, String> {
    let mut apps = HashMap::new();
    let mut reader = csv::Reader::from_reader(csv_text.as_bytes());
    let Ok(headers) = reader.headers() else {
        return apps;
    };
    let Some(name_idx) = headers.iter().position(|h| h == "Name") else {
        return apps;
    };
    let Some(appid_idx) = headers.iter().position(|h| h == "AppID") else {
        return apps;
    };
    for record in reader.records().flatten() {
        let name = record.get(name_idx).unwrap_or("").trim();
        let appid = record.get(appid_idx).unwrap_or("").trim();
        if !name.is_empty() && !appid.is_empty() {
            apps.entry(name.to_lowercase())
                .or_insert_with(|| appid.to_string());
        }
    }
    apps
}

/// Scans Start Menu shortcut folders for `.lnk` files as a fallback for `Get-StartApps`.
fn apps_from_shortcuts() -> HashMap<String, String> {
    let mut apps = HashMap::new();
    let program_data =
        std::env::var("PROGRAMDATA").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let bases = [
        format!(r"{program_data}\Microsoft\Windows\Start Menu\Programs"),
        format!(r"{appdata}\Microsoft\Windows\Start Menu\Programs"),
    ];
    for base in bases {
        let base_path = Path::new(&base);
        if !base_path.is_dir() {
            continue;
        }
        for lnk in find_lnk_files(base_path) {
            if let Some(stem) = lnk.file_stem().and_then(|s| s.to_str()) {
                apps.entry(stem.to_lowercase())
                    .or_insert_with(|| lnk.to_string_lossy().into_owned());
            }
        }
    }
    apps
}

fn find_lnk_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(find_lnk_files(&path));
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            out.push(path);
        }
    }
    out
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Title-cases each whitespace-separated word, mirroring Python's `str.title()`
/// as used for the App tool's response messages.
pub fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Launches a Start Menu app matching `name` (fuzzy, score_cutoff 70).
/// Returns `(response, status_code, pid, matched_name)`; `pid` is `None` when unknown
/// (e.g. `shell:AppsFolder` launches, which `Start-Process` does not report).
/// The Start Menu listing from the last lookup. `Get-StartApps` is a
/// PowerShell run of its own — most of a launch's time — and the listing only
/// changes when software is installed, so it is fetched again only when a name
/// is not found in it.
static START_APPS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// Fills the Start Menu listing in the background, so the first `App` launch
/// does not wait on `Get-StartApps` (~1.3s, most of it PowerShell start-up).
/// A launch that arrives first blocks on the same lock rather than fetching
/// the listing a second time.
pub fn prefetch_start_apps() {
    std::thread::spawn(|| {
        let mut cached = START_APPS.lock().unwrap_or_else(PoisonError::into_inner);
        if cached.is_none() {
            *cached = Some(get_apps_from_start_menu());
        }
    });
}

/// Finds the Start Menu entry best matching `name`, as `(key, AppID or path)`.
fn find_start_app(name: &str) -> Option<(String, String)> {
    let mut cached = START_APPS.lock().unwrap_or_else(PoisonError::into_inner);
    for refresh in [false, true] {
        if refresh || cached.is_none() {
            *cached = Some(get_apps_from_start_menu());
        }
        let apps = cached.as_ref().expect("filled above");
        let keys: Vec<&str> = apps.keys().map(String::as_str).collect();
        if let Some((key, _)) = crate::fuzzy::extract_one(name, keys, 70.0) {
            let key = key.to_string();
            let target = apps[&key].clone();
            return Some((key, target));
        }
        if refresh {
            break;
        }
    }
    None
}

/// Activates a packaged app through the shell, in-process. Going through
/// `Start-Process` cost a PowerShell run, and checking the AppID first
/// another; the shell refuses an unknown AppID by itself.
fn open_apps_folder_item(app_id: &str) -> bool {
    let _ = crate::uia::ensure_com_initialized();
    let target = HSTRING::from(format!("shell:AppsFolder\\{app_id}"));
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute reports success as a value greater than 32.
    result.0 as usize > 32
}

pub fn launch_app(name: &str) -> (String, i32, Option<u32>, String) {
    let Some((matched_key, appid)) = find_start_app(name) else {
        return (
            format!("{} not found in start menu.", title_case(name)),
            1,
            None,
            name.to_string(),
        );
    };
    if Path::new(&appid).exists() || appid.contains('\\') {
        let exe_path = win::resolve_known_folder_guid_path(&appid);
        let command = format!(
            "Start-Process {} -PassThru | Select-Object -ExpandProperty Id",
            ps_quote(&exe_path)
        );
        let (response, status) = powershell::execute_command(&command, 10, None);
        let pid = if status == 0 {
            response.trim().parse::<u32>().ok()
        } else {
            None
        };
        (response, status, pid, matched_key)
    } else if open_apps_folder_item(&appid) {
        (String::new(), 0, None, matched_key)
    } else {
        (
            format!("Invalid app identifier: {appid}"),
            1,
            None,
            matched_key,
        )
    }
}
