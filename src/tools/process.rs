//! `Process` tool: list and kill running processes (docs/SPEC.md §19).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use rmcp::schemars;
use serde::Deserialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::fuzzy;
use crate::params::{self, BoolOrString};

/// `Process` tool modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    List,
    Kill,
}

/// `sort_by` values for `list` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortBy {
    Memory,
    Cpu,
    Name,
}

fn default_sort_by() -> SortBy {
    SortBy::Memory
}

fn default_limit() -> i64 {
    20
}

/// Parameters for the `Process` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProcessParams {
    pub mode: ProcessMode,
    /// `list`: fuzzy name filter (partial_ratio > 60). `kill`: exact,
    /// case-insensitive name match (may match multiple processes).
    pub name: Option<String>,
    /// `kill` mode: target PID, takes priority over `name`.
    pub pid: Option<u32>,
    #[serde(default = "default_sort_by")]
    pub sort_by: SortBy,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// `kill` mode: accepted for compatibility. Windows offers no graceful
    /// termination primitive here, so the process is force-killed either way.
    #[serde(default)]
    pub force: Option<BoolOrString>,
}

/// Dispatches `list`/`kill`. Always returns `Ok` with a formatted message —
/// business failures (no matching process, access denied) are part of the
/// tool's normal text response, matching the Python reference.
pub fn process(params: ProcessParams) -> Result<String, String> {
    match params.mode {
        ProcessMode::List => Ok(list_processes(
            params.name.as_deref(),
            params.sort_by,
            params.limit,
        )),
        ProcessMode::Kill => {
            let force = params::opt_bool(&params.force, false)?;
            Ok(kill_process(params.name.as_deref(), params.pid, force))
        }
    }
}

struct Row {
    pid: u32,
    name: String,
    cpu: f32,
    mem_mb: f64,
}

/// The process table from the previous listing and when it was taken.
///
/// `Process::cpu_usage()` needs two refreshes at least
/// `MINIMUM_CPU_UPDATE_INTERVAL` apart, which made every listing sleep 200ms.
/// Keeping the table lets a listing that follows a recent one measure CPU over
/// the time in between instead.
static PROCESS_SAMPLE: Mutex<Option<(System, Instant)>> = Mutex::new(None);

/// How old the previous sample may be and still open the CPU window; an older
/// one would average usage over too long to say what is busy now.
const CPU_WINDOW_MAX: Duration = Duration::from_secs(5);

/// Only what the listing shows: `System::new_all()` also gathered disks,
/// networks, users and every process's command line, ~35ms per call.
fn listing_refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_cpu().with_memory()
}

fn list_processes(name: Option<&str>, sort_by: SortBy, limit: i64) -> String {
    let mut sample = PROCESS_SAMPLE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let recent = matches!(&*sample, Some((_, taken)) if taken.elapsed() <= CPU_WINDOW_MAX);
    let (system, taken) = sample.get_or_insert_with(|| (System::new(), Instant::now()));
    if !recent {
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, listing_refresh_kind());
        *taken = Instant::now();
    }
    if let Some(wait) = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.checked_sub(taken.elapsed()) {
        std::thread::sleep(wait);
    }
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, listing_refresh_kind());
    *taken = Instant::now();

    let mut rows: Vec<Row> = system
        .processes()
        .values()
        .map(|p| Row {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cpu: p.cpu_usage(),
            mem_mb: p.memory() as f64 / (1024.0 * 1024.0),
        })
        .collect();

    if let Some(name) = name {
        let needle = name.to_lowercase();
        rows.retain(|r| fuzzy::partial_ratio(&needle, &r.name.to_lowercase()) > 60.0);
    }

    match sort_by {
        SortBy::Memory => rows.sort_by(|a, b| {
            b.mem_mb
                .partial_cmp(&a.mem_mb)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortBy::Cpu => rows.sort_by(|a, b| {
            b.cpu
                .partial_cmp(&a.cpu)
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        SortBy::Name => rows.sort_by_key(|r| r.name.to_lowercase()),
    }

    rows.truncate(limit.max(0) as usize);

    if rows.is_empty() {
        return match name {
            Some(name) => format!("No processes found matching {name}."),
            None => "No processes found.".to_string(),
        };
    }

    let shown = rows.len();
    format!("Processes ({shown} shown):\n{}", format_table(&rows))
}

/// Renders a `PID/Name/CPU%/Memory` table, approximating Python's
/// `tabulate(..., tablefmt="simple")` output: numeric PID column right-aligned,
/// the rest left-aligned, columns separated by two spaces.
fn format_table(rows: &[Row]) -> String {
    let headers = ["PID", "Name", "CPU%", "Memory"];
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|r| {
            [
                r.pid.to_string(),
                r.name.clone(),
                format!("{:.1}%", r.cpu),
                format!("{:.1} MB", r.mem_mb),
            ]
        })
        .collect();

    let mut widths = [
        headers[0].len(),
        headers[1].len(),
        headers[2].len(),
        headers[3].len(),
    ];
    for cell in &cells {
        for (w, c) in widths.iter_mut().zip(cell.iter()) {
            *w = (*w).max(c.len());
        }
    }

    let mut lines = Vec::with_capacity(cells.len() + 2);
    lines.push(format_row(&headers.map(str::to_string), &widths));
    lines.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    for cell in &cells {
        lines.push(format_row(cell, &widths));
    }
    lines.join("\n")
}

fn format_row(cell: &[String; 4], widths: &[usize; 4]) -> String {
    let pid = format!("{:>width$}", cell[0], width = widths[0]);
    let name = format!("{:<width$}", cell[1], width = widths[1]);
    let cpu = format!("{:<width$}", cell[2], width = widths[2]);
    let mem = format!("{:<width$}", cell[3], width = widths[3]);
    format!("{pid}  {name}  {cpu}  {mem}")
        .trim_end()
        .to_string()
}

/// Both `force` settings end the process the same way. There is no separate
/// graceful-termination primitive here: the Python reference's
/// `psutil.terminate()`/`kill()` and `sysinfo`'s `Process::kill()` all reach
/// `TerminateProcess`, which gives the target no chance to save or clean up.
///
/// The response says so rather than reporting "Terminated" for
/// `force=false`, which reads as the graceful path and is not what happened.
fn kill_process(name: Option<&str>, pid: Option<u32>, _force: bool) -> String {
    if pid.is_none() && name.is_none() {
        return "Error: Provide either pid or name parameter for kill mode.".to_string();
    }

    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    let mut killed: Vec<String> = Vec::new();

    if let Some(pid) = pid {
        match system.process(Pid::from_u32(pid)) {
            Some(process) => {
                let pname = process.name().to_string_lossy().into_owned();
                if !process.kill() {
                    return format!(
                        "Access denied to kill PID {pid}. Try running as administrator."
                    );
                }
                killed.push(format!("{pname} (PID {pid})"));
            }
            None => return format!("No process with PID {pid} found."),
        }
    } else if let Some(name) = name {
        let needle = name.to_lowercase();
        for process in system.processes().values() {
            let pname = process.name().to_string_lossy();
            if pname.to_lowercase() == needle {
                let pid = process.pid().as_u32();
                if process.kill() {
                    killed.push(format!("{pname} (PID {pid})"));
                }
            }
        }
    }

    if killed.is_empty() {
        return format!(
            "No process matching \"{}\" found or access denied.",
            name.unwrap_or_default()
        );
    }
    format!("Force killed: {}", killed.join(", "))
}
