# 官方 statusLine 的原始输入只在内存解析，落盘白名单含指纹和配额。
param([string]$CaptureDirectory = $PSScriptRoot)
$ErrorActionPreference = 'Stop'
try {
    $state = [Console]::In.ReadToEnd() | ConvertFrom-Json
    $email = [string]$state.email
    if ([string]::IsNullOrWhiteSpace($email) -or $null -eq $state.quota) { exit 0 }
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($email.Trim().ToLowerInvariant()))
        $identity = ([BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant().Substring(0, 24)
    } finally { $sha.Dispose() }
    $quota = [ordered]@{}
    foreach ($entry in $state.quota.PSObject.Properties) {
        if ($entry.Name -notmatch '^[a-z0-9][a-z0-9_-]{0,79}$') { continue }
        $value = $entry.Value
        if ($null -eq $value.remaining_fraction -or $value.disabled -eq $true) { continue }
        $fraction = [double]$value.remaining_fraction
        if ([double]::IsNaN($fraction) -or [double]::IsInfinity($fraction) -or $fraction -lt 0 -or $fraction -gt 1) { continue }
        $reset = $null
        $parsed = [DateTimeOffset]::MinValue
        if ([DateTimeOffset]::TryParse([string]$value.reset_time, [ref]$parsed)) { $reset = $parsed.ToUniversalTime().ToString('o') }
        $resetSeconds = $null
        if ($null -ne $value.reset_in_seconds) {
            $seconds = 0.0
            if ([double]::TryParse([string]$value.reset_in_seconds, [ref]$seconds) -and -not [double]::IsNaN($seconds) -and -not [double]::IsInfinity($seconds) -and $seconds -ge 0 -and $seconds -le 31536000) {
                $resetSeconds = [int64][Math]::Round($seconds)
            }
        }
        $quota[$entry.Name] = @{ remaining_fraction = $fraction; reset_time = $reset; reset_in_seconds = $resetSeconds }
    }
    if ($quota.Count -eq 0) { exit 0 }
    $json = @{ account = $identity; quota = $quota } | ConvertTo-Json -Depth 5 -Compress
    $temporary = Join-Path $CaptureDirectory 'account.tmp'
    [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination (Join-Path $CaptureDirectory 'account.json') -Force
    [Console]::Out.Write('Grandettoken')
} catch {
    # 原始回调及异常可能携带邮箱或会话路径，失败交由父进程超时提示。
    exit 1
}
