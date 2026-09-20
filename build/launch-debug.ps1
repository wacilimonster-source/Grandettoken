# Launch the app with WebView2 remote debugging enabled, so cdp-probe.js can
# inspect the live DOM. ASCII-only.
param(
  # Empty = auto-pick: the bundler renames the binary to mainBinaryName
  # (Grandettoken.exe), a plain `cargo build` leaves tokenscope.exe - accept both.
  [string]$Exe = "",
  [int]$Port = 9222,
  [int]$WaitSec = 15
)

$ErrorActionPreference = 'Stop'
$releaseDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'app\src-tauri\target\release'

if (-not $Exe) {
  # Pick the NEWEST of the two possible outputs: `cargo build` only updates
  # tokenscope.exe, while `cargo tauri build` (re)names Grandettoken.exe - the
  # other one is then a STALE copy, and launching it runs old code silently.
  $cands = @('Grandettoken.exe', 'tokenscope.exe') |
    ForEach-Object { Join-Path $releaseDir $_ } |
    Where-Object { Test-Path $_ } |
    Sort-Object LastWriteTime -Descending
  if ($cands) { $Exe = $cands[0] }
}
if (-not $Exe -or -not (Test-Path $Exe)) {
  throw "no app binary found in $releaseDir (build first: build.ps1 build)"
}
Write-Host "binary: $Exe ($((Get-Item $Exe).LastWriteTime))"

Get-Process -Name @('Grandettoken', 'tokenscope') -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 2

# Append, don't overwrite: the variable may already carry other WebView2 switches.
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = ("--remote-debugging-port=$Port " + $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS).Trim()

# PowerShell 5.1's Start-Process throws ArgumentException "item has already been
# added" when the environment holds two names that differ only in case, because
# it stuffs the environment into an OrdinalIgnoreCase dictionary. Proxy tools set
# both http_proxy and HTTP_PROXY, which is unrelated to this app but makes this
# script fail outright on such machines. Drop the pair and keep the lowercase one
# (same value, so the child's proxy settings are unchanged). Note this only covers
# these four names - any OTHER duplicated variable can still trip Start-Process.
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

Write-Output "RUNNING pid=$($p.Id) debugPort=$Port exe=$Exe"
Write-Output "logs: $logDir"
