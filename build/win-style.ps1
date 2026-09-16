# Read GWL_STYLE of the running TokenScope window. Fixed-size contract:
# a resizable window carries WS_THICKFRAME (0x00040000, the drag border) and
# WS_MAXIMIZEBOX (0x00010000); both must be absent.
# ASCII-only, like every other .ps1 here.
$ErrorActionPreference = 'Stop'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern int GetWindowLong(System.IntPtr h, int i);
'@
$p = Get-Process tokenscope -EA SilentlyContinue
if (-not $p) { throw 'tokenscope is not running' }
$h = $p.MainWindowHandle
if ($h -eq 0) { throw 'no main window handle' }
'{0:X8}' -f [W.U]::GetWindowLong($h, -16)
