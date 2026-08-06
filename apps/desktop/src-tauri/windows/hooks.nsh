; lanecho NSIS Installer Hook (Windows)
;
; Background (solution risk table, same as deskmate's): When listening on a TCP/UDP port for the first time, Windows will pop up a prompt.
; If the user accidentally clicks "Cancel" on the firewall authorization prompt, device discovery will fail silently. Register the inbound rule directly during installation.
; Allow rule; clean up on uninstall. Only allow on private/domain networks (do not allow on public networks to reduce exposure).

!macro NSIS_HOOK_POSTINSTALL
  ; First delete any existing old rule with the same name (in reinstallation/upgrade scenarios), then register it.
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="lanecho" dir=in action=allow program="$INSTDIR\lanecho.exe" enable=yes profile=private,domain'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
!macroend
