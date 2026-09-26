# Load test: connects a bot to a local test server and floods a channel.
#
# Usage: .\scripts\flood.ps1 -Port 16667 -Channel "#a" -Rate 1000 -Seconds 5

param([int]$Port = 16667, [string]$Channel = "#a", [int]$Rate = 1000, [int]$Seconds = 5, [string]$Nick = "flooder")
$ErrorActionPreference = 'Stop'
$c = New-Object System.Net.Sockets.TcpClient('127.0.0.1', $Port)
$s = $c.GetStream()
$w = New-Object System.IO.StreamWriter($s)
$w.NewLine = "`r`n"
$w.AutoFlush = $false
$w.WriteLine("NICK $Nick"); $w.WriteLine("USER $Nick 0 * :flood bot"); $w.Flush()
Start-Sleep -Milliseconds 800
$w.WriteLine("JOIN $Channel"); $w.Flush()
Start-Sleep -Milliseconds 300
$words = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".Split(' ')
$sw = [Diagnostics.Stopwatch]::StartNew()
$sent = 0
$total = $Rate * $Seconds
while ($sent -lt $total) {
    $due = [math]::Min($total, [int]($sw.Elapsed.TotalSeconds * $Rate))
    while ($sent -lt $due) {
        $n = $sent % $words.Length
        $w.WriteLine("PRIVMSG $Channel :message $sent $($words[$n]) $($words[($n + 3) % $words.Length]) https://example.com/$sent")
        $sent++
    }
    $w.Flush()
    Start-Sleep -Milliseconds 10
}
$w.WriteLine("QUIT :done"); $w.Flush()
Start-Sleep -Milliseconds 300
$c.Close()
Write-Host "sent $sent lines in $([math]::Round($sw.Elapsed.TotalSeconds, 2))s"
