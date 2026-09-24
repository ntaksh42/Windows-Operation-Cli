//! PowerShell command execution, mirroring the Python reference's
//! `windows_mcp.powershell` package (docs/SPEC.md §3).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use windows::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleCP};
use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
use windows::Win32::System::WindowsProgramming::{GetComputerNameW, GetUserNameW};
use windows::core::PWSTR;
use windows_registry::{CURRENT_USER, LOCAL_MACHINE, Type};

/// Grace period after `CTRL_BREAK_EVENT` before force-killing the process tree.
const GRACE_PERIOD: Duration = Duration::from_secs(2);
/// Poll interval while waiting for process exit / pipe drain.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

const FALLBACK_PATHEXT: &str =
    ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC;.CPL;.PY;.PYW";

/// A shell started ahead of the command it will run, parked in the bootstrap
/// waiting for that command on stdin.
///
/// Starting `pwsh` is ~500ms of every call — .NET start-up and engine
/// initialization — against a few milliseconds for a typical command. Each
/// call still runs in its own fresh process, exactly as before; the process was
/// just started while the previous call's caller was busy with something else.
struct WarmShell {
    shell: String,
    env: HashMap<String, String>,
    child: Child,
}

static WARM_SHELL: Mutex<Option<WarmShell>> = Mutex::new(None);

/// Takes the parked shell if it was started with exactly this shell and
/// environment. A mismatch — the registry environment changed since, say —
/// means it would not run the command the way a fresh start would, so it is
/// discarded.
fn take_warm_shell(shell: &str, env: &HashMap<String, String>) -> Option<Child> {
    let mut warm = WARM_SHELL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()?;
    if warm.shell == shell && &warm.env == env && matches!(warm.child.try_wait(), Ok(None)) {
        return Some(warm.child);
    }
    let _ = warm.child.kill();
    let _ = warm.child.wait();
    None
}

/// Starts the shell for the next call in the background.
fn park_warm_shell(shell: String, env: HashMap<String, String>) {
    thread::spawn(move || {
        let Ok(child) = spawn_shell(&shell, &env) else {
            return;
        };
        let previous = WARM_SHELL
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(WarmShell { shell, env, child });
        if let Some(mut previous) = previous {
            let _ = previous.child.kill();
            let _ = previous.child.wait();
        }
    });
}

/// Starts `shell` running the bootstrap, which reads the command from stdin.
fn spawn_shell(shell: &str, env: &HashMap<String, String>) -> std::io::Result<Child> {
    let mut args = vec!["-NoProfile".to_string()];
    if shell_basename_lower(shell) == "powershell" {
        args.push("-OutputFormat".to_string());
        args.push("Text".to_string());
    }
    args.push("-EncodedCommand".to_string());
    args.push(build_encoded_command(BOOTSTRAP));

    Command::new(shell)
        .args(&args)
        .current_dir(crate::win::home_dir())
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(creation_flags())
        .spawn()
}

/// `CREATE_NEW_PROCESS_GROUP`, plus `CREATE_NO_WINDOW` when this server has no
/// console of its own.
///
/// A console-less parent makes the shell open a console window of its own,
/// which takes the foreground from whatever the caller was driving — and a
/// parked shell would keep it open. With a console to share, the shell uses
/// that one, which is also what lets `CTRL_BREAK_EVENT` reach it on timeout.
fn creation_flags() -> u32 {
    let has_console = unsafe { GetConsoleCP() } != 0;
    if has_console {
        CREATE_NEW_PROCESS_GROUP.0
    } else {
        CREATE_NEW_PROCESS_GROUP.0 | CREATE_NO_WINDOW.0
    }
}

/// Executes a PowerShell `command`, returning `(output, status_code)`.
///
/// `shell_override` forces a specific shell executable (mainly for tests);
/// pass `None` to auto-detect `pwsh` (falling back to Windows PowerShell 5.1).
pub fn execute_command(
    command: &str,
    timeout_secs: u64,
    shell_override: Option<&str>,
) -> (String, i32) {
    let shell = shell_override
        .map(str::to_string)
        .unwrap_or_else(pick_shell);

    let mut env = prepare_env();
    env.insert("NO_COLOR".to_string(), "1".to_string());

    let child = match take_warm_shell(&shell, &env) {
        Some(child) => Ok(child),
        None => spawn_shell(&shell, &env),
    };
    park_warm_shell(shell, env);
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return (format!("Command execution failed: {e}"), 1),
    };
    // Dropping stdin closes it, which is what ends the bootstrap's read. A
    // write error means the shell already exited; its status says why.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(BASE64.encode(command.as_bytes()).as_bytes());
    }

    let stdout_rx = spawn_reader(child.stdout.take().expect("stdout piped"));
    let stderr_rx = spawn_reader(child.stderr.take().expect("stderr piped"));

    let mut pending = PendingResult::default();
    if !pending.wait_until(
        &mut child,
        &stdout_rx,
        &stderr_rx,
        Instant::now() + Duration::from_secs(timeout_secs),
    ) {
        // Stage 1: ask the process group to exit gracefully.
        let pid = child.id();
        unsafe {
            let _ = GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid);
        }
        if !pending.wait_until(
            &mut child,
            &stdout_rx,
            &stderr_rx,
            Instant::now() + GRACE_PERIOD,
        ) {
            // Stage 2: force-kill the whole process tree.
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            // Best-effort drain so the child handle/reader threads are reaped
            // rather than leaked; the timeout result below is returned either way.
            let _ = pending.wait_until(
                &mut child,
                &stdout_rx,
                &stderr_rx,
                Instant::now() + GRACE_PERIOD,
            );
        }
        return ("Command execution timed out".to_string(), 1);
    }

    let status = pending.status.expect("status set once wait_until succeeds");
    let stdout = String::from_utf8_lossy(&pending.stdout.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&pending.stderr.unwrap_or_default()).into_owned();
    let mut output = if !stdout.is_empty() { stdout } else { stderr };
    let code = status.code().unwrap_or(1);

    if code != 0 && output.contains("Access is denied") && !crate::win::is_elevated() {
        output.push_str(
            "\n\nHINT: This command may require an elevated (Administrator) terminal. \
             The Windows-MCP server is currently running at a lower integrity level.",
        );
    }

    (output, code)
}

/// Accumulates the three pieces of a completed command (exit status, stdout,
/// stderr) across possibly multiple bounded wait attempts, mirroring the
/// Python reference's staged `communicate(timeout=...)` retries: once the
/// first attempt times out, the caller always reports a timeout regardless
/// of whether a later cleanup stage manages to drain everything.
#[derive(Default)]
struct PendingResult {
    status: Option<ExitStatus>,
    stdout: Option<Vec<u8>>,
    stderr: Option<Vec<u8>>,
}

impl PendingResult {
    /// Polls until the exit status and both pipes are available, or `deadline` passes.
    fn wait_until(
        &mut self,
        child: &mut Child,
        stdout_rx: &Receiver<Vec<u8>>,
        stderr_rx: &Receiver<Vec<u8>>,
        deadline: Instant,
    ) -> bool {
        loop {
            if self.status.is_none()
                && let Ok(Some(status)) = child.try_wait()
            {
                self.status = Some(status);
            }
            if self.stdout.is_none()
                && let Ok(bytes) = stdout_rx.try_recv()
            {
                self.stdout = Some(bytes);
            }
            if self.stderr.is_none()
                && let Ok(bytes) = stderr_rx.try_recv()
            {
                self.stderr = Some(bytes);
            }
            if self.status.is_some() && self.stdout.is_some() && self.stderr.is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

fn spawn_reader(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// What every shell runs: read the base64 UTF-8 command from stdin, then run
/// it the way `-Command` would.
///
/// The command runs dot-sourced, in the script scope `-Command` uses. `-Command`
/// exits 1 when the last statement failed, which `$?` after a dot-sourced
/// script block does not reflect, so the check is appended to the command
/// itself. A command that does not parse exits 1 with no output, as it does
/// under `-Command`. The first line warms the formatter and script-block compiler while
/// the shell is still parked, rather than on the caller's time.
const BOOTSTRAP: &str = "$null = Get-ChildItem -LiteralPath $PSHOME -Filter *.dll | \
    Select-Object -First 2 | Format-Table | Out-String; \
    . $(try { [scriptblock]::Create([System.Text.Encoding]::UTF8.GetString(\
    [Convert]::FromBase64String([Console]::In.ReadToEnd())) + \"`n\" + 'if (-not $?) { exit 1 }') } \
    catch { exit 1 })";

/// Builds the `-EncodedCommand` payload: UTF-8 output prefix, encoded as
/// UTF-16LE, then base64. See docs/SPEC.md §3.
fn build_encoded_command(command: &str) -> String {
    let prefixed = format!(
        "$OutputEncoding = [System.Text.Encoding]::UTF8; \
         [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; {command}"
    );
    let mut bytes = Vec::with_capacity(prefixed.len() * 2);
    for unit in prefixed.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    BASE64.encode(bytes)
}

fn shell_basename_lower(shell: &str) -> String {
    std::path::Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(shell)
        .to_lowercase()
}

fn pick_shell() -> String {
    if which("pwsh").is_some() {
        "pwsh".to_string()
    } else {
        "powershell".to_string()
    }
}

/// Minimal `shutil.which` equivalent: scans `PATH` (current process
/// environment), applying `PATHEXT` extensions when `name` has none.
pub(crate) fn which(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let has_ext = std::path::Path::new(name).extension().is_some();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| FALLBACK_PATHEXT.to_string());
    let exts: Vec<String> = pathext
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_lowercase)
        .collect();

    for dir in std::env::split_paths(&path_var) {
        if has_ext {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        } else {
            for ext in &exts {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Prepares a complete environment block for the PowerShell subprocess,
/// filling in variables missing from the (possibly stripped) inherited
/// environment from the registry. See docs/SPEC.md §3.
fn prepare_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    let (machine_vars, machine_path, machine_pathext) = read_reg_env(
        LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let (user_vars, user_path, user_pathext) = read_reg_env(CURRENT_USER, r"Environment");

    // HKCU takes precedence over HKLM for same-named keys, matching Windows'
    // resolution order. Existing (inherited) values are never overwritten.
    let mut merged = machine_vars;
    merged.extend(user_vars);
    for (name, value) in merged {
        if env.get(&name).is_none_or(String::is_empty) {
            env.insert(name, value);
        }
    }

    if !machine_path.is_empty() || !user_path.is_empty() {
        let current_path = env.get("PATH").cloned().unwrap_or_default();
        env.insert(
            "PATH".to_string(),
            dedup_path(&[&current_path, &machine_path, &user_path]),
        );
    }

    let effective_pathext = if !user_pathext.is_empty() {
        user_pathext
    } else {
        machine_pathext
    };
    let current_pathext_has_exe = env
        .get("PATHEXT")
        .is_some_and(|v| v.to_uppercase().contains(".EXE"));
    if !effective_pathext.is_empty() && !current_pathext_has_exe {
        env.insert("PATHEXT".to_string(), effective_pathext);
    }

    if env.get("COMPUTERNAME").is_none_or(String::is_empty)
        && let Some(name) = win32_computer_name()
    {
        env.insert("COMPUTERNAME".to_string(), name);
    }
    if env.get("USERNAME").is_none_or(String::is_empty)
        && let Some(name) = win32_user_name()
    {
        env.insert("USERNAME".to_string(), name);
    }

    let user_profile = crate::win::home_dir();
    env.entry("USERPROFILE".to_string())
        .or_insert_with(|| user_profile.clone());
    let (drive, tail) = split_drive(&user_profile);
    env.entry("HOMEDRIVE".to_string()).or_insert(drive);
    env.entry("HOMEPATH".to_string()).or_insert(tail);
    let computername = env.get("COMPUTERNAME").cloned().unwrap_or_default();
    env.entry("USERDOMAIN".to_string()).or_insert(computername);

    env
}

/// Reads all `REG_SZ`/`REG_EXPAND_SZ` values from `subkey` under `root`.
/// Returns `(other_vars, path, pathext)`; on any registry error, returns
/// empty results (matching the Python reference's best-effort behavior).
fn read_reg_env(
    root: &windows_registry::Key,
    subkey: &str,
) -> (HashMap<String, String>, String, String) {
    let mut vars = HashMap::new();
    let mut path = String::new();
    let mut pathext = String::new();

    let Ok(key) = root.open(subkey) else {
        return (vars, path, pathext);
    };
    let Ok(iter) = key.values() else {
        return (vars, path, pathext);
    };

    for (name, value) in iter {
        let ty = value.ty();
        if !matches!(ty, Type::String | Type::ExpandString) {
            continue;
        }
        let Ok(mut text) = String::try_from(value) else {
            continue;
        };
        if ty == Type::ExpandString {
            text = crate::win::expand_env_string(&text);
        }
        match name.to_uppercase().as_str() {
            "PATH" => path = text,
            "PATHEXT" => pathext = text,
            _ => {
                vars.insert(name, text);
            }
        }
    }

    (vars, path, pathext)
}

/// Joins non-empty PATH segments and deduplicates entries (case-insensitive,
/// ignoring a trailing backslash), keeping the first occurrence's original
/// casing and position — matching the Python reference's `_dedup_path`.
fn dedup_path(segments: &[&str]) -> String {
    let joined = segments
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(";");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in joined.split(';') {
        let norm = entry.to_lowercase();
        let norm = norm.trim_end_matches('\\');
        if !norm.is_empty() && seen.insert(norm.to_string()) {
            out.push(entry);
        }
    }
    out.join(";")
}

/// Splits `path` into `(drive, tail)`, e.g. `"C:\Users\foo"` -> `("C:", "\Users\foo")`.
fn split_drive(path: &str) -> (String, String) {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        (path[..2].to_string(), path[2..].to_string())
    } else {
        (String::new(), path.to_string())
    }
}

fn win32_computer_name() -> Option<String> {
    let mut buf = vec![0u16; 256];
    let mut len = buf.len() as u32;
    unsafe {
        GetComputerNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len).ok()?;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn win32_user_name() -> Option<String> {
    let mut buf = vec![0u16; 256];
    let mut len = buf.len() as u32;
    unsafe {
        GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len).ok()?;
    }
    // GetUserNameW reports the length including the trailing NUL on success.
    let n = (len as usize).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_command_round_trips() {
        let encoded = build_encoded_command("Get-Location");
        let decoded_bytes = BASE64.decode(encoded).unwrap();
        let utf16: Vec<u16> = decoded_bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let decoded = String::from_utf16(&utf16).unwrap();
        assert!(decoded.contains("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8;"));
        assert!(decoded.ends_with("Get-Location"));
    }

    #[test]
    fn dedup_path_prefers_first_occurrence_case_insensitively() {
        let result = dedup_path(&[r"C:\A;C:\B", r"C:\a\", r"C:\C"]);
        assert_eq!(result, r"C:\A;C:\B;C:\C");
    }

    #[test]
    fn dedup_path_skips_empty_segments() {
        let result = dedup_path(&["", r"C:\A", ""]);
        assert_eq!(result, r"C:\A");
    }

    #[test]
    fn split_drive_extracts_drive_letter() {
        assert_eq!(
            split_drive(r"C:\Users\foo"),
            ("C:".to_string(), r"\Users\foo".to_string())
        );
        assert_eq!(
            split_drive(r"\\server\share"),
            (String::new(), r"\\server\share".to_string())
        );
    }

    #[test]
    fn shell_basename_lower_strips_extension_and_path() {
        assert_eq!(
            shell_basename_lower(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            "powershell"
        );
        assert_eq!(shell_basename_lower("pwsh"), "pwsh");
    }
}
