# Launch benchmark: this viewer vs the shells a user might double-click instead.
#
# Windows Photo Viewer is listed but cannot be measured. On Windows 11 25H2
# (build 26200) Microsoft removed it: `photoviewer.dll` is still on disk and still
# contains the string `ImageView_Fullscreen`, but it is unregistered - there is no
# `photoviewer\shell\open\command` - and `rundll32` exits 0 in ~100 ms without
# creating a window, with both the comma and the space calling convention. Kept in
# the list so nobody re-derives it.
#
# IrfanView is single-instance: a second launch hands the file to the running one
# and exits. Every run kills any existing instance first, so each sample is a real
# cold launch rather than a hand-off.
#
# Both comparison apps are given a *single file*, because that is how a shell
# invokes them, so this viewer is measured on the same file. The backend is varied
# because it turned out to be the largest single term in start-up.
#
# **Read the caveats before trusting a number from this.**
#
#  * `$GapMs` defaults to 3000 and must not be reduced. Relaunching the *same*
#    executable in quick succession is slow on this machine: eight launches 1.5 s
#    apart came in at 47-93 ms to window, the same binary launched every 350 ms came
#    in at ~1400 ms. A 30x difference in the measured quantity, caused by the
#    harness. Wall clock rose while CPU stayed put, which is the signature of
#    waiting on something outside the process. Alternating between two *different*
#    exes does not show it - which is why the first version of this script, with one
#    entry per app, looked fine, and the moment three variants of our own exe went
#    into the rotation every number tripled and GL appeared to lose entirely.
#  * The to-window figure on this box is **bimodal** - roughly 50 ms or roughly
#    800 ms for the same binary and the same config - so the median is not a number
#    to act on and `min` is printed alongside it. Peak working set, by contrast, is
#    stable to a few MB and is the most trustworthy figure here.
#  * This is an RDP session sharing a GPU. Wall-clock launch numbers are the least
#    reliable thing this project measures; the internal `--perf` report
#    (bench_launch_ab.ps1) is the instrument for "did my change help".
#
# Run:  powershell -File tests/bench_launch_apps.ps1 -Rounds 7

param(
    [string]$Image = "",
    [string]$Folder = $env:IV_BENCH_FOLDER,
    [int]$Rounds = 7,
    [int]$GapMs = 3000,
    [switch]$VerboseConfig,
    [string]$Ours = "$PSScriptRoot\..\target\release\image-viewer.exe",
    [string]$Oculante = "$PSScriptRoot\..\test-images\oculante\oculante.exe",
    [string]$Only = ""
)

if (-not $Folder -or -not (Test-Path -LiteralPath $Folder)) {
    throw "Pass -Folder <dir>, or set `$env:IV_BENCH_FOLDER to a folder of images."
}

$ErrorActionPreference = "Stop"

if (-not $Image) {
    # A mid-sized photo, which is what someone actually double-clicks. `C:\` is not
    # writable from a normal session, so the `C:\img.jpg` in the original request
    # cannot be created; a real photo from the library stands in for it.
    $Image = (Get-ChildItem $Folder -File -Filter *.jpg |
        Where-Object { $_.Length -gt 2MB -and $_.Length -lt 5MB } |
        Select-Object -First 1).FullName
}
foreach ($p in @($Image, $Folder, $Ours)) {
    if (-not (Test-Path $p)) { throw "not found: $p" }
}
$CfgPath = "$env:APPDATA\image-viewer\config.json"

$APPS = @(
    @{ Name = "image-viewer (file, dx12)"; Exe = $Ours; Arg = "`"$Image`""; Kill = @("image-viewer"); Backend = "dx12" },
    @{ Name = "image-viewer (file, gl)"; Exe = $Ours; Arg = "`"$Image`""; Kill = @("image-viewer"); Backend = "gl" },
    @{ Name = "IrfanView (file)"; Exe = "C:\Program Files\IrfanView\i_view64.exe"; Arg = "`"$Image`""; Kill = @("i_view64") },
    # Oculante is the interesting comparison: egui + wgpu, like this one, so it is
    # the only other app here that pays the same renderer bill. IrfanView and
    # JPEGView_L are Win32/GDI and do not, which is most of why they are light.
    @{ Name = "Oculante (file)"; Exe = $Oculante; Arg = "`"$Image`""; Kill = @("oculante"); Log = "oculante" },
    @{ Name = "Windows Photo Viewer"; Exe = "$env:WINDIR\System32\rundll32.exe"; Arg = "`"C:\Program Files\Windows Photo Viewer\photoviewer.dll`",ImageView_Fullscreen `"$Image`""; Kill = @(); Skip = "removed in Windows 11 24H2/25H2" }
)

# The backend is `renderer.choice` in the config file, so an interleaved run has to
# set it per launch or every variant shares one setting.
#
# The field is `renderer.choice`, **not** `wgpu_backend`. Assigning a property that
# does not exist to a `ConvertFrom-Json` object throws, and that throw is the only
# reason this was caught: an earlier three-block benchmark used the wrong name,
# swallowed the error, and silently measured dx12 three times.
function Set-Backend([string]$b) {
    if (-not $b) { return }
    $j = Get-Content $CfgPath -Raw | ConvertFrom-Json
    $j.renderer.choice = $b
    $j | ConvertTo-Json -Depth 10 | Set-Content $CfgPath -Encoding UTF8
    $check = (Get-Content $CfgPath -Raw | ConvertFrom-Json).renderer.choice
    if ($check -ne $b) { throw "config write failed: wanted '$b', got '$check'" }
}

function Stop-App([string[]]$Names) {
    foreach ($n in $Names) {
        Get-Process -Name $n -ErrorAction SilentlyContinue |
            Stop-Process -Force -ErrorAction SilentlyContinue
    }
}

function Measure-One($App) {
    Stop-App $App.Kill
    Set-Backend $App.Backend
    if ($VerboseConfig) {
        $r = (Get-Content $CfgPath -Raw | ConvertFrom-Json).renderer
        Write-Host ("    [config] {0,-24} choice={1,-6} probed={2}" -f $App.Name, $r.choice, $r.probed)
    }
    Start-Sleep -Milliseconds $GapMs
    # Oculante logs to stdout and, launched via `Start-Process`, inherits this
    # script's handles. The pipeline then never sees end-of-file and the whole run
    # hangs - which is exactly what happened on the first attempt. Redirecting to
    # per-app log files breaks the inheritance.
    $logOut = $null; $logErr = $null
    if ($App.Log) {
        $logOut = "$env:TEMP\iv_$($App.Log)_out.log"
        $logErr = "$env:TEMP\iv_$($App.Log)_err.log"
    }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = $null
    for ($try = 1; $try -le 10; $try++) {
        try {
            if ($logOut) {
                $proc = Start-Process -FilePath $App.Exe -ArgumentList $App.Arg -PassThru `
                    -RedirectStandardOutput $logOut -RedirectStandardError $logErr -ErrorAction Stop
            } else {
                $proc = Start-Process -FilePath $App.Exe -ArgumentList $App.Arg -PassThru -ErrorAction Stop
            }
            break
        } catch { if ($try -eq 10) { throw }; Start-Sleep -Milliseconds 400 }
    }

    $toWindow = $null
    while ($sw.ElapsedMilliseconds -lt 20000) {
        $q = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if (-not $q) { break }   # exited: single-instance hand-off, or a dead app
        try { $q.Refresh() } catch { }
        if ($q.MainWindowHandle -ne 0) { $toWindow = $sw.ElapsedMilliseconds; break }
        Start-Sleep -Milliseconds 5
    }
    if ($null -eq $toWindow) {
        Stop-App $App.Kill
        return [pscustomobject]@{ ToWindow = -1; ToIdle = -1; PeakMB = 0; CpuMs = 0 }
    }

    # Idle = CPU use has flattened, i.e. the scan and the first decode are done.
    $toIdle = $null; $lastCpu = 0L; $stable = 0; $peak = 0L
    while ($sw.ElapsedMilliseconds -lt 25000) {
        $q = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
        if (-not $q) { break }
        $cpu = [int64]$q.TotalProcessorTime.TotalMilliseconds
        if ($q.WorkingSet64 -gt $peak) { $peak = $q.WorkingSet64 }
        if ([Math]::Abs($cpu - $lastCpu) -lt 12) {
            $stable++
            if ($stable -ge 4) { $toIdle = $sw.ElapsedMilliseconds; break }
        } else { $stable = 0 }
        $lastCpu = $cpu
        Start-Sleep -Milliseconds 100
    }
    $q = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    $cpuTotal = if ($q) { [int64]$q.TotalProcessorTime.TotalMilliseconds } else { $lastCpu }
    Stop-App $App.Kill
    if ($null -eq $toIdle) { $toIdle = $sw.ElapsedMilliseconds }
    [pscustomobject]@{
        ToWindow = $toWindow; ToIdle = $toIdle
        PeakMB = [Math]::Round($peak / 1MB, 0); CpuMs = $cpuTotal
    }
}

function MinOf($xs) { return (@($xs | Sort-Object))[0] }
function Med($xs) {
    $s = @($xs | Sort-Object)
    if (-not $s.Count) { return 0 }
    $m = [int][Math]::Floor($s.Count / 2)
    if ($s.Count % 2 -eq 1) { return $s[$m] }
    return [Math]::Round(($s[$m - 1] + $s[$m]) / 2, 0)
}

$live = @($APPS | Where-Object { -not $_.Skip })
if ($Only) { $live = @($live | Where-Object { $_.Name -like "*$Only*" }) }
# Drop anything not built rather than failing: Oculante is a clone under
# test-images and may not have been compiled yet.
$missing = @($live | Where-Object { -not (Test-Path $_.Exe) })
foreach ($m in $missing) { Write-Host ("  (skipping {0}: not built at {1})" -f $m.Name, $m.Exe) }
$live = @($live | Where-Object { Test-Path $_.Exe })
if (-not $live.Count) { throw "nothing to measure" }

Write-Host ""
Write-Host "  launch benchmark, interleaved, $Rounds rounds, ${GapMs}ms between launches"
Write-Host "  file:   $Image"
Write-Host "  note: to-window is bimodal on this box; trust 'min' and the peak column"
Write-Host ""

# Discard round 1: each app pays for creating its config on first run.
foreach ($a in $live) { $null = Measure-One $a }

$data = @{}
foreach ($a in $live) { $data[$a.Name] = @() }
for ($r = 0; $r -lt $Rounds; $r++) {
    $line = "  round {0}:" -f ($r + 1)
    foreach ($a in $live) {
        $m = Measure-One $a
        $data[$a.Name] += $m
        $line += ("  {0} {1,5}/{2,-5}" -f (($a.Name -split ' ')[0]), $m.ToWindow, $m.ToIdle)
    }
    Write-Host $line
}

Write-Host ""
Write-Host ("  {0,-24} {1,8} {2,8} {3,8} {4,8} {5,8} {6,8}" -f "app", "win med", "idle med", "peak MB", "cpu ms", "win min", "idle min")
foreach ($a in $live) {
    $rows = $data[$a.Name]
    Write-Host ("  {0,-24} {1,8} {2,8} {3,8} {4,8} {5,8} {6,8}" -f $a.Name,
        (Med ($rows | ForEach-Object { $_.ToWindow })),
        (Med ($rows | ForEach-Object { $_.ToIdle })),
        (Med ($rows | ForEach-Object { $_.PeakMB })),
        (Med ($rows | ForEach-Object { $_.CpuMs })),
        (MinOf ($rows | ForEach-Object { $_.ToWindow })),
        (MinOf ($rows | ForEach-Object { $_.ToIdle })))
}
foreach ($a in @($APPS | Where-Object { $_.Skip })) {
    Write-Host ("  {0,-24} SKIPPED - {1}" -f $a.Name, $a.Skip)
}
Write-Host ""
