# Local Android release: unit tests -> signed APK -> signed update manifest -> NW archive
# -> GitHub Release (never Latest) + android-stable pointer -> anonymous download check.
# ASCII-only for Windows PowerShell 5.1. Full procedure: RELEASE.md "Android".
#
#   build/release-android.ps1 -Version 0.1.15 -Stage Build     gates + APK + manifest + archive
#   build/release-android.ps1 -Version 0.1.15 -Stage Publish   tag + release + pointer + verify
#   build/release-android.ps1 -Version 0.1.15 -Stage Release   both
#   build/release-android.ps1 -Version 0.1.15 -Stage Accept    after an installed app updated itself
#   build/release-android.ps1 -Stage Prune [-ApplyLocal <plan>] [-ApplyRemote <plan>]
#
# versionCode = major*10000 + minor*100 + patch, so it only ever grows with the version.
param(
  [string]$Version,
  [ValidateSet('Build', 'Publish', 'Release', 'Accept', 'Prune')][string]$Stage = 'Build',
  [string]$ApplyLocal,
  [string]$ApplyRemote,
  [switch]$WithTags
)

. (Join-Path $PSScriptRoot 'release-common.ps1')
Set-Location $script:Root
$channel = Get-Channel 'android'

if ($Stage -eq 'Prune') { Invoke-Prune -ApplyLocal $ApplyLocal -ApplyRemote $ApplyRemote -WithTags:$WithTags; return }
if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)$') { throw '-Version must look like 0.1.15' }
$code = [int]$Matches[1] * 10000 + [int]$Matches[2] * 100 + [int]$Matches[3]
if ([int]$Matches[2] -gt 99 -or [int]$Matches[3] -gt 99 -or $code -le 0) { throw "version $Version does not map to a monotonic versionCode" }
$tag = $channel.tag.Replace('{version}', $Version)
$apkName = "Grandettoken-android-v$Version.apk"
$publicDir = Join-Path $script:Root 'android\release-public'
$publicKey = Join-Path $publicDir 'manifest-public.txt'
$expectedCert = (Get-Content -Raw (Join-Path $publicDir 'certificate-sha256.txt')).Trim().ToLower()
$downloadUrl = "https://github.com/$($script:Config.distribution)/releases/download/$tag/$apkName"
Write-Host "android $tag versionCode=$code"

function Get-KeystorePassword([string]$Material) {
  if ($env:GRANDETTOKEN_ANDROID_STORE_PASSWORD) { return $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD }
  $file = Join-Path $Material 'release-password.dpapi'
  if (Test-Path -LiteralPath $file) {
    # ConvertFrom-SecureString output: DPAPI under the current Windows user, unreadable elsewhere.
    $secure = (Get-Content -Raw -LiteralPath $file).Trim() | ConvertTo-SecureString
  } else {
    $secure = Read-Host 'Keystore password (input hidden)' -AsSecureString
  }
  $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
  try { return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) }
  finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

function Invoke-Build {
  Write-Step 'preflight'
  $head = Assert-ReleasableHead
  Assert-TagFree $tag $head
  $newest = (Get-NativeText 'git tag' { git tag --list 'android-v*' }) -split "`n" |
    Where-Object { $_ -match '^android-v\d+\.\d+\.\d+$' -and $_ -ne $tag } |
    ForEach-Object { [version]$_.Substring(9) } | Sort-Object -Descending | Select-Object -First 1
  if ($newest -and $newest -ge [version]$Version) { throw "android-v$newest already exists; bump -Version" }
  $stage = Get-StageDir "release-$tag"
  $notes = Join-Path $stage 'release-notes.md'
  Invoke-Store @('notes', $script:Root, 'android', $Version, '--out', $notes)
  foreach ($s in @('check-tracked-artifacts', 'check-encoding')) { Invoke-Native $s { node "build/$s.js" } }
  Invoke-Native 'scan-secrets' { powershell -NoProfile -ExecutionPolicy Bypass -File build/scan-secrets.ps1 }

  Write-Step 'gradle (shared toolchain)'
  $tc = Use-Toolchain $channel.toolchain
  $material = Expand-ConfigPath $channel.material
  $keystore = Join-Path $material 'release.p12'
  if (-not (Test-Path -LiteralPath $keystore)) { throw "keystore missing: $keystore" }
  $env:GRANDETTOKEN_ANDROID_KEYSTORE = $keystore
  $env:GRANDETTOKEN_ANDROID_UPDATE_PUBLIC_KEY = (Get-Content -Raw $publicKey).Trim()
  $env:GRANDETTOKEN_ANDROID_CERT_SHA256 = $expectedCert
  $env:GRANDETTOKEN_ANDROID_VERSION_NAME = $Version
  $env:GRANDETTOKEN_ANDROID_VERSION_CODE = "$code"
  $env:GRANDETTOKEN_ANDROID_KEY_ALIAS = $channel.keyAlias
  $storePass = Get-KeystorePassword $material
  $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD = $storePass
  $env:GRANDETTOKEN_ANDROID_KEY_PASSWORD = $storePass
  Push-Location android
  try {
    Invoke-Native 'gradle test + assembleRelease' { .\gradlew.bat :app:testDebugUnitTest :app:assembleRelease --console=plain }
  } finally {
    Pop-Location
    $storePass = $null
    Remove-Item Env:\GRANDETTOKEN_ANDROID_STORE_PASSWORD, Env:\GRANDETTOKEN_ANDROID_KEY_PASSWORD -ErrorAction SilentlyContinue
  }

  Write-Step 'verify APK'
  $built = Join-Path $script:Root 'android\app\build\outputs\apk\release\app-release.apk'
  $tools = Join-Path $tc.android "build-tools\$($tc.buildTools)"
  $badging = (Get-NativeText 'aapt' { & (Join-Path $tools 'aapt.exe') dump badging $built }) -split "`n" | Select-Object -First 1
  foreach ($want in @("name='$($channel.package)'", "versionCode='$code'", "versionName='$Version'")) {
    if ($badging -notlike "*$want*") { throw "APK badging lacks $want" }
  }
  $certs = Get-NativeText 'apksigner' { & (Join-Path $tools 'apksigner.bat') verify --print-certs $built }
  $cert = @([regex]::Matches($certs, 'certificate SHA-256 digest: ([0-9a-f]+)') | ForEach-Object { $_.Groups[1].Value })
  if ($cert.Count -ne 1 -or $cert[0] -ne $expectedCert) { throw "APK signing certificate mismatch: $($cert -join ',')" }
  $apk = Join-Path $stage $apkName
  Copy-Item -LiteralPath $built -Destination $apk -Force

  Write-Step 'update manifest'
  $payload = Join-Path $stage 'payload.json'
  $signed = Join-Path $stage 'android-latest.json'
  Invoke-Native 'make-android-manifest' { node build/make-android-manifest.mjs --version-name $Version --version-code $code --apk $apk --tag $tag --repo $script:Config.distribution --notes-file $notes --out $payload }
  Invoke-Native 'sign-android-manifest' { python build/sign-android-manifest.py --payload $payload --out $signed --public-key $publicKey }
  Invoke-Native 'verify-android-release (local)' { python build/verify-android-release.py --manifest $signed --public-key $publicKey --apk $apk --expected-version-name $Version --expected-version-code $code --expected-download-url $downloadUrl }
  Invoke-Store @('archive', $script:Root, 'android', $Version, '--commit', $head, '--file', $apk, '--file', $signed, '--file', $notes)
}

function Invoke-Publish {
  Write-Step 'publish'
  Initialize-GitHubAuth
  Invoke-Store @('publish', $script:Root, 'android', $Version)
  Write-Step 'anonymous download check'
  $check = Get-StageDir "release-$tag\published"
  $base = "https://github.com/$($script:Config.distribution)/releases/download"
  Invoke-Native 'download manifest' { curl.exe -fsSL --retry 3 -o (Join-Path $check 'android-latest.json') "$base/android-stable/android-latest.json" }
  Invoke-Native 'download apk' { curl.exe -fsSL --retry 3 -o (Join-Path $check $apkName) $downloadUrl }
  Invoke-Native 'verify-android-release (published)' { python build/verify-android-release.py --manifest (Join-Path $check 'android-latest.json') --public-key $publicKey --apk (Join-Path $check $apkName) --expected-version-name $Version --expected-version-code $code --expected-download-url $downloadUrl }
  Write-Host "next: update an installed older app through its update check, then -Stage Accept"
}

switch ($Stage) {
  'Build' { Invoke-Build }
  'Publish' { Invoke-Publish }
  'Release' { Invoke-Build; Invoke-Publish }
  'Accept' { Invoke-Store @('accept', $script:Root, 'android', $Version); Invoke-Prune }
}
