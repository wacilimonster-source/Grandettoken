# Build TokenScope. ASCII-only for the same encoding reason as install-toolchain.ps1.
#
#   .\build.ps1 check    cargo check (fast feedback)
#   .\build.ps1 test     cargo test  (provider extractors + snapshot math)
#   .\build.ps1 dev      run in dev mode
#   .\build.ps1 build    release binary
#   .\build.ps1 bundle   NSIS installer
param([Parameter(Position=0)][string]$Task = 'check')

$ErrorActionPreference = 'Stop'
# When this run started: used to prove the artifact we assert on is from THIS run,
# not a leftover exe from an earlier version (report P0-3 / G-1).
$started = Get-Date

$root     = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
# MinGW must live on an ASCII path: GNU ld cannot resolve its own sysroot files
# when the toolchain sits under a non-ASCII directory (e.g. a Chinese repo path),
# so it installs to the user profile instead of inside the repo.
$mingwBin = Join-Path $env:USERPROFILE 'mingw64\bin'
$tauriDir = Join-Path $root 'app\src-tauri'

foreach ($p in @($cargoBin, $mingwBin)) {
  if (-not (Test-Path $p)) { throw "missing toolchain path: $p  (run install-toolchain.ps1 first)" }
}

$env:Path = "$cargoBin;$mingwBin;$env:Path"
$env:CARGO_HOME  = Join-Path $env:USERPROFILE '.cargo'
$env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'
# Pin the GNU toolchain for this process tree. The project only builds with
# *-pc-windows-gnu (MSVC would need the WebView2 loader as a separate DLL, see
# PROGRESS), but `rustup default` can be pointed at msvc by other projects on
# this machine - then linking fails with `LNK2019 ___chkstk_ms` from sqlite3.o.
# RUSTUP_TOOLCHAIN is inherited by cargo, cargo-tauri and every child cargo run.
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnu'

# GNU binutils (ld, windres) cannot open files under non-ASCII paths: ld fails
# on .o/.rlib inputs, windres fails on the exe icon. The toolchain lives under
# USERPROFILE, so that path matters just as much as the repo root - check both
# and fail fast here instead of letting the linker produce cryptic errors.
foreach ($p in @($root, $env:USERPROFILE)) {
  if ($p -match '[^\x00-\x7F]') {
    throw "path is not ASCII: $p  (GNU ld/windres require an ASCII repo path AND toolchain location)"
  }
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
  Remove-Item $lockFile -Force -EA SilentlyContinue
}
# Atomic take: New-Item without -Force fails when the file already exists, so two
# processes starting together cannot both pass the stale check above (TOCTOU).
try { New-Item -ItemType File -Path $lockFile -Value $PID -ErrorAction Stop | Out-Null }
catch { throw "another build just took the lock: $lockFile" }

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
    'bundle'    {
      # Missing updater signing env vars = tauri-cli waits for a password in a
      # non-interactive session and hangs forever (CPU 0), or produces a setup.exe
      # with no .sig. Fail fast here. NOTE: in PowerShell, "$env:X = ''" DELETES the
      # variable instead of setting an empty value (bitten twice, see RELEASE.md) -
      # and neither Git Bash nor .NET can hand an empty value to a child process.
      # So accept the OTHER documented route: tauri-cli itself falls back to an
      # empty password when its `ci` flag is on (bundle.rs: `or_else(|| if ci {
      # Some("".into()) })`), and that flag is just `env = "CI"`. For an unencrypted
      # key `$env:CI = 'true'` is therefore equivalent to an empty password and
      # works from plain PowerShell. `ci` changes nothing else in the bundler.
      if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
        throw "TAURI_SIGNING_PRIVATE_KEY is not set (path to the signing key). See RELEASE.md step 3"
      }
      if ($null -eq $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -and -not $env:CI) {
        throw "neither TAURI_SIGNING_PRIVATE_KEY_PASSWORD nor CI is set: signing would block on an interactive password prompt. For an unencrypted key set `$env:CI = 'true' (tauri then uses an empty password); for an encrypted one export the password from bash (see RELEASE.md step 3)"
      }
      $relDir = Join-Path $targetRoot 'release'
      # The exe the bundler must be asserted on is the one THIS run produced. A no-op
      # build can skip the final relink: the leftover exe then keeps its old
      # LastWriteTime, so a `-gt $started` filter either picks some other stale
      # binary - and the import-table check below validates the wrong file - or finds
      # nothing and throws "build produced no fresh artifact" right after a
      # SUCCESSFUL build (report P0-3 / G-1). So move every candidate aside first:
      # with the output gone cargo has to relink, and anything found afterwards
      # cannot be a leftover. The pre-build hashes are kept for the note below.
      $stash = Join-Path $targetRoot ('release-pre-bundle-' + $PID)
      $preHash = @{}
      if (Test-Path $relDir) {
        foreach ($f in Get-ChildItem -Path $relDir -Filter '*.exe' -File -EA SilentlyContinue |
                        Where-Object { $_.Name -notmatch '^(cargo|rustc|build-script)' }) {
          $preHash[$f.Name] = (Get-FileHash $f.FullName -Algorithm SHA256).Hash
          New-Item -ItemType Directory -Force -Path $stash | Out-Null
          try { Move-Item $f.FullName (Join-Path $stash $f.Name) -Force }
          catch { throw "cannot move $($f.FullName) out of the way before bundling (is the app still running?): $($_.Exception.Message)" }
          Write-Host "stash $($f.Name) -> $stash (forces a real relink)"
        }
      }
      # PS 5.1 turns ANY native stderr line into a terminating error while
      # $ErrorActionPreference='Stop' is in force - and cargo writes its progress
      # ("Info Looking up installed tauri packages...") to stderr. When the caller
      # drives this script through a pipe or captures its streams (an agent session,
      # CI, `... | Out-String`), that write aborts the script at this line BEFORE the
      # exit code below is read - and the stashed exe is never restored. Relax the
      # preference for the two native calls only: $LASTEXITCODE stays the single
      # source of truth (report P0-3), so a real build failure is still fatal.
      $prevEap = $ErrorActionPreference
      $ErrorActionPreference = 'Continue'
      & cargo tauri build
      # cargo is a NATIVE command: $ErrorActionPreference='Stop' does not catch it, and
      # every later native command (objdump below) OVERWRITES $LASTEXITCODE. Without this
      # capture a failed build printed "ok import table clean" and exited 0 (report P0-3).
      $buildExit = $LASTEXITCODE
      $ErrorActionPreference = $prevEap
      if ($buildExit -ne 0) {
        # Put the previous artifact back: a failed build should not also leave the tree
        # without a usable exe.
        if (Test-Path $stash) {
          foreach ($f in Get-ChildItem -Path $stash -File) { Move-Item $f.FullName (Join-Path $relDir $f.Name) -Force }
          Remove-Item -Recurse -Force $stash -EA SilentlyContinue
          Write-Host "restored the pre-build artifact(s) after the failed build"
        }
        throw "cargo tauri build failed with exit code $buildExit"
      }
      # Deleting the copied DLL is a bet on -L search order - it can lose silently
      # and ship an exe with a runtime DLL dependency this machine happens to have.
      # Assert against the real import table instead (report O-11).
      # Deterministic pick: biggest exe wins, name breaks ties - never LastWriteTime.
      $exePath = Get-ChildItem -Path $relDir -Filter '*.exe' -File -EA SilentlyContinue |
        Where-Object { $_.Name -notmatch '^(cargo|rustc|build-script)' } |
        Sort-Object -Property @{ Expression = { $_.Length }; Descending = $true },
                              @{ Expression = { $_.Name }; Descending = $false } |
        Select-Object -First 1
      if (-not $exePath) { throw "no release exe under $relDir after a successful cargo tauri build (the pre-build copy is kept in $stash)" }
      if ($exePath.LastWriteTime -le $started) {
        throw "$($exePath.Name) was not (re)written by this run even though the pre-build copy was moved aside - refusing to assert on it. Pre-build copy: $stash"
      }
      $newHash = (Get-FileHash $exePath.FullName -Algorithm SHA256).Hash
      if (-not $preHash.ContainsKey($exePath.Name)) {
        Write-Host "ok    $($exePath.Name) is new in this run"
      } elseif ($preHash[$exePath.Name] -eq $newHash) {
        # Byte-identical is expected: same inputs, same linker. Freshness is proved by
        # the stash (the file was gone, so it can only have been recreated by this run).
        Write-Host "note  $($exePath.Name) is byte-identical to the pre-build copy (deterministic relink)"
      } else {
        Write-Host "ok    $($exePath.Name) rebuilt (SHA-256 changed)"
      }
      # The stash is only the safety net for a failed build; the fresh artifact supersedes it.
      Remove-Item -Recurse -Force $stash -EA SilentlyContinue
      $objdump = Join-Path $mingwBin 'objdump.exe'
      if (-not (Test-Path $objdump)) {
        Write-Host "warn  objdump missing - skipped single-exe import check"
      } else {
        # Same PS 5.1 native-stderr trap as cargo above: keep this call from aborting
        # the script when its streams are captured.
        $ErrorActionPreference = 'Continue'
        $bad = & $objdump -p $exePath.FullName 2>$null |
          Select-String '^\s*DLL Name:.*WebView2Loader'
        $ErrorActionPreference = $prevEap
        if ($bad) { throw "single-exe delivery broken: $($exePath.Name) still imports WebView2Loader.dll" }
        Write-Host "ok    import table clean (no WebView2Loader.dll) - $($exePath.Name)"
      }
    }
    default     { throw "unknown task '$Task' (test|check|dev|build|bundle)" }
  }
  if ($LASTEXITCODE -ne 0) { throw "task '$Task' failed with exit code $LASTEXITCODE" }
} finally {
  Remove-Item $lockFile -Force -EA SilentlyContinue
  Pop-Location
}
