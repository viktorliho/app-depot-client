$ErrorActionPreference = 'Stop'

$source = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-setup.iss') -Raw
$setup = [regex]::Match($source, '(?ms)^\[Setup\][^\S\r\n]*\r?\n(.*?)(?=^\[|\z)').Groups[1].Value
$expected = @{
    MinVersion = '10.0.19041'
    ArchitecturesAllowed = 'x64os'
    ArchitecturesInstallIn64BitMode = 'x64os'
    PrivilegesRequired = 'lowest'
}
foreach ($key in $expected.Keys) {
    $matches = [regex]::Matches($setup, "(?mi)^[ \t]*$key[ \t]*=[ \t]*([^\r\n]+)")
    if ($matches.Count -ne 1 -or $matches[0].Groups[1].Value.Trim() -cne $expected[$key]) {
        throw "Expected $key=$($expected[$key]) exactly once in the installer [Setup] section"
    }
}
'Windows installer configuration admits Windows 10 build 19041 and newer on x64, per user.'
