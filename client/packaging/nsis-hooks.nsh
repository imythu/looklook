; 看看客户端 Windows 安装包（Tauri NSIS）的附加步骤：开机启动与旧版快捷方式。
; 文件要保存成带 BOM 的 UTF-8，NSIS 才会按 Unicode 读取里面的中文。
;
; - 安装目录与旧的 install.bat 相同（%LOCALAPPDATA%\Looklook），从旧版升级时原地覆盖。
; - 旧版脚本建的“看看.lnk”（开始菜单、桌面）换成安装包自己的“Looklook”快捷方式。
; - 登录 Windows 后自动运行（只启动服务和托盘，不弹窗口）；托盘菜单“开机启动”可以关掉。

!macro NSIS_HOOK_POSTINSTALL
  Delete "$SMPROGRAMS\看看.lnk"
  Delete "$DESKTOP\看看.lnk"
  CreateShortCut "$SMSTARTUP\看看.lnk" "$INSTDIR\looklook.exe" "run --no-browser" "$INSTDIR\looklook.exe" 0 SW_SHOWMINIMIZED
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$SMSTARTUP\看看.lnk"
  Delete "$SMSTARTUP\looklook-tray-autostart.cmd"
!macroend
