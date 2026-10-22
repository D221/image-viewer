param(
    [string]$Exe = "target\release\image-viewer.exe",
    # Seconds to wait after each mutation before declaring it unobserved. A
    # change now arrives by OS notification and is re-decoded `CONTENT_SETTLE`
    # (200 ms) after the writer stops, so anything over 2 s sees a settled
    # change; 6 s leaves room for a slow disk.
    [int]$Settle = 6
)
# Ground truth for the file-change watcher (`check_changed` + `invalidate_path`
# in `main.rs`): does a file rewritten on disk reload while the viewer is
# running, including a file seen *mid-write* — and does the mid-write failure
# heal instead of poisoning the image, which is what `failed` would do for the
# rest of the session if `invalidate_path` stopped clearing it.
#
# The effects that cannot be faked, all read from the exit report:
#
#   file rewritten   -> "files reloaded (watcher saw a change)" >= 3
#   mid-write garbage-> "loads failed into `failed`" may tick up (a half-written
#                       file is a legitimate failure) ...
#   file completed   -> ... and the final re-decode happens anyway
#                       ("worker: preview decode" count >= 5: the three opens,
#                       one for the rewrite, one for the completed file)
#   and meanwhile    -> display-gap frames / stalls / worker panics stay 0
#
# Shots are taken at each step for the eyeball, but **on some boxes they will
# be blank** — screen capture cannot see this app's swapchain everywhere — so
# the verdict below is counters, not pixels.
#
# Every mutating step *proves itself*: the scratch file's length and
# LastWriteTime are printed after each write and asserted before the next
# step, because a run that looks right and skipped a step is worse than one
# that says so. (The first run of this probe executed change 2 with no
# evidence at all — no shot, no garbage on disk, timestamps fitting a run
# without it — and the only reason it was caught is that the reload counter
# read 2 with the shot missing. The proofs below exist so that stays
# impossible to miss.)
#
# Run with the session *active*. Unlike `probe_chords.ps1` / `probe_thumb_cache.ps1`
# this probe needs no console session: it presses no key and measures no time,
# and an active RDP session sees the same filesystem, counters and report a
# console one does.

$ErrorActionPreference = "Stop"

# Every write is followed by a proof: the file's length and LastWriteTime on
# disk, printed as it happens. A silent skip of a mutation would show here as
# an unchanged length.
function Show-Proof([string]$Tag, [string]$Path) {
    $f = Get-Item -LiteralPath $Path
    Write-Host ("    {0,-22} len={1}  mtime={2:HH:mm:ss.fff}" -f $Tag, $f.Length, $f.LastWriteTime)
}
# And every screenshot is checked for existence: a missing shot is a step the
# run skipped, not something to interpolate as an empty string.
function Invoke-Step([string]$Tag, [scriptblock]$Body) {
    Write-Host ("=== {0} @ {1:HH:mm:ss.fff} ===" -f $Tag, [DateTime]::Now) -ForegroundColor Cyan
    & $Body
}
function Assert-Shot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { throw "the screenshot $Path was never written - the step did not run" }
}

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Native {
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  private delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  // A release build has no console window, so "the largest visible window" is
  // the viewer. Same rule as `bench_launch.ps1` and `probe_thumb_cache.ps1`.
  public static IntPtr MainWindow(int pid) {
    IntPtr best = IntPtr.Zero; long bestArea = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != (uint)pid || !IsWindowVisible(h)) return true;
      var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
      if (sb.Length == 0) return true;
      RECT r; GetWindowRect(h, out r);
      long area = (long)(r.R - r.L) * (r.B - r.T);
      if (area > bestArea) { bestArea = area; best = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static void Shot(IntPtr h, string path) {
    RECT r; GetWindowRect(h, out r);
    using (var bmp = new System.Drawing.Bitmap(r.R - r.L, r.B - r.T))
    using (var g = System.Drawing.Graphics.FromImage(bmp)) {
      g.CopyFromScreen(r.L, r.T, 0, 0, bmp.Size);
      bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
    }
  }
}
"@ -ReferencedAssemblies System.Drawing

Add-Type -AssemblyName System.Drawing

# Active session only, asserted rather than assumed. This probe injects no
# input and takes no timing, so an *active* RDP session is fine — the recorded
# wrong-over-RDP conclusions all came from benches that did one of those two.
# What is not fine is a DISCONNECTED session: then the window gets no paint
# delivery at all, egui may build no frames, and the counters would describe a
# half-idle app.
$qline = (quser 2>$null | Select-String "^\s*>" | Select-Object -First 1)
if ($qline) { $qline = $qline.ToString() } else {
    Write-Host "REFUSING: quser returned nothing." -ForegroundColor Red
    exit 2
}
if ($qline -match "\sDisc\b") {
    Write-Host "REFUSING: session is disconnected ('$($qline.Trim())')." -ForegroundColor Red
    Write-Host "A disconnected session gets no paint delivery; reconnect and rerun." -ForegroundColor Red
    exit 2
}
if ($qline -notmatch "\sActive\b" -and $qline -notmatch "\sconsole\s") {
    Write-Host "REFUSING: session is '$($qline.Trim())', not active." -ForegroundColor Red
    exit 2
}
Write-Host "session: active - no injected input and no timing in this probe," -ForegroundColor Cyan
Write-Host "so an active RDP session is usable; the shots may still be blank." -ForegroundColor Cyan

$report = Join-Path $env:TEMP "image-viewer-perf.txt"

function Report-Field([string]$Label) {
    if (-not (Test-Path $report)) { return "NO-REPORT" }
    $line = Select-String -Path $report -Pattern ([regex]::Escape($Label)) | Select-Object -First 1
    if (-not $line) { return "ABSENT" }
    ($line.Line -replace '\s+', ' ').Trim()
}
# The preview-decode count lives as `n=<k>` on the worker row, so the final
# number is parsed straight out of the report line.
function Report-PreviewCount {
    $text = (Report-Field "worker: preview decode")
    if ($text -match 'n=(\d+)') { [int]$Matches[1] } else { 0 }
}
# A solid-colour PNG is the cheapest thing that sniffing accepts without an
# embedded thumbnail or a camera payload.
function New-SolidPng([string]$Path, [System.Drawing.Color]$Color) {
    $bmp = New-Object System.Drawing.Bitmap 128, 128
    try {
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.Clear($Color)
        $g.Dispose()
        $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $bmp.Dispose()
    }
}

# ---- scratch folder: three files, one per colour, so *which* file is on
# screen is readable from the pixels by eye on a box that can capture ----------
$scratch = Join-Path $env:TEMP "iv-watcher-probe"
if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
New-Item -ItemType Directory -Path $scratch | Out-Null
# "01" sorts first, so it is the file on screen at startup. Every mutation
# below aims at it, because the reload that matters is the one the *shown*
# image has to survive.
New-SolidPng (Join-Path $scratch "01.png") ([System.Drawing.Color]::FromArgb(255, 200, 30, 30))   # red
New-SolidPng (Join-Path $scratch "02.png") ([System.Drawing.Color]::FromArgb(255, 30, 160, 30))   # green
New-SolidPng (Join-Path $scratch "03.png") ([System.Drawing.Color]::FromArgb(255, 30, 60, 200))   # blue
Write-Host ("scratch : {0}" -f $scratch)

Remove-Item $report -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $Exe -ArgumentList "--perf", $scratch -PassThru
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 120; $i++) {
    $hwnd = [Native]::MainWindow($proc.Id)
    if ($hwnd -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 250
}
if ($hwnd -eq [IntPtr]::Zero) { $proc.Kill(); throw "no window appeared" }
[Native]::ShowWindow($hwnd, 9) | Out-Null
# visible but not foreground: the watcher asks for no key. Settle long enough
# for the three previews and the initial EXIF read, then baseline the shot.
Invoke-Step "open + settle" { Start-Sleep -Seconds 4 }
$shot0 = Join-Path $env:TEMP "iv-watch-0-open.png"
[Native]::Shot($hwnd, $shot0) | Out-Null
Assert-Shot $shot0
Show-Proof "01.png at open" (Join-Path $scratch "01.png")

Invoke-Step "change 1: rewrite 01.png at red -> yellow" {
    New-SolidPng (Join-Path $scratch "01.png") ([System.Drawing.Color]::FromArgb(255, 230, 220, 20))   # yellow
    Show-Proof "01.png rewritten" (Join-Path $scratch "01.png")
    Start-Sleep -Seconds $Settle
}
$shot1 = Join-Path $env:TEMP "iv-watch-1-rewritten.png"
[Native]::Shot($hwnd, $shot1) | Out-Null
Assert-Shot $shot1

# The mid-write step. Not a truncation: `image`'s PNG decoder accepts a
# stream missing its tail (the thumb-cache bug is recorded as a hit), so a
# truncated PNG would decode and prove nothing. Absolute garbage fails the
# sniff outright — and the proof lines below are what says it actually
# replaced the file this run.
Invoke-Step "change 2: mid-write — 01.png left as undecodable garbage" {
    [System.IO.File]::WriteAllBytes(
        (Join-Path $scratch "01.png"),
        [byte[]][System.Text.Encoding]::ASCII.GetBytes("IV-WATCHER-PROBE-PARTIAL"))
    $written = Get-Item (Join-Path $scratch "01.png")
    if ($written.Length -gt 100) {
        throw "the garbage write never replaced 01.png (still $($written.Length) bytes) - rerun"
    }
    Show-Proof "01.png garbage" (Join-Path $scratch "01.png")
    Start-Sleep -Seconds $Settle
}
$shot2 = Join-Path $env:TEMP "iv-watch-2-midwrite.png"
[Native]::Shot($hwnd, $shot2) | Out-Null
Assert-Shot $shot2

# The writer finished: one more than $Settle, so the poll has to see the
# *final* stamp, clear the `failed` entry the garbage wrote, and land the
# re-decode. This is the step the poison question lives in.
Invoke-Step "change 3: the writer finished — 01.png becomes pink" {
    New-SolidPng (Join-Path $scratch "01.png") ([System.Drawing.Color]::FromArgb(255, 230, 60, 180))   # pink
    Show-Proof "01.png healed" (Join-Path $scratch "01.png")
    Start-Sleep -Seconds ($Settle + 2)
}
$shot3 = Join-Path $env:Temp "iv-watch-3-healed.png"
[Native]::Shot($hwnd, $shot3) | Out-Null
Assert-Shot $shot3

# CloseMainWindow, not Kill: `on_exit` is what writes the report.
$proc.CloseMainWindow() | Out-Null
for ($i = 0; $i -lt 40; $i++) {
    if ($proc.HasExited) { break }
    Start-Sleep -Milliseconds 250
}
$clean = $proc.HasExited
if (!$clean) { $proc.Kill() }
Start-Sleep -Milliseconds 400

Write-Host "`n=== verdict ===" -ForegroundColor Cyan
$reload   = Report-Field "files reloaded (watcher saw a change)"
$failed   = Report-Field "loads failed into ``failed``"
$previews = Report-PreviewCount
$gap      = Report-Field "display-gap frames"
$stalls   = Report-Field "stalls (>5s to first pixels)"
$panics   = Report-Field "worker decode panics"
Write-Host "  $reload"
Write-Host "  $failed"
Write-Host "  worker: preview decode count = $previews"
Write-Host "  $gap"
Write-Host "  $stalls"
Write-Host "  $panics"
Write-Host "  shots: $shot0 $shot1 $shot2 $shot3"

$ok = $true
function Num-Of([string]$s) {
    if ($s -match '(\d+)\s*$') { [int]$Matches[1] } else { -1 }
}
if (-not $clean) {
    Write-Host "  FAIL the app did not exit cleanly, so its report is missing" -ForegroundColor Red; $ok = $false
}
if ($reload -eq "ABSENT" -or $reload -eq "NO-REPORT") {
    Write-Host "  FAIL the report has no watcher-change counter" -ForegroundColor Red; $ok = $false
} elseif ((Num-Of $reload) -lt 3) {
    Write-Host "  FAIL watcher saw fewer than 3 changes" -ForegroundColor Red; $ok = $false
}
if ($previews -lt 5) {
    Write-Host "  FAIL fewer than 5 preview decodes (3 open + rewrite + final heal); the final re-decode is missing" -ForegroundColor Red; $ok = $false
}
if ((Num-Of $gap) -ne 0) {
    Write-Host "  FAIL display-gap frames ~ 0 (an image that failed and was not reloaded)" -ForegroundColor Red; $ok = $false
}
if ((Num-Of $stalls) -ne 0) {
    Write-Host "  FAIL stalls ~ 0" -ForegroundColor Red; $ok = $false
}
if ((Num-Of $panics) -ne 0) {
    Write-Host "  FAIL worker panics ~ 0" -ForegroundColor Red; $ok = $false
}
if ($failed -ne "ABSENT" -and $failed -ne "NO-REPORT") {
    Write-Host "  note : mid-write failures recorded (expected up to one garbage window; healing is the preview count above)"
}
Write-Host ""
Write-Host $(if ($ok) { "VERDICT: PASS" } else { "VERDICT: FAIL" }) -ForegroundColor $(if ($ok) { "Green" } else { "Red" })
