use anyhow::Result;
use rmcp::{ServiceExt, transport::stdio};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use windows_operation_cli::server::WindowsOperationCliServer;
use windows_operation_cli::{apps, tool_policy};

#[tokio::main]
async fn main() -> Result<()> {
    // UI Automation rectangles and desktop capture use physical pixels. Set this
    // before any HWND-dependent API so cursor, monitor, and input coordinates
    // remain in that same coordinate space on mixed-DPI desktops.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if !tool_policy::tool_is_disabled("App") {
        apps::prefetch_start_apps();
    }
    let service = WindowsOperationCliServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
