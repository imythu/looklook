@echo off
rem Looklook uninstaller for Windows. Double-click %LOCALAPPDATA%\Looklook\uninstall.bat.
setlocal
set "LL_SELF=%~f0"
rem Run from a copy in %TEMP%: this file lives in the folder that is about to be deleted.
if /i not "%~dp0"=="%TEMP%\" (
  copy /y "%~f0" "%TEMP%\looklook-uninstall.bat" >nul
  set "LL_SELF=%TEMP%\looklook-uninstall.bat"
)
powershell -NoProfile -ExecutionPolicy Bypass -Command "$c=[IO.File]::ReadAllText($env:LL_SELF,[Text.Encoding]::UTF8); $i=$c.IndexOf('#'+'PS-BEGIN'); Invoke-Expression $c.Substring($i)"
echo.
pause
exit /b 0
#PS-BEGIN
# 卸载看看客户端（保留数据：%APPDATA%\looklook）。按 UTF-8（无 BOM）+ CRLF 保存，见 install.bat 开头的说明。
$ErrorActionPreference = 'SilentlyContinue'
Get-Process looklook, looklook-term, looklook-mux, ttyd | Stop-Process -Force
# 会话保持程序叫 psmux.exe：只结束看看安装目录里的，不碰用户自己装的 psmux
$app = Join-Path $env:LOCALAPPDATA 'Looklook'
Get-Process psmux | Where-Object { $_.Path -and $_.Path.StartsWith($app, [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force
Start-Sleep -Milliseconds 500
foreach ($dir in [Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Startup'), [Environment]::GetFolderPath('Desktop')) {
  Remove-Item (Join-Path $dir '看看.lnk')
}
Remove-Item (Join-Path ([Environment]::GetFolderPath('Startup')) 'looklook-tray-autostart.cmd')
Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'Looklook')
Write-Host '已卸载。登录信息和终端设置保存在 %APPDATA%\looklook，如不再需要可以手动删除。'
Write-Host 'Uninstalled. Your sign-in and terminal settings stay in %APPDATA%\looklook; delete it if you no longer need it.'
