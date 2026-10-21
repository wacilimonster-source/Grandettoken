# Scan the repo for anything resembling a real credential before pushing.
# ASCII-only, same encoding reason as the other scripts.
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# ---- key material is also recognised by NAME (report P3-25) ----
# Content patterns alone are structurally blind to this project's own updater
# signing key: a private key copied in as `tokenscope.key.bak`, `signing.txt` or
# `secrets.json` contains no token-shaped string at all, so the old scanner called
# the repo CLEAN and exited 0 - even after `git add -f`, which is exactly the case
# .gitignore cannot prevent. The name check also works when the file cannot be
# read as text (a real key is often binary or UTF-16), so it must not depend on
# the reader below.
$keyExt = @('.key', '.pem', '.pfx', '.p12', '.ppk', '.jks', '.keystore', '.minisign', '.secret')
# A name that mentions credentials can still be legitimate repo source. Exactly two
# files here do, and both sit on the guard side of the fence rather than holding key
# material (relative paths, formatted exactly as the HIT lines below print them):
#   \build\scan-secrets.ps1   this scanner - its own name spells out the word it
#                             looks for, so reporting it would fail every run
#   \app\core\src\secrets.rs  the source module that stores provider keys
# Everything else is reported whatever its extension: key material copied in under
# any name (tokenscope.key.bak, signing.txt, secrets.json, secret.ps1) must not be
# skipped silently, and data-shaped names are where a leak actually ends up.
$nameAllowRel = @('\build\scan-secrets.ps1', '\app\core\src\secrets.rs')

function Get-KeyNameReason([string]$name, [string]$rel) {
  if (-not $name) { return $null }
  if ($nameAllowRel -contains $rel) { return $null }
  # *.pub carries public material only (id_ed25519.pub, minisign.pub): never a leak
  if ($name -match '(?i)\.pub$') { return $null }
  $ext = [System.IO.Path]::GetExtension($name).ToLowerInvariant()
  if ($keyExt -contains $ext) { return "extension $ext is key material" }
  if ($name -match '(?i)\.key(\.[^.]*)?$') { return 'name ends in .key (also covers .key.bak / .key.old)' }
  if ($name -match '(?i)^id_(rsa|dsa|ecdsa|ed25519)$') { return 'OpenSSH private key file name' }
  if ($name -match '(?i)(^|[._-])minisign([._-]|$)') { return 'minisign key file name' }
  if ($name -match '(?i)(secret|signing|private[._-]?key|credential)') { return 'credential-ish file name' }
  return $null
}

# Scan what would actually be pushed (git ls-files), not "text-looking files":
# an extension whitelist is blind to exactly the case that matters - a committed
# .key/.pem that got past .gitignore with `git add -f` (report P3-25). The name
# check above closes that hole on both paths below; a name hit is reported even
# when the file cannot be read (binary/UTF-16 key material).
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
  Write-Host 'warn  git ls-files unavailable - falling back to a recursive walk'
  $files = Get-ChildItem $root -Recurse -File -EA SilentlyContinue |
    Where-Object {
      $rel = $_.FullName.Replace($root, '')
      $_.FullName -notmatch '\\target\\' -and
      $_.FullName -notmatch '\\dl\\' -and
      $_.FullName -notmatch '\\mingw64\\' -and
      $_.FullName -notmatch '\\node_modules\\' -and
      $_.FullName -notmatch '\\\.git\\' -and
      ((Get-KeyNameReason $_.Name $rel) -or
       $_.Extension -in @('.rs', '.js', '.json', '.toml', '.html', '.css', '.ps1', '.md', '.txt', '.yml', '.yaml'))
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
  # key, a JWT, an unquoted `password: P@ss1234`, or a base64 minisign private key
  # (the updater key starts with the base64 of "untrusted comment:").
  'ghp_[A-Za-z0-9]{36}',
  'github_pat_[A-Za-z0-9_]{20,}',
  'AKIA[0-9A-Z]{16}',
  'eyJ[A-Za-z0-9_]{10,}\.eyJ[A-Za-z0-9_]{10,}',
  # A bare minisign private-key marker: the words "secret" and "key" separated by
  # whitespace. Written as \s+ rather than a literal space on purpose - this file
  # has to name the phrase somewhere, and the escaped form keeps the scanner from
  # flagging its own source text when it scans itself.
  '(?i)secret\s+key',
  '(?i)password\s*[:=]\s*["'']?[A-Za-z0-9_@#$%.\-]{12,}'
)

# A raw minisign key file is "untrusted comment: <what it is>" on the first line
# and a base64 body on the second.
$miniShape = '(?m)^untrusted comment:([^\r\n]*)\r?\n([A-Za-z0-9+/=]{40,})'

# ---- known public test fixtures -------------------------------------------
#
# The scanner cannot tell a FIXTURE from a real leak, and this repo deliberately
# contains a few strings shaped exactly like credentials - their whole point is
# to prove the redaction actually works. Without an allowlist every run reports
# them, and a check that always fails is a check nobody reads (alert fatigue is
# worse than no alert).
#
# Allowlist by EXACT STRING, never by file or directory - a file-level skip would
# also hide the real leak sitting next to the fixture. Every entry must say where
# it comes from; adding one means answering "whose is it, and why is it safe".
#
# ASCII-only on purpose: PowerShell 5.1 reads .ps1 as GBK on zh-CN Windows, so a
# UTF-8 Chinese comment here corrupts the parse (hit 2026-10-20 - the script died
# with "unexpected token" and the scan silently stopped checking anything).
$knownFixtures = @(
  # The public example from the jwt.io home page: header {"alg":"HS256"},
  # payload {"sub":"1234567890"}. diaglog.rs uses it to assert the raw token
  # never reaches the digest.
  'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U',
  # A synthetic "very long access token" in diaglog.rs: right shape, invented value.
  'eyJhbGciOiJIUzI1NiJ9.very-long-access-token-value-0123456789',
  # A synthetic query-string secret in fetch.rs (the redaction test): invented value,
  # never sent anywhere. It has to be >= 32 chars to be caught by the length-based
  # redaction rule, so it looks like a credential to the scanner - hence this entry.
  'SUPERSECRETVALUE1234567890ABCDEFGH'
)

# Skip a hit when it is part of one of the known fixtures. Reported, not silent.
#
# Matching is fiddly because the patterns capture only a SLICE, and the slice may
# carry a prefix the fixture does not have:
#   pattern `token = "..."`  -> capture `token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9`
#   pattern `eyJ...\.eyJ...` -> capture `eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIi...`
#   the fixture itself       -> `eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIi....dozjgN...`
# Neither plain containment direction covers the first case (the capture has extra
# text the fixture lacks). So compare on the fixture's DISTINCTIVE HEAD: if the
# capture contains the first N characters of a fixture, it is that fixture's text.
# N is small enough that a JWT header (always `{"alg":...,"typ":"JWT"}`) is well
# inside it, and long enough that a real credential will not accidentally contain it.
$fixtureHead = 24
function Test-KnownFixture([string]$value) {
  $v = $value.Trim()
  foreach ($f in $knownFixtures) {
    $head = $f.Substring(0, [Math]::Min($fixtureHead, $f.Length))
    if ($v.Contains($head) -or $f.Contains($v)) { return $true }
  }
  return $false
}

# The same key as Tauri takes it (TAURI_SIGNING_PRIVATE_KEY): base64 of the WHOLE
# file. Public and private keys share the base64 of "untrusted comment: minisign"
# - which is why an earlier version of this script refused to match that prefix at
# all - so match the rest of the comment, where the two differ:
#   ...IHB1YmxpYyBrZXk6             -> public key; this one IS committed on
#                                      purpose, inside app/src-tauri/tauri.conf.json
#   ...IHNlY3JldCBrZXk              -> minisign private key, unencrypted
#   ...IGVuY3J5cHRlZCBzZWNyZXQga2V5 -> minisign private key, encrypted
# The prefix is assembled from two pieces so this file's own text never contains a
# complete pattern (the scanner would otherwise flag itself).
$miniB64 = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWdu'
$miniB64Keys = @(
  ($miniB64 + 'IGVuY3J5cHRlZCBzZWNyZXQga2V5'),
  ($miniB64 + 'IHNlY3JldCBrZXk')
)

# The scan output must stay identifiable without becoming a second usable copy:
# the old code echoed the whole match (capped at 70 chars - still longer than a
# GitHub PAT, whose pattern is exactly 40), so whatever captured this stdout (CI
# log, terminal scrollback, a pasted bug report) held a cleartext credential.
# Show a 6-character prefix plus the length, and say the value was withheld.
function Get-Preview([string]$value) {
  if (-not $value) { return '<empty>' }
  if ($value.Length -le 8) { return ('<{0} chars, value withheld>' -f $value.Length) }
  return ('{0}... [{1} chars total, value withheld]' -f $value.Substring(0, 6), $value.Length)
}

# Keep the historical "HIT  <path>  ::  <detail>" output format.
function Show-Hit([string]$rel, [string]$detail) {
  Write-Host ("HIT  {0}  ::  {1}" -f $rel, $detail)
}

$hits = 0
$skipped = 0
foreach ($f in $files) {
  # $files may hold FileInfo (whitelist path) or plain strings (git ls-files path)
  $full = if ($f -is [string]) { $f } else { $f.FullName }
  $rel = $full.Replace($root, '')

  # (a) filename/shape check first: it must not depend on readable content.
  $why = Get-KeyNameReason ([System.IO.Path]::GetFileName($full)) $rel
  if ($why) {
    Show-Hit $rel ("file name looks like key material ({0})" -f $why)
    $hits++
  }

  # (b) content checks
  $c = Get-Content $full -Raw -EA SilentlyContinue
  if (-not $c) { continue }
  foreach ($p in $patterns) {
    foreach ($m in [regex]::Matches($c, $p)) {
      if (Test-KnownFixture $m.Value) {
        $skipped++
        continue
      }
      Show-Hit $rel (Get-Preview $m.Value)
      $hits++
    }
  }

  # (c) minisign key material in text form. A minisign PUBLIC key box decodes to
  # 42 bytes; every private-key box is at least 100 (106 unencrypted, more when
  # encrypted). That shape test, plus the comment wording, is what separates a
  # leaked private key from the public key that legitimately lives in
  # tauri.conf.json - and that public key is a base64 blob inside JSON, so it has
  # no "untrusted comment:" LINE for this check to match in the first place.
  foreach ($m in [regex]::Matches($c, $miniShape)) {
    $comment = $m.Groups[1].Value
    $decoded = -1
    try { $decoded = [Convert]::FromBase64String($m.Groups[2].Value).Length } catch { $decoded = -1 }
    if ($decoded -ge 100 -or $comment -match '(?i)secret' -or $comment -match '(?i)private') {
      Show-Hit $rel ("minisign key file (comment '{0}', body {1} bytes decoded)" -f $comment, $decoded)
      $hits++
    }
  }
  foreach ($p in $miniB64Keys) {
    if ($c.Contains($p)) {
      Show-Hit $rel "base64 minisign private key (TAURI_SIGNING_PRIVATE_KEY form, value withheld)"
      $hits++
    }
  }
}

Write-Host ""
# Report skipped fixtures too - never silently. Otherwise the allowlist becomes an
# invisible hole: if someone ever adds a real token to that list, the output looks
# exactly the same.
if ($skipped -gt 0) {
  Write-Host ("skipped {0} known test fixture(s) (see `$knownFixtures in this script)" -f $skipped)
}
if ($hits -eq 0) {
  Write-Host "CLEAN - no credential-like strings found."
} else {
  Write-Host ("FOUND {0} potential credential(s). Review before pushing." -f $hits)
  exit 1
}
