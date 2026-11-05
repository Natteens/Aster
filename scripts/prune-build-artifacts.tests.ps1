<#
Failure-path and selection tests for prune-build-artifacts.ps1. Plain PowerShell, no
modules: run with `powershell -NoProfile -File scripts\prune-build-artifacts.tests.ps1`.
Exits non-zero on the first failed check. Uses only a temporary directory and never runs
cargo.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'prune-build-artifacts.ps1')

$script:checks = 0
function Assert-That([bool]$Condition, [string]$Name) {
    $script:checks++
    if (-not $Condition) { Write-Host "FAIL: $Name"; exit 1 }
}
function Assert-Throws([scriptblock]$Action, [string]$Pattern, [string]$Name) {
    $script:checks++
    try { & $Action } catch {
        if ($_.Exception.Message -match $Pattern) { return }
        Write-Host "FAIL: $Name (wrong error: $($_.Exception.Message))"; exit 1
    }
    Write-Host "FAIL: $Name (no error)"; exit 1
}

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("prune-test-" + [guid]::NewGuid().ToString('N'))
$target = Join-Path $root 'target'
$deps = Join-Path $target 'debug\deps'
$examples = Join-Path $target 'debug\examples'
New-Item -ItemType Directory -Force $deps, $examples, (Join-Path $target 'debug\incremental'), (Join-Path $target 'release') | Out-Null
function New-File([string]$Path) { Set-Content -LiteralPath $Path -Value 'x' }

try {
    $liveExe = Join-Path $deps 'parallel-1111111111111111.exe'
    $staleExe = Join-Path $deps 'parallel-2222222222222222.exe'
    $stalePdb = Join-Path $deps 'parallel-2222222222222222.pdb'
    $exampleStale = Join-Path $examples 'matrix-3333333333333333.exe'
    $keep = @(
        (Join-Path $deps 'libaster_runtime-4444444444444444.rlib'),
        (Join-Path $deps 'libaster_runtime-4444444444444444.rmeta'),
        (Join-Path $deps 'aster_runtime-4444444444444444.d'),
        (Join-Path $deps 'aster.exe'),
        (Join-Path $target 'release\aster-5555555555555555.exe')
    )
    foreach ($f in @($liveExe, $staleExe, $stalePdb, $exampleStale) + $keep) { New-File $f }

    # --- target directory resolution -------------------------------------------------
    $repo = 'C:\work\aster'
    Assert-That ((Resolve-TargetDirectory $repo $null) -eq 'C:\work\aster\target') 'default target dir'
    Assert-That ((Resolve-TargetDirectory $repo '') -eq 'C:\work\aster\target') 'blank target dir'
    Assert-That ((Resolve-TargetDirectory $repo 'out\build') -eq 'C:\work\aster\out\build') 'relative dir is rooted at the repository'
    Assert-That ((Resolve-TargetDirectory $repo '..\shared') -eq 'C:\work\shared') 'relative dir with .. is normalized from the repository'
    Assert-That ((Resolve-TargetDirectory $repo 'D:\cache\t') -eq 'D:\cache\t') 'absolute dir is kept'

    # --- live set parsing fails closed -----------------------------------------------
    $escaped = $liveExe -replace '\\', '\\'
    $artifact = '{"reason":"compiler-artifact","executable":"' + $escaped + '"}'
    $okFinished = '{"reason":"build-finished","success":true}'
    $live = Get-LiveExecutables @($artifact, $okFinished)
    Assert-That ($live.Count -eq 1) 'one live executable parsed'
    Assert-Throws { Get-LiveExecutables @() } 'successful build' 'empty output is rejected'
    Assert-Throws { Get-LiveExecutables @($okFinished) } 'no executables' 'zero executables is rejected'
    Assert-Throws { Get-LiveExecutables @($artifact) } 'successful build' 'missing build-finished is rejected'
    Assert-Throws { Get-LiveExecutables @($artifact, '{"reason":"build-finished","success":false}') } 'successful build' 'failed build is rejected'
    Assert-Throws { Get-LiveExecutables @($artifact, '{not json', $okFinished) } 'Cannot parse' 'malformed JSON is rejected'
    $libraryOnly = '{"reason":"compiler-artifact","executable":null}'
    Assert-Throws { Get-LiveExecutables @($libraryOnly, $okFinished) } 'no executables' 'artifacts without executables are rejected'

    # --- live set validation ---------------------------------------------------------
    Assert-LiveSetIsValid $live $target
    $missing = @{ 'x' = (Join-Path $deps 'gone-9999999999999999.exe') }
    Assert-Throws { Assert-LiveSetIsValid $missing $target } 'missing' 'a missing live executable is rejected'
    $elsewhere = Join-Path $root 'other\debug\deps\a-6666666666666666.exe'
    New-Item -ItemType Directory -Force (Split-Path $elsewhere) | Out-Null
    New-File $elsewhere
    Assert-Throws { Assert-LiveSetIsValid @{ 'y' = $elsewhere } $target } 'outside' 'a live executable from another target directory is rejected'

    # --- stale selection -------------------------------------------------------------
    $stale = Get-StaleFiles $live $target
    $names = ($stale | ForEach-Object Name | Sort-Object) -join ','
    Assert-That ($names -eq 'matrix-3333333333333333.exe,parallel-2222222222222222.exe,parallel-2222222222222222.pdb') "only hashed non-live executables are stale ($names)"
    $emptySelection = Get-StaleFiles @{} (Join-Path $root 'nowhere')
    Assert-That ($emptySelection.Count -eq 0) 'a missing directory selects nothing'

    # --- the re-check before deletion ------------------------------------------------
    $incremental = Join-Path $target 'debug\incremental'
    Assert-Throws { Remove-StaleArtifacts $stale $incremental { $true } } 'running' 'deletion is refused while cargo runs'
    Assert-That ((Test-Path $staleExe) -and (Test-Path $stalePdb) -and (Test-Path $exampleStale) -and (Test-Path $incremental)) 'nothing was deleted by the refused run'
    Assert-Throws { Assert-CargoNotRunning { $true } } 'running' 'start-of-run check refuses while cargo runs'
    Assert-CargoNotRunning { $false }

    # --- deletion removes only the stale executables and the incremental cache -------
    Remove-StaleArtifacts $stale $incremental { $false }
    Assert-That (-not (Test-Path $staleExe) -and -not (Test-Path $stalePdb) -and -not (Test-Path $exampleStale)) 'stale executables are deleted'
    Assert-That (-not (Test-Path $incremental)) 'incremental cache is deleted'
    Assert-That ((Test-Path $liveExe) -and (@($keep | Where-Object { -not (Test-Path $_) }).Count -eq 0)) 'live executables, libraries, metadata and release outputs are kept'
} finally {
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "All $script:checks checks passed."
