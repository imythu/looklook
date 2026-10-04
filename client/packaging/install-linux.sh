#!/bin/sh
# 安装看看客户端（Linux）。
#
# 默认安装给当前用户（不需要 root）：程序在 ~/.local/lib/looklook，命令 ~/.local/bin/looklook，
# 用 systemd 用户服务开机自动运行。终端里的命令以当前用户身份运行。
# 以 root 运行时安装为系统服务（/opt/looklook，looklook.service）。
#
#   ./looklook/install.sh            安装并启动
#   ./looklook/install.sh --no-start 只安装
set -eu

SRC=$(cd "$(dirname "$0")" && pwd)
START=1
[ "${1:-}" = "--no-start" ] && START=0

say() { printf '%s\n' "$*"; }

if [ "$(id -u)" = 0 ]; then
  PREFIX=${LOOKLOOK_PREFIX:-/opt/looklook}
  BIN=/usr/local/bin
else
  PREFIX=${LOOKLOOK_PREFIX:-$HOME/.local/lib/looklook}
  BIN=$HOME/.local/bin
fi

say "安装到 / Installing to $PREFIX ..."
mkdir -p "$PREFIX" "$BIN"
# 先停掉正在运行的旧版本，避免替换正在使用的文件。
if [ "$(id -u)" = 0 ]; then systemctl stop looklook.service 2>/dev/null || true
else systemctl --user stop looklook.service 2>/dev/null || true; fi
rm -rf "$PREFIX/fonts"
# 终端服务以前叫 ttyd，现在是 looklook-term：删掉旧文件（还在运行的旧进程由新版启动时清理）
rm -f "$PREFIX/ttyd"
cp -R "$SRC/looklook" "$SRC/looklook-term" "$SRC/fonts" "$SRC/LICENSES" "$PREFIX/"
# 会话保持程序（looklook-mux）可能正被后台任务使用：不能直接覆盖运行中的文件（Text file busy），先写临时文件再改名替换，
# 正在运行的旧进程不受影响，任务继续运行。
cp "$SRC/looklook-mux" "$PREFIX/looklook-mux.new" && chmod 0755 "$PREFIX/looklook-mux.new" && mv -f "$PREFIX/looklook-mux.new" "$PREFIX/looklook-mux"
# trz / tsz 可能正在运行：删掉再复制（不覆盖运行中的文件）
rm -rf "$PREFIX/trzsz" && { cp -R "$SRC/trzsz" "$PREFIX/" 2>/dev/null || true; }
cp "$SRC/uninstall.sh" "$PREFIX/uninstall.sh" 2>/dev/null || true
chmod 0755 "$PREFIX/looklook" "$PREFIX/looklook-term"
ln -sf "$PREFIX/looklook" "$BIN/looklook"

UNIT='[Unit]
Description=看看客户端（Looklook）
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=__EXEC__ run --no-browser
Restart=on-failure
RestartSec=5
# 只结束看看本身；终端里的会话（looklook-mux）（以及其中的任务）在重启、升级时继续运行。
KillMode=process
__USER__
[Install]
WantedBy=__TARGET__
'

# 只有系统确实由 systemd 管理时才安装服务（容器、WSL 等环境里常有 systemctl 但不能用）。
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
  if [ "$(id -u)" = 0 ]; then
    printf '%s' "$UNIT" | sed "s|__EXEC__|$PREFIX/looklook|; s|__USER__|User=root|; s|__TARGET__|multi-user.target|" >/etc/systemd/system/looklook.service
    systemctl daemon-reload || true
    systemctl enable looklook.service >/dev/null 2>&1 || true
    [ $START = 1 ] && { systemctl restart looklook.service || say "启动失败，请查看 / Failed to start, see: journalctl -u looklook"; }
  elif systemctl --user show-environment >/dev/null 2>&1; then
    mkdir -p "$HOME/.config/systemd/user"
    printf '%s' "$UNIT" | sed "s|__EXEC__|$PREFIX/looklook|; s|__USER__||; s|__TARGET__|default.target|" >"$HOME/.config/systemd/user/looklook.service"
    systemctl --user daemon-reload || true
    systemctl --user enable looklook.service >/dev/null 2>&1 || true
    [ $START = 1 ] && { systemctl --user restart looklook.service || say "启动失败，请查看 / Failed to start, see: journalctl --user -u looklook"; }
    # 让服务在没有登录桌面/SSH 时也运行（服务器开机即可远程使用）。
    if ! loginctl show-user "$(id -un)" -p Linger 2>/dev/null | grep -q yes; then
      loginctl enable-linger "$(id -un)" 2>/dev/null || { say "提示：运行 sudo loginctl enable-linger $(id -un) 可以让看看在你没有登录时也保持运行。"; say "Tip: run sudo loginctl enable-linger $(id -un) to keep Looklook running while you're logged out."; }
    fi
  else
    say "没有可用的 systemd 用户服务，请手动启动 / No systemd user service; start it yourself:"
    say "  nohup $PREFIX/looklook run --no-browser >/dev/null 2>&1 &"
  fi
else
  say "没有 systemd，开机后请手动启动 / No systemd; after a reboot start it yourself:"
  say "  nohup $PREFIX/looklook run --no-browser >/dev/null 2>&1 &"
  [ $START = 1 ] && nohup "$PREFIX/looklook" run --no-browser >/dev/null 2>&1 &
fi

# 在 PATH 里就直接写 looklook，否则写完整路径（复制就能运行）
LL=looklook
case ":$PATH:" in *":$BIN:"*) ;; *) LL=$BIN/looklook; say "提示：把 $BIN 加入 PATH 后可以直接使用 looklook 命令 / Tip: add $BIN to PATH to run looklook directly." ;; esac
say ""
say "✓ 看看客户端已安装 / Looklook client installed"
say ""
cmd() { printf '  %s %-8s %s\n' "$LL" "$1" "$2"; }
cmd login "登录，在手机或任意浏览器上批准 / Sign in, approve on your phone or any browser"
cmd status "查看状态 / Show status"
cmd upgrade "升级到最新版本 / Upgrade to the latest version"
cmd -h "全部命令 / All commands"
say ""
say "  通过 SSH 打开管理台 / Open the console over SSH:"
say "    ssh -L 1234:127.0.0.1:1234 <server>  →  http://127.0.0.1:1234/"
