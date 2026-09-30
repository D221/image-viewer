param(
    [string]$Exe = "target\debug\image-viewer.exe"
)
# Ground truth for a keyboard chord: does the action happen when the chord is
# pressed?  Two chords with two different, *externally observable* effects, so
# "nothing happened" is unambiguous:
#
#   Ctrl+C  -> puts an image on the Windows clipboard       (read the clipboard)
#   Ctrl+S  -> writes a new file beside the current image   (check the disk)
#   bare C  -> crop mode (the control: does a plain key arrive at all?)
#
# Run on a console session only: injected input does nothing over RDP.

$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Native {
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  // Injected keys go to whatever is in front, so "I called SetForegroundWindow"
  // is not the same claim as "the app has focus". This is the check that turns
  // a silent no-op into a visible failure - and it is the check whose absence
  // made an earlier run report both chords broken when the app was fine.
  public static bool ForegroundIs(IntPtr h) { return GetForegroundWindow() == h; }
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
  private delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  // A debug build is a console-subsystem app, so the process also owns a
  // console window.  Take the largest *visible* window, which is the viewer.
  public static IntPtr MainWindow(int pid) {
    IntPtr best = IntPtr.Zero; long bestArea = 0;
    EnumWindows(delegate(IntPtr h, IntPtr l) {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p != (uint)pid || !IsWindowVisible(h)) return true;
      var sb = new StringBuilder(256); GetWindowTextW(h, sb, 256);
      if (sb.Length == 0) return true;   // skip the console's blank title
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
  // Screenshot the window itself, so the status bar (which carries the notice)
  // is readable.  The window title does not change for most keys.
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
  // The modifier is held *across* the key press, with real delays.  A chord
  // fired with no gap between modifier-down and key-down can be delivered by
  // Windows with stale modifiers, which looks exactly like "the app drops
  // chords" - which is what a previous harness concluded, wrongly.
  public static void Chord(byte mod, byte key) {
    keybd_event(mod, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(150);
    keybd_event(key, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(80);
    keybd_event(key, 0, KEYUP, UIntPtr.Zero);
    System.Threading.Thread.Sleep(80);
    keybd_event(mod, 0, KEYUP, UIntPtr.Zero);
  }
  public static void Tap(byte key) {
    keybd_event(key, 0, 0, UIntPtr.Zero);
    System.Threading.Thread.Sleep(60);
    keybd_event(key, 0, KEYUP, UIntPtr.Zero);
  }
}
"@

# The clipboard needs an STA thread; pwsh 7 is MTA by default, so the read and
# the clear both go through a child Windows PowerShell (STA by default).
$helperSrc = @'
param([string]$Mode, [string]$Out)
Add-Type -AssemblyName System.Windows.Forms
if ($Mode -eq "clear") {
    [System.Windows.Forms.Clipboard]::Clear()
    Set-Content -Path $Out -Value "CLEARED"
} else {
    $s = "NO-IMAGE"
    if ([System.Windows.Forms.Clipboard]::ContainsImage()) {
        $i = [System.Windows.Forms.Clipboard]::GetImage()
        $s = "IMAGE {0}x{1}" -f $i.Width, $i.Height
    } elseif ([System.Windows.Forms.Clipboard]::ContainsText()) {
        $s = "TEXT " + ([System.Windows.Forms.Clipboard]::GetText() -replace "\s+", " ")
    }
    Set-Content -Path $Out -Value $s
}
'@
$helper = Join-Path $env:TEMP "iv-clip-helper.ps1"
Set-Content -Path $helper -Value $helperSrc

function Clip([string]$Mode) {
    $f = Join-Path $env:TEMP "iv-clip-out.txt"
    Remove-Item $f -ErrorAction SilentlyContinue
    & powershell.exe -NoProfile -STA -File $helper -Mode $Mode -Out $f | Out-Null
    if (Test-Path $f) { (Get-Content $f -Raw).Trim() } else { "INSPECT-FAILED" }
}

# ---- scratch folder with one real photo --------------------------------
$scratch = Join-Path $env:TEMP "iv-chord-probe"
if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
New-Item -ItemType Directory -Path $scratch | Out-Null
$src = Get-ChildItem test-images -Recurse -Include *.jpg -File |
       Where-Object { $_.Length -gt 150000 } | Select-Object -First 1
Copy-Item $src.FullName (Join-Path $scratch "probe.jpg")
Write-Host "scratch : $scratch  ($($src.Name), $($src.Length) bytes)"
Write-Host "clipboard before : $(Clip read)"

$proc = Start-Process -FilePath $Exe -ArgumentList $scratch -PassThru
try {
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 80; $i++) {
        $hwnd = [Native]::MainWindow($proc.Id)
        if ($hwnd -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "no window appeared" }
    [Native]::ShowWindow($hwnd, 9) | Out-Null
    # SetForegroundWindow is refused outright when the caller is not the
    # foreground process, so retry and then *verify*, rather than assume.
    for ($i = 0; $i -lt 20; $i++) {
        [Native]::SetForegroundWindow($hwnd) | Out-Null
        Start-Sleep -Milliseconds 200
        if ([Native]::ForegroundIs($hwnd)) { break }
    }
    if (![Native]::ForegroundIs($hwnd)) {
        throw "could not bring the viewer to the foreground; injected keys would go to whatever is in front, and every chord would silently read as broken"
    }
    Write-Host "foreground: OK (viewer window $hwnd)"
    Start-Sleep -Seconds 3   # let the first decode and the full-res dwell finish

    Clip "clear" | Out-Null
    Start-Sleep -Milliseconds 300

    # A chord only means something if the app was in front for it. Re-check
    # after every one, so "the shortcut is broken" is never the conclusion
    # drawn from a keystroke that went to another window.
    function Assert-Foreground($what) {
        if (![Native]::ForegroundIs($hwnd)) {
            throw "lost the foreground during $what - the keys went elsewhere, so this result is void (not evidence about the app)"
        }
    }

    Write-Host "--- Ctrl+C : expect an image on the clipboard ---"
    Assert-Foreground "setup"
    [Keys]::Chord(0x11, 0x43)
    Start-Sleep -Seconds 2
    Assert-Foreground "Ctrl+C"
    $clip = Clip read
    Write-Host "  clipboard now : $clip"
    Write-Host "  VERDICT       : $(if ($clip -like 'IMAGE *') { 'PASS - copied' } else { 'FAIL - nothing on the clipboard' })"
    [Native]::Shot($hwnd, (Join-Path $env:TEMP "iv-shot-after-ctrlc.png")) | Out-Null

    Write-Host "--- Ctrl+S : expect a new file beside the original ---"
    Assert-Foreground "setup"
    [Keys]::Chord(0x11, 0x53)
    Start-Sleep -Seconds 2
    Assert-Foreground "Ctrl+S"
    $written = @(Get-ChildItem $scratch -File | Where-Object { $_.Name -ne "probe.jpg" })
    Write-Host "  files now     : $(if ($written.Count) { ($written | ForEach-Object Name) -join ', ' } else { 'NONE' })"
    Write-Host "  VERDICT       : $(if ($written.Count) { 'PASS - saved' } else { 'FAIL - no file written' })"
    [Native]::Shot($hwnd, (Join-Path $env:TEMP "iv-shot-after-ctrls.png")) | Out-Null

    Write-Host "--- bare C : control for 'do plain keys arrive?' (enters crop mode) ---"
    Assert-Foreground "setup"
    [Keys]::Tap(0x43)
    Start-Sleep -Milliseconds 700
    Assert-Foreground "bare C"
    [Native]::Shot($hwnd, (Join-Path $env:TEMP "iv-shot-after-barec.png")) | Out-Null
}
finally {
    try { $proc.CloseMainWindow() | Out-Null } catch {}
    Start-Sleep -Milliseconds 800
    if (!$proc.HasExited) { $proc.Kill() }
}
Write-Host "done"
