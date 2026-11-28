# Shared helpers for build/release-desktop.ps1 and build/release-android.ps1.
# ASCII-only on purpose: Windows PowerShell 5.1 reads BOM-less scripts in the ANSI code page.
#
# Toolchains, archives and GitHub publishing live in the NW-wide ..\..\_release directory
# (toolchains.json + release_store.py); this file only wires them to this repository.

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
$env:PYTHONUTF8 = '1'

$script:Root = Split-Path -Parent $PSScriptRoot
$script:NwRelease = Join-Path (Split-Path -Parent $script:Root) '_release'
$script:Store = Join-Path $script:NwRelease 'release_store.py'
$script:Config = Get-Content -Raw -Encoding utf8 -LiteralPath (Join-Path $script:Root 'tool\release.json') | ConvertFrom-Json
if (-not (Test-Path -LiteralPath $script:Store)) { throw "NW release store missing: $script:Store" }

function Write-Step([string]$Text) { Write-Host ""; Write-Host "=== $Text ===" }

# PS 5.1 turns native stderr into terminating errors when streams are captured; keep the exit code
# as the single source of truth instead (same trap build.ps1 documents).
function Invoke-Native([string]$Label, [scriptblock]$Command) {
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { & $Command; $code = $LASTEXITCODE } finally { $ErrorActionPreference = $prev }
  if ($code -ne 0) { throw "$Label failed (exit $code)" }
}

function Get-NativeText([string]$Label, [scriptblock]$Command) {
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { $out = & $Command 2>$null; $code = $LASTEXITCODE } finally { $ErrorActionPreference = $prev }
  if ($code -ne 0) { throw "$Label failed (exit $code)" }
  return (@($out) -join "`n").Trim()
}

function Expand-ConfigPath([string]$Value) { [Environment]::ExpandEnvironmentVariables($Value) }

function Get-Channel([string]$Name) {
  $ch = $script:Config.channels.$Name
  if (-not $ch) { throw "tool/release.json has no channel '$Name'" }
  return $ch
}

# Point this process at a toolchain registered in _release/toolchains.json instead of whatever is
# first on PATH; child processes (build.ps1, cargo, gradle) inherit it.
function Use-Toolchain([string]$Name) {
  $all = Get-Content -Raw -Encoding utf8 -LiteralPath (Join-Path $script:NwRelease 'toolchains.json') | ConvertFrom-Json
  $t = $all.$Name
  if (-not $t) { throw "toolchain '$Name' is not registered in _release/toolchains.json" }
  if ($t.rustToolchain) {
    foreach ($p in @($t.cargoHome, $t.mingw)) { if (-not (Test-Path -LiteralPath $p)) { throw "toolchain path missing: $p" } }
    $env:CARGO_HOME = $t.cargoHome
    $env:RUSTUP_HOME = $t.rustupHome
    $env:RUSTUP_TOOLCHAIN = $t.rustToolchain
    $env:Path = (Join-Path $t.cargoHome 'bin') + ';' + $t.mingw + ';' + $env:Path
    $cli = Get-NativeText 'cargo tauri --version' { cargo tauri --version }
    if ($cli -notmatch [regex]::Escape($t.tauriCli)) { throw "tauri-cli is '$cli', toolchain '$Name' registers $($t.tauriCli)" }
  }
  if ($t.android) {
    foreach ($p in @($t.android, $t.java)) { if (-not (Test-Path -LiteralPath $p)) { throw "toolchain path missing: $p" } }
    $env:ANDROID_HOME = $t.android
    $env:ANDROID_SDK_ROOT = $t.android
    $env:JAVA_HOME = $t.java
    $env:GRADLE_USER_HOME = $t.gradle
    $env:Path = (Join-Path $t.java 'bin') + ';' + $env:Path
  }
  return $t
}

function Invoke-Store([string[]]$Arguments) {
  $args2 = @($script:Store) + $Arguments
  Invoke-Native "release_store $($Arguments[0])" { python @args2 }
}

function Get-StoreText([string[]]$Arguments) {
  $args2 = @($script:Store) + $Arguments
  return Get-NativeText "release_store $($Arguments[0])" { python @args2 }
}

# gh may be logged out; fall back to the credential git already keeps (never written to disk).
function Initialize-GitHubAuth {
  if ($env:GH_TOKEN) { return }
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { & gh auth status *> $null; $code = $LASTEXITCODE } finally { $ErrorActionPreference = $prev }
  if ($code -eq 0) { return }
  $env:GIT_TERMINAL_PROMPT = '0'
  $lines = "protocol=https`nhost=github.com`n`n" | git credential fill 2>$null
  $token = ($lines | Where-Object { $_ -like 'password=*' } | Select-Object -First 1) -replace '^password=', ''
  if (-not $token) { throw 'GitHub auth unavailable: run gh auth login or set GH_TOKEN' }
  $env:GH_TOKEN = $token
}

# The release commit must already be on origin/main: tags on unpushed commits produced wrong
# source archives before (v0.3.31).
function Assert-ReleasableHead {
  Push-Location $script:Root
  try {
    $dirty = Get-NativeText 'git status' { git status --porcelain }
    if ($dirty) { throw "working tree is not clean:`n$dirty" }
    $branch = Get-NativeText 'git branch' { git rev-parse --abbrev-ref HEAD }
    if ($branch -ne 'main') { throw "release from main only (current: $branch)" }
    Invoke-Native 'git fetch' { git fetch --quiet origin main }
    $head = Get-NativeText 'git rev-parse' { git rev-parse HEAD }
    $remote = Get-NativeText 'git rev-parse origin/main' { git rev-parse origin/main }
    if ($head -ne $remote) { throw "HEAD $head is not origin/main ${remote}; push main first" }
    return $head
  } finally { Pop-Location }
}

function Assert-TagFree([string]$Tag, [string]$Head) {
  Push-Location $script:Root
  try {
    $remote = Get-NativeText 'git ls-remote' { git ls-remote origin "refs/tags/$Tag" }
    if ($remote -and -not $remote.StartsWith($Head)) { throw "remote tag $Tag already points elsewhere: $remote" }
    $local = Get-NativeText 'git tag' { git tag --list $Tag }
    if ($local) {
      $at = Get-NativeText 'git rev-parse tag' { git rev-parse "refs/tags/$Tag^{commit}" }
      if ($at -ne $Head) { throw "local tag $Tag points at $at, not HEAD" }
    }
  } finally { Pop-Location }
}

function Get-GateMarker([string]$Kind, [string]$Head) {
  $dir = Join-Path $script:Root 'build\qa\release-gates'
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  return Join-Path $dir "$Kind-$Head.ok"
}

function Get-StageDir([string]$Name) {
  $dir = Join-Path $script:Root "build\qa\$Name"
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  return $dir
}

function Invoke-Prune([string]$ApplyLocal, [string]$ApplyRemote, [switch]$WithTags) {
  Write-Step 'local archive rotation (NW _release)'
  $rot = @('rotate', $script:Root)
  if ($ApplyLocal) { $rot += @('--apply', $ApplyLocal) }
  Invoke-Store $rot
  Write-Step 'GitHub release pruning'
  Initialize-GitHubAuth
  $pr = @('prune-remote', $script:Root)
  if ($WithTags) { $pr += '--with-tags' }
  if ($ApplyRemote) { $pr += @('--apply', $ApplyRemote) }
  Invoke-Store $pr
  if (-not $ApplyLocal -or -not $ApplyRemote) {
    Write-Host 'preview only: rerun with -ApplyLocal/-ApplyRemote <plan> after reviewing the lists above'
  }
}
