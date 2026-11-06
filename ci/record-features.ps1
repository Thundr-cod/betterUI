# Records a short video of each betterUI feature being used, on the CI
# Windows machine. betterUI runs with --demo (made-up audio devices,
# Bluetooth devices and tray icons, since the CI machine has none) and with
# BETTERUI_TEST_LAYOUT set, so every window writes where its buttons are
# and this script can click the real controls.
#
# Usage: ./ci/record-features.ps1 -Exe .\betterui.exe -Out videos

param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Out
)

$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$DataDir = Join-Path $env:APPDATA 'betterUI'
$Ffmpeg = (Get-Command ffmpeg).Source
$env:BETTERUI_TEST_LAYOUT = '1'

Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Input {
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X; public int Y; }
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, int data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
}
"@

# ---- Mouse and keyboard -------------------------------------------------

function Move-To([int] $x, [int] $y, [int] $ms = 400) {
    $p = New-Object Input+POINT
    [Input]::GetCursorPos([ref] $p) | Out-Null
    $steps = [Math]::Max(1, [int]($ms / 15))
    for ($i = 1; $i -le $steps; $i++) {
        $t = $i / $steps
        $t = $t * $t * (3 - 2 * $t)   # ease in/out
        [Input]::SetCursorPos([int]($p.X + ($x - $p.X) * $t), [int]($p.Y + ($y - $p.Y) * $t)) | Out-Null
        Start-Sleep -Milliseconds 15
    }
}

function Click([int] $x, [int] $y, [switch] $Right) {
    Move-To $x $y
    Start-Sleep -Milliseconds 150
    if ($Right) { $down = 0x0008; $up = 0x0010 } else { $down = 0x0002; $up = 0x0004 }
    [Input]::mouse_event($down, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [Input]::mouse_event($up, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

function Drag([int] $x1, [int] $y1, [int] $x2, [int] $y2) {
    Move-To $x1 $y1
    Start-Sleep -Milliseconds 150
    [Input]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Move-To $x2 $y2 700
    [Input]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

function Wheel([int] $notches) {
    for ($i = 0; $i -lt [Math]::Abs($notches); $i++) {
        [Input]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($notches), [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Milliseconds 300
}

function Keys([string] $keys) {
    [System.Windows.Forms.SendKeys]::SendWait($keys)
    Start-Sleep -Milliseconds 250
}

# ---- Where things are (written by betterUI) -----------------------------

function Layout([string] $name) {
    $file = Join-Path $DataDir "layout_$name.txt"
    for ($i = 0; $i -lt 40 -and -not (Test-Path $file); $i++) { Start-Sleep -Milliseconds 100 }
    $map = @{}
    if (Test-Path $file) {
        foreach ($line in Get-Content $file) {
            $p = $line.Split('|')
            if ($p.Count -eq 5) {
                $map[$p[0]] = [pscustomobject]@{ X = [int]$p[1]; Y = [int]$p[2]; W = [int]$p[3]; H = [int]$p[4] }
            }
        }
    }
    return $map
}

function Center($r) { return @([int]($r.X + $r.W / 2), [int]($r.Y + $r.H / 2)) }

function ClickItem([string] $layout, [string] $label, [switch] $Right) {
    $r = (Layout $layout)[$label]
    if ($null -eq $r) { Write-Warning "no '$label' in layout_$layout"; return }
    $c = Center $r
    Click $c[0] $c[1] -Right:$Right
}

# Drag a slider (its clickable area extends 10 px past the track) to a value 0..1.
function SetSlider([string] $layout, [string] $label, [double] $value) {
    $r = (Layout $layout)[$label]
    if ($null -eq $r) { Write-Warning "no '$label' in layout_$layout"; return }
    $left = $r.X + 10; $width = $r.W - 20; $y = [int]($r.Y + $r.H / 2)
    $c = Center $r
    $target = [int]($left + $width * $value)
    Drag ($c[0]) $y $target $y
}

function HoverItem([string] $layout, [string] $label, [int] $ms = 900) {
    $r = (Layout $layout)[$label]
    if ($null -eq $r) { return }
    $c = Center $r
    Move-To $c[0] $c[1]
    Start-Sleep -Milliseconds $ms
}

# ---- Recording ------------------------------------------------------------

function Start-Recording([string] $name) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo $Ffmpeg
    $psi.Arguments = "-hide_banner -loglevel error -y -f gdigrab -framerate 15 -draw_mouse 1 -i desktop " +
        "-c:v libx264 -preset veryfast -crf 30 -pix_fmt yuv420p `"$(Join-Path $Out "$name.mp4")`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    return [System.Diagnostics.Process]::Start($psi)
}

function Stop-Recording($p) {
    Start-Sleep -Milliseconds 800
    $p.StandardInput.Write('q')
    $p.StandardInput.Close()
    if (-not $p.WaitForExit(20000)) { $p.Kill() }
}

function Run-Scenario([string] $name, [scriptblock] $steps) {
    Write-Host "=== $name"
    Get-Process betterui -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-Item (Join-Path $DataDir 'layout_*.txt'), (Join-Path $DataDir 'config.ini') -ErrorAction SilentlyContinue
    $app = Start-Process $Exe -ArgumentList '--demo' -PassThru
    Start-Sleep -Seconds 3
    Move-To 512 400 10
    $rec = Start-Recording $name
    Start-Sleep -Seconds 1
    try { & $steps } catch { Write-Warning "$name failed: $_" }
    Stop-Recording $rec
    if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
    Start-Sleep -Seconds 1
}

# ---- Scenarios ----------------------------------------------------------

Run-Scenario '1-top-bar' {
    Move-To 60 16; Start-Sleep -Milliseconds 800      # RAM
    Move-To 170 16; Start-Sleep -Milliseconds 800     # network
    Move-To 512 16; Start-Sleep -Milliseconds 800     # clock
    # Right-click menu -> Bar opacity -> 30%
    Click 300 16 -Right
    Keys '{DOWN}'; Keys '{RIGHT}'; Keys '{DOWN 7}'; Keys '{ENTER}'
    Start-Sleep -Seconds 2
    # Bar opacity -> 100%
    Click 300 16 -Right
    Keys '{DOWN}'; Keys '{RIGHT}'; Keys '{ENTER}'
    Start-Sleep -Seconds 2
    # Back to 60%
    Click 300 16 -Right
    Keys '{DOWN}'; Keys '{RIGHT}'; Keys '{DOWN 4}'; Keys '{ENTER}'
    Start-Sleep -Seconds 1
}

Run-Scenario '2-audio-flyout' {
    ClickItem 'bar' 'Audio'
    Start-Sleep -Seconds 1
    SetSlider 'audio' 'Slider(Master(Output))' 0.25
    SetSlider 'audio' 'Slider(Master(Output))' 0.85
    ClickItem 'audio' 'Device(Output, 1)'
    ClickItem 'audio' 'Device(Input, 1)'
    ClickItem 'audio' 'Mute(Master(Input))'
    Start-Sleep -Seconds 1
    ClickItem 'audio' 'Tab(Mixer)'
    Start-Sleep -Seconds 1
    SetSlider 'audio' 'Slider(App(0))' 0.35
    ClickItem 'audio' 'Mute(App(2))'
    HoverItem 'audio' 'Slider(App(1))' 300
    Wheel 5
    Start-Sleep -Seconds 1
    Keys '{ESC}'
    # Scroll on the speaker button to change the volume
    HoverItem 'bar' 'Audio' 500
    Wheel -10
    Start-Sleep -Seconds 1
    ClickItem 'bar' 'Audio'
    Start-Sleep -Seconds 2
    Click 300 500    # click elsewhere closes it
}

Run-Scenario '3-bluetooth' {
    ClickItem 'bar' 'Bluetooth'
    Start-Sleep -Seconds 2
    ClickItem 'panel' 'BluetoothSwitch'
    Start-Sleep -Seconds 2
    ClickItem 'panel' 'BluetoothSwitch'
    Start-Sleep -Seconds 2
    HoverItem 'panel' 'Link("ms-settings:bluetooth")'
    Keys '{ESC}'
}

Run-Scenario '4-control-center' {
    ClickItem 'bar' 'ControlCenter'
    Start-Sleep -Seconds 2
    ClickItem 'panel' 'WifiTile'
    Start-Sleep -Seconds 1
    ClickItem 'panel' 'WifiTile'
    Start-Sleep -Seconds 1
    ClickItem 'panel' 'BluetoothTile'
    Start-Sleep -Seconds 1
    SetSlider 'panel' 'Slider(Brightness)' 0.3
    SetSlider 'panel' 'Slider(Volume)' 0.8
    ClickItem 'panel' 'Mute'
    Start-Sleep -Seconds 1
    ClickItem 'panel' 'Mute'
    Start-Sleep -Seconds 1
    Keys '{ESC}'
}

Run-Scenario '5-dock' {
    # Move the terminal running this job out of the way so the dock is free.
    Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.ProcessName -ne 'betterui' } |
        ForEach-Object { [Input]::ShowWindow($_.MainWindowHandle, 6) | Out-Null }   # minimize
    Start-Sleep -Seconds 2
    $dockY = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Height - 8 - 32
    $bottom = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds.Height - 1

    # Open Notepad: it shows up on the dock as a running app.
    $np = Start-Process notepad -PassThru
    Start-Sleep -Seconds 3
    $np.Refresh()
    $hwnd = $np.MainWindowHandle
    # Maximize it: it covers the dock, so the dock hides.
    if ($hwnd -ne 0) { [Input]::ShowWindow($hwnd, 3) | Out-Null }
    Move-To 512 300
    Start-Sleep -Seconds 2
    # Touch the bottom edge: the dock slides back up.
    Move-To 512 $bottom 600
    Start-Sleep -Seconds 1
    # Read the dock's layout fresh before every click.
    function NotepadX {
        $dock = Layout 'dock'
        $key = $dock.Keys | Where-Object { $_ -match 'notepad' } | Select-Object -First 1
        if (-not $key) { Write-Warning "Notepad not found on dock: $($dock.Keys -join ', ')"; return $null }
        return (Center $dock[$key])[0]
    }
    $x = NotepadX
    if ($x) {
        Click $x $dockY                # Notepad is in front -> minimize it
        Start-Sleep -Seconds 2
        $x = NotepadX
        Click $x $dockY                # restore it
        Start-Sleep -Seconds 1
        Move-To 512 300                # move away: dock hides again
        Start-Sleep -Seconds 2
        Move-To 512 $bottom 600        # reveal
        Start-Sleep -Seconds 1
        $x = NotepadX
        Click $x $dockY -Right         # New window / Pin to dock / Close window
        Keys '{DOWN}'; Keys '{DOWN}'; Keys '{ENTER}'   # Pin to dock
        Start-Sleep -Seconds 2
    }
    Stop-Process -Id $np.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    Move-To 512 $bottom 600
    Start-Sleep -Seconds 2
}

Run-Scenario '6-system-tray' {
    ClickItem 'bar' 'Tray'
    Start-Sleep -Seconds 1
    foreach ($i in 0..4) { HoverItem 'tray' "Icon($i)" 900 }
    Keys '{ESC}'
}

Run-Scenario '7-bar-autohide' {
    # Right-click menu -> Bar visibility -> Auto-hide
    Click 300 16 -Right
    Keys '{DOWN}'; Keys '{DOWN}'; Keys '{RIGHT}'; Keys '{DOWN}'; Keys '{ENTER}'
    Move-To 512 400
    Start-Sleep -Seconds 2          # bar slides away
    Move-To 512 0 600               # touch the top edge: it slides back
    Start-Sleep -Seconds 2
    Move-To 512 400
    Start-Sleep -Seconds 2          # and hides again
    Move-To 300 0 600
    Start-Sleep -Milliseconds 800
    # Back to always shown
    Click 300 16 -Right
    Keys '{DOWN}'; Keys '{DOWN}'; Keys '{RIGHT}'; Keys '{ENTER}'
    Move-To 512 400
    Start-Sleep -Seconds 2          # stays put
}

Get-Process betterui -ErrorAction SilentlyContinue | Stop-Process -Force
Get-ChildItem $Out | Format-Table Name, Length
