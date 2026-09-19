# Captures only sta's top-level window (not the whole screen) to a PNG.
#   powershell -File tools/capture-window.ps1 -Out shot.png [-ProcessId <browser pid>] [-ProcessName sta]
# With several instances running, pass -ProcessId (the pid of the browser process you started).
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$ProcessName = "sta",
    [int]$ProcessId = 0
)

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class StaCapture {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdc, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

if ($ProcessId -gt 0) {
    $pids = @([uint32]$ProcessId)
} else {
    $pids = @(Get-Process -Name $ProcessName -ErrorAction Stop | ForEach-Object { [uint32]$_.Id })
}
$script:best = [IntPtr]::Zero
$script:bestArea = 0
$cb = [StaCapture+EnumWindowsProc]{
    param($h, $l)
    $procId = [uint32]0
    [void][StaCapture]::GetWindowThreadProcessId($h, [ref]$procId)
    if ($pids -contains $procId -and [StaCapture]::IsWindowVisible($h) -and [StaCapture]::GetWindow($h, 4) -eq [IntPtr]::Zero) {
        $r = New-Object StaCapture+RECT
        [void][StaCapture]::GetWindowRect($h, [ref]$r)
        $area = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
        if ($area -gt $script:bestArea) { $script:best = $h; $script:bestArea = $area }
    }
    return $true
}
[void][StaCapture]::EnumWindows($cb, [IntPtr]::Zero)
if ($script:best -eq [IntPtr]::Zero) { Write-Error "no visible top-level window for $ProcessName"; exit 1 }

$rect = New-Object StaCapture+RECT
[void][StaCapture]::GetWindowRect($script:best, [ref]$rect)
$w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
# PW_RENDERFULLCONTENT (2) is required for GPU-composited Chromium content.
[void][StaCapture]::PrintWindow($script:best, $hdc, 2)
$g.ReleaseHdc($hdc)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out (${w}x${h})"
