# Check what's available for a git push: git itself, SSH keys, existing config.
$ErrorActionPreference = 'Continue'

Write-Host "=== git ==="
$git = Get-Command git -EA SilentlyContinue
if ($git) { Write-Host ("found: " + $git.Source) } else { Write-Host "not on PATH" }
foreach ($p in @(
  'C:\Program Files\Git\cmd\git.exe',
  'C:\Program Files (x86)\Git\cmd\git.exe',
  (Join-Path $env:LOCALAPPDATA 'Programs\Git\cmd\git.exe')
)) { if (Test-Path $p) { Write-Host ("found: " + $p) } }

Write-Host ""
Write-Host "=== ssh keys (~/.ssh) ==="
$sshDir = Join-Path $env:USERPROFILE '.ssh'
if (Test-Path $sshDir) {
  Get-ChildItem $sshDir | ForEach-Object { Write-Host ("  " + $_.Name) }
} else {
  Write-Host "  no ~/.ssh directory"
}

Write-Host ""
Write-Host "=== ssh client ==="
$ssh = Get-Command ssh -EA SilentlyContinue
if ($ssh) { Write-Host ("found: " + $ssh.Source) } else { Write-Host "no ssh on PATH" }

Write-Host ""
Write-Host "=== existing git config ==="
if (Test-Path (Join-Path $env:USERPROFILE '.gitconfig')) {
  Get-Content (Join-Path $env:USERPROFILE '.gitconfig')
} else {
  Write-Host "no ~/.gitconfig"
}
