; lanecho NSIS 安装器钩子(Windows)
;
; 背景(方案风险表, deskmate 同款): 首次监听 TCP/UDP 端口时 Windows 会弹
; 防火墙授权, 用户误点"取消"将导致设备发现静默失败。安装时直接注册入站
; 放行规则, 卸载时清理。仅放行专用/域网络(public 网络不放行, 降低暴露面)。

!macro NSIS_HOOK_POSTINSTALL
  ; 先删除可能存在的同名旧规则(重装/升级场景), 再注册
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="lanecho" dir=in action=allow program="$INSTDIR\lanecho.exe" enable=yes profile=private,domain'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="lanecho"'
!macroend
