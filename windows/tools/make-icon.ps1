# Draws the Voice Not icon and writes assets\voice-not.ico plus a preview PNG.
#
# There is no resource compiler in this toolchain (no windres, no rc.exe), so the
# icon is drawn here and injected into the built exe by tools\embed-icon.ps1.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root 'assets'
New-Item -ItemType Directory -Force -Path $assets | Out-Null

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$ink = [System.Drawing.Color]::FromArgb(255, 91, 91, 214)      # indigo, reads on light and dark
$rim = [System.Drawing.Color]::FromArgb(255, 255, 255, 255)

function New-MicPng([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.Clear([System.Drawing.Color]::Transparent)

    # At 16 px the full glyph turns to mush: the strokes are barely a pixel wide.
    # Small sizes therefore get bolder strokes and no base line, which is the
    # usual "pixel-hinted" variant of an icon.
    $tiny = $size -le 16
    $bold = if ($size -le 24) { 1.3 } else { 1.0 }

    # Everything below is laid out on a 32x32 grid and scaled.
    $s = $size / 32.0
    $brush = New-Object System.Drawing.SolidBrush($ink)

    # Capsule body. The rectangle spans exactly the two circles' centres, so the
    # union is smooth: any other span leaves a step where the widths disagree.
    $cap = 4.2 * $s * $bold
    $cx = 16.0 * $s
    $top = 8.4 * $s
    $bottom = 15.4 * $s
    $g.FillRectangle($brush, [float]($cx - $cap), [float]$top, [float]($cap * 2), [float]($bottom - $top))
    $g.FillEllipse($brush, [float]($cx - $cap), [float]($top - $cap), [float]($cap * 2), [float]($cap * 2))
    $g.FillEllipse($brush, [float]($cx - $cap), [float]($bottom - $cap), [float]($cap * 2), [float]($cap * 2))

    # Cradle: the lower part of a circle around the body.
    $pen = New-Object System.Drawing.Pen($ink, [float](2.4 * $s * $bold))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $cradle = 8.6 * $s
    $cradleY = 13.0 * $s
    $g.DrawArc($pen, [float]($cx - $cradle), [float]($cradleY - $cradle), [float]($cradle * 2), [float]($cradle * 2), 22, 136)

    # Stem, and a base line that only the larger sizes can afford.
    $g.DrawLine($pen, [float]$cx, [float](21.0 * $s), [float]$cx, [float](25.4 * $s))
    if (-not $tiny) {
        $g.DrawLine($pen, [float](11.8 * $s), [float](25.8 * $s), [float](20.2 * $s), [float](25.8 * $s))
    }

    $g.Dispose()
    $stream = New-Object System.IO.MemoryStream
    $bmp.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $stream.ToArray()
    $stream.Dispose()

    if ($size -eq 256) {
        [System.IO.File]::WriteAllBytes((Join-Path $assets 'voice-not-preview.png'), $bytes)
    }
    $bmp.Dispose()
    return , $bytes
}

"drawing $($sizes.Count) sizes..."
$pngs = @()
foreach ($size in $sizes) { $pngs += , (New-MicPng $size) }

# Contact sheet of the small sizes at 8x, to judge whether they stay legible.
$zoom = 8
$sheetWidth = (16 + 24 + 32) * $zoom
$sheetHeight = 32 * $zoom
$sheet = New-Object System.Drawing.Bitmap($sheetWidth, $sheetHeight, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$sg = [System.Drawing.Graphics]::FromImage($sheet)
$sg.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$sg.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::Half
$sg.Clear([System.Drawing.Color]::FromArgb(255, 245, 245, 248))
$x = 0
foreach ($size in @(16, 24, 32)) {
    $stream = New-Object System.IO.MemoryStream(, $pngs[$sizes.IndexOf($size)])
    $bmp = New-Object System.Drawing.Bitmap($stream)
    $drawWidth = $size * $zoom
    $sg.DrawImage($bmp, $x, 0, $drawWidth, $drawWidth)
    $x += $drawWidth
    $bmp.Dispose()
    $stream.Dispose()
}
$sg.Dispose()
$sheet.Save((Join-Path $assets 'voice-not-preview-small.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$sheet.Dispose()

# Assemble the ICO container: header, one directory entry per image, then data.
$ms = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ms)
$bw.Write([uint16]0)                  # reserved
$bw.Write([uint16]1)                  # type: icon
$bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $size = $sizes[$i]
    $dim = if ($size -ge 256) { 0 } else { $size }   # 0 means 256
    $bw.Write([byte]$dim)
    $bw.Write([byte]$dim)
    $bw.Write([byte]0)                # palette colours
    $bw.Write([byte]0)                # reserved
    $bw.Write([uint16]1)              # planes
    $bw.Write([uint16]32)             # bits per pixel
    $bw.Write([uint32]$pngs[$i].Length)
    $bw.Write([uint32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($png in $pngs) { $bw.Write($png) }
$bw.Flush()

$ico = Join-Path $assets 'voice-not.ico'
[System.IO.File]::WriteAllBytes($ico, $ms.ToArray())
$bw.Dispose(); $ms.Dispose()

"wrote $ico ($([math]::Round((Get-Item $ico).Length/1KB,1)) KB)"
"wrote $(Join-Path $assets 'voice-not-preview.png')"
