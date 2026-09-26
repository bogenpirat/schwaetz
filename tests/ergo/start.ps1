# Starts a throwaway Ergo IRC server for integration/manual testing.
#
# Usage:  $p = .\tests\ergo\start.ps1 -Port 16667      # returns the Process
#
# Downloads the pinned Ergo release (tests/ergo/VERSION) into tests/ergo/bin on first use and
# creates a fresh state directory per run (in-memory history, open registration, no throttling).

param([int]$Port = 16667, [string]$StateDir = "")
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$version = (Get-Content "$root/VERSION").Trim()
$dist = "$root/bin/ergo-$version-windows-x86_64"
if (-not (Test-Path "$dist/ergo.exe")) {
    New-Item -ItemType Directory -Force "$root/bin" | Out-Null
    $zip = "$root/bin/ergo.zip"
    Invoke-WebRequest -Uri "https://github.com/ergochat/ergo/releases/download/v$version/ergo-$version-windows-x86_64.zip" -OutFile $zip
    Expand-Archive -Force $zip "$root/bin"
    Remove-Item $zip
}
if (-not $StateDir) { $StateDir = Join-Path ([System.IO.Path]::GetTempPath()) "schwaetz-ergo-$Port-$PID" }
Remove-Item -Recurse -Force $StateDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $StateDir | Out-Null

$distFwd = (Resolve-Path $dist).Path -replace '\\', '/'
$yaml = Get-Content "$dist/default.yaml" -Raw
$yaml = $yaml -replace '"127\.0\.0\.1:6667":', "`"127.0.0.1:$Port`":"
$yaml = $yaml -replace '\s*"\[::1\]:6667":[^\r\n]*', ''
$yaml = $yaml -replace '(?s)\r?\n        ":6697":.*?proxy: false', ''
$yaml = $yaml -replace 'throttle: true', 'throttle: false'
$yaml = $yaml -replace 'max-concurrent-connections: 16', 'max-concurrent-connections: 512'
$yaml = $yaml -replace 'path: languages', "path: `"$distFwd/languages`""
$yaml = $yaml -replace 'motd: ergo\.motd', "motd: `"$distFwd/ergo.motd`""
$yaml = $yaml -replace 'name: ergo\.test', 'name: irc.schwaetz.test'
# No fakelag: load tests need the server to relay floods at full speed.
$yaml = $yaml -replace '(?m)^(fakelag:\r?\n\s*#[^\r\n]*\r?\n\s*enabled:) true', '$1 false'
Set-Content "$StateDir/ircd.yaml" $yaml -Encoding utf8

Push-Location $StateDir
try {
    & "$dist/ergo.exe" initdb --conf ircd.yaml --quiet 2>&1 | Out-Null
    $p = Start-Process -FilePath "$dist/ergo.exe" -ArgumentList 'run', '--conf', 'ircd.yaml' -WorkingDirectory $StateDir -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$StateDir/stdout.log" -RedirectStandardError "$StateDir/stderr.log"
} finally { Pop-Location }

# Wait until the port accepts connections.
$deadline = (Get-Date).AddSeconds(15)
while ((Get-Date) -lt $deadline) {
    try { $c = New-Object System.Net.Sockets.TcpClient('127.0.0.1', $Port); $c.Close(); break } catch { Start-Sleep -Milliseconds 100 }
}
$p
