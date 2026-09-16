# Build the single-file WebView2 loader archive for the GNU toolchain.
#
# Tauri's webview2-com-sys links WebView2Loader.dll dynamically on Windows-GNU,
# which forces the delivery to be "exe + DLL". MSVC links the same code
# statically (WebView2LoaderStatic.lib). This script repacks that static library
# together with a tiny MSVC-CRT shim (msvc-shim.c + msvc-alias.S) into a GNU
# archive named libWebView2Loader.dll.a, so `-lWebView2Loader.dll` resolves to
# static code and the exe has no DLL import at all.
#
# ASCII-only (PowerShell 5.1 reads .ps1 as GBK on zh-CN Windows).
$ErrorActionPreference = 'Stop'

$root     = Split-Path -Parent $PSScriptRoot
$srcDir   = Join-Path $PSScriptRoot 'webview2-static'
$outDir   = Join-Path $srcDir 'out'
$mingwBin = Join-Path $env:USERPROFILE 'mingw64\bin'
if (-not (Test-Path $mingwBin)) { $mingwBin = Join-Path $PSScriptRoot 'mingw64\bin' }
$gcc  = Join-Path $mingwBin 'gcc.exe'
$ar   = Join-Path $mingwBin 'ar.exe'
if (-not (Test-Path $gcc)) { throw "missing gcc: $gcc (run install-toolchain.ps1 first)" }

# Locate WebView2LoaderStatic.lib inside the cargo registry checkout of webview2-com-sys.
$regSrc = Join-Path $env:USERPROFILE '.cargo\registry\src'
$lib = Get-ChildItem -Path $regSrc -Directory -Filter 'webview2-com-sys-*' -Recurse -Depth 1 -EA SilentlyContinue |
  ForEach-Object { Join-Path $_.FullName 'x64\WebView2LoaderStatic.lib' } |
  Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $lib) { throw "WebView2LoaderStatic.lib not found under $regSrc (build the project once so cargo fetches webview2-com-sys)" }
Write-Host "static loader: $lib"

$tmp = Join-Path $env:TEMP ('wv2static-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

try {
  # 1. compile the MSVC-CRT shim + the mangled-name aliases
  & $gcc -c (Join-Path $srcDir 'msvc-shim.c')  -o (Join-Path $tmp 'msvc-shim.o')  -O2
  if ($LASTEXITCODE -ne 0) { throw "gcc failed on msvc-shim.c" }
  & $gcc -c (Join-Path $srcDir 'msvc-alias.S') -o (Join-Path $tmp 'msvc-alias.o')
  if ($LASTEXITCODE -ne 0) { throw "gcc failed on msvc-alias.S" }

  # 2. unpack the MSVC archive. Its member names are relative paths, so the
  #    directories have to exist before `ar x` can write them out.
  Copy-Item $lib (Join-Path $tmp 'loader.lib')
  Push-Location $tmp
  try {
    $members = & $ar t loader.lib
    foreach ($m in $members) {
      $dir = Split-Path -Parent $m
      if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    }
    & $ar x loader.lib
    if ($LASTEXITCODE -ne 0) { throw "ar x failed" }
  } finally { Pop-Location }

  # 3. repack: loader objects + shim. One archive, so the shim members get
  #    pulled in the same pass that pulls the loader objects.
  $objs = @(Get-ChildItem -Path $tmp -Recurse -Filter '*.obj' | ForEach-Object { $_.FullName })
  if ($objs.Count -lt 3) { throw "unexpected loader object count: $($objs.Count)" }
  $archive = Join-Path $outDir 'libWebView2Loader.dll.a'
  if (Test-Path $archive) { Remove-Item $archive -Force }
  & $ar rcs $archive @objs (Join-Path $tmp 'msvc-shim.o') (Join-Path $tmp 'msvc-alias.o')
  if ($LASTEXITCODE -ne 0) { throw "ar rcs failed" }

  $kb = [math]::Round((Get-Item $archive).Length / 1KB)
  Write-Host "ok    $archive ($kb KB, $($objs.Count) loader objects + shim)"
} finally {
  Remove-Item -Recurse -Force $tmp -EA SilentlyContinue
}
