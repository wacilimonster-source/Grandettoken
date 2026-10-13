# Scan the repo for anything resembling a real credential before pushing.
# ASCII-only, same encoding reason as the other scripts.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# Scan what would actually be pushed (git ls-files), not "text-looking files":
# an extension whitelist is blind to exactly the case that matters - a committed
# .key/.pem that got past .gitignore with `git add -f` (report P3-25).
# Unreadable/binary files are skipped by the reader below anyway.
$files = @()
try {
  $tracked = & git -C $root ls-files 2>$null
  if ($LASTEXITCODE -eq 0 -and $tracked) {
    $files = $tracked | ForEach-Object { Join-Path $root $_ } | Where-Object { Test-Path $_ }
  }
} catch {
  $files = @()
}
if (-not $files) {
  Write-Host 'warn  git ls-files unavailable - falling back to extension whitelist'
  $files = Get-ChildItem $root -Recurse -File -EA SilentlyContinue |
    Where-Object {
      $_.FullName -notmatch '\\target\\' -and
      $_.FullName -notmatch '\\dl\\' -and
      $_.FullName -notmatch '\\mingw64\\' -and
      $_.FullName -notmatch '\\node_modules\\' -and
      $_.FullName -notmatch '\\\.git\\' -and
      $_.Extension -in @('.rs', '.js', '.json', '.toml', '.html', '.css', '.ps1', '.md', '.txt', '.yml', '.yaml')
    }
}

Write-Host ("scanning {0} files" -f $files.Count)

# Patterns that would indicate a REAL credential rather than a placeholder.
$patterns = @(
  'sk-[A-Za-z0-9_-]{20,}',
  'sk-ant-[A-Za-z0-9_-]{20,}',
  'Bearer\s+[A-Za-z0-9_.-]{24,}',
  '(?i)api[_-]?key\s*[:=]\s*["'']?[A-Za-z0-9_-]{20,}',
  '(?i)secret\s*[:=]\s*["''][A-Za-z0-9_-]{16,}',
  '(?i)password\s*[:=]\s*["''][^"'']{8,}',
  '(?i)token\s*[:=]\s*["''][A-Za-z0-9_-]{24,}',
  # Gaps listed in report P3-25: none of the above matched a GitHub PAT, an AWS
  # key, a JWT, an unquoted `password: P@ss1234`, or a base64 minisign secret key
  # (the updater key starts with the base64 of "untrusted comment:").
  'ghp_[A-Za-z0-9]{36}',
  'github_pat_[A-Za-z0-9_]{20,}',
  'AKIA[0-9A-Z]{16}',
  'eyJ[A-Za-z0-9_]{10,}\.eyJ[A-Za-z0-9_]{10,}',
  # NOTE: we deliberately do NOT match the base64 of "untrusted comment:" - the
  # updater PUBLIC key in tauri.conf.json starts with exactly that, so the pattern
  # would fire on every run. A committed private key is caught by RELEASE.md's
  # workflow instead (the key file itself is covered by *.key via git ls-files).
  '(?i)password\s*[:=]\s*["'']?[A-Za-z0-9_@#$%.\-]{12,}'
)

$hits = 0
foreach ($f in $files) {
  # $files may hold FileInfo (whitelist path) or plain strings (git ls-files path)
  $full = if ($f -is [string]) { $f } else { $f.FullName }
  $c = Get-Content $full -Raw -EA SilentlyContinue
  if (-not $c) { continue }
  foreach ($p in $patterns) {
    foreach ($m in [regex]::Matches($c, $p)) {
      $shown = $m.Value
      if ($shown.Length -gt 70) { $shown = $shown.Substring(0, 70) + '...' }
      Write-Host ("HIT  {0}  ::  {1}" -f $full.Replace($root, ''), $shown)
      $hits++
    }
  }
}

Write-Host ""
if ($hits -eq 0) {
  Write-Host "CLEAN - no credential-like strings found."
} else {
  Write-Host ("FOUND {0} potential credential(s). Review before pushing." -f $hits)
  exit 1
}
