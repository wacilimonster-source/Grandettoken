# Stop any running app instance. ASCII-only.
# Product was renamed in 0.1.3 (mainBinaryName = Grandettoken); match both names so
# old green builds and the installed app are both covered. Get-Process matches the
# process name WITHOUT the .exe extension.
$names = @('Grandettoken', 'tokenscope')
Get-Process -Name $names -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 1
$r = Get-Process -Name $names -EA SilentlyContinue
if ($r) { Write-Output 'still running' } else { Write-Output 'app stopped' }
