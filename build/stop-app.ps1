# Stop any running TokenScope instance. ASCII-only.
Get-Process tokenscope -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 1
$r = Get-Process tokenscope -EA SilentlyContinue
if ($r) { Write-Output 'still running' } else { Write-Output 'app stopped' }
