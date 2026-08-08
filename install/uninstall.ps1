$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script:Product = "aster"
$script:Target = "windows-x64"
$script:AllowedEntries = @("bin", "stdlib", "LICENSE", "install-manifest.json", "install-state.json")

function Throw-UninstallerError {
    param([Parameter(Mandatory = $true)][string]$Message)
    throw $Message
}

function Get-NativeWindowsArchitecture {
    # The .NET OS-architecture property is missing from the framework surface bound in some
    # Windows PowerShell 5.1 sessions, so query Win32 for the native architecture instead.
    try {
        if (-not ([Management.Automation.PSTypeName]'Aster.Install.NativeArchitectureV1').Type) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace Aster.Install
{
    public static class NativeArchitectureV1
    {
        [StructLayout(LayoutKind.Sequential)]
        private struct SystemInfo
        {
            public ushort ProcessorArchitecture;
            public ushort Reserved;
            public uint PageSize;
            public IntPtr MinimumApplicationAddress;
            public IntPtr MaximumApplicationAddress;
            public UIntPtr ActiveProcessorMask;
            public uint NumberOfProcessors;
            public uint ProcessorType;
            public uint AllocationGranularity;
            public ushort ProcessorLevel;
            public ushort ProcessorRevision;
        }

        [DllImport("kernel32.dll")]
        private static extern void GetNativeSystemInfo(out SystemInfo lpSystemInfo);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool IsWow64Process2(IntPtr hProcess, out ushort pProcessMachine, out ushort pNativeMachine);

        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentProcess();

        public static string Resolve()
        {
            // GetNativeSystemInfo reports AMD64 for an x64 process emulated on ARM64, so prefer
            // IsWow64Process2 (Windows 10 1709+) which always reports the real native machine.
            try
            {
                ushort processMachine;
                ushort nativeMachine;
                if (IsWow64Process2(GetCurrentProcess(), out processMachine, out nativeMachine))
                {
                    return FromImageFileMachine(nativeMachine);
                }
            }
            catch (EntryPointNotFoundException) { }
            catch (DllNotFoundException) { }

            SystemInfo info;
            GetNativeSystemInfo(out info);
            return FromProcessorArchitecture(info.ProcessorArchitecture);
        }

        private static string FromImageFileMachine(ushort machine)
        {
            switch (machine)
            {
                case 0x8664: return "AMD64";
                case 0xAA64: return "ARM64";
                case 0x014C: return "X86";
                case 0x01C0:
                case 0x01C4: return "ARM";
                case 0x0200: return "IA64";
                default: return "UNKNOWN(0x" + machine.ToString("X4") + ")";
            }
        }

        private static string FromProcessorArchitecture(ushort architecture)
        {
            switch (architecture)
            {
                case 9: return "AMD64";
                case 12: return "ARM64";
                case 0: return "X86";
                case 5: return "ARM";
                case 6: return "IA64";
                default: return "UNKNOWN(" + architecture.ToString() + ")";
            }
        }
    }
}
'@
        }
        $architecture = [Aster.Install.NativeArchitectureV1]::Resolve()
    }
    catch {
        Throw-UninstallerError "Unable to determine the native Windows architecture: $($_.Exception.Message)"
    }
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        Throw-UninstallerError "Unable to determine the native Windows architecture."
    }
    return $architecture
}

function Test-SupportedWindowsArchitecture {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Architecture)
    return $Architecture -ceq "AMD64"
}

function Assert-WindowsX64 {
    if (
        [Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
        -not [Environment]::Is64BitOperatingSystem -or
        -not [Environment]::Is64BitProcess
    ) {
        Throw-UninstallerError "This uninstaller supports Windows x64 only."
    }
    $architecture = Get-NativeWindowsArchitecture
    if (-not (Test-SupportedWindowsArchitecture $architecture)) {
        Throw-UninstallerError "Unsupported Windows architecture: $architecture. Expected x64."
    }
}

function Assert-SafeInstallDirectory {
    param([Parameter(Mandatory = $true)][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        Throw-UninstallerError "ASTER_INSTALL_DIR must not be empty."
    }
    if (
        $Value.IndexOfAny(@([char]0, [char]10, [char]13)) -ge 0 -or
        $Value -match '(^|[\\/])\.\.([\\/]|$)'
    ) {
        Throw-UninstallerError "ASTER_INSTALL_DIR contains an unsafe path."
    }
    $full = [IO.Path]::GetFullPath($Value).TrimEnd('\', '/')
    $root = [IO.Path]::GetPathRoot($full).TrimEnd('\', '/')
    $homePath = if ([string]::IsNullOrWhiteSpace($HOME)) { "" } else { [IO.Path]::GetFullPath($HOME).TrimEnd('\', '/') }
    $localAppData = if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { "" } else { [IO.Path]::GetFullPath($env:LOCALAPPDATA).TrimEnd('\', '/') }
    if (
        [string]::IsNullOrWhiteSpace($full) -or
        $full -eq $root -or
        ($homePath -and $full -ieq $homePath) -or
        ($localAppData -and $full -ieq $localAppData)
    ) {
        Throw-UninstallerError "ASTER_INSTALL_DIR is too broad to remove safely."
    }
    if ((Test-Path -LiteralPath (Join-Path $full ".git")) -and (Test-Path -LiteralPath (Join-Path $full "Cargo.toml"))) {
        Throw-UninstallerError "ASTER_INSTALL_DIR must not be the repository root."
    }
    $candidate = $full
    while (-not [string]::IsNullOrWhiteSpace($candidate)) {
        if (Test-Path -LiteralPath $candidate) {
            $item = Get-Item -Force -LiteralPath $candidate
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                Throw-UninstallerError "ASTER_INSTALL_DIR must not traverse a symlink or reparse point."
            }
        }
        $parent = [IO.Path]::GetDirectoryName($candidate)
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $candidate) { break }
        $candidate = $parent
    }
    return $full
}

function Read-InstallState {
    param([Parameter(Mandatory = $true)][string]$InstallDirectory)
    $statePath = Join-Path $InstallDirectory "install-state.json"
    if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) {
        Throw-UninstallerError "The installation directory is not managed by the ASTER installer."
    }
    try {
        $state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json
    }
    catch {
        Throw-UninstallerError "install-state.json is not valid JSON."
    }
    foreach ($name in @("schema", "product", "version", "target")) {
        if (-not ($state.PSObject.Properties.Name -contains $name)) {
            Throw-UninstallerError "install-state.json is invalid for this ASTER uninstaller."
        }
    }
    if (
        $state.schema -ne 1 -or
        $state.product -ne $script:Product -or
        [string]::IsNullOrWhiteSpace([string]$state.version) -or
        $state.target -ne $script:Target
    ) {
        Throw-UninstallerError "install-state.json is invalid or targets another platform."
    }
    return $state
}

function Assert-ManagedEntries {
    param([Parameter(Mandatory = $true)][string]$InstallDirectory)
    foreach ($entry in @(Get-ChildItem -Force -LiteralPath $InstallDirectory)) {
        if ($script:AllowedEntries -notcontains $entry.Name) {
            Throw-UninstallerError "The managed installation contains an unexpected entry: $($entry.Name)"
        }
        if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Throw-UninstallerError "The managed installation contains a symlink or reparse point: $($entry.Name)"
        }
        if ($entry.PSIsContainer) {
            foreach ($nested in @(Get-ChildItem -Recurse -Force -LiteralPath $entry.FullName)) {
                if (($nested.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    $relative = $nested.FullName.Substring($InstallDirectory.Length).TrimStart('\', '/')
                    Throw-UninstallerError "The managed installation contains a symlink or reparse point: $relative"
                }
            }
        }
    }
}

function Get-WindowsPathWithoutEntry {
    param(
        [AllowEmptyString()][string]$CurrentPath,
        [Parameter(Mandatory = $true)][string]$Entry
    )
    $normalizedEntry = [IO.Path]::GetFullPath($Entry).TrimEnd('\', '/')
    $kept = [Collections.Generic.List[string]]::new()
    foreach ($existing in @($CurrentPath -split ';')) {
        $normalizedExisting = $existing.Trim().TrimEnd('\', '/')
        if (-not [string]::IsNullOrWhiteSpace($normalizedExisting) -and $normalizedExisting -ieq $normalizedEntry) {
            continue
        }
        $kept.Add($existing)
    }
    return [string]::Join(";", $kept)
}

function Remove-AsterUserPath {
    param([Parameter(Mandatory = $true)][string]$BinDirectory)
    if ($env:ASTER_INSTALL_SKIP_PATH -eq "1") { return }
    $current = [string][Environment]::GetEnvironmentVariable("Path", "User")
    $updated = Get-WindowsPathWithoutEntry $current $BinDirectory
    if ($updated -cne $current) {
        [Environment]::SetEnvironmentVariable("Path", $updated, "User")
    }
    $env:Path = Get-WindowsPathWithoutEntry ([string]$env:Path) $BinDirectory
}

function Invoke-AsterUninstall {
    Assert-WindowsX64
    $installValue = if (Test-Path Env:ASTER_INSTALL_DIR) {
        $env:ASTER_INSTALL_DIR
    }
    else {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            Throw-UninstallerError "LOCALAPPDATA is not available."
        }
        Join-Path $env:LOCALAPPDATA "Aster"
    }
    $installDirectory = Assert-SafeInstallDirectory $installValue
    if (-not (Test-Path -LiteralPath $installDirectory)) {
        Write-Host "ASTER is not installed"
        return
    }
    $children = @(Get-ChildItem -Force -LiteralPath $installDirectory)
    if ($children.Count -eq 0) {
        [IO.Directory]::Delete($installDirectory)
        Write-Host "ASTER is not installed"
        return
    }

    [void](Read-InstallState $installDirectory)
    Assert-ManagedEntries $installDirectory
    Remove-AsterUserPath (Join-Path $installDirectory "bin")

    foreach ($name in $script:AllowedEntries) {
        $path = Join-Path $installDirectory $name
        if (-not (Test-Path -LiteralPath $path)) { continue }
        $item = Get-Item -Force -LiteralPath $path
        if ($item.PSIsContainer) {
            Remove-Item -Recurse -Force -LiteralPath $path
        }
        else {
            Remove-Item -Force -LiteralPath $path
        }
    }
    if (@(Get-ChildItem -Force -LiteralPath $installDirectory).Count -ne 0) {
        Throw-UninstallerError "The installation directory was not empty after removing managed entries."
    }
    [IO.Directory]::Delete($installDirectory)

    Write-Host ""
    Write-Host "ASTER uninstalled successfully"
    Write-Host ""
    Write-Host "Location: $installDirectory"
    Write-Host ""
    Write-Host "Open terminals may retain the previous PATH until restarted."
}

try {
    Invoke-AsterUninstall
}
catch {
    [Console]::Error.WriteLine("error: " + $_.Exception.Message)
    exit 1
}
