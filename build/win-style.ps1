# Read GWL_STYLE of the running app window. Fixed-size contract:
# a resizable window carries WS_THICKFRAME (0x00040000, the drag border) and
# WS_MAXIMIZEBOX (0x00010000); both must be absent.
# ASCII-only, like every other .ps1 here.
$ErrorActionPreference = 'Stop'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern int GetWindowLong(System.IntPtr h, int i);
'@
# Match both binary names: bundler output is Grandettoken.exe (mainBinaryName),
# a plain `cargo build` produces tokenscope.exe.
$p = Get-Process -Name @('Grandettoken', 'tokenscope') -EA SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { throw 'app (Grandettoken/tokenscope) is not running or has no main window' }
$h = $p.MainWindowHandle
if ($h -eq 0) { throw 'no main window handle' }
'{0:X8}' -f [W.U]::GetWindowLong($h, -16)
