# Local desktop release: gates -> signed NSIS bundle -> NW archive -> GitHub Release -> verify.
# ASCII-only for Windows PowerShell 5.1. Full procedure: RELEASE.md.
#
#   build/release-desktop.ps1 -Stage Preflight   checks + full gates (writes a per-commit pass marker)
#   build/release-desktop.ps1 -Stage Build       bundle + sign + latest.json + archive (needs the marker)
#   build/release-desktop.ps1 -Stage Publish     tag + draft + byte check + publish + verify-release.js
#   build/release-desktop.ps1 -Stage Release     the three stages above in order
#   build/release-desktop.ps1 -Stage Accept      after an old install upgraded itself successfully
#   build/release-desktop.ps1 -Stage Prune       preview history cleanup; -ApplyLocal/-ApplyRemote <plan>
param(
  [ValidateSet('Preflight', 'Build', 'Publish', 'Release', 'Accept', 'Prune')][string]$Stage = 'Preflight',
  [string]$ApplyLocal,
  [string]$ApplyRemote,
  [switch]$WithTags
)

. (Join-Path $PSScriptRoot 'release-common.ps1')
Set-Location $script:Root
$channel = Get-Channel 'desktop'
$tauri = Join-Path $script:Root 'app\src-tauri'

function Get-DesktopVersion {
  $conf = (Get-Content -Raw -Encoding utf8 (Join-Path $tauri 'tauri.conf.json') | ConvertFrom-Json).version
  $cargo = [regex]::Match((Get-Content -Raw -Encoding utf8 (Join-Path $tauri 'Cargo.toml')), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
  $lock = [regex]::Match((Get-Content -Raw -Encoding utf8 (Join-Path $tauri 'Cargo.lock')), 'name = "tokenscope"\r?\nversion = "([^"]+)"').Groups[1].Value
  if ($conf -ne $cargo -or $conf -ne $lock) { throw "version mismatch: tauri.conf.json=$conf Cargo.toml=$cargo Cargo.lock=$lock" }
  if ($conf -notmatch '^\d+\.\d+\.\d+$') { throw "invalid version $conf" }
  return $conf
}

$version = Get-DesktopVersion
$tag = $channel.tag.Replace('{version}', $version)
$exe = "Grandettoken_${version}_x64-setup.exe"
Write-Host "desktop $tag"

function Invoke-Preflight {
  Write-Step 'preflight'
  $head = Assert-ReleasableHead
  Assert-TagFree $tag $head
  $notes = Join-Path (Get-StageDir "release-$tag") 'release-notes.md'
  Invoke-Store @('notes', $script:Root, 'desktop', $version, '--out', $notes)
  Initialize-GitHubAuth
  $latest = Get-NativeText 'gh latest' { gh api "repos/$($script:Config.distribution)/releases/latest" --jq .tag_name }
  if ($latest -match '^v(\d+\.\d+\.\d+)$' -and [version]$Matches[1] -gt [version]$version) { throw "GitHub Latest $latest is newer than $version" }
  Use-Toolchain $channel.toolchain | Out-Null
  $key = Expand-ConfigPath $channel.signingKey
  if (-not (Test-Path -LiteralPath $key)) { throw "updater signing key missing: $key" }
  Write-Step 'gates'
  Invoke-Native 'run-gates' { powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'run-gates.ps1') }
  Set-Content -LiteralPath (Get-GateMarker 'desktop' $head) -Value (Get-Date -Format o) -Encoding ascii
  Write-Host "preflight passed for $head"
}

function Invoke-Build {
  Write-Step 'build'
  $head = Assert-ReleasableHead
  Assert-TagFree $tag $head
  if (-not (Test-Path -LiteralPath (Get-GateMarker 'desktop' $head))) { throw "gates have not passed for ${head}; run -Stage Preflight first" }
  Use-Toolchain $channel.toolchain | Out-Null
  $env:TAURI_SIGNING_PRIVATE_KEY = Expand-ConfigPath $channel.signingKey
  # Unencrypted key: CI=true makes tauri-cli use an empty password. "$env:X = ''" would delete
  # the variable in PowerShell and the signer would wait for input forever (RELEASE.md).
  $env:CI = 'true'
  Invoke-Native 'bundle' { powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build.ps1') bundle }
  if (-not (Get-NativeText 'git tag' { git tag --list $tag })) { Invoke-Native 'git tag' { git tag $tag $head } }
  $stage = Get-StageDir "release-$tag"
  $notes = Join-Path $stage 'release-notes.md'
  Invoke-Store @('notes', $script:Root, 'desktop', $version, '--out', $notes)
  Invoke-Native 'make-latest-json' { node build/make-latest-json.js --notes-file $notes }
  $nsis = Join-Path $tauri 'target\release\bundle\nsis'
  $exePath = Join-Path $nsis $exe
  Invoke-Native 'verify-signature' { node build/verify-signature.js --local $exePath --sig "$exePath.sig" }
  $manifest = Get-Content -Raw -Encoding utf8 (Join-Path $nsis 'latest.json') | ConvertFrom-Json
  if ($manifest.platforms.'windows-x86_64'.signature -cne (Get-Content -Raw -Encoding utf8 "$exePath.sig").Trim()) {
    throw 'latest.json does not carry the verified installer signature'
  }
  Invoke-Store @('archive', $script:Root, 'desktop', $version, '--commit', $head,
    '--file', $exePath, '--file', "$exePath.sig", '--file', (Join-Path $nsis 'latest.json'), '--file', $notes)
}

function Invoke-Publish {
  Write-Step 'publish'
  Assert-ReleasableHead | Out-Null
  Initialize-GitHubAuth
  Invoke-Store @('publish', $script:Root, 'desktop', $version)
  Write-Step 'verify-release.js'
  Invoke-Native 'verify-release' { node --use-env-proxy build/verify-release.js }
  Write-Host "next: install $version over an older build, confirm the in-app update, then -Stage Accept"
}

switch ($Stage) {
  'Preflight' { Invoke-Preflight }
  'Build' { Invoke-Build }
  'Publish' { Invoke-Publish }
  'Release' { Invoke-Preflight; Invoke-Build; Invoke-Publish }
  'Accept' { Invoke-Store @('accept', $script:Root, 'desktop', $version); Invoke-Prune }
  'Prune' { Invoke-Prune -ApplyLocal $ApplyLocal -ApplyRemote $ApplyRemote -WithTags:$WithTags }
}
