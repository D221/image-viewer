# Frame rate: GL vs DX12, on a *console* session.
#
# This is the one measurement in the renderer work that could not be taken over
# RDP, and it is the reason `PROBE_ORDER` leads with GL on incomplete evidence:
# memory (343 MB -> 189 MB) and device-creation time (455 ms -> 186 ms) were
# measured, frame rate was not. On a frame rate, DX12 may simply be faster.
#
# Every trap this project has already fallen into is designed out below, and each
# one is a trap that produced a *plausible wrong number* rather than a crash:
#
#   * RDP            - `quser` must say `console`, not `rdp-tcp#N`, and the video
#                      adapter must not be a Microsoft Remote Display Adapter.
#                      The whole "wgpu runs at 12 fps" investigation was this.
#   * Hidden window  - a hidden window gets no ordinary paint delivery, so every
#                      fps measured that way is garbage. Never `-WindowStyle Hidden`.
#   * frames/lifetime- folds in start-up and idle. This reads the *interval* ring
#                      instead, sampled entirely after a long settle.
#   * Ring age       - the 256-entry ring's contents depend on sample age, so the
#                      settle is 10 s and the run is 30 s: at 30-60 fps the ring
#                      holds 4-8 s of pure animation by the end.
#   * Median only    - a bimodal distribution needs every sample. Both p50 and p95
#                      are printed, and so is the raw interval list for round 1.
#
# Interleaved A/B within each round, because measuring all of GL and then all of
# DX12 lets machine drift invent a regression - that produced a phantom 120 ms
# regression in bench_launch_ab.ps1.
#
# Run:  powershell -File tests/bench_framerate.ps1 -Rounds 3 -SettleSec 10 -RunSec 30

param(
    # A *small* animation by default, on purpose. `load_animation` decodes every
    # frame before the first one is shown, so a 7.9 MB GIF spends its first seconds
    # decoding and the frame interval it produces afterwards says more about the
    # decoder than about the renderer. The 14 KB fixture this defaults to decodes in
    # milliseconds, so what is measured is playback and nothing else.
    [string]$Gif = "$env:TEMP\iv_small.gif",
    [int]$Rounds = 3,
    [int]$SettleSec = 10,
    [int]$RunSec = 30,
    [string]$Ours = "$PSScriptRoot\..\target\release\image-viewer.exe"
)

$ErrorActionPreference = "Stop"
$report = "$env:TEMP\image-viewer-perf.txt"
$cfg = "$env:APPDATA\image-viewer\config.json"

# --- the session check, before anything else -------------------------------
$q = (quser 2>&1 | Out-String)
if ($q -match 'rdp-tcp#') {
    throw "This is an RDP session. Frame rates measured here are meaningless - see AGENTS.md. Session:`n$q"
}
$gpu = (Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }) -join "; "
if ($gpu -match 'Remote Display') {
    throw "The video adapter is a Remote Display Adapter, so this is remote. Frame rates are meaningless.`n$gpu"
}
if (-not (Test-Path $Gif)) { throw "no such gif: $Gif" }
if (-not (Test-Path $Ours)) { throw "build the release binary first: $Ours" }
Write-Host ""
Write-Host "  session: console (verified)   adapter: $gpu"
Write-Host "  file: $Gif"

function Set-Backend([string]$b) {
    $j = Get-Content $cfg -Raw | ConvertFrom-Json
    $j.renderer.choice = $b
    $j | ConvertTo-Json -Depth 10 | Set-Content $cfg -Encoding UTF8
    # Read it back. A `renderer.choice` write that silently did not take effect
    # is how an earlier run reported DX12's footprint for a GL build and concluded
    # the backend made no difference.
    $got = (Get-Content $cfg -Raw | ConvertFrom-Json).renderer.choice
    if ($got -ne $b) { throw "config write failed: wanted '$b', got '$got'" }
}

function Measure-Fps([string]$backend) {
    Set-Backend $backend
    Get-Process -Name "image-viewer" -EA SilentlyContinue | Stop-Process -Force
    Remove-Item $report -EA SilentlyContinue
    Start-Sleep -Seconds 2

    # Visible on purpose. A hidden window is not painted and produces no frames.
    # `--perf` is what makes the app write the report; without it there is nothing
    # to read, and the first run of this script failed on exactly that.
    $p = Start-Process -FilePath $Ours -ArgumentList "--perf", "`"$Gif`"" -PassThru
    Start-Sleep -Seconds $SettleSec
    if ($p.HasExited) { throw "the app exited during settle (exit $($p.ExitCode))" }
    Start-Sleep -Seconds $RunSec

    $q = Get-Process -Id $p.Id -EA SilentlyContinue
    if (-not $q) { throw "the app exited during the run" }
    $null = $q.CloseMainWindow()
    if (-not $p.WaitForExit(15000)) { $p | Stop-Process -Force }
    Start-Sleep -Milliseconds 700

    if (-not (Test-Path $report)) { throw "no perf report at $report" }
    $txt = Get-Content $report -Raw

    function Field([string]$name) {
        $m = [regex]::Match($txt, [regex]::Escape($name) + '\s+(\d+)')
        if ($m.Success) { [int]$m.Groups[1].Value } else { $null }
    }
    # "frame interval" prints `n=1528  15,0ms  35,1ms  35,3ms  35,3ms` - two things
    # about that line cost the first two versions of this script:
    #   * the unit is **milliseconds**, and assuming microseconds reports 0.0 fps;
    #   * the decimal separator is the **locale's**, which here is a comma. The
    #     first version's `[\d.]+` matched "16" out of "16,6" and silently threw the
    #     fraction away. The separator is normalised, and the unit is read rather
    #     than assumed.
    $fi = [regex]::Match($txt, 'frame interval\s+n=(\d+)\s+([\d.,]+)(ms|us)\s+([\d.,]+)(ms|us)\s+([\d.,]+)(ms|us)\s+([\d.,]+)(ms|us)')
    $p50 = $p95 = $null
    if ($fi.Success) {
        $u1 = $fi.Groups[3].Value
        $u2 = $fi.Groups[5].Value
        if ($u1 -ne $u2) { throw "p50 and p95 report different units ('$u1', '$u2')" }
        $scale = if ($u1 -eq 'ms') { 1.0 } else { 0.001 }
        $p50 = [double]($fi.Groups[2].Value -replace ',', '.') * $scale
        $p95 = [double]($fi.Groups[4].Value -replace ',', '.') * $scale
    }

    $frames = Field 'frames built'
    $withEvents = Field 'frames with input events'
    [pscustomobject]@{
        Backend    = $backend
        Frames     = $frames
        WithEvents = $withEvents
        P50ms      = $p50
        P95ms      = $p95
        Report     = $txt
    }
}

# fps from a millisecond interval. A plain function rather than a scriptblock:
# `& $block $arg` on a variable that happens to hold a number invokes *that* as a
# command, which is a confusing way to learn that PowerShell has no opinion about
# what you meant.
function Get-Fps($ms) {
    if ($ms -and $ms -gt 0) { [Math]::Round(1000.0 / $ms, 1) } else { $null }
}

$gl = @(); $dx = @()
$first = $true
foreach ($r in 1..$Rounds) {
    foreach ($b in @('gl', 'dx12')) {
        $m = Measure-Fps $b
        $fps = Get-Fps $m.P50ms
        # `frames built` over the run is the primary number: it is a count, not a
        # percentile of a ring, so it does not depend on sample age. The interval
        # percentiles are printed alongside as a cross-check.
        $line = "  round {0}  {1,-5}  frames {2,6}  => {3,6} fps   (interval p50 {4}ms p95 {5}ms, input events {6})" -f `
            $r, $m.Backend, $m.Frames, [Math]::Round($m.Frames / $RunSec, 1), $m.P50ms, $m.P95ms, $m.WithEvents
        Write-Host $line
        if ($b -eq 'gl') { $gl += $m } else { $dx += $m }
    }
    if ($first) {
        Write-Host ""
        Write-Host "  raw interval block from the first GL run (whole report in $report):"
        ($gl[0].Report -split "`n" | Select-String -Pattern 'frame interval' -Context 0,3) |
            ForEach-Object { "    " + $_.Line.Trim(); $_.Context.PostContext | Select-Object -First 3 | ForEach-Object { "    " + $_.Trim() } }
        $first = $false
    }
}

function Summary($name, $runs) {
    $fr = @($runs | ForEach-Object { $_.Frames })
    $ms = @($runs | ForEach-Object { $_.P50ms } | Where-Object { $_ })
    if (-not $fr) { Write-Host "  $name : no data"; return }
    $sf = @($fr | Sort-Object); $sm = @($ms | Sort-Object)
    Write-Host ("  {0,-5} frames/run : {1}   median {2}" -f $name, ($sf -join ' '), $sf[[int][Math]::Floor($sf.Count / 2)])
    if ($sm) { Write-Host ("  {0,-5} p50 ms     : {1}   median {2}" -f $name, ($sm -join ' '), $sm[[int][Math]::Floor($sm.Count / 2)]) }
}
Write-Host ""
Summary 'gl' $gl
Summary 'dx12' $dx

$gm = (@($gl | ForEach-Object { $_.Frames }) | Measure-Object -Average).Average
$dm = (@($dx | ForEach-Object { $_.Frames }) | Measure-Object -Average).Average
if ($gm -and $dm) {
    $d = [Math]::Round(100.0 * ($gm - $dm) / $dm, 2)
    Write-Host ""
    Write-Host ("  GL vs DX12, frames over {0}s: GL {1:N1}/s, DX12 {2:N1}/s  -> GL is {3}% {4}" -f `
        $RunSec, ($gm / $RunSec), ($dm / $RunSec), [Math]::Abs($d), $(if ($gm -ge $dm) { 'faster' } else { 'slower' }))
    if ([Math]::Abs($d) -lt 2) { Write-Host "  Within 2%: no measurable difference. The smaller backend wins on memory and device-creation time, so it is the better default." }
}
Set-Backend 'auto'
Write-Host "  config restored to auto"
