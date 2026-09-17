# Build TokenScope. ASCII-only for the same encoding reason as install-toolchain.ps1.
#
#   .\build.ps1 check    cargo check (fast feedback)
#   .\build.ps1 test     cargo test  (provider extractors + snapshot math)
#   .\build.ps1 dev      run in dev mode
#   .\build.ps1 build    release binary
#   .\build.ps1 bundle   NSIS installer
param([Parameter(Position=0)][string]$Task = 'check')

$ErrorActionPreference = 'Stop'

$root     = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
# MinGW must live on an ASCII path: GNU ld cannot resolve its own sysroot files
# when the toolchain sits under a non-ASCII directory (e.g. a Chinese repo path),
# so it installs to the user profile instead of inside the repo.
$mingwBin = Join-Path $env:USERPROFILE 'mingw64\bin'
if (-not (Test-Path $mingwBin)) { $mingwBin = Join-Path $PSScriptRoot 'mingw64\bin' }
$tauriDir = Join-Path $root 'app\src-tauri'

foreach ($p in @($cargoBin, $mingwBin)) {
  if (-not (Test-Path $p)) { throw "missing toolchain path: $p  (run install-toolchain.ps1 first)" }
}

$env:Path = "$cargoBin;$mingwBin;$env:Path"
$env:CARGO_HOME  = Join-Path $env:USERPROFILE '.cargo'
$env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'

# GNU binutils (ld, windres) cannot open files under non-ASCII paths: ld fails
# on .o/.rlib inputs, windres fails on the exe icon. Fail fast here instead of
# letting the linker produce cryptic errors halfway through the build.
if ($root -match '[^\x00-\x7F]') {
  throw "repo path is not ASCII: $root  (GNU ld/windres require an ASCII repo path)"
}

# MinGW's gcc is the linker for the *-pc-windows-gnu target.
$env:CC  = Join-Path $mingwBin 'gcc.exe'
$env:CXX = Join-Path $mingwBin 'g++.exe'

# ---- single-exe delivery: static WebView2 loader ----
# `-lWebView2Loader.dll` only matches `libWebView2Loader.dll.a`. The crate's build
# script copies the real DLL into its OUT_DIR and adds that dir to the search path;
# which one wins depends on -L order. So before every build: 1) make sure our
# static archive exists, 2) delete that DLL so only the archive can match.
$wvArchive = Join-Path $root 'build\webview2-static\out\libWebView2Loader.dll.a'
if (-not (Test-Path $wvArchive)) {
  Write-Host "prep  static WebView2 loader archive"
  & (Join-Path $PSScriptRoot 'make-webview2-static.ps1')
  if ($LASTEXITCODE -ne 0 -or -not (Test-Path $wvArchive)) { throw "make-webview2-static.ps1 failed" }
}
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $tauriDir 'target' }
foreach ($profile in @('release', 'debug')) {
  $buildDir = Join-Path $targetRoot (Join-Path $profile 'build')
  if (-not (Test-Path $buildDir)) { continue }
  Get-ChildItem -Path $buildDir -Directory -Filter 'webview2-com-sys-*' -EA SilentlyContinue |
    ForEach-Object {
      foreach ($n in @('WebView2Loader.dll', 'WebView2Loader.dll.lib')) {
        $f = Join-Path $_.FullName ('out\x64\' + $n)
        if (Test-Path $f) { Remove-Item $f -Force; Write-Host "strip $f (static link, single exe)" }
      }
    }
}

$coreDir = Join-Path $root 'app\core'

# ---- one build at a time ----
# Two cargo processes sharing one target dir corrupt the incremental cache:
# rustc then panics with "no entry found for key" (or "os error 5" while copying
# rmeta), which reads like a compiler bug and wastes an hour. Refuse to start
# when another build holds the lock. A lock older than 2h is treated as stale.
$lockFile = Join-Path $targetRoot '.build-lock'
New-Item -ItemType Directory -Force -Path $targetRoot | Out-Null
if (Test-Path $lockFile) {
  $age = (Get-Date) - (Get-Item $lockFile).LastWriteTime
  if ($age.TotalHours -lt 2) {
    throw "another build is running (lock: $lockFile, age $([math]::Round($age.TotalMinutes)) min). Wait for it, or delete the file if you are sure nothing else is building."
  }
  Write-Host "warn  stale build lock ($([math]::Round($age.TotalHours,1))h old) - taking over"
}
Set-Content -Path $lockFile -Value $PID -Encoding ASCII

Push-Location $tauriDir
try {
  switch ($Task) {
    # Core tests link only serde/rusqlite, so they run in a second and do not
    # need the WebView2 runtime the way the Tauri binary does.
    'test-core' { Push-Location $coreDir; try { & cargo test } finally { Pop-Location } }
    'test'      { Push-Location $coreDir; try { & cargo test } finally { Pop-Location } }
    'check'     { & cargo check --all-targets }
    'dev'       { & cargo run }
    'build'     { & cargo build --release }
    'bundle'    { & cargo tauri build }
    default     { throw "unknown task '$Task' (test|check|dev|build|bundle)" }
  }
  if ($LASTEXITCODE -ne 0) { throw "task '$Task' failed with exit code $LASTEXITCODE" }
} finally {
  Remove-Item $lockFile -Force -EA SilentlyContinue
  Pop-Location
}
