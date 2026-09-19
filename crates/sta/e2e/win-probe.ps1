# Native probes of sta's top-level window, used by shell-e2e.mjs and chrome-e2e.mjs.
# No global input injection and no screen capture: only messages to our own process's windows.
#
#   win-probe.ps1 -ProcessId <pid> info                 -> JSON {hwnd,left,top,width,height,dpi,zoomed,iconic,thickFrame,enabled,foreground}
#   win-probe.ps1 -ProcessId <pid> hittest "x,y;x,y"    -> JSON [WM_NCHITTEST codes] (window-relative pixels)
#   win-probe.ps1 -ProcessId <pid> close                -> posts WM_CLOSE (the Alt+F4 / taskbar path)
#   win-probe.ps1 -ProcessId <pid> restore              -> posts WM_SYSCOMMAND SC_RESTORE
#   win-probe.ps1 -ProcessId <pid> dialogs              -> JSON [{hwnd,title,owner}] visible #32770 dialogs of the process
#   win-probe.ps1 -ProcessId <pid> closedialogs         -> posts WM_CLOSE to those dialogs
#   win-probe.ps1 -ProcessId <pid> modifiers            -> JSON {ctrl,shift,alt} (GetAsyncKeyState; detects stuck keys)
#   win-probe.ps1 -ProcessId 0 consoles                 -> JSON [{hwnd,cls,pid,title,visible}] console windows on the whole desktop
#
# Hit-test codes: 1 CLIENT, 2 CAPTION, 10 LEFT, 11 RIGHT, 12 TOP, 13 TOPLEFT, 14 TOPRIGHT,
# 15 BOTTOM, 16 BOTTOMLEFT, 17 BOTTOMRIGHT.
param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true, Position = 0)][string]$Cmd,
    [Parameter(Position = 1)][string]$Points = ""
)
$ErrorActionPreference = "Stop"
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class StaProbe {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr hWnd, uint cmd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr hWnd, int index);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr hWnd, StringBuilder name, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int max);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

if ($Cmd -eq "modifiers") {
    $down = { param($vk) ([StaProbe]::GetAsyncKeyState($vk) -band 0x8000) -ne 0 }
    [ordered]@{ ctrl = (& $down 0x11); shift = (& $down 0x10); alt = (& $down 0x12) } | ConvertTo-Json -Compress
    exit 0
}

# Every console-class top-level window on the desktop, whoever owns it. -ProcessId is ignored.
# The desktop half of the "no console window while testing" rule for a suite that cannot arm the
# in-browser watcher (migration-e2e): sampling, not hooking, so it sees a window that is *up* when
# it is called, not one that only flashed between two samples.
if ($Cmd -eq "consoles") {
    $script:consoles = @()
    $scan = [StaProbe+EnumWindowsProc] {
        param($h, $l)
        $cls = New-Object System.Text.StringBuilder 128
        [void][StaProbe]::GetClassName($h, $cls, 128)
        $name = $cls.ToString()
        if ($name -eq "ConsoleWindowClass" -or $name -eq "CASCADIA_HOSTING_WINDOW_CLASS" -or $name -eq "PseudoConsoleWindow") {
            $procId = [uint32]0
            [void][StaProbe]::GetWindowThreadProcessId($h, [ref]$procId)
            $title = New-Object System.Text.StringBuilder 256
            [void][StaProbe]::GetWindowText($h, $title, 256)
            $script:consoles += [ordered]@{
                hwnd    = [int64]$h
                cls     = $name
                pid     = [int64]$procId
                title   = $title.ToString()
                visible = [bool][StaProbe]::IsWindowVisible($h)
            }
        }
        return $true
    }
    [void][StaProbe]::EnumWindows($scan, [IntPtr]::Zero)
    ConvertTo-Json -Compress -Depth 4 -InputObject @($script:consoles)
    exit 0
}

# Visible top-level windows of the process: the largest unowned one is the main window (minimized
# windows are still visible, so this also finds an iconic window); #32770 windows are dialogs.
$script:best = [IntPtr]::Zero
$script:area = -1
$script:dialogs = @()
$callback = [StaProbe+EnumWindowsProc] {
    param($h, $l)
    $procId = [uint32]0
    [void][StaProbe]::GetWindowThreadProcessId($h, [ref]$procId)
    if ($procId -eq $ProcessId -and [StaProbe]::IsWindowVisible($h)) {
        $cls = New-Object System.Text.StringBuilder 64
        [void][StaProbe]::GetClassName($h, $cls, 64)
        $owner = [StaProbe]::GetWindow($h, 4)
        if ($cls.ToString() -eq "#32770") {
            $title = New-Object System.Text.StringBuilder 256
            [void][StaProbe]::GetWindowText($h, $title, 256)
            $script:dialogs += [ordered]@{ hwnd = [int64]$h; title = $title.ToString(); owner = [int64]$owner }
        } elseif ($owner -eq [IntPtr]::Zero) {
            $r = New-Object StaProbe+RECT
            [void][StaProbe]::GetWindowRect($h, [ref]$r)
            $a = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
            if ($a -gt $script:area) { $script:area = $a; $script:best = $h }
        }
    }
    return $true
}
[void][StaProbe]::EnumWindows($callback, [IntPtr]::Zero)

if ($Cmd -eq "dialogs") { ConvertTo-Json -Compress -InputObject @($script:dialogs); exit 0 }
if ($Cmd -eq "closedialogs") {
    foreach ($d in $script:dialogs) { [void][StaProbe]::PostMessage([IntPtr]$d.hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
    ConvertTo-Json -Compress -InputObject @{ closed = @($script:dialogs).Count }
    exit 0
}

$h = $script:best
if ($h -eq [IntPtr]::Zero) { Write-Output '{"error":"no window"}'; exit 1 }
$r = New-Object StaProbe+RECT
[void][StaProbe]::GetWindowRect($h, [ref]$r)

switch ($Cmd) {
    "info" {
        $style = [StaProbe]::GetWindowLong($h, -16)
        [ordered]@{
            hwnd       = [int64]$h
            left       = $r.Left
            top        = $r.Top
            width      = $r.Right - $r.Left
            height     = $r.Bottom - $r.Top
            dpi        = [StaProbe]::GetDpiForWindow($h)
            zoomed     = [StaProbe]::IsZoomed($h)
            iconic     = [StaProbe]::IsIconic($h)
            thickFrame = (($style -band 0x40000) -ne 0)
            enabled    = [StaProbe]::IsWindowEnabled($h)
            foreground = ([StaProbe]::GetForegroundWindow() -eq $h)
        } | ConvertTo-Json -Compress
    }
    "hittest" {
        $codes = @()
        foreach ($p in $Points.Split(";")) {
            if (-not $p) { continue }
            $xy = $p.Split(",")
            $x = $r.Left + [int]$xy[0]
            $y = $r.Top + [int]$xy[1]
            $lp = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
            $codes += [int][StaProbe]::SendMessage($h, 0x84, [IntPtr]::Zero, $lp)
        }
        ConvertTo-Json -Compress -InputObject @($codes)
    }
    "close" { [void][StaProbe]::PostMessage($h, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); '{"posted":"WM_CLOSE"}' }
    "restore" { [void][StaProbe]::PostMessage($h, 0x112, [IntPtr]0xF120, [IntPtr]::Zero); '{"posted":"SC_RESTORE"}' }
    default { Write-Output "{`"error`":`"unknown command $Cmd`"}"; exit 2 }
}
