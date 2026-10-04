#!/bin/sh
# 安装看看客户端（macOS，Apple 芯片）。
#
# 安装到 ~/Applications/Looklook，登录系统后自动运行（LaunchAgent），然后在浏览器打开管理台。
# 安装包没有 Apple 签名：这里会移除下载时加上的“隔离”标记，否则系统会拦截运行。
#
# 运行起来后菜单栏（右上角）会有一个托盘图标（详细设计任务 7b），右键可以打开管理台、
# 启动/停止服务、开关开机启动——那个开关操作的就是这里创建的同一个 LaunchAgent（同一个
# Label：com.looklook.client），不会跟安装时装的自启动产生冲突，见 docs/FAQ.md。
set -eu
SRC=$(cd "$(dirname "$0")" && pwd)
APP="$HOME/Applications/Looklook"
LABEL=com.looklook.client
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

echo "安装到 $APP ..."
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
mkdir -p "$APP" "$HOME/Library/LaunchAgents"
rm -rf "$APP/fonts"
# 终端服务以前叫 ttyd，现在是 looklook-term：删掉旧文件（还在运行的旧进程由新版启动时清理）
rm -f "$APP/ttyd"
cp -R "$SRC/looklook" "$SRC/looklook-term" "$SRC/fonts" "$SRC/LICENSES" "$APP/"
# 会话保持程序可能正被后台任务使用：先写临时文件再改名替换，不打断正在运行的任务。
cp "$SRC/looklook-mux" "$APP/looklook-mux.new" && chmod 0755 "$APP/looklook-mux.new" && mv -f "$APP/looklook-mux.new" "$APP/looklook-mux"
# trz / tsz 可能正在运行：删掉再复制（不覆盖运行中的文件）
rm -rf "$APP/trzsz" && { cp -R "$SRC/trzsz" "$APP/" 2>/dev/null || true; }
cp "$SRC/uninstall.sh" "$APP/uninstall.sh" 2>/dev/null || true
chmod 0755 "$APP/looklook" "$APP/looklook-term"
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true

cat >"$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array><string>$APP/looklook</string><string>run</string><string>--no-browser</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/looklook.log</string>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/looklook.log</string>
</dict>
</plist>
PL
launchctl bootstrap "gui/$(id -u)" "$PLIST"

sleep 2
"$APP/looklook" open || true
echo ""
echo "✓ 看看客户端已安装，并会在每次登录 Mac 后自动运行。"
echo "  以后打开管理台：$APP/looklook open"
