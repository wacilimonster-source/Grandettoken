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

function Get-File($url, $out, $minBytes = 0) {
  if (Test-Path $out) {
    $have = (Get-Item $out).Length
    if ($minBytes -gt 0 -and $have -lt $minBytes) {
      # A truncated download (or an HTML error page saved under this name) used to be
      # accepted forever by the bare Test-Path: rustup-init then failed with a
      # confusing error, or did nothing at all. Same idea as the pinned winlibs
      # SHA-256 below - never trust a leftover file just because it exists.
      Write-Host "redo  $out is $have bytes (< $minBytes) - discarding and re-downloading"
      Remove-Item $out -Force
    } else {
      Write-Host "skip  $out"
      return
    }
  }
  Write-Host "get   $url"
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  Invoke-WebRequest -Uri $url -OutFile $out -UseBasicParsing -TimeoutSec 900
  $sw.Stop()
  $len = (Get-Item $out).Length
  if ($minBytes -gt 0 -and $len -lt $minBytes) {
    Remove-Item $out -Force -EA SilentlyContinue
    throw "$out is only $len bytes (expected at least $minBytes): truncated download or an error page. Delete build\dl and re-run; if the URL now serves something else, update this script deliberately."
  }
  $mb = [math]::Round($len / 1MB, 1)
  Write-Host "done  $mb MB in $([math]::Round($sw.Elapsed.TotalSeconds,1))s"
}

# 1. rustup-init. The URL always serves the newest rustup, so it cannot be pinned by
#    hash; a size floor is what catches a truncated file or a saved error page
#    (a real rustup-init.exe is >8 MB, so 1 MB never rejects a good download).
$rustup = Join-Path $dl 'rustup-init.exe'
Get-File 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-gnu/rustup-init.exe' $rustup 1MB

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
  if (-not $inner) { throw "winlibs.zip contains no top-level directory - delete build\dl\winlibs.zip and re-run" }
  if (Test-Path $mingw) {
    # %USERPROFILE%\mingw64 already exists (older unpack, or a nested one left behind
    # by the old bug). `Move-Item $inner $mingw -Force` into an EXISTING directory
    # NESTS the tree - mingw64\mingw64\bin\gcc.exe - and the old script still printed
    # "ok MinGW -> ...", so every later build failed with a confusing "gcc not found".
    # Merge the CONTENTS into place instead, then prove gcc is where it belongs.
    Write-Host "merge $($inner.FullName) -> $mingw (existing directory kept)"
    Get-ChildItem -LiteralPath $inner.FullName -Force | ForEach-Object {
      Copy-Item -LiteralPath $_.FullName -Destination $mingw -Recurse -Force
    }
  } else {
    Move-Item $inner.FullName $mingw -Force
  }
  Remove-Item -Recurse -Force $tmp
  if (-not (Test-Path (Join-Path $mingw 'bin\gcc.exe'))) {
    throw "MinGW unpack failed: $mingw\bin\gcc.exe is missing. Delete the whole '$mingw' directory (it holds a broken or nested unpack) and run install-toolchain.ps1 again."
  }
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
# Native commands do NOT throw under $ErrorActionPreference='Stop': a broken or
# absent default toolchain still printed "toolchain ready." and exited 0, because
# only the tauri-cli probe below ever looked at $LASTEXITCODE (report P3-21).
# Every probe now has its exit code examined; a missing path is reported as such.
function Assert-Tool($exe, $arguments, $what) {
  if (-not (Test-Path $exe)) {
    throw "$what not found: $exe - the toolchain install above did not complete; delete the directory and run install-toolchain.ps1 again"
  }
  $out = & $exe @arguments
  $code = $LASTEXITCODE
  if ($code -ne 0) { throw "$what failed (exit code $code): $exe $($arguments -join ' ')" }
  return $out
}
Write-Host (Assert-Tool (Join-Path $cargoBin 'rustc.exe') @('-V') 'rustc')
Write-Host (Assert-Tool (Join-Path $cargoBin 'cargo.exe') @('-V') 'cargo')
(Assert-Tool (Join-Path $mingw 'bin\gcc.exe') @('--version') 'gcc' | Select-Object -First 1) | Write-Host
# tauri-cli is required by `build.ps1 bundle`. Compiling it takes 10+ minutes, so
# this script only CHECKS and tells the operator what to run - it does not build it.
# Calling a path that does not exist raises a terminating CommandNotFound, and with
# $ErrorActionPreference='Stop' that kills the script right here - on exactly the
# machine where cargo-tauri is missing, so the helpful Write-Warning below is never
# reached (report P3-20). Check existence first.
$tauriCli = Join-Path $cargoBin 'cargo-tauri.exe'
if (Test-Path $tauriCli) {
  & $tauriCli --version *> $null
} else {
  $LASTEXITCODE = 1
}
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $tauriCli)) {
  Write-Warning "tauri-cli not installed - 'build.ps1 bundle' will fail. Run: cargo install tauri-cli --locked (10+ min, once)"
} else {
  Write-Host "ok    tauri-cli present"
}
Write-Host ""
Write-Host "toolchain ready."
Write-Host "CARGO_BIN=$cargoBin"
Write-Host "MINGW_BIN=$(Join-Path $mingw 'bin')"
