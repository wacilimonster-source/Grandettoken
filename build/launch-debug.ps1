# Launch the app with WebView2 remote debugging enabled, so cdp-probe.js can
# inspect the live DOM. ASCII-only.
param(
  [string]$Exe = 'C:\Users\wacil\.zcode\workspace\default\app\src-tauri\target\release\tokenscope.exe',
  [int]$Port = 9222,
  [int]$WaitSec = 15
)

Get-Process tokenscope -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 2

$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port"

$p = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds $WaitSec

if ($p.HasExited) {
  Write-Output "EXITED code=$($p.ExitCode)"
  exit 1
}

Write-Output "RUNNING pid=$($p.Id) debugPort=$Port"
