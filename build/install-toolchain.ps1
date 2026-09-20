# Install Rust GNU toolchain + MinGW-w64 without admin rights.
# ASCII-only on purpose: PowerShell reads .ps1 as ANSI/GBK on zh-CN Windows,
# which corrupts UTF-8 comments and breaks string parsing.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Derive from script location so the repo is self-contained on any machine.
# MinGW goes to the user profile (ASCII path). GNU ld cannot resolve its sysroot
# under a non-ASCII directory, so it must NOT live inside a non-ASCII repo path.
$root  = $PSScriptRoot
$dl    = Join-Path $root 'dl'
$mingw = Join-Path $env:USERPROFILE 'mingw64'
New-Item -ItemType Directory -Force -Path $dl | Out-Null

function Get-File($url, $out) {
  if (Test-Path $out) { Write-Host "skip  $out"; return }
  Write-Host "get   $url"
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  Invoke-WebRequest -Uri $url -OutFile $out -UseBasicParsing -TimeoutSec 900
  $sw.Stop()
  $mb = [math]::Round((Get-Item $out).Length / 1MB, 1)
  Write-Host "done  $mb MB in $([math]::Round($sw.Elapsed.TotalSeconds,1))s"
}

# 1. rustup-init
$rustup = Join-Path $dl 'rustup-init.exe'
Get-File 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe' $rustup

# 2. MinGW-w64 (WinLibs standalone zip). Pinned by SHA-256: a corrupted or
#    swapped archive used to unpack straight into the toolchain silently.
$zip = Join-Path $dl 'winlibs.zip'
$winlibsUrl = 'https://github.com/brechtsanders/winlibs_mingw/releases/download/14.2.0posix-19.1.1-12.0.0-ucrt-r2/winlibs-x86_64-posix-seh-gcc-14.2.0-mingw-w64ucrt-12.0.0-r2.zip'
$winlibsSha256 = 'D41933CEF13113018418D7B596319C2ED59A567395BBB87AFE27A171E111D553'
Get-File $winlibsUrl $zip
$actual = (Get-FileHash $zip -Algorithm SHA256).Hash
if ($actual -ne $winlibsSha256) {
  throw "winlibs.zip SHA-256 mismatch: expected $winlibsSha256, got $actual. Delete build\dl\winlibs.zip and re-run; if a NEW release is intentional, update the pinned hash in this script deliberately."
}
Write-Host "ok    winlibs.zip SHA-256 verified"

if (-not (Test-Path (Join-Path $mingw 'bin\gcc.exe'))) {
  Write-Host "unzip $zip"
  $tmp = Join-Path $dl 'x'
  if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  $inner = Get-ChildItem $tmp -Directory | Select-Object -First 1
  Move-Item $inner.FullName $mingw -Force
  Remove-Item -Recurse -Force $tmp
  Write-Host "ok    MinGW -> $mingw"
} else {
  Write-Host "skip  MinGW already present"
}

# 3. rustup toolchain
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
  Write-Host "skip  cargo already present"
} else {
  Write-Host "rust  installing stable-x86_64-pc-windows-gnu (this takes a few minutes)"
  $env:CARGO_HOME  = Join-Path $env:USERPROFILE '.cargo'
  $env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'
  & $rustup -y --no-modify-path --profile minimal --default-host x86_64-pc-windows-gnu --default-toolchain stable
  if ($LASTEXITCODE -ne 0) { throw "rustup-init failed with exit code $LASTEXITCODE" }
}

# 4. verify
$env:Path = "$cargoBin;" + (Join-Path $mingw 'bin') + ";$env:Path"
Write-Host ""
Write-Host "=== verify ==="
& (Join-Path $cargoBin 'rustc.exe') -V
& (Join-Path $cargoBin 'cargo.exe') -V
& (Join-Path $mingw 'bin\gcc.exe') --version | Select-Object -First 1
# tauri-cli is required by `build.ps1 bundle`. Compiling it takes 10+ minutes, so
# this script only CHECKS and tells the operator what to run - it does not build it.
& (Join-Path $cargoBin 'cargo-tauri.exe') --version *> $null
if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $cargoBin 'cargo-tauri.exe'))) {
  Write-Warning "tauri-cli not installed - 'build.ps1 bundle' will fail. Run: cargo install tauri-cli --locked (10+ min, once)"
} else {
  Write-Host "ok    tauri-cli present"
}
Write-Host ""
Write-Host "toolchain ready."
Write-Host "CARGO_BIN=$cargoBin"
Write-Host "MINGW_BIN=$(Join-Path $mingw 'bin')"
