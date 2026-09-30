# Direct A/B: this app vs Oculante, alternating, nothing else running.
#
# No config rewriting, no other apps, no idle heuristics beyond a fixed gap. The
# point is to find out whether the 29x window difference measured by
# bench_launch_apps.ps1 is a real difference between the two programs or an
# artifact of catching one in each mode of a bimodal distribution - which is
# exactly what happened to Oculante (2106 ms standalone, 48 ms in the harness) and
# to this app (47-93 ms standalone, 1448 ms in the harness).
#
# Prints every sample, because a median of a bimodal distribution is worse than
# useless: it is a number that describes neither mode.
#
# Run:  powershell -File tests/bench_ab_oculante.ps1 -Rounds 10 -GapMs 2000

param(
    [string]$Image = "",
    [string]$Folder = "C:\Users\user\Downloads\phone_backup\Download\Ttg",
    [int]$Rounds = 10,
    [int]$GapMs = 2000,
    # Set once, before the run - NOT per launch. Rewriting the config between
    # launches is what made earlier harnesses report the dx12 working set for a
    # gl run, and it is not clear why.
    [string]$Backend = "",
    [string]$Ours = "$PSScriptRoot\..\target\release\image-viewer.exe",
    [string]$Oculante = "$PSScriptRoot\..\test-images\oculante\oculante.exe"
)

$ErrorActionPreference = "Stop"
if (-not $Image) {
    $Image = (Get-ChildItem $Folder -File -Filter *.jpg |
        Where-Object { $_.Length -gt 2MB -and $_.Length -lt 5MB } |
        Select-Object -First 1).FullName
}
foreach ($p in @($Image, $Ours, $Oculante)) { if (-not (Test-Path $p)) { throw "not found: $p" } }

if ($Backend) {
    $cfg = "$env:APPDATA\image-viewer\config.json"
    $j = Get-Content $cfg -Raw | ConvertFrom-Json
    $j.renderer.choice = $Backend
    $j | ConvertTo-Json -Depth 10 | Set-Content $cfg -Encoding UTF8
    $got = (Get-Content $cfg -Raw | ConvertFrom-Json).renderer.choice
    if ($got -ne $Backend) { throw "config write failed: wanted '$Backend', got '$got'" }
    Write-Host "  (renderer.choice = $Backend, set once for the whole run)"
}

# Ours defaults to auto -> dx12, which is the shipped default and therefore the
# thing being compared. Left alone deliberately: rewriting it per launch is what
# made earlier harnesses report the dx12 memory figure for a gl run.
$APPS = @(
    @{ Name = "ours"; Exe = $Ours; Kill = @("image-viewer"); Log = $null },
    @{ Name = "oculante"; Exe = $Oculante; Kill = @("oculante"); Log = "ocu" }
)

function One($App) {
    foreach ($n in $App.Kill) {
        Get-Process -Name $n -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    }
    Start-Sleep -Milliseconds $GapMs
    $out = $null; $err = $null
    if ($App.Log) { $out = "$env:TEMP\iv_$($App.Log)_out.log"; $err = "$env:TEMP\iv_$($App.Log)_err.log" }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = if ($App.Log) {
        Start-Process -FilePath $App.Exe -ArgumentList "`"$Image`"" -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
    } else {
        Start-Process -FilePath $App.Exe -ArgumentList "`"$Image`"" -PassThru
    }
    $win = -1; $peak = 0L
    for ($i = 0; $i -lt 400; $i++) {
        Start-Sleep -Milliseconds 10
        $q = Get-Process -Id $p.Id -EA SilentlyContinue
        if (-not $q) { break }
        if ($q.WorkingSet64 -gt $peak) { $peak = $q.WorkingSet64 }
        try { $q.Refresh() } catch { }
        if ($q.MainWindowHandle -ne 0) { $win = $sw.ElapsedMilliseconds; break }
    }
    # Sample memory at a fixed point after the window, so the comparison is not
    # "whoever had longer to allocate".
    if ($win -ge 0) {
        Start-Sleep -Milliseconds 1500
        $q = Get-Process -Id $p.Id -EA SilentlyContinue
        if ($q -and $q.WorkingSet64 -gt $peak) { $peak = $q.WorkingSet64 }
    }
    foreach ($n in $App.Kill) {
        Get-Process -Name $n -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
    }
    [pscustomobject]@{ Win = $win; PeakMB = [Math]::Round($peak / 1MB, 0) }
}

function Report($name, $xs) {
    $s = @($xs | Sort-Object)
    $med = $s[[int][Math]::Floor($s.Count / 2)]
    # A gap between the lower and upper half is the signature of bimodality, and
    # reporting only the median would hide it.
    $lo = $s[0]; $hi = $s[-1]
    $mid = $s[[int][Math]::Floor($s.Count / 2)]
    $sorted = ($s -join " ")
    Write-Host ("  {0,-10} min {1,5}  med {2,5}  max {3,5}   samples: {4}" -f $name, $lo, $med, $hi, $sorted)
}

Write-Host ""
Write-Host "  ours vs Oculante, alternating, $Rounds rounds, ${GapMs}ms gap"
Write-Host "  file: $Image"
Write-Host "  (every sample printed; the distribution is the point)"
Write-Host ""
$res = @{ ours = @(); oculante = @() }
foreach ($a in $APPS) { $null = One $a }   # discard: first run pays for settings
for ($r = 0; $r -lt $Rounds; $r++) {
    foreach ($a in $APPS) {
        $m = One $a
        $res[$a.Name] += $m
    }
    Write-Host ("  round {0}:  ours {1,5} ms / {2,4} MB    oculante {3,5} ms / {4,4} MB" -f ($r + 1),
        $res.ours[-1].Win, $res.ours[-1].PeakMB, $res.oculante[-1].Win, $res.oculante[-1].PeakMB)
}
Write-Host ""
Report "ours" ($res.ours | ForEach-Object { $_.Win })
Report "oculante" ($res.oculante | ForEach-Object { $_.Win })
Write-Host ""
Report "ours MB" ($res.ours | ForEach-Object { $_.PeakMB })
Report "oculante MB" ($res.oculante | ForEach-Object { $_.PeakMB })
Write-Host ""
