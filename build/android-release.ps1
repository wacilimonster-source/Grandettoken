param(
    [Parameter(Mandatory = $true)][string]$VersionName,
    [Parameter(Mandatory = $true)][int]$VersionCode,
    [string]$Keystore = "$HOME\.android\Grandettoken\release.p12",
    [string]$Alias = "",
    [string]$ManifestUrl = ""
)

$ErrorActionPreference = "Stop"
if ($VersionName -notmatch '^\d+\.\d+\.\d+$') {
    throw "VersionName must look like 1.0.1"
}
if ($VersionCode -le 0) {
    throw "VersionCode must be a positive integer"
}
if (-not (Test-Path $Keystore)) {
    throw "Keystore not found: $Keystore"
}

$material = Split-Path -Parent (Resolve-Path $Keystore).Path
$publicKeyFile = Join-Path $material "manifest-public.txt"
$certFile = Join-Path $material "certificate-sha256.txt"
if (-not (Test-Path $publicKeyFile)) { throw "Missing public key: $publicKeyFile" }
if (-not (Test-Path $certFile)) { throw "Missing cert fingerprint: $certFile" }

$env:GRANDETTOKEN_ANDROID_KEYSTORE = (Resolve-Path $Keystore).Path
$env:GRANDETTOKEN_ANDROID_UPDATE_PUBLIC_KEY = (Get-Content $publicKeyFile -Raw).Trim()
$env:GRANDETTOKEN_ANDROID_CERT_SHA256 = (Get-Content $certFile -Raw).Trim()
$env:GRANDETTOKEN_ANDROID_VERSION_NAME = $VersionName
$env:GRANDETTOKEN_ANDROID_VERSION_CODE = "$VersionCode"
if ($ManifestUrl) {
    $env:GRANDETTOKEN_ANDROID_UPDATE_MANIFEST_URL = $ManifestUrl
}

if (-not $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD) {
    $secure = Read-Host "Keystore password (input hidden)" -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try {
        $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD =
            [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    } finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
    }
}
if (-not $env:GRANDETTOKEN_ANDROID_KEY_PASSWORD) {
    $env:GRANDETTOKEN_ANDROID_KEY_PASSWORD = $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD
}
if (-not $Alias) {
    Write-Host "Key aliases in the keystore (no passwords are printed):"
    & keytool -list -keystore $env:GRANDETTOKEN_ANDROID_KEYSTORE `
        -storepass $env:GRANDETTOKEN_ANDROID_STORE_PASSWORD 2>$null |
        Select-String "Alias name" | ForEach-Object { $_.Line.Trim() }
    $Alias = Read-Host "Key alias"
}
$env:GRANDETTOKEN_ANDROID_KEY_ALIAS = $Alias

try {
    Push-Location (Join-Path $PSScriptRoot "..\android")
    & .\gradlew.bat :app:assembleRelease --console=plain
    if ($LASTEXITCODE -ne 0) { throw "Gradle assembleRelease failed" }
    $apk = Join-Path (Get-Location) "app\build\outputs\apk\release\app-release.apk"
    if (-not (Test-Path $apk)) { throw "APK not found: $apk" }
    Write-Host ""
    Write-Host "Signed APK: $apk"
    Write-Host "VersionName: $VersionName  VersionCode: $VersionCode"
    Write-Host "This script only builds locally. Publishing and manifest signing are separate steps."
} finally {
    Pop-Location
    Remove-Item Env:\GRANDETTOKEN_ANDROID_STORE_PASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:\GRANDETTOKEN_ANDROID_KEY_PASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:\GRANDETTOKEN_ANDROID_KEY_ALIAS -ErrorAction SilentlyContinue
}
