; Lanecho NSIS Installer Hook (Windows)
;
; Background (solution risk table, same as deskmate's): When listening on a TCP/UDP port for the first time, Windows will pop up a prompt.
; If the user accidentally clicks "Cancel" on the firewall authorization prompt, device discovery will fail silently. Register the inbound rule directly during installation.
; Allow rule; clean up on uninstall. Only allow on private/domain networks (do not allow on public networks to reduce exposure).

!macro NSIS_HOOK_POSTINSTALL
  ; Remove both the legacy lowercase rule and the current display name before
  ; registering the executable produced by the renamed binary target.
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Lanecho"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Lanecho" dir=in action=allow program="$INSTDIR\Lanecho.exe" enable=yes profile=private,domain'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Lanecho"'
!macroend
