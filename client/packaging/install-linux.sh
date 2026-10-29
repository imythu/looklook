#!/bin/sh
# 安装看看客户端（Linux）。
#
# 默认安装给当前用户（不需要 root）：程序在 ~/.local/lib/looklook，命令 ~/.local/bin/looklook，
# 用 systemd 用户服务开机自动运行。终端里的命令以当前用户身份运行。
# 以 root 运行时安装为系统服务（/opt/looklook，looklook.service）。
#
#   ./looklook/install.sh            安装并启动
#   ./looklook/install.sh --no-start 只安装
#
# 没有 systemd 时（Docker 容器、WSL 等）用 nohup 在后台启动；再次运行会先结束旧版本（stop.sh）再启动新版本。
# 这一版同时放进数据目录（LOOKLOOK_HOME，默认 ~/.local/share/looklook）的 versions/，自动更新也装在那里：
# 容器只映射数据目录就能保留程序和更新，重建容器后用 {数据目录}/bin/looklook 启动。
# LOOKLOOK_CONTAINER=1：由 Docker 主机上的一键安装（/install-docker.sh）调用，只安装、不装服务，启动由它负责。
# LOOKLOOK_INSTALL_KEY=LLK-…：装完、启动后用安装密钥登录（云服务器、开机脚本）。
set -eu

SRC=$(cd "$(dirname "$0")" && pwd)
START=1
[ "${1:-}" = "--no-start" ] && START=0
CONTAINER=${LOOKLOOK_CONTAINER:-0}
SYSTEMD=0
[ "$CONTAINER" != 1 ] && command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ] && SYSTEMD=1

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
# 先停掉正在运行的旧版本。
if [ $SYSTEMD = 1 ]; then
  if [ "$(id -u)" = 0 ]; then systemctl stop looklook.service 2>/dev/null || true
  else systemctl --user stop looklook.service 2>/dev/null || true; fi
else
  sh "$SRC/stop.sh" "$PREFIX" 2>/dev/null || true
fi
rm -rf "$PREFIX/fonts"
# 终端服务以前叫 ttyd，现在是 looklook-term：删掉旧文件（还在运行的旧进程由新版启动时清理）
rm -f "$PREFIX/ttyd"
cp -R "$SRC/fonts" "$SRC/LICENSES" "$PREFIX/"
# 程序文件先写临时文件再改名替换：终端里可能还有正在运行的 looklook（例如 looklook ssh），直接覆盖会失败（Text file busy）。
for f in looklook looklook-term; do
  cp "$SRC/$f" "$PREFIX/$f.new" && chmod 0755 "$PREFIX/$f.new" && mv -f "$PREFIX/$f.new" "$PREFIX/$f"
done
# 会话保持程序（looklook-mux）可能正被后台任务使用：不能直接覆盖运行中的文件（Text file busy），先写临时文件再改名替换，
# 正在运行的旧进程不受影响，任务继续运行。
cp "$SRC/looklook-mux" "$PREFIX/looklook-mux.new" && chmod 0755 "$PREFIX/looklook-mux.new" && mv -f "$PREFIX/looklook-mux.new" "$PREFIX/looklook-mux"
# trz / tsz 可能正在运行：删掉再复制（不覆盖运行中的文件）
rm -rf "$PREFIX/trzsz" && { cp -R "$SRC/trzsz" "$PREFIX/" 2>/dev/null || true; }
cp "$SRC/uninstall.sh" "$PREFIX/uninstall.sh" 2>/dev/null || true
cp "$SRC/stop.sh" "$PREFIX/stop.sh" 2>/dev/null && chmod 0755 "$PREFIX/stop.sh" || true
ln -sf "$PREFIX/looklook" "$BIN/looklook"
# 放进数据目录的 versions/<版本>，稳定入口 {数据目录}/bin/looklook 指向它。用户主动安装的版本（包括降级）为准：
# 数据目录里更高的版本一起删掉，否则启动时又会跳过去。LOOKLOOK_NO_VERSIONS：运行刚装的这一版本身，不跳到数据目录里的版本。
if ! LOOKLOOK_NO_VERSIONS=1 "$PREFIX/looklook" install-version "$PREFIX" >/dev/null; then
  say "提示：没能把程序放进数据目录，自动更新后的版本在重建容器后可能会丢失 / Note: couldn't put the program into the data directory"
fi

# 第一次安装时默认允许局域网访问：服务器、NAS 往往没有屏幕，同一网络里的手机、电脑可以直接打开管理台。
# 已经装过的不改（保留用户自己的选择）。容器里不开：端口映射后来源地址都像局域网。
LAN_ON=0
if [ "$CONTAINER" != 1 ] && [ "$("$PREFIX/looklook" access lan on --initial 2>/dev/null)" = enabled ]; then LAN_ON=1; fi

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
if [ "$CONTAINER" = 1 ]; then
  :
elif [ $SYSTEMD = 1 ]; then
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

# 安装密钥（LOOKLOOK_INSTALL_KEY，网站的 install.sh --key 传进来，docs/PLAN_2026-10-27.md §5）：启动后直接登录，
# 云服务器、开机脚本不用到浏览器里批准。looklook login 自己读这个环境变量。
if [ -n "${LOOKLOOK_INSTALL_KEY:-}" ] && [ $START = 1 ]; then
  i=0
  until "$PREFIX/looklook" status >/dev/null 2>&1; do
    i=$((i + 1))
    [ $i -ge 30 ] && break
    sleep 1
  done
  "$PREFIX/looklook" login || say "用安装密钥登录没有成功，稍后可以再运行 / Sign-in with the install key failed; run later: looklook login --key …"
fi

# Docker 主机上的一键安装自己给出后续命令
if [ "$CONTAINER" = 1 ]; then say "✓ 看看客户端已安装 / Looklook client installed"; exit 0; fi

# 在 PATH 里就直接写 looklook，否则写完整路径（复制就能运行）
LL=looklook
case ":$PATH:" in *":$BIN:"*) ;; *) LL=$BIN/looklook; say "提示：把 $BIN 加入 PATH 后可以直接使用 looklook 命令 / Tip: add $BIN to PATH to run looklook directly." ;; esac
say ""
say "✓ 看看客户端已安装，这台电脑上和同一 Wi‑Fi 里不用登录就能用 / Looklook client installed. It works on this computer and the same Wi‑Fi without signing in"
say ""
cmd() { printf '  %s %-8s %s\n' "$LL" "$1" "$2"; }
cmd login "开通远程访问，出门在外也能用手机打开（新用户注册送一年）/ Turn on remote access to open it from your phone anywhere (first year free)"
cmd status "查看状态 / Show status"
cmd restart "重启，终端里的任务不受影响 / Restart; running tasks keep running"
cmd access "局域网访问、白名单 / LAN access and whitelist"
cmd upgrade "升级到最新版本 / Upgrade to the latest version"
cmd -h "全部命令 / All commands"
say ""
say "  通过 SSH 打开管理台 / Open the console over SSH:"
say "    ssh -L 1234:127.0.0.1:1234 <server>  →  http://127.0.0.1:1234/"
if [ $LAN_ON = 1 ]; then
  IP=$(hostname -I 2>/dev/null | awk '{print $1}')
  say ""
  say "  已开启局域网访问：同一网络里的手机、电脑可以直接打开 http://${IP:-<这台电脑的IP>}:1234/"
  say "  方便没有屏幕的服务器和 NAS 使用。不需要的话运行：$LL access lan off"
  say "  LAN access is on: phones and computers on the same network can open http://${IP:-<this-computer-IP>}:1234/"
  say "  Handy for headless servers and NAS. To turn it off: $LL access lan off"
fi
