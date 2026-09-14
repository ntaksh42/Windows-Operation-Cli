//! Cross-cutting tool configuration and audit logging.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static AUDIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug)]
pub struct ToolCall {
    name: &'static str,
    finished: bool,
}

impl ToolCall {
    pub fn begin(name: &'static str) -> Result<Self, String> {
        if tool_is_disabled(name) {
            write_audit(name, false);
            return Err(format!("Tool '{name}' is disabled by configuration."));
        }
        Ok(Self {
            name,
            finished: false,
        })
    }

    pub fn finish(mut self, ok: bool) {
        write_audit(self.name, ok);
        self.finished = true;
    }
}

impl Drop for ToolCall {
    fn drop(&mut self) {
        if !self.finished {
            write_audit(self.name, false);
        }
    }
}

fn tool_is_disabled(name: &str) -> bool {
    std::env::var("WINDOWS_MCP_DISABLED_TOOLS")
        .ok()
        .is_some_and(|configured| {
            configured
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate.eq_ignore_ascii_case(name))
        })
}

fn write_audit(tool: &str, ok: bool) {
    let Ok(path) = std::env::var("WINDOWS_MCP_AUDIT_LOG") else {
        return;
    };
    let Ok(_lock) = AUDIT_LOCK.get_or_init(|| Mutex::new(())).lock() else {
        return;
    };
    let record = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "tool": tool,
        "ok": ok,
    });
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_names_are_case_insensitive_and_trimmed() {
        unsafe { std::env::set_var("WINDOWS_MCP_DISABLED_TOOLS", " Click,SCROLL ") };
        assert!(tool_is_disabled("click"));
        assert!(tool_is_disabled("Scroll"));
        assert!(!tool_is_disabled("Move"));
        unsafe { std::env::remove_var("WINDOWS_MCP_DISABLED_TOOLS") };
    }

    #[test]
    fn audit_log_contains_only_required_fields() {
        let path = std::env::temp_dir().join(format!(
            "windows-operation-cli-audit-test-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        unsafe { std::env::set_var("WINDOWS_MCP_AUDIT_LOG", &path) };
        ToolCall::begin("CursorPosition").unwrap().finish(true);
        unsafe { std::env::remove_var("WINDOWS_MCP_AUDIT_LOG") };

        let line = std::fs::read_to_string(&path).unwrap();
        let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record["tool"], "CursorPosition");
        assert_eq!(record["ok"], true);
        assert!(record["ts"].is_string());
        assert_eq!(record.as_object().unwrap().len(), 3);
        std::fs::remove_file(path).unwrap();
    }
}
