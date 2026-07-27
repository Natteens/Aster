$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script:Product = "aster"
$script:Target = "windows-x64"
$script:DefaultBaseUrl = "https://github.com/Natteens/Aster/releases/latest/download"
$script:ArchiveName = "aster-windows-x64.zip"
$script:ChecksumName = "aster-windows-x64.zip.sha256"
$script:MaximumArchiveBytes = 268435456
$script:MaximumChecksumBytes = 4096
$script:RequiredStdlibModules = @(
    "aster/math.aster",
    "aster/text/text.aster",
    "aster/core/core.aster",
    "aster/io/io.aster",
    "aster/collections/collections.aster"
)

function Throw-InstallerError {
    param([Parameter(Mandatory = $true)][string]$Message)
    throw $Message
}

function Test-PathComponentTraversal {
    param([Parameter(Mandatory = $true)][string]$Path)
    return $Path -match '(^|[\\/])\.\.([\\/]|$)'
}

function Assert-SafeBaseUri {
    param(
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][bool]$AllowInsecure
    )
    if ([string]::IsNullOrWhiteSpace($Value)) {
        Throw-InstallerError "ASTER_INSTALL_BASE_URL must not be empty."
    }
    $uri = $null
    if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref]$uri)) {
        Throw-InstallerError "ASTER_INSTALL_BASE_URL must be an absolute URL."
    }
    if (-not [string]::IsNullOrEmpty($uri.UserInfo)) {
        Throw-InstallerError "ASTER_INSTALL_BASE_URL must not contain credentials."
    }
    if ($uri.Scheme -eq "https") {
        return $uri
    }
    if ($uri.Scheme -eq "http" -and $AllowInsecure) {
        return $uri
    }
    Throw-InstallerError "ASTER_INSTALL_BASE_URL must use HTTPS. Set ASTER_INSTALL_ALLOW_INSECURE=1 only for local tests."
}

function Assert-WindowsX64 {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Throw-InstallerError "This installer supports Windows x64 only."
    }
    if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
        Throw-InstallerError "This installer requires a 64-bit Windows process on Windows x64."
    }
    $architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    if ($architecture -ne "X64") {
        Throw-InstallerError "Unsupported Windows architecture: $architecture. Expected x64."
    }
}

function Assert-SafeInstallDirectory {
    param([Parameter(Mandatory = $true)][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not be empty."
    }
    if ($Value.IndexOfAny(@([char]0, [char]10, [char]13)) -ge 0) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not contain control characters."
    }
    if (Test-PathComponentTraversal $Value) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not contain unresolved '..' components."
    }
    $full = [IO.Path]::GetFullPath($Value).TrimEnd('\', '/')
    $root = [IO.Path]::GetPathRoot($full).TrimEnd('\', '/')
    if ([string]::IsNullOrWhiteSpace($full) -or $full -eq $root) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not be a filesystem root."
    }
    $homePath = if ([string]::IsNullOrWhiteSpace($HOME)) { "" } else { [IO.Path]::GetFullPath($HOME).TrimEnd('\', '/') }
    $localAppData = if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { "" } else { [IO.Path]::GetFullPath($env:LOCALAPPDATA).TrimEnd('\', '/') }
    if (($homePath -and $full -ieq $homePath) -or ($localAppData -and $full -ieq $localAppData)) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not be the entire home or LOCALAPPDATA directory."
    }
    if ((Test-Path -LiteralPath (Join-Path $full ".git")) -and (Test-Path -LiteralPath (Join-Path $full "Cargo.toml"))) {
        Throw-InstallerError "ASTER_INSTALL_DIR must not be the repository root."
    }
    if (Test-Path -LiteralPath $full) {
        $item = Get-Item -Force -LiteralPath $full
        if (-not $item.PSIsContainer) {
            Throw-InstallerError "ASTER_INSTALL_DIR exists and is not a directory."
        }
    }
    $candidate = $full
    while (-not [string]::IsNullOrWhiteSpace($candidate)) {
        if (Test-Path -LiteralPath $candidate) {
            $item = Get-Item -Force -LiteralPath $candidate
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                Throw-InstallerError "ASTER_INSTALL_DIR must not traverse a symlink or reparse point."
            }
        }
        $parent = [IO.Path]::GetDirectoryName($candidate)
        if ([string]::IsNullOrWhiteSpace($parent) -or $parent -eq $candidate) {
            break
        }
        $candidate = $parent
    }
    return $full
}

function Get-RequiredJsonProperty {
    param(
        [Parameter(Mandatory = $true)]$Object,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$Label
    )
    if (-not ($Object.PSObject.Properties.Name -contains $Name)) {
        Throw-InstallerError "$Label is missing '$Name'."
    }
    return $Object.$Name
}

function Read-JsonFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label
    )
    try {
        return Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
    }
    catch {
        Throw-InstallerError "$Label is not valid JSON."
    }
}

function Get-InstallDirectoryState {
    param([Parameter(Mandatory = $true)][string]$InstallDirectory)
    if (-not (Test-Path -LiteralPath $InstallDirectory)) {
        return [pscustomobject]@{ Kind = "Missing"; State = $null }
    }
    $children = @(Get-ChildItem -Force -LiteralPath $InstallDirectory)
    if ($children.Count -eq 0) {
        return [pscustomobject]@{ Kind = "Empty"; State = $null }
    }
    $statePath = Join-Path $InstallDirectory "install-state.json"
    if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) {
        Throw-InstallerError "The installation directory is not empty and is not managed by the ASTER installer."
    }
    $state = Read-JsonFile $statePath "install-state.json"
    if (
        (Get-RequiredJsonProperty $state "schema" "install-state.json") -ne 1 -or
        (Get-RequiredJsonProperty $state "product" "install-state.json") -ne $script:Product -or
        (Get-RequiredJsonProperty $state "target" "install-state.json") -ne $script:Target -or
        [string]::IsNullOrWhiteSpace([string](Get-RequiredJsonProperty $state "version" "install-state.json"))
    ) {
        Throw-InstallerError "install-state.json is invalid for this ASTER installer."
    }
    return [pscustomobject]@{ Kind = "Managed"; State = $state }
}

function Assert-ManagedInstallEntries {
    param([Parameter(Mandatory = $true)][string]$InstallDirectory)
    $allowed = @("bin", "stdlib", "LICENSE", "install-manifest.json", "install-state.json")
    foreach ($entry in @(Get-ChildItem -Force -LiteralPath $InstallDirectory)) {
        if ($allowed -notcontains $entry.Name) {
            Throw-InstallerError "The managed installation contains an unexpected entry: $($entry.Name)"
        }
        if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            Throw-InstallerError "The managed installation contains a symlink or reparse point: $($entry.Name)"
        }
        if ($entry.PSIsContainer) {
            foreach ($nested in @(Get-ChildItem -Recurse -Force -LiteralPath $entry.FullName)) {
                if (($nested.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                    $relative = $nested.FullName.Substring($InstallDirectory.Length).TrimStart('\', '/')
                    Throw-InstallerError "The managed installation contains a symlink or reparse point: $relative"
                }
            }
        }
    }
}

function Get-LimitedDownload {
    param(
        [Parameter(Mandatory = $true)][Uri]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][long]$MaximumBytes,
        [Parameter(Mandatory = $true)][bool]$AllowInsecure
    )
    Add-Type -AssemblyName System.Net.Http
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $true
    $handler.MaxAutomaticRedirections = 5
    $client = [Net.Http.HttpClient]::new($handler)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd("ASTER-Installer/1")
    $response = $null
    $inputStream = $null
    $outputStream = $null
    $failure = $null
    [long]$total = 0
    try {
        $response = $client.GetAsync($Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            Throw-InstallerError "Download failed with HTTP status $([int]$response.StatusCode): $($Uri.AbsolutePath)"
        }
        [void](Assert-SafeBaseUri $response.RequestMessage.RequestUri.AbsoluteUri $AllowInsecure)
        $declaredLength = $response.Content.Headers.ContentLength
        if ($null -ne $declaredLength -and $declaredLength -gt $MaximumBytes) {
            Throw-InstallerError "Download exceeds the allowed size: $($Uri.AbsolutePath)"
        }
        $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $outputStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = New-Object byte[] 81920
        while (($read = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $read
            if ($total -gt $MaximumBytes) {
                Throw-InstallerError "Download exceeds the allowed size: $($Uri.AbsolutePath)"
            }
            $outputStream.Write($buffer, 0, $read)
        }
        if ($total -eq 0) {
            Throw-InstallerError "Downloaded file is empty: $($Uri.AbsolutePath)"
        }
    }
    catch {
        $failure = $_.Exception.Message
    }
    finally {
        if ($null -ne $outputStream) { $outputStream.Dispose() }
        if ($null -ne $inputStream) { $inputStream.Dispose() }
        if ($null -ne $response) { $response.Dispose() }
        $client.Dispose()
        $handler.Dispose()
    }
    if ($null -ne $failure) {
        if (Test-Path -LiteralPath $Destination) {
            Remove-Item -Force -LiteralPath $Destination
        }
        Throw-InstallerError "Download failed: $($Uri.AbsolutePath). $failure"
    }
    return $total
}

function Get-Sha256Hex {
    param([Parameter(Mandatory = $true)][string]$Path)
    $algorithm = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try {
        return ([BitConverter]::ToString($algorithm.ComputeHash($stream))).Replace("-", "").ToLowerInvariant()
    }
    finally {
        $stream.Dispose()
        $algorithm.Dispose()
    }
}

function Verify-ChecksumSidecar {
    param(
        [Parameter(Mandatory = $true)][string]$ArchivePath,
        [Parameter(Mandatory = $true)][string]$ChecksumPath
    )
    $text = [IO.File]::ReadAllText($ChecksumPath)
    if ($text -notmatch '\A([0-9A-Fa-f]{64})[ \t]+([^\r\n]+)\r?\n?\z') {
        Throw-InstallerError "The checksum file has an invalid format."
    }
    $expected = $Matches[1].ToLowerInvariant()
    $actual = Get-Sha256Hex $ArchivePath
    if ($actual -ne $expected) {
        Throw-InstallerError "SHA-256 verification failed for the ASTER archive."
    }
}

function Assert-SafeArchiveEntryName {
    param([Parameter(Mandatory = $true)][string]$Name)
    if ([string]::IsNullOrWhiteSpace($Name) -or $Name.Contains([char]0)) {
        Throw-InstallerError "The archive contains an empty or invalid path."
    }
    if ($Name.Contains("\")) {
        Throw-InstallerError "The archive contains a backslash path."
    }
    if ($Name.StartsWith("/") -or $Name -match '^[A-Za-z]:/' -or $Name.StartsWith("//")) {
        Throw-InstallerError "The archive contains an absolute path."
    }
    $trimmed = $Name.TrimEnd("/")
    foreach ($component in $trimmed.Split("/")) {
        if ([string]::IsNullOrEmpty($component) -or $component -eq "." -or $component -eq "..") {
            Throw-InstallerError "The archive contains path traversal."
        }
    }
}

function Inspect-ZipArchive {
    param([Parameter(Mandatory = $true)][string]$ArchivePath)
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $roots = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($entry in $archive.Entries) {
            $name = $entry.FullName
            Assert-SafeArchiveEntryName $name
            if (-not $seen.Add($name)) {
                Throw-InstallerError "The archive contains a duplicate entry: $name"
            }
            $unixType = (($entry.ExternalAttributes -shr 16) -band 0xF000)
            if ($unixType -eq 0xA000 -or ($entry.ExternalAttributes -band [int][IO.FileAttributes]::ReparsePoint) -ne 0) {
                Throw-InstallerError "The archive contains an unsupported symlink or reparse entry."
            }
            [void]$roots.Add($name.TrimEnd("/").Split("/")[0])
        }
        if ($roots.Count -ne 1) {
            Throw-InstallerError "The archive must contain exactly one root directory."
        }
        $root = @($roots)[0]
        if ($root -notmatch '^aster-(?<version>[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?)-windows-x64$') {
            Throw-InstallerError "The archive root does not match the ASTER Windows release format."
        }
        $required = @(
            "$root/",
            "$root/bin/",
            "$root/bin/aster.exe",
            "$root/stdlib/",
            "$root/stdlib/aster/",
            "$root/LICENSE",
            "$root/install-manifest.json"
        )
        foreach ($name in $required) {
            if (-not $seen.Contains($name)) {
                Throw-InstallerError "The archive is missing required entry: $name"
            }
        }
        foreach ($name in $seen) {
            $relative = $name.Substring($root.Length).TrimStart("/")
            if (
                $relative -ne "" -and
                $relative -ne "LICENSE" -and
                $relative -ne "install-manifest.json" -and
                $relative -ne "bin/" -and
                $relative -ne "bin/aster.exe" -and
                $relative -ne "stdlib/" -and
                -not $relative.StartsWith("stdlib/aster/")
            ) {
                Throw-InstallerError "The archive contains an unexpected entry: $name"
            }
        }
        return [pscustomobject]@{
            Root = $root
            Version = $Matches["version"]
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Expand-ValidatedZip {
    param(
        [Parameter(Mandatory = $true)][string]$ArchivePath,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    $destinationRoot = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + "\"
    $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        foreach ($entry in $archive.Entries) {
            $relative = $entry.FullName.Replace("/", "\")
            $output = [IO.Path]::GetFullPath((Join-Path $Destination $relative))
            if (-not $output.StartsWith($destinationRoot, [StringComparison]::OrdinalIgnoreCase)) {
                Throw-InstallerError "The archive extraction path escaped the staging directory."
            }
            if ($entry.FullName.EndsWith("/")) {
                [void][IO.Directory]::CreateDirectory($output)
            }
            else {
                [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($output))
                $inputStream = $entry.Open()
                $outputStream = [IO.File]::Open($output, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $inputStream.CopyTo($outputStream)
                }
                finally {
                    $outputStream.Dispose()
                    $inputStream.Dispose()
                }
            }
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Assert-RelativeManifestPath {
    param(
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][string]$Label
    )
    if (
        [string]::IsNullOrWhiteSpace($Value) -or
        [IO.Path]::IsPathRooted($Value) -or
        $Value.Contains("\") -or
        (Test-PathComponentTraversal $Value)
    ) {
        Throw-InstallerError "install-manifest.json contains an invalid relative $Label path."
    }
}

function Validate-InstallRoot {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$ExpectedVersion
    )
    $manifestPath = Join-Path $Root "install-manifest.json"
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        Throw-InstallerError "The installation is missing install-manifest.json."
    }
    $manifest = Read-JsonFile $manifestPath "install-manifest.json"
    $schema = Get-RequiredJsonProperty $manifest "schema" "install-manifest.json"
    $product = [string](Get-RequiredJsonProperty $manifest "product" "install-manifest.json")
    $version = [string](Get-RequiredJsonProperty $manifest "version" "install-manifest.json")
    $target = [string](Get-RequiredJsonProperty $manifest "target" "install-manifest.json")
    $entrypoint = [string](Get-RequiredJsonProperty $manifest "entrypoint" "install-manifest.json")
    $stdlib = [string](Get-RequiredJsonProperty $manifest "stdlib" "install-manifest.json")
    $license = [string](Get-RequiredJsonProperty $manifest "license" "install-manifest.json")
    if ($schema -ne 1 -or $product -ne $script:Product -or $target -ne $script:Target) {
        Throw-InstallerError "install-manifest.json does not describe an ASTER Windows x64 installation."
    }
    if ([string]::IsNullOrWhiteSpace($version) -or $version -ne $ExpectedVersion) {
        Throw-InstallerError "install-manifest.json contains an invalid version."
    }
    Assert-RelativeManifestPath $entrypoint "entrypoint"
    Assert-RelativeManifestPath $stdlib "stdlib"
    Assert-RelativeManifestPath $license "license"
    if ($entrypoint -ne "bin/aster.exe" -or $stdlib -ne "stdlib" -or $license -ne "LICENSE") {
        Throw-InstallerError "install-manifest.json contains unsupported installation paths."
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Root "bin\aster.exe") -PathType Leaf)) {
        Throw-InstallerError "The installation is missing bin/aster.exe."
    }
    if (-not (Test-Path -LiteralPath (Join-Path $Root "LICENSE") -PathType Leaf)) {
        Throw-InstallerError "The installation is missing LICENSE."
    }
    foreach ($module in $script:RequiredStdlibModules) {
        $modulePath = Join-Path (Join-Path $Root "stdlib") $module.Replace("/", "\")
        if (-not (Test-Path -LiteralPath $modulePath -PathType Leaf)) {
            Throw-InstallerError "The installation has an incomplete standard library."
        }
    }
    return $manifest
}

function Invoke-AsterProcess {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )
    Push-Location $WorkingDirectory
    try {
        try {
            $output = @(& $Binary @Arguments 2>&1)
        }
        catch {
            Throw-InstallerError "Installed ASTER could not be started."
        }
        $status = $LASTEXITCODE
        return [pscustomobject]@{
            Status = $status
            Output = ($output -join [Environment]::NewLine)
        }
    }
    finally {
        Pop-Location
    }
}

function Test-InstalledCli {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$Version
    )
    $binary = Join-Path $InstallDirectory "bin\aster.exe"
    $projectDirectory = Join-Path ([IO.Path]::GetTempPath()) ("aster-install-project-" + [Guid]::NewGuid().ToString("N"))
    [void][IO.Directory]::CreateDirectory($projectDirectory)
    $source = Join-Path $projectDirectory "main.aster"
    [IO.File]::WriteAllText(
        $source,
        "using aster.math; public class Program { public static int Main() { return Math.Max(40, 2); } }`n",
        [Text.UTF8Encoding]::new($false)
    )
    $oldStdlib = [Environment]::GetEnvironmentVariable("ASTER_STDLIB", "Process")
    [Environment]::SetEnvironmentVariable("ASTER_STDLIB", $null, "Process")
    try {
        $versionResult = Invoke-AsterProcess $binary @("--version") $projectDirectory
        if ($versionResult.Status -ne 0 -or -not $versionResult.Output.Contains($Version)) {
            Throw-InstallerError "Installed ASTER failed --version validation."
        }
        foreach ($command in @("check", "dump-hir", "dump-mir")) {
            $result = Invoke-AsterProcess $binary @($command, $source) $projectDirectory
            if ($result.Status -ne 0) {
                Throw-InstallerError "Installed ASTER failed '$command' validation."
            }
        }
        $runResult = Invoke-AsterProcess $binary @("run", $source) $projectDirectory
        if ($runResult.Status -ne 0 -or $runResult.Output.Trim() -ne "40") {
            Throw-InstallerError "Installed ASTER failed run validation."
        }
    }
    finally {
        [Environment]::SetEnvironmentVariable("ASTER_STDLIB", $oldStdlib, "Process")
        Remove-Item -Recurse -Force -LiteralPath $projectDirectory
    }
}

function Test-InstallationHealth {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$Version
    )
    try {
        [void](Validate-InstallRoot $InstallDirectory $Version)
        Test-InstalledCli $InstallDirectory $Version
        return $true
    }
    catch {
        return $false
    }
}

function Get-UpdatedWindowsPath {
    param(
        [AllowEmptyString()][string]$CurrentPath,
        [Parameter(Mandatory = $true)][string]$Entry
    )
    $normalizedEntry = [IO.Path]::GetFullPath($Entry).TrimEnd('\', '/')
    foreach ($existing in @($CurrentPath -split ';')) {
        if ([string]::IsNullOrWhiteSpace($existing)) { continue }
        $normalizedExisting = $existing.Trim().TrimEnd('\', '/')
        if ($normalizedExisting -ieq $normalizedEntry) {
            return $CurrentPath
        }
    }
    if ([string]::IsNullOrWhiteSpace($CurrentPath)) {
        return $normalizedEntry
    }
    return "$normalizedEntry;$CurrentPath"
}

function Add-AsterUserPath {
    param([Parameter(Mandatory = $true)][string]$BinDirectory)
    if ($env:ASTER_INSTALL_SKIP_PATH -eq "1") {
        return
    }
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $updated = Get-UpdatedWindowsPath ([string]$current) $BinDirectory
    if ($updated.Length -gt 32767) {
        Throw-InstallerError "The user PATH is too long to add ASTER safely."
    }
    if ($updated -cne [string]$current) {
        [Environment]::SetEnvironmentVariable("Path", $updated, "User")
    }
    $processPath = Get-UpdatedWindowsPath ([string]$env:Path) $BinDirectory
    $env:Path = $processPath
}

function Write-InstallState {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$Version
    )
    $state = [ordered]@{
        schema = 1
        product = $script:Product
        version = $Version
        target = $script:Target
    }
    $json = ($state | ConvertTo-Json -Depth 2) + "`n"
    [IO.File]::WriteAllText(
        (Join-Path $InstallDirectory "install-state.json"),
        $json,
        [Text.UTF8Encoding]::new($false)
    )
}

function Publish-ManagedReplacement {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [Parameter(Mandatory = $true)][string]$StagingDirectory,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)]$PreviousState,
        [Parameter(Mandatory = $true)][bool]$PreviousWasHealthy,
        [Parameter(Mandatory = $true)][scriptblock]$Validator
    )
    $backupDirectory = $InstallDirectory + ".backup-" + [Guid]::NewGuid().ToString("N")
    $oldMoved = $false
    $newPublished = $false
    try {
        [IO.Directory]::Move($InstallDirectory, $backupDirectory)
        $oldMoved = $true
        [IO.Directory]::Move($StagingDirectory, $InstallDirectory)
        $newPublished = $true
        Write-InstallState $InstallDirectory $Version
        & $Validator $InstallDirectory $Version "Final"
        Remove-Item -Recurse -Force -LiteralPath $backupDirectory
        return
    }
    catch {
        $originalError = $_.Exception.Message
        if ($newPublished -and (Test-Path -LiteralPath $InstallDirectory)) {
            Remove-Item -Recurse -Force -LiteralPath $InstallDirectory
        }
        try {
            if ($oldMoved -and (Test-Path -LiteralPath $backupDirectory)) {
                [IO.Directory]::Move($backupDirectory, $InstallDirectory)
            }
            $restored = Get-InstallDirectoryState $InstallDirectory
            if (
                $restored.Kind -ne "Managed" -or
                [string]$restored.State.version -ne [string]$PreviousState.version
            ) {
                Throw-InstallerError "The restored install-state.json does not match the previous installation."
            }
            if ($PreviousWasHealthy) {
                & $Validator $InstallDirectory ([string]$PreviousState.version) "Rollback"
            }
        }
        catch {
            Throw-InstallerError "ASTER update failed: $originalError Rollback also failed. Installation: $InstallDirectory Backup: $backupDirectory"
        }
        Throw-InstallerError "ASTER update failed: $originalError The previous installation was restored. Location: $InstallDirectory"
    }
}

function Invoke-AsterInstall {
    Assert-WindowsX64
    $allowInsecure = $env:ASTER_INSTALL_ALLOW_INSECURE -eq "1"
    $baseValue = if (Test-Path Env:ASTER_INSTALL_BASE_URL) {
        $env:ASTER_INSTALL_BASE_URL
    }
    else {
        $script:DefaultBaseUrl
    }
    $baseUri = Assert-SafeBaseUri $baseValue $allowInsecure
    $installValue = if (Test-Path Env:ASTER_INSTALL_DIR) {
        $env:ASTER_INSTALL_DIR
    }
    else {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
            Throw-InstallerError "LOCALAPPDATA is not available."
        }
        Join-Path $env:LOCALAPPDATA "Aster"
    }
    $installDirectory = Assert-SafeInstallDirectory $installValue
    $directoryState = Get-InstallDirectoryState $installDirectory
    if ($directoryState.Kind -eq "Managed") {
        Assert-ManagedInstallEntries $installDirectory
    }

    $downloadDirectory = Join-Path ([IO.Path]::GetTempPath()) ("aster-install-download-" + [Guid]::NewGuid().ToString("N"))
    $extractDirectory = Join-Path ([IO.Path]::GetTempPath()) ("aster-install-extract-" + [Guid]::NewGuid().ToString("N"))
    $archivePath = Join-Path $downloadDirectory $script:ArchiveName
    $checksumPath = Join-Path $downloadDirectory $script:ChecksumName
    $stagingDirectory = $null
    $publishedNew = $false
    $removedEmptyDirectory = $false
    [void][IO.Directory]::CreateDirectory($downloadDirectory)
    [void][IO.Directory]::CreateDirectory($extractDirectory)
    try {
        $archiveUri = [Uri]::new($baseUri.AbsoluteUri.TrimEnd("/") + "/" + $script:ArchiveName)
        $checksumUri = [Uri]::new($baseUri.AbsoluteUri.TrimEnd("/") + "/" + $script:ChecksumName)
        [void](Get-LimitedDownload $archiveUri $archivePath $script:MaximumArchiveBytes $allowInsecure)
        [void](Get-LimitedDownload $checksumUri $checksumPath $script:MaximumChecksumBytes $allowInsecure)
        Verify-ChecksumSidecar $archivePath $checksumPath

        $archiveInfo = Inspect-ZipArchive $archivePath
        Expand-ValidatedZip $archivePath $extractDirectory
        $extractedRoot = Join-Path $extractDirectory $archiveInfo.Root
        [void](Validate-InstallRoot $extractedRoot $archiveInfo.Version)

        $installedVersion = $null
        $previousWasHealthy = $false
        $operation = "Install"
        if ($directoryState.Kind -eq "Managed") {
            $installedVersion = [string]$directoryState.State.version
            $previousWasHealthy = Test-InstallationHealth $installDirectory $installedVersion
            if ($installedVersion -eq $archiveInfo.Version -and $previousWasHealthy) {
                Add-AsterUserPath (Join-Path $installDirectory "bin")
                Write-Host ""
                Write-Host "ASTER is already installed and healthy"
                Write-Host ""
                Write-Host "Version: $installedVersion"
                Write-Host "Location: $installDirectory"
                return
            }
            $operation = if ($installedVersion -eq $archiveInfo.Version) { "Repair" } else { "Update" }
        }

        $parent = [IO.Path]::GetDirectoryName($installDirectory)
        [void][IO.Directory]::CreateDirectory($parent)
        $stagingDirectory = $installDirectory + ".staging-" + [Guid]::NewGuid().ToString("N")
        Copy-Item -Recurse -LiteralPath $extractedRoot -Destination $stagingDirectory
        [void](Validate-InstallRoot $stagingDirectory $archiveInfo.Version)
        Test-InstalledCli $stagingDirectory $archiveInfo.Version

        if ($directoryState.Kind -eq "Managed") {
            $validator = {
                param($path, $version, $phase)
                Test-InstalledCli $path $version
            }
            Publish-ManagedReplacement `
                $installDirectory `
                $stagingDirectory `
                $archiveInfo.Version `
                $directoryState.State `
                $previousWasHealthy `
                $validator
            $stagingDirectory = $null
            Add-AsterUserPath (Join-Path $installDirectory "bin")
            Write-Host ""
            if ($operation -eq "Repair") {
                Write-Host "ASTER repaired successfully"
                Write-Host ""
                Write-Host "Version: $($archiveInfo.Version)"
                Write-Host "Location: $installDirectory"
            }
            else {
                Write-Host "ASTER updated successfully"
                Write-Host ""
                Write-Host "Previous version: $installedVersion"
                Write-Host "Current version: $($archiveInfo.Version)"
                Write-Host "Location: $installDirectory"
            }
            return
        }

        if ($directoryState.Kind -eq "Empty") {
            [IO.Directory]::Delete($installDirectory)
            $removedEmptyDirectory = $true
        }
        [IO.Directory]::Move($stagingDirectory, $installDirectory)
        $stagingDirectory = $null
        $publishedNew = $true

        Write-InstallState $installDirectory $archiveInfo.Version
        Test-InstalledCli $installDirectory $archiveInfo.Version
        Add-AsterUserPath (Join-Path $installDirectory "bin")

        Write-Host ""
        Write-Host "ASTER installed successfully"
        Write-Host ""
        Write-Host "Version: $($archiveInfo.Version)"
        Write-Host "Target: $($script:Target)"
        Write-Host "Location: $installDirectory"
        Write-Host "Command: aster"
        Write-Host ""
        Write-Host "Open a new terminal and run:"
        Write-Host "  aster --version"
    }
    catch {
        if ($publishedNew -and (Test-Path -LiteralPath $installDirectory)) {
            Remove-Item -Recurse -Force -LiteralPath $installDirectory
        }
        if ($removedEmptyDirectory -and -not (Test-Path -LiteralPath $installDirectory)) {
            [void][IO.Directory]::CreateDirectory($installDirectory)
        }
        throw
    }
    finally {
        if ($null -ne $stagingDirectory -and (Test-Path -LiteralPath $stagingDirectory)) {
            Remove-Item -Recurse -Force -LiteralPath $stagingDirectory
        }
        if (Test-Path -LiteralPath $downloadDirectory) {
            Remove-Item -Recurse -Force -LiteralPath $downloadDirectory
        }
        if (Test-Path -LiteralPath $extractDirectory) {
            Remove-Item -Recurse -Force -LiteralPath $extractDirectory
        }
    }
}

try {
    Invoke-AsterInstall
}
catch {
    [Console]::Error.WriteLine("error: " + $_.Exception.Message)
    exit 1
}
