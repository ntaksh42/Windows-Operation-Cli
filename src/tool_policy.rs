//! Cross-cutting tool configuration and audit logging.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static AUDIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static INPUT_LOCK: Mutex<()> = Mutex::new(());

/// Serialize complete MCP input operations, without locking their nested helpers.
pub fn with_input_lock<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = INPUT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    operation()
}

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

pub fn tool_is_disabled(name: &str) -> bool {
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
    fn input_operations_do_not_overlap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    with_input_lock(|| {
                        assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    })
                });
            }
        });
    }

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
