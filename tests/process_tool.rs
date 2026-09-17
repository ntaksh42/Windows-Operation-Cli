//! The `Process` tool against processes this test owns.
//!
//! `kill` mode ends processes, so everything here targets a throwaway child
//! started by the test itself — never a process that was already running.

#![cfg(target_os = "windows")]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows_operation_cli::tools::process::{ProcessMode, ProcessParams, SortBy, process};

/// A sleeping helper process that sits idle until the test kills it.
struct Victim(Child);

impl Victim {
    fn spawn() -> Option<Self> {
        // A sleeping PowerShell: it consumes no CPU, and the bounded sleep
        // means it exits on its own if the test fails before killing it, so
        // nothing is left behind. `timeout.exe` is not usable here — it exits
        // immediately when its stdin is not a console.
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 120"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()
            .map(Self)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn is_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Victim {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn list(name: Option<&str>, limit: i64) -> String {
    process(ProcessParams {
        mode: ProcessMode::List,
        name: name.map(str::to_string),
        pid: None,
        sort_by: SortBy::Memory,
        limit,
        force: None,
    })
    .expect("list failed")
}

#[test]
#[ignore = "starts and kills a throwaway process; run with --ignored"]
fn list_reports_running_processes_within_the_limit() {
    let listing = list(None, 5);
    let rows = listing
        .lines()
        .filter(|line| line.contains("PID") || line.chars().any(|c| c.is_ascii_digit()))
        .count();
    assert!(rows > 0, "the listing had no rows:\n{listing}");
    // The limit bounds what is shown; a listing that ignored it would dump
    // every process on the machine.
    assert!(
        listing.lines().count() < 40,
        "limit=5 produced {} lines:\n{listing}",
        listing.lines().count()
    );
}

/// The name filter has to actually narrow the listing.
#[test]
#[ignore = "starts and kills a throwaway process; run with --ignored"]
fn list_filters_by_name() {
    let Some(victim) = Victim::spawn() else {
        eprintln!("skipped: could not start the sleeping helper");
        return;
    };
    // Give the process table a moment to include it.
    std::thread::sleep(Duration::from_millis(500));

    let listing = list(Some("powershell"), 20);
    assert!(
        listing.to_lowercase().contains("powershell"),
        "the filtered listing did not include the running process:\n{listing}"
    );
    drop(victim);
}

/// Killing by PID has to end that exact process.
#[test]
#[ignore = "starts and kills a throwaway process; run with --ignored"]
fn kill_by_pid_ends_that_process() {
    let Some(mut victim) = Victim::spawn() else {
        eprintln!("skipped: could not start the sleeping helper");
        return;
    };
    std::thread::sleep(Duration::from_millis(500));
    assert!(victim.is_running(), "the victim exited before it was killed");

    let response = process(ProcessParams {
        mode: ProcessMode::Kill,
        name: None,
        pid: Some(victim.pid()),
        sort_by: SortBy::Memory,
        limit: 20,
        force: None,
    })
    .expect("kill failed");
    assert!(
        response.contains("killed") || response.contains("Killed"),
        "unexpected kill response: {response}"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while victim.is_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!victim.is_running(), "the process was still running after kill");
}

/// A PID that does not exist must be reported, not silently accepted.
#[test]
#[ignore = "starts and kills a throwaway process; run with --ignored"]
fn killing_an_unknown_pid_is_reported() {
    // PIDs are recycled, so use one that cannot be live: the maximum is far
    // below u32::MAX on Windows.
    let response = process(ProcessParams {
        mode: ProcessMode::Kill,
        name: None,
        pid: Some(u32::MAX - 1),
        sort_by: SortBy::Memory,
        limit: 20,
        force: None,
    })
    .expect("kill returned an unexpected error");
    assert!(
        response.contains("No process"),
        "an unknown PID should say so: {response}"
    );
}

/// Neither target given is a caller mistake, and must not kill anything.
#[test]
#[ignore = "starts and kills a throwaway process; run with --ignored"]
fn kill_without_a_target_is_rejected() {
    let response = process(ProcessParams {
        mode: ProcessMode::Kill,
        name: None,
        pid: None,
        sort_by: SortBy::Memory,
        limit: 20,
        force: None,
    })
    .expect("kill returned an unexpected error");
    assert!(
        response.starts_with("Error:"),
        "kill with no target should be an error: {response}"
    );
}
