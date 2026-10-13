# Stop any running app instance. ASCII-only.
# Product was renamed in 0.1.3 (mainBinaryName = Grandettoken); match both names so
# old green builds and the installed app are both covered. Get-Process matches the
# process name WITHOUT the .exe extension.
$names = @('Grandettoken', 'tokenscope')
Get-Process -Name $names -EA SilentlyContinue | Stop-Process -Force -EA SilentlyContinue
Start-Sleep -Seconds 1
$r = Get-Process -Name $names -EA SilentlyContinue
if ($r) { Write-Output 'still running' } else { Write-Output 'app stopped' }
# Callers link files into the release dir right after this; a leftover instance makes
# the linker fail. Exit non-zero so `build.ps1` can notice instead of building on top
# of a running app (report P3-31 - the message alone was easy to ignore).
if ($r) { exit 1 }
