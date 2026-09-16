# Scan the repo for anything resembling a real credential before pushing.
# ASCII-only, same encoding reason as the other scripts.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

$files = Get-ChildItem $root -Recurse -File -EA SilentlyContinue |
  Where-Object {
    $_.FullName -notmatch '\\target\\' -and
    $_.FullName -notmatch '\\dl\\' -and
    $_.FullName -notmatch '\\mingw64\\' -and
    $_.FullName -notmatch '\\node_modules\\' -and
    $_.FullName -notmatch '\\\.git\\' -and
    $_.Extension -in @('.rs', '.js', '.json', '.toml', '.html', '.css', '.ps1', '.md', '.txt', '.yml', '.yaml')
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
  '(?i)token\s*[:=]\s*["''][A-Za-z0-9_-]{24,}'
)

$hits = 0
foreach ($f in $files) {
  $c = Get-Content $f.FullName -Raw -EA SilentlyContinue
  if (-not $c) { continue }
  foreach ($p in $patterns) {
    foreach ($m in [regex]::Matches($c, $p)) {
      $shown = $m.Value
      if ($shown.Length -gt 70) { $shown = $shown.Substring(0, 70) + '...' }
      Write-Host ("HIT  {0}  ::  {1}" -f $f.FullName.Replace($root, ''), $shown)
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
