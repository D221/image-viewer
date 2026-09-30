# Launch-time benchmark: this viewer vs JPEGView.
#
# Both are GUI apps, so the metrics have to come from outside the process. Two
# numbers that are comparable and that together say most of the story:
#
#   to window   process start -> a visible top-level window exists. This is pure
#               start-up cost: config, folder scan, window and renderer setup. It
#               says nothing about the picture, only that the app is up.
#   to idle     process start -> CPU use flattens out. Everything above, plus
#               the first decode. This is the number a person actually waits for,
#               because an app that has a window but is still churning is not ready.
#
# Also reported: peak working set and total CPU time, because "fast to a window"
# bought with a stall afterwards is not faster.
#
# Run:  powershell -File tests/bench_launch.ps1 -Runs 5

param(
    [string]$Folder = "C:\Users\user\Downloads\phone_backup\Download\Ttg",
    [int]$Runs = 5,
    [string]$Ours = "$PSScriptRoot\..\target\release\image-viewer.exe",
    [string]$Theirs = "C:\Program Files\JPEGView\JPEGView.exe"
)

$ErrorActionPreference = "Stop"

# Window discovery is `Process.MainWindowHandle`, polled, not P/Invoke.
#
# The first version enumerated windows with `EnumWindows` from an `Add-Type`
# callback, and it reported "(no window)" for JPEGView while JPEGView was running
# perfectly well with a titled window. PowerShell scriptblock delegates do not
# reliably mutate variables captured from the enclosing scope, so the callback
# silently collected nothing. `MainWindowHandle` is the framework doing the same
# job, and it also has the property we want: it finds the *main* window, so a
# stray dialog cannot be mistaken for the app being up.
function Get-MainWindow($Proc) {
    try { $Proc.Refresh() } catch { return 0 }
    return $Proc.MainWindowHandle
}

function Stop-Stragglers {
    foreach ($n in @("image-viewer", "JPEGView")) {
        Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Milliseconds 400
}

# One run. Returns ms-to-window, ms-to-idle, peak working set, CPU ms, and the
# title of the first window that appeared (so a stray dialog is visible rather
# than silently skewing the number).
function Measure-Launch([string]$Exe, [string]$Arg, [int]$IdleMs = 12, [int]$StableFor = 4) {
    Stop-Stragglers
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $Exe -ArgumentList $Arg -PassThru

    # --- to window ---
    $toWindow = $null
    $title = ""
    while ($sw.ElapsedMilliseconds -lt 20000) {
        if ((Get-MainWindow $proc) -ne 0) {
            try { $title = $proc.MainWindowTitle } catch { $title = "" }
            $toWindow = $sw.ElapsedMilliseconds
            break
        }
        Start-Sleep -Milliseconds 5
    }
    if ($null -eq $toWindow) {
        $proc | Stop-Process -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{ ToWindow = -1; ToIdle = -1; PeakMB = 0; CpuMs = 0; Title = "(no window)" }
    }

    # --- to idle ---
    # CPU time stops advancing once the folder scan and the first decode are done.
    # Sampling has to survive the process exiting on its own, hence the guards.
    $toIdle = $null
    $lastCpu = [int64]0
    $stable = 0
    $peak = 0L
    while ($sw.ElapsedMilliseconds -lt 30000) {
        $p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if (-not $p) { break }
        $cpu = [int64]$p.TotalProcessorTime.TotalMilliseconds
        $ws = [int64]$p.WorkingSet64
        if ($ws -gt $peak) { $peak = $ws }
        if ([Math]::Abs($cpu - $lastCpu) -lt $IdleMs) {
            $stable++
            if ($stable -ge $StableFor) { $toIdle = $sw.ElapsedMilliseconds; break }
        } else {
            $stable = 0
        }
        $lastCpu = $cpu
        Start-Sleep -Milliseconds 100
    }
    $final = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    $cpuTotal = if ($final) { [int64]$final.TotalProcessorTime.TotalMilliseconds } else { $lastCpu }
    $proc | Stop-Process -Force -ErrorAction SilentlyContinue
    if ($null -eq $toIdle) { $toIdle = $sw.ElapsedMilliseconds }

    return [pscustomobject]@{
        ToWindow = $toWindow
        ToIdle   = $toIdle
        PeakMB   = [Math]::Round($peak / 1MB, 1)
        CpuMs    = $cpuTotal
        Title    = $title
    }
}

function Median($xs) {
    $s = @($xs | Sort-Object)
    if ($s.Count -eq 0) { return 0 }
    $m = [int][Math]::Floor($s.Count / 2)
    if ($s.Count % 2 -eq 1) { return $s[$m] }
    return [Math]::Round(($s[$m - 1] + $s[$m]) / 2, 1)
}

if (-not (Test-Path $Ours)) { throw "not found: $Ours  (cargo build --release first)" }
if (-not (Test-Path $Theirs)) { throw "not found: $Theirs" }
if (-not (Test-Path $Folder)) { throw "not found: $Folder" }

$imgCount = (Get-ChildItem $Folder -File | Where-Object { $_.Extension -match '^\.(jpg|jpeg|png|webp|gif|heic|heif|avif|jxl|tif|tiff|bmp|ico|qoi)$' }).Count
Write-Host ""
Write-Host "  launch benchmark"
Write-Host "  folder: $Folder  ($imgCount images)"
Write-Host "  runs:   $Runs  (first run of each is discarded - it writes a config or an INI)"
Write-Host ""

$results = @{}
foreach ($app in @(
    @{ Name = "image-viewer"; Exe = $Ours; Arg = "`"$Folder`"" },
    @{ Name = "JPEGView"; Exe = $Theirs; Arg = "`"$Folder`"" }
)) {
    # Discard run 1: it pays for creating the config/INI.
    $null = Measure-Launch $app.Exe $app.Arg
    $rows = @()
    for ($i = 0; $i -lt $Runs; $i++) { $rows += Measure-Launch $app.Exe $app.Arg }
    $results[$app.Name] = $rows
    Write-Host ("  {0,-14} window {1,6} ms   idle {2,6} ms   peak {3,6} MB   cpu {4,6} ms   [{5}]" -f `
        $app.Name,
        (Median ($rows | ForEach-Object { $_.ToWindow })),
        (Median ($rows | ForEach-Object { $_.ToIdle })),
        (Median ($rows | ForEach-Object { $_.PeakMB })),
        (Median ($rows | ForEach-Object { $_.CpuMs })),
        $rows[0].Title)
}
Write-Host ""
$o = $results["image-viewer"]; $j = $results["JPEGView"]
$ow = Median ($o | ForEach-Object { $_.ToWindow }); $jw = Median ($j | ForEach-Object { $_.ToWindow })
$oi = Median ($o | ForEach-Object { $_.ToIdle });   $ji = Median ($j | ForEach-Object { $_.ToIdle })
if ($jw -gt 0) { Write-Host ("  window: {0:N2}x JPEGView     idle: {1:N2}x JPEGView" -f ($ow / $jw), ($oi / $ji)) }
Write-Host ""
