#!/bin/sh
# 结束正在运行的看看（`looklook run`），给没有 systemd 的环境用（Docker 容器等；有 systemd 时用 systemctl stop）。
# 只结束看看本身：终端里的会话（looklook-mux）和其中的任务继续运行，终端里运行的 `looklook ssh` 等命令也不碰。
# Stops the running Looklook (`looklook run`) where there is no systemd. Terminal sessions keep running.
# 参数：安装目录（默认是这个脚本所在的目录）/ Optional argument: install directory
PREFIX=${1:-$(cd "$(dirname "$0")" && pwd)}
pids=
for d in /proc/[0-9]*; do
  exe=$(readlink "$d/exe" 2>/dev/null) || continue
  # 升级后旧进程的程序文件已被替换：显示为 looklook.old… 或 “(deleted)”
  case $exe in "$PREFIX/looklook" | "$PREFIX/looklook (deleted)" | "$PREFIX"/looklook.old*) ;; *) continue ;; esac
  tr '\0' '\n' <"$d/cmdline" 2>/dev/null | grep -qx run || continue
  pids="$pids ${d#/proc/}"
done
[ -n "$pids" ] || exit 0
# shellcheck disable=SC2086
kill $pids 2>/dev/null || true
i=0
while [ $i -lt 10 ]; do
  alive=
  for p in $pids; do kill -0 "$p" 2>/dev/null && alive="$alive $p"; done
  [ -z "$alive" ] && exit 0
  sleep 1
  i=$((i + 1))
done
# shellcheck disable=SC2086
kill -9 $alive 2>/dev/null || true
