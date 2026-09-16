#Requires -Version 5.1
<#
.SYNOPSIS
Installs windows-operation-cli for Claude Code.

.DESCRIPTION
Copies the release binary to a per-user install directory, installs the bundled
Claude Code skill to %USERPROFILE%\.claude\skills so it is available in every
project, and registers the MCP server in Claude Code's user-scope configuration
via `claude mcp add --scope user`.

The binary comes from one of three places, in order of preference:
  -FromRelease  downloads a prebuilt archive from GitHub Releases (no Rust needed)
  -SkipBuild    reuses an existing target\release binary, or the one sitting next
                to this script in an unpacked release archive
  (default)     builds from source with `cargo build --release`

.PARAMETER InstallDir
Where to place the binary. Defaults to %LOCALAPPDATA%\Programs\windows-operation-cli.

.PARAMETER SkipBuild
Reuse an existing windows-operation-cli.exe instead of running `cargo build --release`.

.PARAMETER FromRelease
Download a prebuilt binary from GitHub Releases instead of building. Requires no
Rust toolchain. Implies -SkipBuild.

.PARAMETER Version
Release tag to download with -FromRelease. Defaults to `latest`.

.EXAMPLE
./install.ps1 -FromRelease
Installs the latest prebuilt release without a Rust toolchain.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\windows-operation-cli'),
    [switch]$SkipBuild,
    [switch]$FromRelease,
    [string]$Version = 'latest'
)

$ErrorActionPreference = 'Stop'
$repoRoot = $PSScriptRoot
$Repository = 'ntaksh42/Windows-Operation-Cli'
$AssetName = 'windows-operation-cli-windows-x64.zip'

# 1. Obtain the release binary. Three sources, see .DESCRIPTION.
#    Debug builds are too slow for 4K screenshots, so this is always a release build.
$downloadRoot = $null
if ($FromRelease) {
    $uri = if ($Version -eq 'latest') {
        "https://github.com/$Repository/releases/latest/download/$AssetName"
    } else {
        "https://github.com/$Repository/releases/download/$Version/$AssetName"
    }
    $downloadRoot = Join-Path ([System.IO.Path]::GetTempPath()) "woc-install-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Force -Path $downloadRoot | Out-Null
    $zipPath = Join-Path $downloadRoot $AssetName
    Write-Host "Downloading $uri"
    # Invoke-WebRequest defaults to a progress bar that makes large downloads crawl
    # on Windows PowerShell 5.1.
    $prevProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $uri -OutFile $zipPath -UseBasicParsing
    } finally {
        $ProgressPreference = $prevProgress
    }
    Expand-Archive -Path $zipPath -DestinationPath $downloadRoot -Force
    $searchRoots = @($downloadRoot)
} else {
    # A repository checkout builds into target\release; an unpacked release
    # archive has the exe next to this script.
    $searchRoots = @($repoRoot, (Join-Path $repoRoot 'target\release'))
}

$exeSource = $searchRoots |
    ForEach-Object { Join-Path $_ 'windows-operation-cli.exe' } |
    Where-Object { Test-Path $_ } |
    Select-Object -First 1

if (-not $exeSource -and -not ($SkipBuild -or $FromRelease)) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'cargo not found. Install Rust (https://rustup.rs), or rerun with -FromRelease to download a prebuilt binary instead.'
    }
    Write-Host 'Building release binary...'
    cargo build --release --manifest-path (Join-Path $repoRoot 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE." }
    $exeSource = Join-Path $repoRoot 'target\release\windows-operation-cli.exe'
}

if (-not $exeSource -or -not (Test-Path $exeSource)) {
    throw "Binary not found. Searched: $($searchRoots -join ', ')"
}

try {
    # 2. Copy the binary to the install directory.
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $exePath = Join-Path $InstallDir 'windows-operation-cli.exe'
    Copy-Item $exeSource $exePath -Force
    Write-Host "Installed binary: $exePath"

    # 3. Install the skill user-wide. The repo copy under .claude\skills only
    #    loads when working inside this repository; copying it to the user skills
    #    directory makes it available to every Claude Code project. Release
    #    archives ship the same files under skills\.
    $skillSource = @(
        (Join-Path (Split-Path $exeSource -Parent) 'skills\windows-operation-cli'),
        (Join-Path $repoRoot '.claude\skills\windows-operation-cli')
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1

    if ($skillSource) {
        $skillDest = Join-Path $env:USERPROFILE '.claude\skills\windows-operation-cli'
        New-Item -ItemType Directory -Force -Path $skillDest | Out-Null
        Copy-Item (Join-Path $skillSource '*') $skillDest -Recurse -Force
        Write-Host "Installed skill: $skillDest"
    } else {
        Write-Warning 'Skill files not found; skipping skill installation.'
    }

    # 4. Register the MCP server in Claude Code's user-scope config (~/.claude.json).
    if (Get-Command claude -ErrorAction SilentlyContinue) {
        # Remove a stale registration first so the path update takes effect.
        # `claude mcp remove` fails when the entry does not exist - that is fine.
        $prevEap = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        claude mcp remove --scope user windows-computeruse *> $null
        claude mcp remove --scope user windows-operation-cli *> $null
        $ErrorActionPreference = $prevEap

        claude mcp add --scope user windows-operation-cli -- $exePath
        if ($LASTEXITCODE -ne 0) { throw 'claude mcp add failed.' }
        Write-Host 'Registered MCP server "windows-operation-cli" in Claude Code user config.'
    } else {
        Write-Warning 'claude CLI not found. Register the server manually:'
        Write-Host "  claude mcp add --scope user windows-operation-cli -- `"$exePath`""
    }
} finally {
    if ($downloadRoot -and (Test-Path $downloadRoot)) {
        Remove-Item $downloadRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host 'Done. Restart Claude Code sessions to pick up the server and skill.'
