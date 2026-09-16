# Install PortableGit into the user directory. No admin required, no installer.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$dl   = Join-Path $PSScriptRoot 'dl'
$git  = Join-Path $PSScriptRoot 'git'
New-Item -ItemType Directory -Force -Path $dl | Out-Null

$url = 'https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.5/PortableGit-2.55.0.5-64-bit.7z.exe'
$sfx = Join-Path $dl 'PortableGit.7z.exe'

if (-not (Test-Path (Join-Path $git 'cmd\git.exe'))) {
  if (-not (Test-Path $sfx)) {
    Write-Host "get   $url"
    Invoke-WebRequest -Uri $url -OutFile $sfx -UseBasicParsing -TimeoutSec 900
    Write-Host ("done  {0} MB" -f [math]::Round((Get-Item $sfx).Length / 1MB, 1))
  } else {
    Write-Host "skip  archive already present"
  }

  Write-Host "unpack -> $git"
  New-Item -ItemType Directory -Force -Path $git | Out-Null
  # Self-extracting 7z: -o<dir> -y extracts silently.
  $p = Start-Process -FilePath $sfx -ArgumentList "-o`"$git`"", '-y' -Wait -PassThru -NoNewWindow
  if ($p.ExitCode -ne 0) { throw "extraction failed, exit code $($p.ExitCode)" }
} else {
  Write-Host "skip  git already unpacked"
}

$gitExe = Join-Path $git 'cmd\git.exe'
if (-not (Test-Path $gitExe)) { throw "git.exe not found after extraction: $gitExe" }

Write-Host ""
Write-Host "=== verify ==="
& $gitExe --version
Write-Host ""
Write-Host "GIT_EXE=$gitExe"
