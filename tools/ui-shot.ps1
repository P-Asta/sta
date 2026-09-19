# Screenshot a sta UI page in mock mode with headless Edge (no sta build needed).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/ui-shot.ps1 -Path '/sidebar/?mock' -Width 248 -Height 900 -Out sidebar.png
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/ui-shot.ps1 -Path '/_gallery/' -Width 1280 -Height 3000 -Out gallery-dark.png -Dark -Console
#
# A thin wrapper around tools/ui-shot.mjs (needs Node 22+ on PATH), which does all the work: it
# serves ui/ in-process (tools/ui-serve.mjs, correct MIME types), starts headless Edge with a
# throwaway profile, drives it over the DevTools protocol, writes the PNG and always stops Edge
# (whole process tree) and the server. Process management lives in Node on purpose: in Windows
# PowerShell 5.1, redirected native stderr (`taskkill ... 2>&1`) turns into terminating errors
# under $ErrorActionPreference = 'Stop'.
#   The viewport is pinned with Emulation.setDeviceMetricsOverride, so any size works, including
#   sidebar widths below Edge's minimum window width (~500 px), which `--window-size` and
#   `--screenshot` silently widen.
#   -Budget   real time (ms) to wait after the load event before capturing (default 3000); mock
#             pages are additionally awaited until they sent ui.ready (up to 10 s).
#   -Dark     appends dark=1 (fixture dark colors) and emulates a dark prefers-color-scheme.
#   -Console  also prints every console message and external load failure.
# Exit codes: 0 captured and clean; 1 capture failed; 2 captured but the page reported problems
# (console errors, exceptions, failed loads of ui/ files, no ui.ready, blank page). Problems are
# printed with or without -Console.
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$Width = 1280,
    [int]$Height = 900,
    [switch]$Dark,
    [switch]$Console,
    [int]$Budget = 3000,
    [double]$Scale = 1,
    [string]$Edge = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe',
    [string]$Node = 'node',
    # Obsolete (the server is Node now); accepted so older command lines keep working.
    [string]$Python = ''
)

$ErrorActionPreference = 'Stop'
$script = Join-Path $PSScriptRoot 'ui-shot.mjs'
if (-not (Test-Path $script)) { throw "missing $script" }

# Resolve the output path against the caller's current directory.
$outPath = [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).Path, $Out))
$inv = [Globalization.CultureInfo]::InvariantCulture
$nodeArgs = @(
    $script,
    '--path', $Path,
    '--out', $outPath,
    '--width', $Width.ToString($inv),
    '--height', $Height.ToString($inv),
    '--budget', $Budget.ToString($inv),
    '--scale', $Scale.ToString($inv),
    '--edge', $Edge
)
if ($Dark) { $nodeArgs += '--dark' }
if ($Console) { $nodeArgs += '--console' }

# ui-shot.mjs writes only to stdout. Should Node itself print to stderr (a crash), don't let
# PowerShell 5.1 turn that into a terminating NativeCommandError: the exit code decides.
$code = 1
$ErrorActionPreference = 'Continue'
try {
    & $Node @nodeArgs
    $code = $LASTEXITCODE
} catch {
    Write-Output "ui-shot: could not run node: $($_.Exception.Message)"
    $code = 1
} finally {
    $ErrorActionPreference = 'Stop'
}
if ($null -eq $code) { $code = 1 }
exit $code
