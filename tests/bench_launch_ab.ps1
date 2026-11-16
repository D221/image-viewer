# A/B two builds of this app, interleaved, using its own --perf report.
#
# Why interleaved: the first attempt at this measured all of build A, then all of
# build B, and the machine drifted enough between the two blocks to invent a 120 ms
# "regression" out of nothing - JPEGView's own CPU time moved from 828 ms to
# 1250 ms over the same period. Alternating A,B,A,B means any drift lands on both.
#
# Why the internal report rather than the external harness: `startup -> first
# frame` is measured inside the process with a monotonic clock, and polling a
# window handle from PowerShell cannot resolve 100 ms on a loaded machine.
#
# Run:  powershell -File tests/bench_launch_ab.ps1 -Runs 7

param(
    [string]$Folder = $env:IV_BENCH_FOLDER,
    [int]$Runs = 7,
    # NB: not `$A`/`$B`. PowerShell variable names are case-insensitive, so a loop
    # variable `$a` silently overwrote the parameter and the second iteration tried
    # to `Start-Process -FilePath 809.4`. That surfaced as "The system cannot find
    # the file specified", which reads exactly like a missing binary and sent me
    # looking for a Windows pending-delete race that was never happening.
    [string]$PathA = "$env:TEMP\iv_ab\iv_old.exe",
    [string]$PathB = "$env:TEMP\iv_ab\iv_new.exe",
    [string]$LabelA = "old (3 folder walks)",
    [string]$LabelB = "new (1 folder walk)"
)

if (-not $Folder -or -not (Test-Path -LiteralPath $Folder)) {
    throw "Pass -Folder <dir>, or set `$env:IV_BENCH_FOLDER to a folder of images."
}

$ErrorActionPreference = "Stop"
$report = "$env:TEMP\image-viewer-perf.txt"
$scratch = "$env:TEMP\iv_ab_run"

function Get-PerfNumber([string]$Master) {
    # Each run executes a *fresh copy* of the master binary, in a scratch directory.
    #
    # Two earlier versions of this harness were wrong in ways that had nothing to do
    # with the app:
    #   * killing by the name "image-viewer" matches neither renamed copy, so a
    #     process leaked per run, they competed for the CPU, and each one overwrote
    #     the same perf report;
    #   * force-killing a process leaves its image mapped and pending deletion, and
    #     the next `CreateProcess` on that path fails with ERROR_FILE_NOT_FOUND for
    #     seconds afterwards - which looks exactly like "the file is missing" and
    #     killed the run. Copying fresh each time makes that impossible, and a
    #     graceful close is still preferred so the report gets written.
    $base = [System.IO.Path]::GetFileNameWithoutExtension($Master)
    Get-Process -Name $base -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 250

    if (-not (Test-Path $scratch)) { New-Item -ItemType Directory -Force -Path $scratch | Out-Null }
    $exe = Join-Path $scratch "$base.exe"
    Copy-Item $Master $exe -Force
    Remove-Item $report -ErrorAction SilentlyContinue

    $p = Start-Process -FilePath $exe -ArgumentList @("--perf", "`"$Folder`"") -PassThru
    Start-Sleep -Seconds 4
    $null = $p.CloseMainWindow()
    # Wait for a clean exit: that is what writes the report.
    if (-not $p.WaitForExit(6000)) { $p | Stop-Process -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 200

    if (-not (Test-Path $report)) { return $null }
    $line = (Select-String -Path $report -Pattern "startup -> first frame").Line
    if ($line -match '([\d.]+)ms\s+[\d.]+ms\s+[\d.]+ms\s+[\d.]+ms') {
        return [double]$Matches[1]
    }
    return $null
}

function Stats($xs) {
    $s = @($xs | Sort-Object)
    $m = [int][Math]::Floor($s.Count / 2)
    $med = if ($s.Count % 2 -eq 1) { $s[$m] } else { ($s[$m - 1] + $s[$m]) / 2 }
    return [pscustomobject]@{
        Min = $s[0]
        Med = [Math]::Round($med, 1)
        Max = $s[-1]
    }
}

foreach ($e in @($PathA, $PathB)) { if (-not (Test-Path $e)) { throw "not found: $e" } }
# Discard the first pair: page cache and shader cache effects.
$null = Get-PerfNumber $PathA
$null = Get-PerfNumber $PathB

$ra = @(); $rb = @()
Write-Host ""
Write-Host "  A/B: startup -> first frame, interleaved, $Runs pairs"
Write-Host "  folder: $Folder"
Write-Host ""
for ($i = 0; $i -lt $Runs; $i++) {
    $msA = Get-PerfNumber $PathA
    $msB = Get-PerfNumber $PathB
    if ($null -ne $msA) { $ra += $msA }
    if ($null -ne $msB) { $rb += $msB }
    Write-Host ("  pair {0}:  A {1,7} ms   B {2,7} ms   delta {3,7} ms" -f ($i + 1), $msA, $msB, ($msB - $msA))
}

$sa = Stats $ra; $sb = Stats $rb
Write-Host ""
Write-Host ("  {0,-24} min {1,7} ms   median {2,7} ms   max {3,7} ms" -f $LabelA, $sa.Min, $sa.Med, $sa.Max)
Write-Host ("  {0,-24} min {1,7} ms   median {2,7} ms   max {3,7} ms" -f $LabelB, $sb.Min, $sb.Med, $sb.Max)
$d = $sb.Med - $sa.Med
Write-Host ""
Write-Host ("  median delta: {0:+0.0;-0.0;0.0} ms  ({1:P1})" -f $d, ($d / $sa.Med))
Write-Host ""
