#!/bin/sh
# 卸载看看客户端（保留数据目录里的登录信息和设置；终端里正在运行的任务不受影响）。
# 数据目录里的程序（versions/、bin/，自动更新装的版本）一起删掉：它们是程序，不是用户数据。
set -eu
if [ "$(id -u)" = 0 ]; then PREFIX=${LOOKLOOK_PREFIX:-/opt/looklook}; else PREFIX=${LOOKLOOK_PREFIX:-$HOME/.local/lib/looklook}; fi
if [ -n "${LOOKLOOK_HOME:-}" ]; then DATA=$LOOKLOOK_HOME; else DATA=${XDG_DATA_HOME:-$HOME/.local/share}/looklook; fi
# 没有 systemd 时（Docker 容器等）手动启动的看看也结束掉（包括从数据目录里的版本运行的）
STOP=$PREFIX/stop.sh
[ -f "$STOP" ] || STOP=$DATA/bin/stop.sh
[ -f "$STOP" ] && sh "$STOP" "$PREFIX" 2>/dev/null || true
if [ "$(id -u)" = 0 ]; then
  systemctl disable --now looklook.service 2>/dev/null || true
  rm -f /etc/systemd/system/looklook.service /usr/local/bin/looklook
  systemctl daemon-reload 2>/dev/null || true
  rm -rf "$PREFIX"
else
  systemctl --user disable --now looklook.service 2>/dev/null || true
  rm -f "$HOME/.config/systemd/user/looklook.service" "$HOME/.local/bin/looklook"
  systemctl --user daemon-reload 2>/dev/null || true
  rm -rf "$PREFIX"
fi
rm -rf "$DATA/versions" "$DATA/bin"
echo "已卸载。登录信息和终端设置保存在 $DATA，如不再需要可以手动删除。"
