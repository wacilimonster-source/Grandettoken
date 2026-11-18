# Read GWL_STYLE of the running app window.
# The fixed panel omits WS_THICKFRAME (resize border) and WS_MAXIMIZEBOX.
# ASCII-only, like every other .ps1 here.
param([string]$WindowTitle)
$ErrorActionPreference = 'Stop'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern int GetWindowLong(System.IntPtr h, int i);
'@
# Match both binary names: bundler output is Grandettoken.exe (mainBinaryName),
# a plain `cargo build` produces tokenscope.exe.
if ($WindowTitle) {
    $windows = @(Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -eq $WindowTitle })
    if ($windows.Count -ne 1) { throw "expected one window with the CDP target title; found $($windows.Count)" }
    $p = $windows[0]
} else {
    $p = Get-Process -Name @('Grandettoken', 'tokenscope') -EA SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
}
if (-not $p) { throw 'app (Grandettoken/tokenscope) is not running or has no main window' }
$h = $p.MainWindowHandle
if ($h -eq 0) { throw 'no main window handle' }
'{0:X8}' -f [W.U]::GetWindowLong($h, -16)
