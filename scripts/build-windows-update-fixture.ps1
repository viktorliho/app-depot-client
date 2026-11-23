param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
if ($env:APP_DEPOT_OFFICIAL_RELEASE -eq '1') { throw 'Update fixtures are not official releases' }

$output = [IO.Directory]::CreateDirectory($OutputDirectory).FullName
$source = Join-Path ([IO.Path]::GetTempPath()) "app-depot-fixture-source-$([guid]::NewGuid())"
[IO.Directory]::CreateDirectory($source) | Out-Null
$archive = Join-Path $source 'source.zip'
$checkout = Join-Path $source 'checkout'
$root = Split-Path -Parent $PSScriptRoot
$new = Join-Path $output 'B'
$old = Join-Path $output 'A'

Push-Location $root
try {
    # Both fixtures use the current application and Setup scripts. Only A's
    # isolated workspace version is reduced to exercise an actual upgrade.
    & git archive --format=zip --output=$archive HEAD
    if ($LASTEXITCODE) { throw 'Cannot make an isolated update fixture source copy' }
    Expand-Archive -LiteralPath $archive -DestinationPath $checkout
    if (!$env:CARGO_TARGET_DIR) {
        $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    }
    & "$PSScriptRoot\build-windows.ps1" -Stable -UpdateFixture -OutputDirectory $new

    $manifest = Join-Path $checkout 'Cargo.toml'
    $lock = Join-Path $checkout 'Cargo.lock'
    $content = [IO.File]::ReadAllText($manifest)
    $version = [regex]::Match($content, '(?m)^version = "(\d+\.\d+\.\d+)"\r?$').Groups[1].Value
    if (!$version -or $version -eq '0.0.0') { throw 'Invalid workspace version for update fixture' }
    [IO.File]::WriteAllText($manifest, $content.Replace("version = `"$version`"", 'version = "0.0.0"'))
    $lockContent = [IO.File]::ReadAllText($lock)
    $lockContent = [regex]::Replace($lockContent, '(?m)(^name = "app-depot-(?:cli|core|desktop|update-helper)"\r?\nversion = ")' + [regex]::Escape($version) + '"', '${1}0.0.0"')
    # The CLI package is named app-depot in this workspace.
    $lockContent = $lockContent.Replace("name = `"app-depot`"`nversion = `"$version`"", "name = `"app-depot`"`nversion = `"0.0.0`"")
    $lockContent = $lockContent.Replace("name = `"app-depot`"`r`nversion = `"$version`"", "name = `"app-depot`"`r`nversion = `"0.0.0`"")
    [IO.File]::WriteAllText($lock, $lockContent)
    & "$checkout\scripts\build-windows.ps1" -Stable -UpdateFixture -OutputDirectory $old

    $setup = 'AppDepot-Desktop-Windows-x64-Setup.exe'
    $tag = "v$version"
    $image = "AppDepot-$version-x86_64.AppImage"
    $assets = foreach ($name in $image, "$image.sha256", "$image.zsync", $setup, 'SHA256SUMS') {
        $file = Join-Path $new $name
        $exists = Test-Path -LiteralPath $file -PathType Leaf
        @{
            name = $name
            size = if ($exists) { (Get-Item -LiteralPath $file).Length } else { 1 }
            digest = if ($exists) { "sha256:$((Get-FileHash -LiteralPath $file).Hash.ToLowerInvariant())" } else { "sha256:$('0' * 64)" }
            browser_download_url = "https://github.com/viktorliho/app-depot-client/releases/download/$tag/$name"
        }
    }
    @{
        tag_name = $tag
        draft = $false
        prerelease = $false
        body = 'Isolated Windows update fixture'
        assets = @($assets)
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $new 'latest.json') -Encoding Ascii
} finally {
    Pop-Location
}
