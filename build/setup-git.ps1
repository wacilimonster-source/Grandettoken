# Initialise the repo, generate an SSH key, and stage the first commit.
# The push itself needs the public key added to GitHub first.
$ErrorActionPreference = 'Stop'

$root   = Split-Path -Parent $PSScriptRoot
$gitExe = Join-Path $PSScriptRoot 'git\cmd\git.exe'
$sshKey = Join-Path $env:USERPROFILE '.ssh\id_ed25519_tokenscope'
$remote = 'git@github.com:wacilimonster-source/Grandettoken.git'

if (-not (Test-Path $gitExe)) { throw "git not found at $gitExe" }

# git needs its own usr/bin on PATH for subcommands and its bundled ssh.
$env:Path = (Join-Path $PSScriptRoot 'git\cmd') + ';' +
            (Join-Path $PSScriptRoot 'git\usr\bin') + ';' +
            (Join-Path $PSScriptRoot 'git\mingw64\bin') + ';' + $env:Path

Set-Location $root

Write-Host "=== git identity ==="
$name  = & $gitExe config --global user.name  2>$null
$email = & $gitExe config --global user.email 2>$null
if (-not $name)  { & $gitExe config --global user.name  'wacil'; Write-Host "set user.name = wacil" }
else { Write-Host "user.name  = $name" }
if (-not $email) { & $gitExe config --global user.email 'wacilimonster-source@users.noreply.github.com'; Write-Host "set user.email" }
else { Write-Host "user.email = $email" }

Write-Host ""
Write-Host "=== init repo ==="
if (Test-Path (Join-Path $root '.git')) {
  Write-Host "already a repo"
} else {
  & $gitExe init -b main | Out-Null
  Write-Host "initialised on branch main"
}

# ── SSH key ──
Write-Host ""
Write-Host "=== ssh key ==="
$sshDir = Join-Path $env:USERPROFILE '.ssh'
New-Item -ItemType Directory -Force -Path $sshDir | Out-Null
if (Test-Path $sshKey) {
  Write-Host "key already exists: $sshKey"
} else {
  $sshKeygen = Join-Path $PSScriptRoot 'git\usr\bin\ssh-keygen.exe'
  & $sshKeygen -t ed25519 -C 'wacilimonster-source@users.noreply.github.com' -f $sshKey -N '""' | Out-Null
  Write-Host "generated: $sshKey"
}

# Point git at this key for github.com
$sshCfg = Join-Path $sshDir 'config'
$entry = @"
Host github.com
  HostName github.com
  User git
  IdentityFile $($sshKey -replace '\\','/')
  IdentitiesOnly yes
"@
if ((Test-Path $sshCfg) -and (Select-String -Path $sshCfg -Pattern 'Host github.com' -Quiet)) {
  Write-Host "~/.ssh/config already has a github.com entry, leaving it alone"
} else {
  Add-Content -Path $sshCfg -Value $entry
  Write-Host "wrote github.com entry to ~/.ssh/config"
}

Write-Host ""
Write-Host "=== remote ==="
$existing = & $gitExe remote 2>$null
if ($existing -contains 'origin') {
  & $gitExe remote set-url origin $remote
  Write-Host "origin updated"
} else {
  & $gitExe remote add origin $remote
  Write-Host "origin added"
}
& $gitExe remote -v

Write-Host ""
Write-Host "=== public key (add this at https://github.com/settings/keys) ==="
Get-Content "$sshKey.pub"
