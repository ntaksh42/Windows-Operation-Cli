use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

mod apps;
mod capture;
mod display;
mod fuzzy;
mod ia2;
mod input_sim;
mod keys;
mod params;
mod powershell;
mod server;
mod state;
mod tool_policy;
mod tools;
mod uia;
mod vdm;
mod win;
mod window;

use server::WindowsComputerUseServer;

#[tokio::main]
async fn main() -> Result<()> {
    // UI Automation rectangles and desktop capture use physical pixels. Set this
    // before any HWND-dependent API so cursor, monitor, and input coordinates
    // remain in that same coordinate space on mixed-DPI desktops.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let service = WindowsComputerUseServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
