# Read GWL_STYLE of the running app window.
# The fixed panel omits WS_THICKFRAME (resize border) and WS_MAXIMIZEBOX.
# ASCII-only, like every other .ps1 here.
param([int]$AppProcessId)
$ErrorActionPreference = 'Stop'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern int GetWindowLong(System.IntPtr h, int i);
'@
# Match both binary names: bundler output is Grandettoken.exe (mainBinaryName),
# a plain `cargo build` produces tokenscope.exe.
$pids = @(Get-Process -Name @('Grandettoken', 'tokenscope') -EA SilentlyContinue |
  Where-Object { -not $AppProcessId -or $_.Id -eq $AppProcessId } |
  ForEach-Object { $_.Id })
if ($pids.Count -ne 1) { throw "expected exactly one app process; found $($pids.Count)" }
# Enumerate top-level windows of the app process and pick the REAL main window.
# Process.MainWindowHandle is a heuristic that tauri helper windows
# (tray_icon_app / global_hotkey_app / Tao Thread Event Target / single-instance
# "siw") can win, and their styles differ per run -> false FAIL. Also, every
# tauri webview window shares the "Tauri Window" class, and transient capture
# windows may coexist (destroy is async), so pin the MAIN window by its
# configured title while it is visible.
Add-Type -Namespace W -Name E -MemberDefinition @'
public delegate bool EnumProc(System.IntPtr h, System.IntPtr l);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, System.IntPtr l);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(System.IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern int GetClassName(System.IntPtr h, System.Text.StringBuilder s, int n);
[DllImport("user32.dll")] public static extern int GetWindowText(System.IntPtr h, System.Text.StringBuilder s, int n);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(System.IntPtr h);
'@
$found = @()
$cb = [W.E+EnumProc]{
  param($h, $l)
  $pid2 = 0
  [void][W.E]::GetWindowThreadProcessId($h, [ref]$pid2)
  if ($pid2 -eq $pids[0]) {
    $c = New-Object System.Text.StringBuilder 256
    [void][W.E]::GetClassName($h, $c, 256)
    if ($c.ToString() -eq 'Tauri Window') {
      $t = New-Object System.Text.StringBuilder 256
      [void][W.E]::GetWindowText($h, $t, 256)
      $script:found += [PSCustomObject]@{
        Hwnd = $h; Title = $t.ToString(); Visible = [W.E]::IsWindowVisible($h)
      }
    }
  }
  return $true
}
[void][W.E]::EnumWindows($cb, [System.IntPtr]::Zero)
$main = @($found | Where-Object { $_.Title -eq 'Grandettoken' -and $_.Visible })
if ($main.Count -ne 1) {
  throw "expected exactly one visible main window; found $($main.Count) of $($found.Count) Tauri Window(s)"
}
'{0:X8}' -f [W.U]::GetWindowLong($main[0].Hwnd, -16)
