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

# PowerShell 5.1's Start-Process throws ArgumentException "item has already been
# added" when the environment holds two names that differ only in case, because
# it stuffs the environment into an OrdinalIgnoreCase dictionary. Proxy tools set
# both http_proxy and HTTP_PROXY, which is unrelated to this app but makes this
# script fail outright on such machines. Drop the pair and keep the lowercase one
# (same value, so the child's proxy settings are unchanged).
foreach ($n in 'http_proxy', 'https_proxy', 'all_proxy', 'no_proxy') {
  $v = [Environment]::GetEnvironmentVariable($n)
  if ($v) {
    [Environment]::SetEnvironmentVariable($n.ToUpper(), $null, 'Process')
    [Environment]::SetEnvironmentVariable($n, $null, 'Process')
    [Environment]::SetEnvironmentVariable($n, $v, 'Process')
  }
}

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
