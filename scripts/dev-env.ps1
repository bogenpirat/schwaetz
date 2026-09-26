# Imports an MSVC developer environment into the current PowerShell session.
#
# Usage:  . .\scripts\dev-env.ps1
#
# Only needed when `cargo build` fails with "link.exe not found" — typically when the default
# MSVC toolset of a Visual Studio install is incomplete. Picks the newest toolset that actually
# contains link.exe and runs vcvarsall.bat for it.

$ErrorActionPreference = 'Stop'
if (Get-Command link.exe -ErrorAction SilentlyContinue | Where-Object { $_.Source -like '*MSVC*' }) { return }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { throw 'vswhere.exe not found; install Visual Studio Build Tools with the C++ workload.' }
$vs = & $vswhere -latest -products * -property installationPath
if (-not $vs) { throw 'No Visual Studio installation found.' }

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$hostDir = if ($arch -eq 'arm64') { 'HostARM64\arm64' } else { 'HostX64\x64' }
$toolset = Get-ChildItem "$vs\VC\Tools\MSVC" -Directory |
    Where-Object { Test-Path (Join-Path $_.FullName "bin\$hostDir\link.exe") } |
    Sort-Object { [version]$_.Name } -Descending |
    Select-Object -First 1
if (-not $toolset) { throw "No MSVC toolset with link.exe under $vs\VC\Tools\MSVC." }

$vcvars = "$vs\VC\Auxiliary\Build\vcvarsall.bat"
$ver = ([version]$toolset.Name)
$lines = cmd /c "`"$vcvars`" $arch -vcvars_ver=$($ver.Major).$($ver.Minor) >nul 2>&1 && set" 2>$null
foreach ($line in $lines) {
    if ($line -match '^([^=]+)=(.*)$') { Set-Item -Path "env:$($Matches[1])" -Value $Matches[2] }
}
Write-Host "MSVC $($toolset.Name) environment loaded ($arch)."
