# windows-computeruse

Windows desktop automation MCP server (Rust).

## Install (Claude Code)

```powershell
./install.ps1
```

Builds the release binary, copies it to
`%LOCALAPPDATA%\Programs\windows-computeruse`, installs the bundled skill
(`skills/windows-computeruse`) to `%USERPROFILE%\.claude\skills` so it
loads in every project, and registers the MCP server in Claude Code's
user-scope configuration (`claude mcp add --scope user`). Pass `-SkipBuild` to
reuse an existing `target\release` binary.

## Build

```
cargo build --release
```

## Run

```
cargo run --release
```

Use the release profile for desktop automation. Debug builds are substantially
slower when resizing and encoding 4K screenshots.

`Snapshot` scans only the foreground application by default, including its
same-process popup windows. Use `window` to target one titled application or
`scope="all"` for whole-desktop discovery. Returned UI
tree lines include generation-scoped element ids and supported semantic
actions; pass an id to `InvokeElement` to activate the control without screen
coordinates.

Prefer `WaitFor` after UI actions so execution resumes as soon as the expected
window or element appears. Fixed `Wait` durations always add their full delay.

For long-running sessions, batch-thin screenshot history on the client side to
keep image context and memory usage bounded.

## Configuration

| Environment variable | Description |
| --- | --- |
| `WINDOWS_MCP_ALLOW_SENSITIVE_FILES=1` | Allows FileSystem access to sensitive names such as `.env*`, `id_rsa*`, `*.pem`, `*.key`, `*.pfx`, and `credentials*`. Access is denied by default. |
| `WINDOWS_MCP_DISABLED_TOOLS` | Comma-separated tool names to disable, matched case-insensitively. |
| `WINDOWS_MCP_AUDIT_LOG` | JSONL audit-log path. Each call records only its timestamp, tool name, and success status. |
| `WINDOWS_MCP_INPUT_SETTLE_MS` | Delay in milliseconds after input actions. Defaults to `50`; values are clamped to `0`–`5000`. |

## Troubleshooting

| Symptom | Likely cause | Action |
| --- | --- | --- |
| Clicks land away from the target | DPI scaling, coordinates copied from a downscaled screenshot, or a monitor positioned at negative desktop coordinates | Use `DisplayInventory` to inspect display bounds and scaling. When a screenshot reports a coordinate multiplier, convert image coordinates back to screen coordinates before clicking. Keep negative coordinates for monitors left of or above the primary display. |
| A UI Automation element is not found | The target is outside the current Snapshot scope, has not loaded yet, or does not expose UIA metadata | Use `WaitFor`, target the window explicitly, or retry Snapshot with `scope="all"`. Coordinate clicking remains available for controls without UIA support. |
| WaitFor or Snapshot times out | The application is slow or the scan scope is too broad | Increase `timeout`/`timeout_ms` within the documented limit, increase the WaitFor `interval`, or narrow Snapshot to one window. |

Enable Snapshot timing diagnostics before starting the server:

```powershell
$env:WINDOWS_MCP_PROFILE_SNAPSHOT = '1'
cargo run --release
```
