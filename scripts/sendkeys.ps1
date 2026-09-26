# Sends keystrokes to a running schwaetz.exe (for scripted UI checks).
# Keys use WScript SendKeys syntax: "%4" = Alt+4, "^j" = Ctrl+J, "{ENTER}", "hello".
#
# Usage: .\scripts\sendkeys.ps1 -Keys "%4" [-ProcessId 1234] [-DelayMs 300]

param([Parameter(Mandatory)][string[]]$Keys, [int]$ProcessId = 0, [int]$DelayMs = 250)
$ErrorActionPreference = 'Stop'
$p = if ($ProcessId) { Get-Process -Id $ProcessId } else { Get-Process schwaetz | Select-Object -First 1 }
$sh = New-Object -ComObject WScript.Shell
[void]$sh.AppActivate($p.Id)
Start-Sleep -Milliseconds 200
foreach ($k in $Keys) {
    $sh.SendKeys($k)
    Start-Sleep -Milliseconds $DelayMs
}
