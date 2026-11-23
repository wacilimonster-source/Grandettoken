; Remove the managed config we wrote into external CLI clients before uninstall
; (9A / magpie TakeLeftovers). The exe's --cleanup-inject branch only touches
; those config files and logs failures; it never blocks the uninstall.
; ASCII-only on purpose: makensis reads this file as ANSI.
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\Grandettoken.exe" --cleanup-inject'
!macroend
