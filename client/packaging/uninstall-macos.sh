#!/bin/sh
# 卸载看看客户端（保留数据；终端里正在运行的任务不受影响）。
set -eu
launchctl bootout "gui/$(id -u)/com.looklook.client" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/com.looklook.client.plist"
rm -rf "$HOME/Applications/Looklook"
echo "已卸载。登录信息和终端设置保存在 ~/Library/Application Support/looklook，如不再需要可以手动删除。"
