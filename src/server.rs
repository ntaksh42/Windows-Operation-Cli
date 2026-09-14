//! `ServerHandler` implementation and tool router for the MCP server.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
};

use crate::tool_policy::ToolCall;
use crate::tools::app::{self, AppParams};
use crate::tools::caret_info::{self, CaretInfoParams};
use crate::tools::click::{self, ClickParams};
use crate::tools::clipboard::{self, ClipboardParams};
use crate::tools::cursor_position::{self, CursorPositionParams};
use crate::tools::display_inventory::{self, DisplayInventoryParams};
use crate::tools::doctor::{self, DoctorParams};
use crate::tools::filesystem::{self, FileSystemParams};
use crate::tools::invoke_element::{self, InvokeElementParams};
use crate::tools::move_mouse::{self, MoveParams};
use crate::tools::multi_edit::{self, MultiEditParams};
use crate::tools::multi_select::{self, MultiSelectParams};
use crate::tools::notification::{self, NotificationParams};
use crate::tools::process::{self, ProcessParams};
use crate::tools::registry::{self, RegistryParams};
use crate::tools::scrape::{self, ScrapeParams};
use crate::tools::screenshot::{self, ScreenshotParams};
use crate::tools::scroll::{self, ScrollParams};
use crate::tools::shell::{self, PowerShellParams};
use crate::tools::shortcut::{self, ShortcutParams};
use crate::tools::snapshot::{self, SnapshotParams};
use crate::tools::typing::{self, TypeParams};
use crate::tools::wait::{self, WaitParams};
use crate::tools::wait_for::{self, WaitForParams};

macro_rules! begin_tool {
    ($name:literal) => {
        match ToolCall::begin($name) {
            Ok(call) => call,
            Err(message) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(message)]));
            }
        }
    };
}

/// Wraps a tool's `Result<String, String>` into an MCP `CallToolResult`:
/// `Ok` becomes success content, `Err` becomes an `isError` result (an
/// expected, caller-facing failure — not a protocol-level error).
fn as_call_result(
    result: Result<String, String>,
    audit: ToolCall,
) -> Result<CallToolResult, McpError> {
    audit.finish(result.is_ok());
    match result {
        Ok(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
        Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
    }
}

fn text_succeeded(message: &str) -> bool {
    !message.trim_start().starts_with("Error")
}

/// MCP server exposing Windows desktop automation tools.
#[derive(Debug, Clone)]
pub struct WindowsComputerUseServer;

#[tool_router]
impl WindowsComputerUseServer {
    #[tool(
        name = "InvokeElement",
        description = "Invokes a structured UI element id from the most recent Snapshot using UI Automation semantics. Set fallback_to_click=true to explicitly allow a validated coordinate click when no semantic action is available."
    )]
    async fn invoke_element(
        &self,
        Parameters(params): Parameters<InvokeElementParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("InvokeElement");
        let result = tokio::task::spawn_blocking(move || invoke_element::invoke_element(params))
            .await
            .unwrap_or_else(|e| Err(format!("InvokeElement tool panicked: {e}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Wait",
        description = "Wait for a number of seconds (1-60) before returning.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn wait(
        &self,
        Parameters(WaitParams { duration }): Parameters<WaitParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Wait");
        let result = wait::wait(duration).await;
        audit.finish(result.is_ok());
        match result {
            Ok(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    #[tool(
        name = "WaitFor",
        description = "Polls the desktop (no screenshot) until a condition is met or timeout elapses. condition: text_exists/active_window/element_exists/element_enabled/focused_element (aliases: text/window/element/enabled/focused). text/window_name provide the target to match (casefold substring). timeout (default 10s, max 120s) and interval (default 0.25s, max 5s) control polling. Returns an error on timeout.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn wait_for_tool(
        &self,
        Parameters(params): Parameters<WaitForParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("WaitFor");
        let result = tokio::task::spawn_blocking(move || wait_for::wait_for(params))
            .await
            .unwrap_or_else(|e| Err(format!("WaitFor tool panicked: {e}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Click",
        description = "Performs mouse clicks at specified coordinates [x, y] or passing a UI element's label/id. Supports button types: 'left' for selection/activation, 'right' for context menus, 'middle'. Supports clicks: 0=hover only, 1=single, 2=double, 3=triple. modifier optionally holds shift/ctrl/alt/win during the click. Provide either loc or label."
    )]
    async fn click(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Click");
        let result = tokio::task::spawn_blocking(move || click::click(params))
            .await
            .unwrap_or_else(|error| Err(format!("Click tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "CursorPosition",
        description = "Returns the current mouse cursor position in screen coordinates.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cursor_position(
        &self,
        Parameters(CursorPositionParams {}): Parameters<CursorPositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("CursorPosition");
        audit.finish(true);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            cursor_position::cursor_position(),
        )]))
    }

    #[tool(
        name = "CaretInfo",
        description = "Returns the caret position or up to 200 characters of selected text from the focused UI Automation text element.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn caret_info(
        &self,
        Parameters(CaretInfoParams {}): Parameters<CaretInfoParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("CaretInfo");
        let result = tokio::task::spawn_blocking(caret_info::caret_info)
            .await
            .unwrap_or_else(|error| Err(format!("CaretInfo tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Type",
        description = "Types text at specified coordinates [x, y] or passing a UI element's label/id. Set clear=True to clear existing text first, False to append. Set press_enter=True to submit after typing. Set caret_position to 'start' (beginning), 'end' (end), or 'idle' (default). Provide either loc or label."
    )]
    async fn type_tool(
        &self,
        Parameters(params): Parameters<TypeParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Type");
        let result = tokio::task::spawn_blocking(move || typing::type_text(params))
            .await
            .unwrap_or_else(|error| Err(format!("Type tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Scroll",
        description = "Scrolls at coordinates [x, y], a UI element's label/id, or current mouse position if loc=None. Type: vertical (default) or horizontal. Direction: up/down for vertical, left/right for horizontal. modifier optionally holds shift/ctrl/alt/win during scrolling. wheel_times controls amount (1 wheel ≈ 3-5 lines)."
    )]
    async fn scroll(
        &self,
        Parameters(params): Parameters<ScrollParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Scroll");
        let result = tokio::task::spawn_blocking(move || scroll::scroll(params))
            .await
            .unwrap_or_else(|error| Err(format!("Scroll tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Move",
        description = "Moves mouse cursor to coordinates [x, y] or passing a UI element's label/id. Set drag=True to perform a drag-and-drop operation from the current mouse position to the target coordinates, or provide from_loc=[x, y] to make the drag explicit-start and atomic in one tool call. Optional duration controls bounded intermediate movement. Default (drag=False) is a simple cursor move (hover). Provide either loc or label."
    )]
    async fn move_tool(
        &self,
        Parameters(params): Parameters<MoveParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Move");
        let result = tokio::task::spawn_blocking(move || move_mouse::move_mouse(params))
            .await
            .unwrap_or_else(|error| Err(format!("Move tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "Shortcut",
        description = "Executes keyboard shortcuts using key combinations separated by +. Examples: \"ctrl+c\" (copy), \"ctrl+v\" (paste), \"alt+tab\" (switch apps), \"win+r\" (Run dialog), \"win\" (Start menu), \"ctrl+shift+esc\" (Task Manager). Use for quick actions and system commands."
    )]
    async fn shortcut(
        &self,
        Parameters(params): Parameters<ShortcutParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Shortcut");
        let result = tokio::task::spawn_blocking(move || shortcut::shortcut(params))
            .await
            .unwrap_or_else(|error| Err(format!("Shortcut tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "MultiSelect",
        description = "Selects multiple items such as files, folders, or checkboxes if press_ctrl=True, or performs multiple clicks if False. Pass locs (list of coordinates) or labels (list of UI element labels/ids)."
    )]
    async fn multi_select(
        &self,
        Parameters(params): Parameters<MultiSelectParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("MultiSelect");
        let result = tokio::task::spawn_blocking(move || multi_select::multi_select(params))
            .await
            .unwrap_or_else(|error| Err(format!("MultiSelect tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "MultiEdit",
        description = "Enters text into multiple input fields at specified coordinates locs=[[x,y,text], ...] or using labels=[[label,text], ...]. Provide either locs or labels."
    )]
    async fn multi_edit(
        &self,
        Parameters(params): Parameters<MultiEditParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("MultiEdit");
        let result = tokio::task::spawn_blocking(move || multi_edit::multi_edit(params))
            .await
            .unwrap_or_else(|error| Err(format!("MultiEdit tool panicked: {error}")));
        as_call_result(result, audit)
    }

    #[tool(
        name = "DisplayInventory",
        description = "Read active display layout and DPI metadata. Reports display index, device name, monitor/work-area bounds, resolution, orientation, primary flag, effective DPI, and scale.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn display_inventory(
        &self,
        Parameters(DisplayInventoryParams {}): Parameters<DisplayInventoryParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("DisplayInventory");
        audit.finish(true);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            display_inventory::display_inventory(),
        )]))
    }

    #[tool(
        name = "Doctor",
        description = "Runs environment diagnostics for UI Automation, displays, DXGI capture, PowerShell, administrator status, and virtual desktop support. Returns JSON with checks and blockers.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn doctor(
        &self,
        Parameters(DoctorParams {}): Parameters<DoctorParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Doctor");
        let result = tokio::task::spawn_blocking(doctor::doctor).await;
        audit.finish(result.is_ok());
        let message = result
            .unwrap_or_else(|error| format!(r#"{{"blockers":["Doctor tool panicked: {error}"]}}"#));
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    #[tool(
        name = "Screenshot",
        description = "Captures a fast screenshot-first desktop snapshot with cursor position, desktop/window summaries, and an image. This path skips UI tree extraction for speed. Use Snapshot when you need interactive element ids, scrollable regions, or browser DOM extraction. Note: the returned image may be downscaled for efficiency; when it is, multiply image coordinates by the ratio of original size to displayed size to get the actual screen coordinates for mouse actions (Click, Move, etc.).",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn screenshot(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Screenshot");
        let result = tokio::task::spawn_blocking(move || screenshot::screenshot(&params))
            .await
            .unwrap_or_else(|e| Err(format!("Screenshot tool panicked: {e}")));
        audit.finish(result.is_ok());
        match result {
            Ok(output) => Ok(CallToolResult::success(vec![
                ContentBlock::text(output.text),
                ContentBlock::image(BASE64.encode(output.png_bytes), "image/png"),
            ])),
            Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Error capturing screenshot: {e}. Please try again."
            ))])),
        }
    }

    #[tool(
        name = "Snapshot",
        description = "Captures desktop state and a structured UI accessibility map. UI tree scanning defaults to the foreground window; use window for one fuzzy title match or scope=all for whole-desktop discovery. Element lines include generation-scoped ids and supported actions for InvokeElement. use_vision=true adds an annotated screenshot. timeout_ms (default 2000, range 100-30000) bounds the total UIA scan.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Snapshot");
        let result = tokio::task::spawn_blocking(move || snapshot::snapshot(&params))
            .await
            .unwrap_or_else(|e| Err(format!("Snapshot tool panicked: {e}")));
        audit.finish(result.is_ok());
        match result {
            Ok(output) => {
                let mut content = vec![ContentBlock::text(output.text)];
                if let Some(png_bytes) = output.png_bytes {
                    content.push(ContentBlock::image(BASE64.encode(png_bytes), "image/png"));
                }
                Ok(CallToolResult::success(content))
            }
            Err(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
        }
    }

    #[tool(
        name = "FileSystem",
        description = "Manages file system operations: 'read' (supports negative tail offsets), 'write', 'edit' (unique exact replacement), 'copy', 'move', 'delete' (supports recursive and dry_run), 'list', 'search' (glob plus optional content_pattern regex), and 'info'. Relative paths are resolved from the user's Desktop folder. Use absolute paths to access other locations."
    )]
    async fn file_system(
        &self,
        Parameters(params): Parameters<FileSystemParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("FileSystem");
        let message = filesystem::file_system(params);
        audit.finish(text_succeeded(&message));
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    #[tool(
        name = "Registry",
        description = "Read and write the Windows Registry. Keywords: regedit, registry key, HKEY, HKCU, HKLM, Windows settings, registry value. Use mode=\"get\" to read a value, mode=\"set\" to create/update a value, mode=\"delete\" to remove a value or key, mode=\"list\" to list values and sub-keys under a path. Paths use PowerShell format (e.g. \"HKCU:\\Software\\MyApp\", \"HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\")."
    )]
    async fn registry(
        &self,
        Parameters(params): Parameters<RegistryParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Registry");
        let message = registry::registry(params);
        audit.finish(text_succeeded(&message));
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    #[tool(
        name = "Scrape",
        description = "Fetch/scrape web page content from a URL. Keywords: scrape, fetch, browse, web, URL, extract, download, read webpage. Performs a lightweight HTTP request to the URL and returns the page content converted to Markdown.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn scrape(
        &self,
        Parameters(params): Parameters<ScrapeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Scrape");
        let result = scrape::scrape(params, Some(&context.peer)).await;
        audit.finish(result.is_ok());
        match result {
            Ok(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    #[tool(
        name = "PowerShell",
        description = "Shell/command execution. A comprehensive system tool for executing any PowerShell commands: navigate the file system, manage files and processes, and execute system-level operations."
    )]
    async fn power_shell(
        &self,
        Parameters(PowerShellParams { command, timeout }): Parameters<PowerShellParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("PowerShell");
        let message = tokio::task::spawn_blocking(move || shell::powershell(&command, timeout))
            .await
            .unwrap_or_else(|e| format!("Response: Command execution failed: {e}\nStatus Code: 1"));
        audit.finish(message.contains("Status Code: 0"));
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    #[tool(
        name = "App",
        description = "Open/start/launch applications and manage windows. Four modes: 'launch' (opens an application by Start Menu name), 'launch_executable' (launches one executable path with separated argv and optional cwd), 'resize' (adjusts a named or active window), and 'switch' (brings a specific window into focus)."
    )]
    async fn app(
        &self,
        Parameters(params): Parameters<AppParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("App");
        let result = tokio::task::spawn_blocking(move || app::app(params))
            .await
            .unwrap_or_else(|e| Err(format!("App tool panicked: {e}")));
        audit.finish(result.is_ok());
        match result {
            Ok(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    #[tool(
        name = "Clipboard",
        description = "Copy/paste clipboard operations. Keywords: copy, paste, cut, clipboard, text transfer. Use mode=\"get\" to read current clipboard content, mode=\"set\" to set clipboard text."
    )]
    async fn clipboard(
        &self,
        Parameters(ClipboardParams { mode, text }): Parameters<ClipboardParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Clipboard");
        let message = clipboard::clipboard(mode, text);
        audit.finish(text_succeeded(&message));
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    #[tool(
        name = "Notification",
        description = "Sends a Windows toast notification with a title and message."
    )]
    async fn notification(
        &self,
        Parameters(NotificationParams {
            title,
            message,
            app_id,
        }): Parameters<NotificationParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Notification");
        let result = notification::send_notification(&title, &message, &app_id);
        audit.finish(text_succeeded(&result));
        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
    }

    #[tool(
        name = "Process",
        description = "List and kill running processes. Use mode=\"list\" to list running processes with filtering and sorting options. Use mode=\"kill\" to terminate processes by PID or name."
    )]
    async fn process(
        &self,
        Parameters(params): Parameters<ProcessParams>,
    ) -> Result<CallToolResult, McpError> {
        let audit = begin_tool!("Process");
        let result = tokio::task::spawn_blocking(move || process::process(params))
            .await
            .unwrap_or_else(|e| Err(format!("Process tool panicked: {e}")));
        audit.finish(result.is_ok());
        match result {
            Ok(message) => Ok(CallToolResult::success(vec![ContentBlock::text(message)])),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }
}

// `name` must be set explicitly: rmcp's default `Implementation::from_build_env()`
// bakes in `env!("CARGO_CRATE_NAME")` from rmcp's own build, not this crate's,
// so the server would otherwise report itself as "rmcp".
#[tool_handler(
    name = "windows-computeruse",
    instructions = "Windows desktop automation MCP server."
)]
impl ServerHandler for WindowsComputerUseServer {}
