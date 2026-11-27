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
$windows = @(Get-Process -Name @('Grandettoken', 'tokenscope') -EA SilentlyContinue |
  Where-Object { $_.MainWindowHandle -ne 0 -and (-not $AppProcessId -or $_.Id -eq $AppProcessId) })
if ($windows.Count -ne 1) { throw "expected exactly one app window; found $($windows.Count)" }
$p = $windows[0]
if (-not $p) { throw 'app (Grandettoken/tokenscope) is not running or has no main window' }
$h = $p.MainWindowHandle
if ($h -eq 0) { throw 'no main window handle' }
'{0:X8}' -f [W.U]::GetWindowLong($h, -16)
