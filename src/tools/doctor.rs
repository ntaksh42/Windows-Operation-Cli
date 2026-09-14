//! `Doctor` tool: reports environment diagnostics as JSON.

use rmcp::schemars;
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DoctorParams {}

pub fn doctor() -> String {
    let mut blockers = Vec::new();

    let uia = crate::uia::ensure_com_initialized().and_then(|()| {
        crate::uia::create_automation()
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    if let Err(error) = &uia {
        blockers.push(format!("UI Automation initialization failed: {error}"));
    }

    let monitor_count = crate::display::get_displays().len();
    if monitor_count == 0 {
        blockers.push("No active monitors were detected.".to_string());
    }

    let dxgi = crate::capture::dxgi_available();
    if let Err(error) = &dxgi {
        blockers.push(format!("DXGI capture initialization failed: {error}"));
    }

    let shell = ["pwsh.exe", "powershell.exe"]
        .into_iter()
        .find(|name| command_exists(name));
    if shell.is_none() {
        blockers.push("Neither pwsh.exe nor powershell.exe was found on PATH.".to_string());
    }

    let elevated = unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() };
    let virtual_desktop = crate::vdm::api_available();
    if let Err(error) = &virtual_desktop {
        blockers.push(format!(
            "Virtual desktop API initialization failed: {error}"
        ));
    }

    serde_json::to_string_pretty(&serde_json::json!({
        "checks": {
            "uia_com": uia.is_ok(),
            "monitor_count": monitor_count,
            "dxgi": dxgi.is_ok(),
            "powershell": shell,
            "administrator": elevated,
            "virtual_desktop_api": virtual_desktop.is_ok(),
        },
        "blockers": blockers,
    }))
    .expect("doctor report only contains serializable values")
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new("where.exe")
        .arg(name)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_detection_rejects_missing_program() {
        assert!(!command_exists(
            "windows-operation-cli-definitely-missing.exe"
        ));
    }
}
