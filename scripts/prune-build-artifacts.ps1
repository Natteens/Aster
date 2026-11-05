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
profile, sources, toolchains and ~/.cargo are not touched. Anything deleted is rebuilt on
demand.

Safety rules:
- Dry run unless -Apply is given.
- Nothing is deleted unless Cargo reported a successful build, produced a non-empty list of
  executables, every one of them exists, and every one lies under <target>\debug.
- cargo and rustc must not be running, checked at the start and again immediately before
  the first deletion.
- A relative CARGO_TARGET_DIR is resolved against the repository root, not the current
  directory.
#>
[CmdletBinding()]
param([switch]$Apply)

$ErrorActionPreference = 'Stop'

function Get-Stem([string]$Path) {
    return ($Path.ToLowerInvariant() -replace '/', '\') -replace '\.(exe|pdb)$', ''
}

function Resolve-TargetDirectory([string]$Repository, [string]$Configured) {
    if ([string]::IsNullOrWhiteSpace($Configured)) {
        return [System.IO.Path]::GetFullPath((Join-Path $Repository 'target'))
    }
    if ([System.IO.Path]::IsPathRooted($Configured)) {
        return [System.IO.Path]::GetFullPath($Configured)
    }
    return [System.IO.Path]::GetFullPath((Join-Path $Repository $Configured))
}

function Test-CargoRunning {
    return [bool](Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue)
}

function Assert-CargoNotRunning([scriptblock]$Probe = { Test-CargoRunning }) {
    if (& $Probe) {
        throw 'cargo or rustc is running; refusing to touch the build directory.'
    }
}

# Parses `cargo ... --message-format=json` output into the set of executables it built.
# Fails closed: a line that is not valid JSON, a missing or unsuccessful build-finished
# message, or an empty result is an error.
function Get-LiveExecutables([string[]]$Messages) {
    $live = @{}
    $finished = $null
    foreach ($line in $Messages) {
        if ($line -notmatch '^\{') { continue }
        try { $message = $line | ConvertFrom-Json } catch { throw "Cannot parse a cargo message: $line" }
        if ($message.reason -eq 'build-finished') { $finished = [bool]$message.success }
        elseif ($message.reason -eq 'compiler-artifact' -and $message.executable) {
            $live[(Get-Stem $message.executable)] = [string]$message.executable
        }
    }
    if ($finished -ne $true) { throw 'Cargo did not report a successful build; nothing was deleted.' }
    if ($live.Count -eq 0) { throw 'Cargo reported no executables; nothing was deleted.' }
    return $live
}

# Every live executable must exist and lie under <target>\debug, otherwise the list does not
# describe this target directory and deleting "everything else" would be wrong.
function Assert-LiveSetIsValid($Live, [string]$Target) {
    $debug = (Join-Path $Target 'debug').TrimEnd('\') + '\'
    foreach ($path in $Live.Values) {
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Live executable is missing: $path; nothing was deleted."
        }
        if (-not ([System.IO.Path]::GetFullPath($path)).StartsWith($debug, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Live executable is outside $debug : $path; nothing was deleted."
        }
    }
}

function Get-StaleFiles($Live, [string]$Target) {
    $stale = foreach ($directory in 'deps', 'examples') {
        $path = Join-Path $Target "debug\$directory"
        if (Test-Path -LiteralPath $path) {
            Get-ChildItem -LiteralPath $path -File | Where-Object {
                $_.Name -match '-[0-9a-f]{16}\.(exe|pdb)$' -and -not $Live.ContainsKey((Get-Stem $_.FullName))
            }
        }
    }
    return @($stale)
}

function Remove-StaleArtifacts($Stale, [string]$Incremental, [scriptblock]$Probe = { Test-CargoRunning }) {
    Assert-CargoNotRunning $Probe
    $Stale | Remove-Item -Force
    if (Test-Path -LiteralPath $Incremental) { Remove-Item -LiteralPath $Incremental -Recurse -Force }
}

function Invoke-Prune([bool]$ApplyChanges) {
    $repo = Split-Path -Parent $PSScriptRoot
    $target = Resolve-TargetDirectory $repo $env:CARGO_TARGET_DIR
    Assert-CargoNotRunning

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

    $live = Get-LiveExecutables $messages
    Assert-LiveSetIsValid $live $target
    $stale = Get-StaleFiles $live $target
    $bytes = ($stale | Measure-Object Length -Sum).Sum
    $incremental = Join-Path $target 'debug\incremental'

    '{0} live executables kept; {1} stale executables ({2:N2} GB){3}' -f `
        $live.Count, $stale.Count, ($bytes / 1e9), $(if (Test-Path -LiteralPath $incremental) { ', plus the incremental cache.' } else { '.' })

    if (-not $ApplyChanges) {
        Write-Host 'Dry run. Re-run with -Apply to delete them.'
        return
    }
    Remove-StaleArtifacts $stale $incremental
    Write-Host ('Deleted {0} executables.' -f $stale.Count)
}

# Dot-sourcing (`. .\prune-build-artifacts.ps1`) loads the functions for the tests only.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Prune $Apply.IsPresent
}
