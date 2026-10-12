# Drives a real mouse drag for testing drag-out on Windows: moves the
# pointer onto a point in one window (the demo, examples/dragout_demo.rs),
# presses the left button, moves in steps to another window (the log target,
# examples/dragout_target.rs, or an Explorer folder), waits, then releases the
# button, or presses Escape with -Escape.  The input is injected with
# mouse_event, so it goes through the same path as a real mouse and the drag
# runs in OLE's modal loop.  Run it in the logged-in user's desktop (a
# scheduled task with /it over SSH), since a session without a desktop has no
# pointer.
#
#   powershell -File dragout_mover.ps1 -FromX 60 -FromY 90 -ToTitle "Slerp drag target"
param(
    [string]$FromTitle = 'Slerp drag-out demo',
    # Where to press, in the source window's client area (pixels).
    [int]$FromX = 60, [int]$FromY = 90,
    # The window to drop on (title), and where in its client area; -1 is the middle.
    [string]$ToTitle = 'Slerp drag target',
    # The window class to find the target by instead of its title (an Explorer
    # folder window is CabinetWClass; the first one is used).
    [string]$ToClass = '',
    [int]$ToX = -1, [int]$ToY = -1,
    # Optional places to move the two windows to first (left, top).
    [int[]]$PlaceFrom = @(), [int[]]$PlaceTo = @(),
    [int]$Steps = 30, [int]$StepMs = 25, [int]$HoverMs = 800,
    [switch]$Escape,
    # Only click the -ClickAfter windows, without dragging.
    [switch]$NoDrag,
    # While hovering, make the source window paint synchronously (WM_PAINT sent to
    # it), to check that painting during OLE's loop doesn't upset the event loop.
    [switch]$PaintDuringHover,
    # Save a screenshot (PNG path) while hovering over the target, to see the drag picture.
    [string]$ShotAtHover = '',
    # Titles of windows to click afterwards, to see whether each takes the click
    # (a window left holding the mouse capture would swallow them).
    [string[]]$ClickAfter = @(),
    [string]$Log = 'C:\dragtest\mover.log'
)

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Native {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vk);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool RedrawWindow(IntPtr h, IntPtr rect, IntPtr region, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, System.Text.StringBuilder text, int max);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
'@
[Native]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path $Log) | Out-Null
function Say($text) { Add-Content -Path $Log -Value ("{0:HH:mm:ss.fff} {1}" -f (Get-Date), $text) }
Remove-Item $Log -ErrorAction SilentlyContinue

$MOVE = 1; $LEFTDOWN = 2; $LEFTUP = 4
$sw = [Native]::GetSystemMetrics(0); $sh = [Native]::GetSystemMetrics(1)
Say "screen ${sw}x${sh}"

function Find($title, $class) {
    if ($class) { return [Native]::FindWindow($class, [NullString]::Value) }
    return [Native]::FindWindow([NullString]::Value, $title)
}
function Place($title, $at, $class) {
    $h = Find $title $class
    if ($h -eq [IntPtr]::Zero) { Say "no window titled '$title'"; return }
    if ($at.Count -ge 2) { [Native]::SetWindowPos($h, [IntPtr]::Zero, $at[0], $at[1], 0, 0, 0x0001 -bor 0x0004) | Out-Null }
}
Place $FromTitle $PlaceFrom ''
Place $ToTitle $PlaceTo $ToClass
Start-Sleep -Milliseconds 500

function ClientPoint($title, $x, $y, $class) {
    $h = Find $title $class
    if ($h -eq [IntPtr]::Zero) { Say "no window titled '$title'"; exit 2 }
    $r = New-Object Native+RECT
    [Native]::GetClientRect($h, [ref]$r) | Out-Null
    if ($x -lt 0) { $x = [int]($r.Right / 2) }
    if ($y -lt 0) { $y = [int]($r.Bottom / 2) }
    $p = New-Object Native+POINT
    $p.X = $x; $p.Y = $y
    [Native]::ClientToScreen($h, [ref]$p) | Out-Null
    return $p
}
function PointerTo($x, $y) {
    [Native]::mouse_event($MOVE -bor 0x8000, [int]($x * 65535 / ($sw - 1)), [int]($y * 65535 / ($sh - 1)), 0, [UIntPtr]::Zero)
}

if (-not $NoDrag) {
    $from = ClientPoint $FromTitle $FromX $FromY ''
    $to = ClientPoint $ToTitle $ToX $ToY $ToClass
    Say "from ($($from.X), $($from.Y)) to ($($to.X), $($to.Y))"
    PointerTo $from.X $from.Y
    Start-Sleep -Milliseconds 400
    $c = New-Object Native+POINT
    [Native]::GetCursorPos([ref]$c) | Out-Null
    Say "cursor at ($($c.X), $($c.Y))"
    [Native]::mouse_event($LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    [Native]::GetCursorPos([ref]$c) | Out-Null
    Say "pressed, button state $([Native]::GetAsyncKeyState(1)), cursor ($($c.X), $($c.Y))"
    Start-Sleep -Milliseconds 200
    # A few pixels first, so the interface sees a drag begin, then across.
    PointerTo ($from.X + 6) ($from.Y + 6)
    Start-Sleep -Milliseconds 300
    for ($i = 1; $i -le $Steps; $i++) {
        PointerTo ($from.X + 6 + ($to.X - $from.X - 6) * $i / $Steps) ($from.Y + 6 + ($to.Y - $from.Y - 6) * $i / $Steps)
        Start-Sleep -Milliseconds $StepMs
    }
    [Native]::GetCursorPos([ref]$c) | Out-Null
    Say "arrived, cursor ($($c.X), $($c.Y))"
    Start-Sleep -Milliseconds ($HoverMs / 2)
    if ($PaintDuringHover) {
        $source = Find $FromTitle ''
        for ($i = 0; $i -lt 5; $i++) {
            # RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW
            $ok = [Native]::RedrawWindow($source, [IntPtr]::Zero, [IntPtr]::Zero, 0x1 -bor 0x4 -bor 0x80 -bor 0x100)
            Say "painted the source window during the drag: $ok"
            Start-Sleep -Milliseconds 100
        }
    }
    if ($ShotAtHover) {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
        $bitmap = New-Object Drawing.Bitmap $screen.Width, $screen.Height
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($screen.Location, [Drawing.Point]::Empty, $screen.Size)
        $bitmap.Save($ShotAtHover)
        Say "screenshot saved to $ShotAtHover"
    }
    Start-Sleep -Milliseconds ($HoverMs / 2)
    if ($Escape) {
        [Native]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 100
        [Native]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero)
        Say 'escape'
        Start-Sleep -Milliseconds 300
    }
    [Native]::mouse_event($LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Say 'released'
    Start-Sleep -Milliseconds 800
}
foreach ($title in $ClickAfter) {
    $p = ClientPoint $title (-1) (-1) ''
    Say "clicking '$title' at ($($p.X), $($p.Y))"
    PointerTo $p.X $p.Y
    Start-Sleep -Milliseconds 300
    $under = New-Object Native+POINT
    [Native]::GetCursorPos([ref]$under) | Out-Null
    Say "cursor at ($($under.X), $($under.Y))"
    [Native]::mouse_event($LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 100
    [Native]::mouse_event($LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
    $text = New-Object System.Text.StringBuilder 256
    [Native]::GetWindowText([Native]::GetForegroundWindow(), $text, 256) | Out-Null
    Say "clicked '$title', the foreground window is now '$($text.ToString())'"
}
Say 'done'
