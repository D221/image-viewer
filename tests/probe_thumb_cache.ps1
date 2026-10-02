param(
    [string]$Exe = "target\release\image-viewer.exe",
    # Seconds to let the grid fill before deciding it is not going to.
    [int]$Settle = 25
)
# Ground truth for the grid-thumbnail cache: does pressing G actually fill a grid,
# does the cache get written, and does a *second* open read from it?
#
# Three externally observable effects, because "nothing happened" must never be
# ambiguous:
#
#   G pressed        -> cells painted            (screenshot of the window)
#   first open       -> entries on disk          (%LOCALAPPDATA%\image-viewer\thumbs)
#   second open      -> cache HITS in the report (%TEMP%\image-viewer-perf.txt)
#
# The report is written by `on_exit`, so the window has to be *closed* and not
# killed; a killed process reports nothing and looks like a broken feature.
#
# Run on a console session only: injected input does nothing over RDP, and every
# key below would then read as "the feature is broken".

$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Native {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, IntPtr pid);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  // Injected keys go to whatever is in front, so "I called SetForegroundWindow"
  // is not the same claim as "the app has focus". This is the check that turns a
  // silent no-op into a visible failure.
  public static bool ForegroundIs(IntPtr h) { return GetForegroundWindow() == h; }
  // Take the foreground the way a user's click would.
  //
  // Plain SetForegroundWindow is *refused* unless the calling process owns the
  // current foreground window, and a harness launched from a terminal does not:
  // the terminal does. Run from wezterm and every key then goes to the terminal,
  // the grid never opens, and the run "proves" the feature broken. Attaching our
  // input queue to the foreground thread's is the documented way to become
  // eligible, and the NOSIZE|NOMOVE topmost/notopmost pair makes sure the window is
  // actually at the front of the z-order as well as focused.
  public static bool Focus(IntPtr h) {
    uint fgThread = GetWindowThreadProcessId(GetForegroundWindow(), IntPtr.Zero);
    uint meThread = GetCurrentThreadId();
    bool attached = fgThread != 0 && fgThread != meThread && AttachThreadInput(meThread, fgThread, true);
    try {
      BringWindowToTop(h);
      const uint NOSIZE = 0x0001, NOMOVE = 0x0002, NOACTIVATE = 0x0010;
      SetWindowPos(h, new IntPtr(-1), 0, 0, 0, 0, NOSIZE | NOMOVE);   // HWND_TOPMOST
      SetWindowPos(h, new IntPtr(-2), 0, 0, 0, 0, NOSIZE | NOMOVE | NOACTIVATE); // HWND_NOTOPMOST
      SetForegroundWindow(h);
      SetFocus(h);
      return GetForegroundWindow() == h;
    } finally {
      if (attached) { AttachThreadInput(meThread, fgThread, false); }
    }
  }
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  private delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  // A release build has no console window, so "the largest visible window" is the
  // viewer. `bench_launch.ps1` uses the same rule.
  public static IntPtr MainWindow(int pid) {
    IntPtr best = IntPtr.Zero; long bestArea = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != (uint)pid || !IsWindowVisible(h)) return true;
      var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
      if (sb.Length == 0) return true;
      long w, ht; GetWindowRectArea(h, out w, out ht);
      long area = w * ht;
      if (area > bestArea) { bestArea = area; best = h; }
      return true;
    }, IntPtr.Zero);
    return best;
  }
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
  public static void GetWindowRectArea(IntPtr h, out long w, out long ht) {
    RECT r; GetWindowRect(h, out r); w = r.R - r.L; ht = r.B - r.T;
  }
  // Screenshot the window itself, so the grid cells are readable and not just
  // "a window appeared".
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

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Keys {
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  const uint KEYUP = 0x0002;
  public static void Tap(byte key) {
    keybd_event(key, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    keybd_event(key, 0, KEYUP, UIntPtr.Zero);
  }
}
"@

# Console session only. Asserted rather than assumed: over RDP every key below
# silently goes nowhere and the whole run would "prove" the feature broken.
$sess = (quser 2>$null | Select-String "^\s*>" | Select-Object -First 1).ToString()
if ($sess -notmatch "\sconsole\s") {
    Write-Host "REFUSING: session is '$($sess.Trim())', not 'console'." -ForegroundColor Red
    Write-Host "Injected input does nothing over RDP; run this at the machine." -ForegroundColor Red
    exit 2
}
if ((Get-CimInstance Win32_VideoController).Name -match "Remote Display") {
    Write-Host "REFUSING: a Remote Display Adapter is attached." -ForegroundColor Red
    exit 2
}
Write-Host "session: console, no Remote Display Adapter - timings from this run are usable"

$cacheDir = Join-Path $env:LOCALAPPDATA "image-viewer\thumbs"
$report   = Join-Path $env:TEMP "image-viewer-perf.txt"

function Cache-Entries {
    if (-not (Test-Path $cacheDir)) { return 0 }
    @(Get-ChildItem $cacheDir -Recurse -File -Filter *.png).Count
}
function Cache-Bytes {
    if (-not (Test-Path $cacheDir)) { return 0 }
    ((Get-ChildItem $cacheDir -Recurse -File | Measure-Object Length -Sum).Sum)
}
# Read one counter out of the perf report by its name, so a reordered report does
# not silently read the wrong line.
function Report-Field([string]$Label) {
    if (-not (Test-Path $report)) { return "NO-REPORT" }
    $line = Select-String -Path $report -Pattern ([regex]::Escape($Label)) | Select-Object -First 1
    if (-not $line) { return "ABSENT" }
    ($line.Line -replace '\s+', ' ').Trim()
}

# ---- scratch folder: the *residue*, which is the whole point -----------------
# Large scans with no embedded EXIF thumbnail (12-20 ms if they had one, ~700 ms
# without) plus one animation, so both halves of the change are exercised.
$scratch = Join-Path $env:TEMP "iv-thumb-probe"
if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
New-Item -ItemType Directory -Path $scratch | Out-Null
$big = Get-ChildItem "test-images\*" -File |
       Where-Object { $_.Length -gt 15MB -and $_.Extension -match '^\.(jpg|jpeg|png)$' } |
       Select-Object -First 3
foreach ($f in $big) { Copy-Item $f.FullName (Join-Path $scratch $f.Name) }
if (@(Get-ChildItem $scratch -File).Count -eq 0) {
    Write-Host "REFUSING: no large files in test-images/ to probe with." -ForegroundColor Red
    Write-Host "The residue - files with no embedded thumbnail - is the whole point here." -ForegroundColor Red
    exit 2
}
Copy-Item "test-images\MouseUtils.gif" (Join-Path $scratch "anim.gif") -ErrorAction SilentlyContinue
Write-Host ("scratch : {0}  ({1} files)" -f $scratch, @(Get-ChildItem $scratch -File).Count)
foreach ($f in Get-ChildItem $scratch -File) {
    Write-Host ("  {0,-34} {1,6} MB" -f $f.Name, [math]::Round($f.Length / 1MB, 1))
}

function Open-Grid([string]$Tag) {
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
    for ($i = 0; $i -lt 20; $i++) {
        if ([Native]::Focus($hwnd)) { break }
        Start-Sleep -Milliseconds 250
    }
    if (![Native]::ForegroundIs($hwnd)) {
        $proc.Kill(); throw "could not focus the viewer; every key would read as broken"
    }
    # Let the first preview and its full-res dwell finish before the grid asks for
    # anything, or the two compete for the same four workers.
    Start-Sleep -Seconds 3

    $before = Cache-Entries
    $sw = [Diagnostics.Stopwatch]::StartNew()
    [Keys]::Tap(0x47)   # G
    # Poll for cells landing on disk rather than sleeping a guessed interval: the
    # cold case is ~700 ms a cell and the warm case ~1 ms, and a fixed wait would
    # either waste a minute or miss the effect entirely.
    $landed = 0
    while ($sw.Elapsed.TotalSeconds -lt $Settle) {
        $landed = (Cache-Entries) - $before
        if ($landed -gt 0) { break }
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Seconds 2   # let the rest of the visible cells finish
    if (![Native]::ForegroundIs($hwnd)) {
        $proc.CloseMainWindow() | Out-Null; Start-Sleep -Milliseconds 800
        if (!$proc.HasExited) { $proc.Kill() }
        throw "lost the foreground during $Tag - void, not evidence about the app"
    }
    $shot = Join-Path $env:TEMP "iv-thumb-$Tag.png"
    [Native]::Shot($hwnd, $shot) | Out-Null

    $entries = Cache-Entries
    # CloseMainWindow, not Kill: `on_exit` is what writes the report.
    $proc.CloseMainWindow() | Out-Null
    for ($i = 0; $i -lt 40; $i++) {
        if ($proc.HasExited) { break }
        Start-Sleep -Milliseconds 250
    }
    $clean = $proc.HasExited
    if (!$clean) { $proc.Kill() }
    Start-Sleep -Milliseconds 400

    [pscustomobject]@{
        Tag       = $Tag
        Landed    = $landed
        Entries   = $entries
        Bytes     = (Cache-Bytes)
        CleanExit = $clean
        Shot      = $shot
        Hits      = (Report-Field "grid thumbnails from cache")
        Misses    = (Report-Field "grid thumbnails decoded")
        ToThumbs  = (Report-Field "gallery open")
    }
}

Write-Host "`n=== run 1: cold (cache removed first) ===" -ForegroundColor Cyan
Remove-Item $cacheDir -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "cache before: $(Cache-Entries) entries"
$cold = Open-Grid "cold"

Write-Host "`n=== run 2: warm (same files, nothing touched) ===" -ForegroundColor Cyan
Write-Host "cache before: $(Cache-Entries) entries"
$warm = Open-Grid "warm"

Write-Host "`n=== verdict ===" -ForegroundColor Cyan
Write-Host ("  run      entries  MB   landed  clean-exit")
Write-Host ("  {0,-6}  {1,7}  {2,-4}  {3,6}  {4}" -f $cold.Tag, $cold.Entries, [math]::Round($cold.Bytes/1MB,1), $cold.Landed, $cold.CleanExit)
Write-Host ("  {0,-6}  {1,7}  {2,-4}  {3,6}  {4}" -f $warm.Tag, $warm.Entries, [math]::Round($warm.Bytes/1MB,1), $warm.Landed, $warm.CleanExit)
Write-Host ""
Write-Host "  cold : $($cold.Hits)"
Write-Host "  cold : $($cold.Misses)"
Write-Host "  warm : $($warm.Hits)"
Write-Host "  warm : $($warm.Misses)"
Write-Host "  cold : $($cold.ToThumbs)"
Write-Host "  warm : $($warm.ToThumbs)"
Write-Host ""
Write-Host "  shots: $($cold.Shot)  $($warm.Shot)"

$ok = $true
if ($cold.Entries -lt 1) { Write-Host "  FAIL cold run wrote no cache entries" -ForegroundColor Red; $ok = $false }
if (-not $cold.CleanExit) { Write-Host "  FAIL cold run did not exit cleanly, so its report is missing" -ForegroundColor Red; $ok = $false }
if (-not $warm.CleanExit) { Write-Host "  FAIL warm run did not exit cleanly" -ForegroundColor Red; $ok = $false }
if ($warm.Hits -eq "ABSENT" -or $warm.Hits -eq "NO-REPORT") {
    Write-Host "  FAIL the report has no cache-hit counter" -ForegroundColor Red; $ok = $false
} elseif ($warm.Hits -match "\s0(\s|$)") {
    Write-Host "  FAIL the warm run reported 0 cache hits" -ForegroundColor Red; $ok = $false
}
Write-Host ""
Write-Host $(if ($ok) { "VERDICT: PASS" } else { "VERDICT: FAIL" }) -ForegroundColor $(if ($ok) { "Green" } else { "Red" })
