# Reads pixels of a PNG (window captures made by tools/capture-window.ps1), used by chrome-e2e.mjs.
#
#   png-pixel.ps1 -Path <png> -Points "x,y;x,y"   -> JSON ["#rrggbb", ...] (image pixel coordinates)
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Points
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$bitmap = [System.Drawing.Bitmap]::FromFile($Path)
try {
    $out = @()
    foreach ($p in $Points.Split(";")) {
        if (-not $p) { continue }
        $xy = $p.Split(",")
        $x = [Math]::Min([Math]::Max([int]$xy[0], 0), $bitmap.Width - 1)
        $y = [Math]::Min([Math]::Max([int]$xy[1], 0), $bitmap.Height - 1)
        $c = $bitmap.GetPixel($x, $y)
        $out += ("#{0:x2}{1:x2}{2:x2}" -f $c.R, $c.G, $c.B)
    }
    ConvertTo-Json -Compress -InputObject @($out)
} finally {
    $bitmap.Dispose()
}
