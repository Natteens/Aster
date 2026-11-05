<#
.SYNOPSIS
Removes stale test and example executables from target\debug.

.DESCRIPTION
Cargo names every test and example executable <name>-<hash>, and the hash changes with
the build configuration: --all-features, RUSTFLAGS, the debug-info and incremental
settings, and the toolchain each produce a new set. Cargo never deletes the old sets, and
every executable statically links the compiler and the JIT, so each configuration change
leaves gigabytes behind.

This script keeps exactly the executables that `cargo test --workspace --all-targets`
produces with the current settings, and deletes the other hashed executables plus the
regenerable incremental cache. Libraries, fingerprints, build scripts, the release
profile and ~/.cargo are not touched. Anything deleted is rebuilt on demand.

Dry run unless -Apply is given. Refuses to run while cargo or rustc is running.
#>
[CmdletBinding()]
param([switch]$Apply)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }

if (Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue) {
    throw 'cargo or rustc is running; refusing to touch the build directory.'
}

Write-Host 'Building (or confirming) the canonical test executables...'
Push-Location $repo
try {
    # Windows PowerShell 5.1 turns cargo's progress output on stderr into errors under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $messages = cargo test --workspace --all-targets --no-run --message-format=json
    $exit = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($exit -ne 0) { throw "cargo exited with status $exit" }
} finally {
    Pop-Location
}

function Get-Stem([string]$path) {
    return ($path.ToLowerInvariant() -replace '/', '\') -replace '\.(exe|pdb)$', ''
}

$live = @{}
foreach ($line in $messages) {
    if ($line -match '^\{' -and $line -match '"executable":"') {
        $message = $line | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and $message.executable) {
            $live[(Get-Stem $message.executable)] = $true
        }
    }
}

$stale = foreach ($directory in 'deps', 'examples') {
    $path = Join-Path $target "debug\$directory"
    if (Test-Path $path) {
        Get-ChildItem $path -File | Where-Object {
            $_.Name -match '-[0-9a-f]{16}\.(exe|pdb)$' -and -not $live.ContainsKey((Get-Stem $_.FullName))
        }
    }
}
$stale = @($stale)
$bytes = ($stale | Measure-Object Length -Sum).Sum
$incremental = Join-Path $target 'debug\incremental'

'{0} live executables kept; {1} stale executables ({2:N2} GB){3}' -f `
    $live.Count, $stale.Count, ($bytes / 1e9), $(if (Test-Path $incremental) { ', plus the incremental cache.' } else { '.' })

if (-not $Apply) {
    Write-Host 'Dry run. Re-run with -Apply to delete them.'
    return
}
$stale | Remove-Item -Force
if (Test-Path $incremental) { Remove-Item $incremental -Recurse -Force }
Write-Host ('Deleted {0} executables.' -f $stale.Count)
