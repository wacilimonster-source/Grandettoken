# Pre-release gates for the desktop app, run serially (AGENTS.md section 4.1 / 6.7).
# ASCII-only for Windows PowerShell 5.1.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File build/run-gates.ps1
#
# Exit code 0 only when every gate passed. The list mirrors AGENTS.md section 4.1; when a gate
# is added there or in ci.yml, add it here too.
param([switch]$SkipRust)

. (Join-Path $PSScriptRoot 'release-common.ps1')
$started = Get-Date
Set-Location $script:Root
# Loopback fixtures must not go through a configured proxy (AGENTS.md 6.7).
$env:NO_PROXY = '127.0.0.1,localhost,::1'

function Gate([string]$Name, [scriptblock]$Command) {
  Invoke-Native $Name $Command
  Write-Host "  ok $Name"
}

Write-Step 'static gates (zero dependency)'
foreach ($s in @('check-tracked-artifacts', 'check-silent-failures', 'check-file-size', 'check-css-forms',
                 'check-encoding', 'check-app-split', 'check-lock-unwrap')) {
  $file = "build/$s.js"
  Gate "$s --self-test" { node $file --self-test }
  Gate $s { node $file }
}
Gate 'check-ipc-contract' { node build/check-ipc-contract.js }
Gate 'verify-expiry' { node build/verify-expiry.js }
Gate 'verify-qoder-expiry --self-test' { node build/verify-qoder-expiry.js --self-test }
Gate 'verify-quota-plan' { node build/verify-quota-plan.js }
$syntax = @(Get-ChildItem app/src/*.js | ForEach-Object FullName) + (Resolve-Path app/src-tauri/src/google_cloud_capture.js).Path
foreach ($f in $syntax) { Gate "node --check $(Split-Path -Leaf $f)" { node --check $f } }

Write-Step 'jsdom smoke'
$prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
node -e "require('jsdom')" 2>$null; $hasJsdom = ($LASTEXITCODE -eq 0)
$ErrorActionPreference = $prev
if (-not $hasJsdom) { Gate 'npm install jsdom' { npm install jsdom --no-save --no-audit --no-fund } }
foreach ($s in @('jsdom-smoke', 'verify-vault', 'verify-codex-reset', 'verify-google-cloud',
                 'verify-antigravity-capture', 'verify-gateway', 'verify-gateway-magpie')) {
  $file = "build/$s.js"
  Gate $s { node $file }
}

if ($SkipRust) {
  Write-Host 'warn  Rust gates skipped (-SkipRust): not a release-grade run'
} else {
  Write-Step 'rust'
  Use-Toolchain (Get-Channel 'desktop').toolchain | Out-Null
  Push-Location app/core
  try {
    Gate 'core cargo test' { cargo test --locked }
    Gate 'core cargo check' { cargo check --all-targets --locked }
  } finally { Pop-Location }
  # src-tauri cannot run cargo test on this machine (STATUS_ENTRYPOINT_NOT_FOUND, AGENTS.md 6.7).
  Push-Location app/src-tauri
  try { Gate 'src-tauri cargo check' { cargo check --all-targets --locked } } finally { Pop-Location }
}

Write-Step 'secret scan'
Gate 'scan-secrets' { powershell -NoProfile -ExecutionPolicy Bypass -File build/scan-secrets.ps1 }

Write-Host ""
Write-Host ("all gates passed in {0:n1} min" -f ((Get-Date) - $started).TotalMinutes)
