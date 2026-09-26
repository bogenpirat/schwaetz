# Renders the schwätz app icon (a speech bubble) into crates/app/res/schwaetz.ico.
# Only needed when changing the design; the generated .ico is committed.

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'
$out = Join-Path (Split-Path $PSScriptRoot -Parent) 'crates/app/res/schwaetz.ico'
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null

function Render([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.InterpolationMode = 'HighQualityBicubic'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0

    # Bubble body with a tail, gradient blue → violet.
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = 56 * $s; $x = 16 * $s; $y = 20 * $s; $w = 224 * $s; $h = 180 * $s
    $path.AddArc($x, $y, $r, $r, 180, 90)
    $path.AddArc($x + $w - $r, $y, $r, $r, 270, 90)
    $path.AddArc($x + $w - $r, $y + $h - $r, $r, $r, 0, 90)
    $path.AddLine($x + $w - $r, $y + $h, 100 * $s, $y + $h)
    $path.AddLine(100 * $s, $y + $h, 52 * $s, 244 * $s)
    $path.AddLine(52 * $s, 244 * $s, 64 * $s, $y + $h)
    $path.AddArc($x, $y + $h - $r, $r, $r, 90, 90)
    $path.CloseFigure()
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, 0), (New-Object System.Drawing.PointF $size, $size), ([System.Drawing.Color]::FromArgb(255, 94, 140, 255)), ([System.Drawing.Color]::FromArgb(255, 168, 108, 255))
    $g.FillPath($brush, $path)

    # Three dots.
    $dot = [System.Drawing.Brushes]::White
    $d = 34 * $s
    foreach ($cx in 76, 128, 180) {
        $g.FillEllipse($dot, ($cx * $s) - $d / 2, 110 * $s - $d / 2, $d, $d)
    }
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return , $ms.ToArray()
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$images = foreach ($sz in $sizes) { , (Render $sz) }
$fs = [System.IO.File]::Create($out)
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $sz = $sizes[$i]; $data = $images[$i]
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([UInt16]1); $bw.Write([UInt16]32)
    $bw.Write([UInt32]$data.Length); $bw.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($data in $images) { $bw.Write($data) }
$bw.Close()
Write-Host "Wrote $out"
