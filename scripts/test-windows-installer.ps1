param([Parameter(Mandatory)][string]$PackageDirectory)
$ErrorActionPreference = 'Stop'

$package = (Resolve-Path -LiteralPath $PackageDirectory).Path
$binary = Join-Path $package 'app-depot-desktop.exe'
$setup = Join-Path $package 'AppDepot-Desktop-Windows-x64-Setup.exe'
$checksums = @(Get-Content -LiteralPath (Join-Path $package 'SHA256SUMS'))
foreach ($file in @($setup, $binary)) {
    $hash = (Get-FileHash -LiteralPath $file).Hash.ToLowerInvariant()
    if ("$hash  $(Split-Path -Leaf $file)" -cnotin $checksums) {
        throw "Checksum mismatch: $file"
    }
}

function Invoke-SmokeProcess([string]$File, [string]$Arguments) {
    $process = Start-Process -FilePath $File -ArgumentList $Arguments -Wait -PassThru -WindowStyle Hidden
    if ($process.ExitCode -ne 0) { throw "$File exited with $($process.ExitCode)" }
}

$fixture = [IO.Directory]::CreateTempSubdirectory('app-depot-installer-').FullName
$install = Join-Path $fixture 'program'
$library = Join-Path $fixture 'library'
[IO.Directory]::CreateDirectory($library) | Out-Null
$sentinel = Join-Path $library 'retained.txt'
[IO.File]::WriteAllText($sentinel, 'retain user data')
$env:APP_DEPOT_LIBRARY_DATA_DIR = $library
$entry = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.viktorliho.AppDepot_is1'
if (Test-Path $entry) { throw 'Installer testing requires a disposable account without an existing App Depot installation' }
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'App Depot Desktop.lnk'
if (Test-Path -LiteralPath $shortcut) { throw 'Existing Start menu shortcut would be overwritten' }
$options = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /DIR=""$install"""
foreach ($pass in @('initial install', 'reinstall update')) {
    Invoke-SmokeProcess $setup "$options /LOG=""$fixture\$pass.log"""
    $installed = Join-Path $install 'app-depot-desktop.exe'
    if ((Get-FileHash -LiteralPath $installed).Hash -ne (Get-FileHash -LiteralPath $binary).Hash) {
        throw 'Installed binary differs from the release executable'
    }
    if (!(Test-Path $entry) -or !(Test-Path -LiteralPath $shortcut)) {
        throw 'Expected a per-user uninstall entry and Start menu shortcut'
    }
    Invoke-SmokeProcess $installed '--version'
}
Invoke-SmokeProcess (Join-Path $install 'unins000.exe') '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
if ((Test-Path -LiteralPath $installed) -or (Test-Path $entry) -or (Test-Path -LiteralPath $shortcut)) {
    throw 'Uninstaller left registered application files'
}
if ([IO.File]::ReadAllText($sentinel) -ne 'retain user data') {
    throw 'Installer or uninstaller changed the library'
}
