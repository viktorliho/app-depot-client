param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$InnoCompiler = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    [ValidatePattern('^[1-9][0-9]{0,3}$')][string]$Candidate = '1',
    [switch]$BinaryOnly
)
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$out = [IO.Directory]::CreateDirectory($OutputDirectory).FullName
$version = [regex]::Match((Get-Content "$root\Cargo.toml" -Raw), '(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"$').Groups[1].Value
if (!$version) { throw 'Workspace version missing' }
$displayVersion = "$version-windows-rc.$Candidate"
$numericVersion = "$version.$Candidate"
$icon = Join-Path $out 'AppDepot.ico'
$resource = Join-Path $out 'desktop.res'
$resourceSource = Join-Path $out 'desktop.rc'
$exe = Join-Path $out 'app-depot-desktop.exe'
$setupName = 'AppDepot-Desktop-Windows-x64-Setup.exe'

# A modern ICO can store the original 256x256 PNG without re-encoding it.
[byte[]]$header = 0,0,1,0,1,0,0,0,0,0,1,0,32,0
$png = [IO.File]::ReadAllBytes("$root\assets\io.github.viktorliho.AppDepot-256.png")
[IO.File]::WriteAllBytes($icon, $header + [BitConverter]::GetBytes([int]$png.Length) + [BitConverter]::GetBytes(22) + $png)
$iconRc = $icon.Replace('\', '\\')
$versionRc = $numericVersion.Replace('.', ',')
@"
1 ICON "$iconRc"
1 VERSIONINFO
FILEVERSION $versionRc
PRODUCTVERSION $versionRc
FILEOS 0x40004
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "App Depot Desktop"
      VALUE "FileVersion", "$numericVersion"
      VALUE "OriginalFilename", "app-depot-desktop.exe"
      VALUE "ProductName", "App Depot Desktop"
      VALUE "ProductVersion", "$displayVersion"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"@ | Set-Content -LiteralPath $resourceSource -Encoding Ascii

Push-Location $root
try {
    & rc.exe /nologo "/fo$resource" $resourceSource
    if ($LASTEXITCODE) { throw 'Windows resource compilation failed' }
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
    $metadata = & cargo +1.100.0 metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE) { throw 'Cargo metadata failed' }
    $targetDir = ($metadata | ConvertFrom-Json).target_directory
    & cargo +1.100.0 rustc --locked --release -p app-depot-desktop --bin app-depot-desktop --target x86_64-pc-windows-msvc -- -C "link-arg=$resource"
    if ($LASTEXITCODE) { throw 'Windows release build failed' }
    Copy-Item -LiteralPath "$targetDir\x86_64-pc-windows-msvc\release\app-depot-desktop.exe" -Destination $exe
    $headers = & dumpbin.exe /nologo /headers $exe
    if ($LASTEXITCODE -or !($headers -match '8664 machine \(x64\)') -or !($headers -match 'Windows GUI')) {
        throw 'Expected a native x64 Windows GUI executable'
    }
    $dependencies = & dumpbin.exe /nologo /dependents $exe
    if ($LASTEXITCODE -or ($dependencies -match '(?i)\b(?:vcruntime|msvcp|ucrtbased)[^ ]*\.dll')) {
        throw 'The executable imports a non-inbox Visual C++ runtime'
    }
    if ((Get-Item -LiteralPath $exe).VersionInfo.ProductVersion -ne $displayVersion) {
        throw 'Incorrect Windows product version'
    }
    $files = @($exe)
    if (!$BinaryOnly) {
        if (!(Test-Path -LiteralPath $InnoCompiler -PathType Leaf)) { throw 'Inno Setup compiler not found. Pass -InnoCompiler.' }
        & $InnoCompiler "/DAppDepotVersion=$displayVersion" "/DAppDepotNumericVersion=$numericVersion" "/DAppDepotIcon=$icon" "/DAppDepotExecutable=$exe" "/O$out" "$PSScriptRoot\windows-setup.iss"
        if ($LASTEXITCODE) { throw 'Inno Setup packaging failed' }
        $files += Join-Path $out $setupName
    }
    $manifest = Join-Path $out 'SHA256SUMS'
    $files | ForEach-Object {
        "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 -LiteralPath $_).Hash.ToLowerInvariant(), (Split-Path -Leaf $_)
    } | Set-Content -LiteralPath $manifest -Encoding Ascii
    Get-Content -LiteralPath $manifest
    Get-Item -LiteralPath $files | Select-Object Name, Length | Format-Table -AutoSize
} finally {
    Pop-Location
}
