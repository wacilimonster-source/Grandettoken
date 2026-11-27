<#
.SYNOPSIS
  Run native WebView2 CDP verification scripts serially against Grandettoken.

.DESCRIPTION
  Refuses to run if Grandettoken/tokenscope is already running (will never kill
  a user instance). Creates a SHA-256 verified backup of *.db* and *.json files
  directly under %APPDATA%\com.wacil.tokenscope before launching debug CDP,
  runs the serial CDP verification suite, restores all files with byte-for-byte
  SHA-256 verification in finally, and writes logs/summary into OutDir.
#>
param(
  [int]$Port = 9222,
  [string]$Exe = '',
  [string]$Filter = '',
  [string]$OutDir = '',
  [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$defaultScripts = @(
  'verify-settings-entry.js',
  'verify-checkin-center.js',
  'verify-resize.js',
  'verify-qoder-expiry.js',
  'verify-ui-0923.js',
  'verify-focus-ring.js',
  'verify-form.js',
  'verify-gateway-layout.js'
)

function Write-Utf8NoBom {
  param([string]$Path, [string]$Text)
  $dir = Split-Path -Parent $Path
  if ($dir -and -not (Test-Path -LiteralPath $dir)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
  }
  $enc = New-Object System.Text.UTF8Encoding($false)
  [System.IO.File]::WriteAllText($Path, $Text, $enc)
}

function Get-FileSha256 {
  param([string]$Path)
  (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
}

function Get-ProtectedDataFiles {
  param([string]$DataDir)
  if (-not (Test-Path -LiteralPath $DataDir)) {
    return @()
  }
  @(Get-ChildItem -LiteralPath $DataDir -File -ErrorAction Stop | Where-Object {
    $_.Name -like '*.db*' -or $_.Name -like '*.json'
  })
}

function Assert-NoRunningInstance {
  param([scriptblock]$ProcessProbe)
  $procs = @(& $ProcessProbe)
  if ($procs.Count -gt 0) {
    $desc = ($procs | ForEach-Object { "$($_.ProcessName)(pid=$($_.Id))" }) -join ', '
    throw "Refusing to start: Grandettoken is already running ($desc). Close it manually first."
  }
}

function Backup-DataDir {
  param(
    [string]$DataDir,
    [string]$BackupDir
  )
  New-Item -ItemType Directory -Force -Path $BackupDir | Out-Null
  $files = @(Get-ProtectedDataFiles -DataDir $DataDir)
  $entries = @()
  foreach ($f in $files) {
    $origHash = Get-FileSha256 -Path $f.FullName
    $bakPath = Join-Path $BackupDir $f.Name
    Copy-Item -LiteralPath $f.FullName -Destination $bakPath -Force
    $bakHash = Get-FileSha256 -Path $bakPath
    if ($bakHash -ne $origHash) {
      throw "Backup SHA-256 mismatch for $($f.Name): src=$origHash bak=$bakHash"
    }
    $entries += [PSCustomObject]@{
      Name       = $f.Name
      SourcePath = $f.FullName
      BackupPath = $bakPath
      Sha256     = $origHash
    }
  }
  return [PSCustomObject]@{
    DataDir   = $DataDir
    BackupDir = $BackupDir
    Entries   = $entries
  }
}

function Restore-DataDir {
  param([object]$Manifest)
  if ($null -eq $Manifest) { return }
  $dataDir = $Manifest.DataDir
  $backupDir = $Manifest.BackupDir
  $entries = @($Manifest.Entries)

  # Verify backup integrity before modifying DataDir.
  foreach ($e in $entries) {
    if (-not (Test-Path -LiteralPath $e.BackupPath)) {
      throw "Backup file missing during restore: $($e.BackupPath) (backup dir preserved: $backupDir)"
    }
    $bakHash = Get-FileSha256 -Path $e.BackupPath
    if ($bakHash -ne $e.Sha256) {
      throw "Backup SHA-256 mismatch for $($e.Name): want=$($e.Sha256) got=$bakHash (backup dir preserved: $backupDir)"
    }
  }

  if (Test-Path -LiteralPath $dataDir) {
    foreach ($cur in @(Get-ProtectedDataFiles -DataDir $dataDir)) {
      Remove-Item -LiteralPath $cur.FullName -Force
    }
  } elseif ($entries.Count -gt 0) {
    New-Item -ItemType Directory -Force -Path $dataDir | Out-Null
  }

  foreach ($e in $entries) {
    Copy-Item -LiteralPath $e.BackupPath -Destination $e.SourcePath -Force
    $restoredHash = Get-FileSha256 -Path $e.SourcePath
    if ($restoredHash -ne $e.Sha256) {
      throw "Restore SHA-256 mismatch for $($e.Name): want=$($e.Sha256) got=$restoredHash (backup dir preserved: $backupDir)"
    }
  }

  $expectedNames = @{}
  foreach ($e in $entries) { $expectedNames[$e.Name] = $true }
  foreach ($after in @(Get-ProtectedDataFiles -DataDir $dataDir)) {
    if (-not $expectedNames.ContainsKey($after.Name)) {
      throw "Unexpected leftover file after restore: $($after.FullName) (backup dir preserved: $backupDir)"
    }
  }

  if (Test-Path -LiteralPath $backupDir) {
    Remove-Item -LiteralPath $backupDir -Recurse -Force
  }
}

function Wait-CdpTarget {
  param(
    [int]$CdpPort,
    [int]$TimeoutSec = 20
  )
  $url = "http://127.0.0.1:$CdpPort/json/list"
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    $wc = New-Object System.Net.WebClient
    try {
      $wc.Proxy = $null
      $wc.Encoding = [System.Text.Encoding]::UTF8
      $raw = $wc.DownloadString($url)
      $targets = @($raw | ConvertFrom-Json)
      $appPage = $targets | Where-Object {
        $_.type -eq 'page' -and $_.url -and ($_.url -match 'tauri|localhost|127\.0\.0\.1|index\.html')
      } | Select-Object -First 1
      if ($null -ne $appPage) {
        return $true
      }
    } catch {
      # CDP port not listening yet; retry until deadline.
      $null = $_
    } finally {
      $wc.Dispose()
    }
    Start-Sleep -Milliseconds 300
  }
  return $false
}

function Wait-ProcessExitByName {
  param(
    [string[]]$ProcessNames = @('Grandettoken', 'tokenscope'),
    [int]$TimeoutSec = 10
  )
  $procs = @(Get-Process -Name $ProcessNames -ErrorAction SilentlyContinue)
  if ($procs.Count -eq 0) { return }
  $procs | Stop-Process -Force -ErrorAction SilentlyContinue
  $deadline = (Get-Date).AddSeconds($TimeoutSec)
  while ((Get-Date) -lt $deadline) {
    $left = @(Get-Process -Name $ProcessNames -ErrorAction SilentlyContinue)
    if ($left.Count -eq 0) { return }
    Start-Sleep -Milliseconds 200
  }
  throw "Timed out waiting for debug app process exit before DB restore."
}

function Invoke-CheckSuite {
  param(
    [string[]]$ScriptPaths,
    [int]$CdpPort,
    [string]$FilterPattern,
    [string]$OutputDir
  )
  if (-not (Test-Path -LiteralPath $OutputDir)) {
    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
  }

  $results = @()
  $prevNoProxy = $env:NO_PROXY
  $env:NO_PROXY = '127.0.0.1,localhost,::1'
  try {
    foreach ($sp in $ScriptPaths) {
      $name = Split-Path -Leaf $sp
      if ($FilterPattern -and ($name -notlike "*$FilterPattern*")) {
        $results += [PSCustomObject]@{ Script = $name; Status = 'SKIP'; ExitCode = 0; DurationMs = 0; Log = '-' }
        continue
      }
      if (-not (Test-Path -LiteralPath $sp)) {
        $results += [PSCustomObject]@{ Script = $name; Status = 'SKIP'; ExitCode = 0; DurationMs = 0; Log = 'missing' }
        continue
      }

      $logName = ([System.IO.Path]::GetFileNameWithoutExtension($name)) + '.log'
      $logPath = Join-Path $OutputDir $logName
      $sw = [System.Diagnostics.Stopwatch]::StartNew()
      $prevEap = $ErrorActionPreference
      $ErrorActionPreference = 'Continue'
      try {
        $lines = & node $sp $CdpPort 2>&1
        $code = $LASTEXITCODE
      } finally {
        $ErrorActionPreference = $prevEap
      }
      $sw.Stop()

      $text = ($lines | ForEach-Object { $_.ToString() }) -join "`n"
      if ($text.Length -gt 0 -and -not $text.EndsWith("`n")) { $text += "`n" }
      Write-Utf8NoBom -Path $logPath -Text $text

      $status = if ($code -eq 0) { 'PASS' } else { 'FAIL' }
      $results += [PSCustomObject]@{
        Script     = $name
        Status     = $status
        ExitCode   = $code
        DurationMs = [int]$sw.ElapsedMilliseconds
        Log        = $logName
      }
    }
  } finally {
    if ($null -eq $prevNoProxy) {
      Remove-Item Env:NO_PROXY -ErrorAction SilentlyContinue
    } else {
      $env:NO_PROXY = $prevNoProxy
    }
  }

  $passCnt = @($results | Where-Object { $_.Status -eq 'PASS' }).Count
  $failCnt = @($results | Where-Object { $_.Status -eq 'FAIL' }).Count
  $skipCnt = @($results | Where-Object { $_.Status -eq 'SKIP' }).Count

  $sb = New-Object System.Text.StringBuilder
  [void]$sb.AppendLine(("Native CDP Check Summary (port={0}, out={1})" -f $CdpPort, $OutputDir))
  [void]$sb.AppendLine(("{0,-30} {1,-6} {2,6} {3,9}  {4}" -f 'Script', 'Status', 'Code', 'Time(ms)', 'Log'))
  [void]$sb.AppendLine(('-' * 72))
  foreach ($r in $results) {
    [void]$sb.AppendLine(("{0,-30} {1,-6} {2,6} {3,9}  {4}" -f $r.Script, $r.Status, $r.ExitCode, $r.DurationMs, $r.Log))
  }
  [void]$sb.AppendLine(('-' * 72))
  [void]$sb.AppendLine(("Totals: PASS={0} FAIL={1} SKIP={2}" -f $passCnt, $failCnt, $skipCnt))

  $summaryText = $sb.ToString()
  Write-Utf8NoBom -Path (Join-Path $OutputDir 'summary.txt') -Text $summaryText
  Write-Host $summaryText

  return [PSCustomObject]@{
    Pass     = $passCnt
    Fail     = $failCnt
    Skip     = $skipCnt
    Results  = $results
    ExitCode = $(if ($failCnt -gt 0) { 1 } else { 0 })
  }
}

function Invoke-SelfTest {
  Write-Host 'run-native-checks.ps1 SelfTest:'
  $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("gt-native-selftest-" + $PID + "-" + [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
  New-Item -ItemType Directory -Force -Path $tmpRoot | Out-Null
  try {
    # 1. Unreachable CDP port must fail cleanly within short timeout.
    $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $unusedPort = ($listener.LocalEndpoint).Port
    $listener.Stop()
    $reachable = Wait-CdpTarget -CdpPort $unusedPort -TimeoutSec 1
    if ($reachable) { throw 'SelfTest failed: Wait-CdpTarget returned true on closed port' }
    Write-Host '  OK unreachable CDP port rejected cleanly'

    # 2. Running-instance guard must refuse an active process and pass when none.
    Assert-NoRunningInstance -ProcessProbe { @() }
    $dummyProc = Start-Process -FilePath 'node' -ArgumentList '-e', 'setTimeout(() => {}, 15000)' -PassThru -WindowStyle Hidden
    try {
      $threw = $false
      try {
        Assert-NoRunningInstance -ProcessProbe { @(Get-Process -Id $dummyProc.Id -ErrorAction SilentlyContinue) }
      } catch {
        $threw = $true
      }
      if (-not $threw) { throw 'SelfTest failed: Assert-NoRunningInstance did not refuse active process' }
    } finally {
      Stop-Process -Id $dummyProc.Id -Force -ErrorAction SilentlyContinue
      Wait-Process -Id $dummyProc.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
    Write-Host '  OK running-instance guard refuses active process'

    # 3. Backup & SHA-256 restore handles mutation, deletion, stray files, and tamper detection.
    $simData = Join-Path $tmpRoot 'sim-data'
    $simBak = Join-Path $tmpRoot 'sim-bak'
    New-Item -ItemType Directory -Force -Path $simData | Out-Null
    $dbFile = Join-Path $simData 'tokenscope.db'
    $walFile = Join-Path $simData 'tokenscope.db-wal'
    $jsonFile = Join-Path $simData 'config.json'
    $keepFile = Join-Path $simData 'diag.log'
    [System.IO.File]::WriteAllBytes($dbFile, [byte[]](1..128))
    [System.IO.File]::WriteAllBytes($walFile, [byte[]](10..50))
    Write-Utf8NoBom -Path $jsonFile -Text '{"ok":true}'
    Write-Utf8NoBom -Path $keepFile -Text 'keep-me'
    $origDbHash = Get-FileSha256 -Path $dbFile
    $origWalHash = Get-FileSha256 -Path $walFile
    $origJsonHash = Get-FileSha256 -Path $jsonFile
    $origKeepHash = Get-FileSha256 -Path $keepFile

    $manifest = Backup-DataDir -DataDir $simData -BackupDir $simBak
    [System.IO.File]::WriteAllBytes($dbFile, [byte[]](255, 0, 1))
    Remove-Item -LiteralPath $jsonFile -Force
    Write-Utf8NoBom -Path (Join-Path $simData 'stray.db-wal') -Text 'mutated-wal'
    Write-Utf8NoBom -Path (Join-Path $simData 'stray.json') -Text '{}'
    Restore-DataDir -Manifest $manifest

    if ((Get-FileSha256 -Path $dbFile) -ne $origDbHash) { throw 'SelfTest failed: db hash mismatch' }
    if ((Get-FileSha256 -Path $walFile) -ne $origWalHash) { throw 'SelfTest failed: wal hash mismatch' }
    if ((Get-FileSha256 -Path $jsonFile) -ne $origJsonHash) { throw 'SelfTest failed: json hash mismatch' }
    if ((Get-FileSha256 -Path $keepFile) -ne $origKeepHash) { throw 'SelfTest failed: unrelated file modified' }
    if (Test-Path -LiteralPath (Join-Path $simData 'stray.db-wal')) { throw 'SelfTest failed: stray db-wal left behind' }
    if (Test-Path -LiteralPath (Join-Path $simData 'stray.json')) { throw 'SelfTest failed: stray json left behind' }
    if (Test-Path -LiteralPath $simBak) { throw 'SelfTest failed: backup dir not cleaned after verified restore' }

    # Tampered backup must fail loudly and keep backup dir intact.
    $simBakBad = Join-Path $tmpRoot 'sim-bak-bad'
    $badManifest = Backup-DataDir -DataDir $simData -BackupDir $simBakBad
    Write-Utf8NoBom -Path (Join-Path $simBakBad 'config.json') -Text '{"tampered":true}'
    $tamperThrew = $false
    try {
      Restore-DataDir -Manifest $badManifest
    } catch {
      $tamperThrew = $true
    }
    if (-not $tamperThrew) { throw 'SelfTest failed: tampered backup did not fail' }
    if (-not (Test-Path -LiteralPath $simBakBad)) { throw 'SelfTest failed: backup dir deleted despite mismatch' }
    Write-Host '  OK isolated SHA-256 backup/restore & tamper detection'

    # 4. Simulated serial runner: PASS suite returns 0; suite with FAIL script returns 1.
    $passJs = Join-Path $tmpRoot 'verify-sim-pass.js'
    $failJs = Join-Path $tmpRoot 'verify-sim-fail.js'
    $skipJs = Join-Path $tmpRoot 'verify-sim-skip.js'
    Write-Utf8NoBom -Path $passJs -Text "console.log('sim-pass'); process.exit(0);"
    Write-Utf8NoBom -Path $failJs -Text "console.error('sim-fail'); process.exit(1);"
    Write-Utf8NoBom -Path $skipJs -Text "process.exit(0);"

    $outPass = Join-Path $tmpRoot 'out-pass'
    $r1 = Invoke-CheckSuite -ScriptPaths @($passJs, $skipJs) -CdpPort 9222 -FilterPattern 'sim-pass' -OutputDir $outPass
    if ($r1.ExitCode -ne 0 -or $r1.Pass -ne 1 -or $r1.Skip -ne 1 -or $r1.Fail -ne 0) {
      throw "SelfTest failed: pass suite returned Pass=$($r1.Pass) Fail=$($r1.Fail) Skip=$($r1.Skip) Exit=$($r1.ExitCode)"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $outPass 'summary.txt'))) { throw 'SelfTest failed: missing summary.txt' }

    $outFail = Join-Path $tmpRoot 'out-fail'
    $r2 = Invoke-CheckSuite -ScriptPaths @($passJs, $failJs) -CdpPort 9222 -FilterPattern '' -OutputDir $outFail
    if ($r2.ExitCode -ne 1 -or $r2.Pass -ne 1 -or $r2.Fail -ne 1) {
      throw "SelfTest failed: fail suite returned Pass=$($r2.Pass) Fail=$($r2.Fail) Exit=$($r2.ExitCode)"
    }
    Write-Host '  OK serial runner PASS(0) and FAIL(1) exit paths'
    Write-Host 'run-native-checks.ps1 SelfTest PASS'
  } finally {
    Remove-Item -LiteralPath $tmpRoot -Recurse -Force -ErrorAction SilentlyContinue
  }
}

if ($SelfTest) {
  Invoke-SelfTest
  exit 0
}

# Live run: refuse if Grandettoken/tokenscope is already running BEFORE touching any data or calling launch-debug.
Assert-NoRunningInstance -ProcessProbe {
  @(Get-Process -Name @('Grandettoken', 'tokenscope') -ErrorAction SilentlyContinue)
}

if (-not $OutDir) {
  $OutDir = Join-Path $repoRoot ("build\qa\native-" + (Get-Date -Format 'yyyy-MM-dd'))
}

$dataDir = Join-Path $env:APPDATA 'com.wacil.tokenscope'
$backupDir = Join-Path ([System.IO.Path]::GetTempPath()) ("tokenscope-native-bak-" + $PID + "-" + [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
$manifest = Backup-DataDir -DataDir $dataDir -BackupDir $backupDir
Write-Host ("Backed up {0} file(s) from {1} -> {2}" -f @($manifest.Entries).Count, $dataDir, $backupDir)

$suiteExit = 1
try {
  $launchArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $PSScriptRoot 'launch-debug.ps1'), '-Port', $Port)
  if ($Exe) {
    $launchArgs += @('-Exe', $Exe)
  }
  & powershell @launchArgs
  if ($LASTEXITCODE -ne 0) {
    throw "launch-debug.ps1 failed with exit code $LASTEXITCODE"
  }

  if (-not (Wait-CdpTarget -CdpPort $Port -TimeoutSec 20)) {
    throw "CDP endpoint http://127.0.0.1:$Port/json/list did not expose an app page within 20s"
  }

  $appProcesses = @(Get-Process -Name @('Grandettoken', 'tokenscope') -ErrorAction SilentlyContinue)
  if ($appProcesses.Count -ne 1) { throw "expected exactly one debug app process" }
  $previousAppPid = $env:CDP_APP_PID
  try {
    $env:CDP_APP_PID = [string]$appProcesses[0].Id
    $scriptPaths = @($defaultScripts | ForEach-Object { Join-Path $PSScriptRoot $_ })
    $run = Invoke-CheckSuite -ScriptPaths $scriptPaths -CdpPort $Port -FilterPattern $Filter -OutputDir $OutDir
    $suiteExit = $run.ExitCode
  } finally {
    $env:CDP_APP_PID = $previousAppPid
  }
} finally {
  Wait-ProcessExitByName -ProcessNames @('Grandettoken', 'tokenscope') -TimeoutSec 10
  Restore-DataDir -Manifest $manifest
  Write-Host ("Restored & SHA-256 verified {0} file(s) back to {1}" -f @($manifest.Entries).Count, $dataDir)
}

exit $suiteExit
