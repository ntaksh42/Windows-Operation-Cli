#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

struct McpServer {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Value>,
}

impl McpServer {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_windows-operation-cli"))
            .env("WINDOWS_MCP_DISABLED_TOOLS", "App,Click")
            .env("WINDOWS_MCP_INPUT_OVERLAY", "0")
            .env_remove("WINDOWS_MCP_AUDIT_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("server stdout");
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str::<Value>(&line)
                    && sender.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("server stdin");
        writeln!(stdin, "{message}").expect("write request");
        stdin.flush().expect("flush request");
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    /// Sends a request and returns the response with the matching id,
    /// skipping notifications and anything else the server emits meanwhile.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let message = self
                .responses
                .recv_timeout(remaining)
                .unwrap_or_else(|_| panic!("no response to request {id} ({method})"));
            if message.get("method").is_none() && message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.request(id, "tools/call", json!({"name": name, "arguments": arguments}))
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn result_text(response: &Value) -> String {
    response["result"]["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn assert_tool_error(response: &Value, expected_text: &str) {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], json!(true), "{response}");
    let text = result_text(response);
    assert!(text.contains(expected_text), "{text}");
}

#[test]
fn mcp_contract_preserves_business_errors_and_rejects_invalid_requests() {
    let mut server = McpServer::spawn();

    let initialize = server.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "regression-test", "version": "1"},
        }),
    );
    assert!(initialize.get("error").is_none(), "{initialize}");
    assert!(initialize["result"].is_object(), "{initialize}");
    server.notify("notifications/initialized");

    let tools = server.request(2, "tools/list", json!({}));
    let listed = tools["result"]["tools"].as_array().expect("tools array");
    assert!(
        listed.iter().any(|tool| tool["name"] == "Snapshot"),
        "{tools}"
    );

    let click = server.call_tool(3, "Click", json!({"loc": [10, 20]}));
    assert_tool_error(&click, "disabled");

    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("__woc_mcp_missing_file__.txt");
    let read = server.call_tool(
        4,
        "FileSystem",
        json!({"mode": "read", "path": missing.to_string_lossy()}),
    );
    assert!(read.get("error").is_none(), "{read}");
    assert_eq!(read["result"]["isError"], json!(false), "{read}");
    assert!(result_text(&read).contains("Error: File not found"), "{read}");

    let timeout = server.call_tool(5, "Snapshot", json!({"timeout_ms": 1}));
    assert_tool_error(&timeout, "timeout_ms");

    let dom_without_tree =
        server.call_tool(6, "Snapshot", json!({"use_dom": true, "use_ui_tree": false}));
    assert_tool_error(&dom_without_tree, "requires use_ui_tree");

    let screenshot = server.call_tool(
        7,
        "Screenshot",
        json!({"window": "not opened", "display": [0]}),
    );
    assert_tool_error(&screenshot, "cannot be combined");

    let missing_mode = server.call_tool(8, "FileSystem", json!({"path": "unused"}));
    assert_tool_error(&missing_mode, "missing field `mode`");
}
