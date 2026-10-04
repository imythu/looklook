#!/bin/sh
# 卸载看看客户端（保留数据目录；终端里正在运行的任务不受影响）。
set -eu
if [ "$(id -u)" = 0 ]; then
  systemctl disable --now looklook.service 2>/dev/null || true
  rm -f /etc/systemd/system/looklook.service /usr/local/bin/looklook
  systemctl daemon-reload 2>/dev/null || true
  rm -rf "${LOOKLOOK_PREFIX:-/opt/looklook}"
else
  systemctl --user disable --now looklook.service 2>/dev/null || true
  rm -f "$HOME/.config/systemd/user/looklook.service" "$HOME/.local/bin/looklook"
  systemctl --user daemon-reload 2>/dev/null || true
  rm -rf "${LOOKLOOK_PREFIX:-$HOME/.local/lib/looklook}"
fi
echo "已卸载。登录信息和终端设置保存在 ~/.local/share/looklook，如不再需要可以手动删除。"
