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

$coreDir = Join-Path $root 'app\core'

Push-Location $tauriDir
try {
  switch ($Task) {
    # 逻辑层的测试独立跑:只链接 serde/rusqlite 等基础库,秒级完成,
    # 不像 Tauri 二进制那样需要 WebView2 运行时才能启动。
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
  Pop-Location
}
