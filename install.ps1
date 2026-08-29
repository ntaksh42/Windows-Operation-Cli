#Requires -Version 5.1
<#
.SYNOPSIS
Installs windows-computeruse for Claude Code.

.DESCRIPTION
Builds the release binary, copies it to a per-user install directory,
installs the bundled Claude Code skill to %USERPROFILE%\.claude\skills so it
is available in every project, and registers the MCP server in Claude Code's
user-scope configuration via `claude mcp add --scope user`.

.PARAMETER InstallDir
Where to place the binary. Defaults to %LOCALAPPDATA%\Programs\windows-computeruse.

.PARAMETER SkipBuild
Reuse an existing target\release\windows-computeruse.exe instead of running
`cargo build --release`.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\windows-computeruse'),
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = $PSScriptRoot

# 1. Build the release binary (debug builds are too slow for 4K screenshots).
$exeSource = Join-Path $repoRoot 'target\release\windows-computeruse.exe'
if (-not $SkipBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo not found. Install Rust (https://rustup.rs) or rerun with -SkipBuild if a release binary already exists.'
    }
    Write-Host 'Building release binary...'
    cargo build --release --manifest-path (Join-Path $repoRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE." }
}
if (-not (Test-Path $exeSource)) {
    throw "Binary not found: $exeSource"
}

# 2. Copy the binary to the install directory.
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$exePath = Join-Path $InstallDir 'windows-computeruse.exe'
Copy-Item $exeSource $exePath -Force
Write-Host "Installed binary: $exePath"

# 3. Install the skill user-wide so it is available to every Claude Code
#    project. skills\ in the repo is the distribution source, not a live
#    project skill.
$skillSource = Join-Path $repoRoot 'skills\windows-computeruse'
$skillDest = Join-Path $env:USERPROFILE '.claude\skills\windows-computeruse'
New-Item -ItemType Directory -Force -Path $skillDest | Out-Null
Copy-Item (Join-Path $skillSource '*') $skillDest -Recurse -Force
Write-Host "Installed skill: $skillDest"

# 4. Register the MCP server in Claude Code's user-scope config (~/.claude.json).
if (Get-Command claude -ErrorAction SilentlyContinue) {
    # Remove a stale registration first so the path update takes effect.
    # `claude mcp remove` fails when the entry does not exist - that is fine.
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    claude mcp remove --scope user windows-computeruse *> $null
    $ErrorActionPreference = $prevEap

    claude mcp add --scope user windows-computeruse -- $exePath
    if ($LASTEXITCODE -ne 0) { throw 'claude mcp add failed.' }
    Write-Host 'Registered MCP server "windows-computeruse" in Claude Code user config.'
} else {
    Write-Warning 'claude CLI not found. Register the server manually:'
    Write-Host "  claude mcp add --scope user windows-computeruse -- `"$exePath`""
}

Write-Host 'Done. Restart Claude Code sessions to pick up the server and skill.'
