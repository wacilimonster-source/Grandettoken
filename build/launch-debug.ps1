# Launch the app with WebView2 remote debugging enabled, so cdp-probe.js can
# inspect the live DOM. ASCII-only.
param(
  [string]$Exe = "$(Split-Path -Parent $PSScriptRoot)\app\src-tauri\target\release\tokenscope.exe",
  [int]$Port = 9222,
  [int]$WaitSec = 15
)

Get-Process tokenscope -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 2

$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=$Port"

# Capture panics/stderr: the app is subsystem=windows, so without a redirected
# handle a crash leaves no trace anywhere.
$logDir = Join-Path $env:TEMP 'tokenscope-debug'
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$outLog = Join-Path $logDir 'stdout.log'
$errLog = Join-Path $logDir 'stderr.log'

$p = Start-Process -FilePath $Exe -PassThru -RedirectStandardOutput $outLog -RedirectStandardError $errLog
Start-Sleep -Seconds $WaitSec

if ($p.HasExited) {
  Write-Output "EXITED code=$($p.ExitCode)"
  Write-Output "stderr: $errLog"
  Get-Content $errLog -EA SilentlyContinue | Select-Object -Last 20
  exit 1
}

Write-Output "RUNNING pid=$($p.Id) debugPort=$Port"
Write-Output "logs: $logDir"
