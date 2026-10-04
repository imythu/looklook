@echo off
rem Looklook installer for Windows. Double-click this file in the extracted "looklook" folder.
rem The PowerShell part below the marker does the work; this header only starts it.
setlocal
set "LL_SELF=%~f0"
powershell -NoProfile -ExecutionPolicy Bypass -Command "$c=[IO.File]::ReadAllText($env:LL_SELF,[Text.Encoding]::UTF8); $i=$c.IndexOf('#'+'PS-BEGIN'); Invoke-Expression $c.Substring($i)"
set "LL_CODE=%ERRORLEVEL%"
echo.
pause
exit /b %LL_CODE%
#PS-BEGIN
# 安装看看客户端（Windows）。双击解压后 looklook 文件夹里的 install.bat 即可（以前是 install.ps1，
# 双击 .ps1 只会弹出“选择打开方式”，所以改成 .bat 外壳 + 内嵌的 PowerShell）。
# 本文件按 UTF-8（无 BOM）+ CRLF 保存：cmd 只解析上面几行纯英文的外壳，下面的中文由 PowerShell 按 UTF-8 读取。
#
# 安装到 %LOCALAPPDATA%\Looklook，创建开始菜单与桌面快捷方式，并在登录 Windows 后自动运行。
# 安装包没有微软签名：首次运行时如果出现“Windows 已保护你的电脑”，点“更多信息”→“仍要运行”。
#
# release 构建没有控制台窗口（详细设计任务 7a），运行后在系统托盘（任务栏右下角）出现一个
# 图标，右键可以打开管理台、启动/停止服务、开关开机启动。这里创建的“启动”文件夹快捷方式是
# 安装时默认打开的开机启动方式；托盘菜单里的“开机启动”开关走的是另一个独立机制（见
# docs/FAQ.md 的“托盘 / 开机启动”一节），两者可能同时存在，不会因此跑出两份看看。
$ErrorActionPreference = 'Stop'
try {
  $src = Split-Path -Parent $env:LL_SELF
  $app = Join-Path $env:LOCALAPPDATA 'Looklook'

  # 在压缩包里直接双击时，资源管理器只把这一个文件解压到临时目录：提示先解压。
  if (-not (Test-Path (Join-Path $src 'looklook.exe'))) {
    Write-Host '✗ 没有找到 looklook.exe。请先把压缩包“全部解压”，再双击解压出来的 install.bat。' -ForegroundColor Red
    Write-Host '  Could not find looklook.exe. Extract the whole zip first, then run install.bat from the extracted folder.'
    exit 1
  }

  Write-Host "安装到 $app ..."
  Get-Process looklook, looklook-term, ttyd -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 500
  New-Item -ItemType Directory -Force -Path $app | Out-Null
  # 终端服务以前叫 ttyd.exe，现在是 looklook-term.exe；安装脚本以前是 .ps1
  foreach ($old in 'ttyd.exe', 'uninstall.ps1') { Remove-Item -Force (Join-Path $app $old) -ErrorAction SilentlyContinue }
  if (Test-Path (Join-Path $app 'fonts')) { Remove-Item -Recurse -Force (Join-Path $app 'fonts') }
  if (Test-Path (Join-Path $app 'trzsz')) { Remove-Item -Recurse -Force (Join-Path $app 'trzsz') -ErrorAction SilentlyContinue }
  # 会话保持程序（psmux.exe）可能正被后台任务使用，不能结束它（任务会丢），也不能直接覆盖运行中的文件：
  # 运行中的 exe 可以挪走，所以先把旧文件挪开，再放新文件。psmux 按运行中程序的文件名认自己的会话服务，
  # 所以旧的 psmux.exe 挪进单独的目录、保留原名（改成 psmux.exe.old 的话，正在跑的会话会被当成已退出，终端打不开）。
  # 1.2.0 里它叫 looklook-mux.exe（改名后无法使用），直接改名挪走。
  Get-ChildItem $app -Filter 'psmux.exe.old*' -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
  $mux = Join-Path $app 'psmux.exe'
  if (Test-Path $mux) {
    $keep = Join-Path $app ('psmux.exe.old' + (Get-Random))
    New-Item -ItemType Directory -Force -Path $keep | Out-Null
    Move-Item -Force $mux (Join-Path $keep 'psmux.exe') -ErrorAction SilentlyContinue
  }
  $legacy = Join-Path $app 'looklook-mux.exe'
  if (Test-Path $legacy) {
    Remove-Item -Force "$legacy.old" -ErrorAction SilentlyContinue
    Rename-Item -Force $legacy 'looklook-mux.exe.old' -ErrorAction SilentlyContinue
  }
  Copy-Item -Force (Join-Path $src 'psmux.exe') $app
  Get-ChildItem $app -Filter '*.exe.old*' -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
  foreach ($item in 'looklook.exe', 'looklook-term.exe', 'fonts', 'trzsz', 'LICENSES', 'uninstall.bat') {
    $p = Join-Path $src $item
    if (Test-Path $p) { Copy-Item -Recurse -Force $p $app }
  }
  # 去掉“从网络下载”的标记，避免每次运行都弹出安全警告。
  Get-ChildItem -Recurse $app | Unblock-File -ErrorAction SilentlyContinue

  $shell = New-Object -ComObject WScript.Shell
  function New-Shortcut($path, $arguments, $style) {
    $s = $shell.CreateShortcut($path)
    $s.TargetPath = Join-Path $app 'looklook.exe'
    $s.Arguments = $arguments
    $s.WorkingDirectory = $app
    $s.WindowStyle = $style
    $s.Description = '看看：随时随地操作你的电脑和 AI 助手'
    $s.Save()
  }
  $programs = [Environment]::GetFolderPath('Programs')
  $startup = [Environment]::GetFolderPath('Startup')
  $desktop = [Environment]::GetFolderPath('Desktop')
  New-Shortcut (Join-Path $programs '看看.lnk') '' 1
  New-Shortcut (Join-Path $desktop '看看.lnk') '' 1
  # 登录后在后台（最小化窗口）运行，不自动打开浏览器。
  New-Shortcut (Join-Path $startup '看看.lnk') 'run --no-browser' 7

  Start-Process -FilePath (Join-Path $app 'looklook.exe') -WorkingDirectory $app
  Write-Host ''
  Write-Host '✓ 看看客户端已安装，桌面上有“看看”图标，登录 Windows 后会自动运行。' -ForegroundColor Green
  Write-Host '  没有窗口：看看在系统托盘（任务栏右下角的小图标，可能要点“^”展开）里运行，右键图标可以打开管理台、暂停/退出。'
  Write-Host '  Looklook is installed and runs in the system tray (bottom-right of the taskbar).'
} catch {
  Write-Host ''
  Write-Host "✗ 安装失败 / Install failed: $($_.Exception.Message)" -ForegroundColor Red
  exit 1
}
