#!/usr/bin/env bash
# 打包发布：为每个平台生成安装包，放在 dist/。
#
#   scripts/package.sh                       全部平台
#   scripts/package.sh linux-x86_64 ...      指定平台：linux-x86_64 linux-aarch64 darwin-aarch64 windows-x86_64
#
# 安装包内容（顶层目录 looklook/）：looklook 可执行文件、trzsz/（trz、tsz；Windows 上是顶层的 trz.exe、tsz.exe）、ttyd 1.7.7（改名为 looklook-term）、会话保持程序（tmux 改名为 looklook-mux；Windows 上的 psmux 保留原名 psmux.exe，见 src/paths.rs 的 MUX_EXE）、终端字体、许可证、安装脚本。
# Linux 包可直接用看看网页“下载”页的一行命令安装：curl -fsSL <地址> | tar -xz && ./looklook/install.sh
#
# Windows / macOS 另外生成桌面应用安装程序（Tauri，见 src/desktop.rs 与 tauri.conf.json），给普通用户双击安装：
#   looklook-{版本}-windows-x86_64-setup.exe   NSIS 安装程序（装到 %LOCALAPPDATA%\Looklook，开始菜单、登录自启）
#   looklook-{版本}-darwin-aarch64.dmg         拖进“应用程序”的 Looklook.app（ad-hoc 签名，没有 Apple 公证）
# 压缩包里的 looklook 就是同一个桌面应用程序，仍然用于自动更新（src/updater.rs 只认 zip / tar.gz）。
# 额外需要：Tauri CLI（npm ci 装的 @tauri-apps/cli，或 cargo-tauri）；在 Linux 上打 Windows 包还要
# x86_64-w64-mingw32-windres（binutils-mingw-w64-x86-64，嵌入图标与清单；预处理用 zig，见 scripts/windres-zig.sh）和 makensis（nsis）；
# Tauri CLI 在 Linux 上还会用 pkg-config 查 appindicator（libayatana-appindicator3-dev），即使目标是 Windows。
#
# 需要：Rust（rustup 目标见下）、cargo-zigbuild、zig 0.15、Node.js（构建管理台页面）、python3 + fontTools（生成字体）。
# 发布构建可设置 LOOKLOOK_SERVER_URL（默认服务端）与 LOOKLOOK_PLATFORM_KEYS（内置平台公钥，p1=base64url），
# 它们在编译时会被异或混淆写进可执行文件（build.rs + src/hardening.rs，详细设计任务 3；只是提高
# 静态分析成本，不是加密，见 docs/FAQ.md）。
# 可选加固（任务 3）：LOOKLOOK_PACK=1 额外用 upx 压一遍可执行文件（没装 upx 会跳过并警告；
# 默认不压 macOS 二进制，会影响代码签名，见 pack_upx() 和 docs/FAQ.md）。
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ALL=(linux-x86_64 linux-aarch64 darwin-aarch64 windows-x86_64)
declare -A TRIPLE=(
  [linux-x86_64]=x86_64-unknown-linux-musl
  [linux-aarch64]=aarch64-unknown-linux-musl
  [darwin-aarch64]=aarch64-apple-darwin
  [windows-x86_64]=x86_64-pc-windows-gnu
)
[[ -d /opt/zig/zig-x86_64-linux-0.15.2 ]] && export PATH="/opt/zig/zig-x86_64-linux-0.15.2:$PATH"
if [[ $# -eq 0 ]]; then set -- "${ALL[@]}"; fi

if [[ ${SKIP_UI:-0} != 1 ]]; then
  echo "=== 构建管理台页面" >&2
  [[ -d node_modules ]] || npm ci --no-audit --no-fund
  npm run check:locales
  npm run build
fi
scripts/fetch-vendor.sh fonts "$@"

DIST=dist
rm -rf "$DIST/stage"
mkdir -p "$DIST/stage"

licenses() { # licenses <目录> <平台>
  local d=$1/LICENSES t=$2
  mkdir -p "$d"
  tar xzf .cache/downloads/ttyd-1.7.7-src.tar.gz -O ttyd-1.7.7/LICENSE >"$d/ttyd-LICENSE.txt"
  cp vendor/fonts/LICENSE-*.txt "$d/"
  if [[ $t == windows-* ]]; then
    python3 -c 'import sys,zipfile; sys.stdout.buffer.write(zipfile.ZipFile(sys.argv[1]).read("LICENSE"))' .cache/downloads/psmux-v3.3.8-windows-x64.zip >"$d/psmux-LICENSE.txt"
  else
    tar xzf .cache/downloads/tmux-3.7c.tar.gz -O tmux-3.7c/COPYING >"$d/tmux-LICENSE.txt"
    tar xzf .cache/downloads/libevent-2.1.12-stable.tar.gz -O libevent-2.1.12-stable/LICENSE >"$d/libevent-LICENSE.txt"
    tar xzf .cache/downloads/ncurses-6.5.tar.gz -O ncurses-6.5/COPYING >"$d/ncurses-LICENSE.txt"
  fi
  local rathole
  rathole=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d -name 'rathole-0.5.*' | head -1)
  [[ -n $rathole ]] && cp "$rathole/LICENSE" "$d/rathole-LICENSE.txt"
  cp .cache/downloads/trzsz-1.2.0-LICENSE "$d/trzsz-LICENSE.txt"
  cat >"$d/THIRD-PARTY.md" <<'EOF'
# 第三方组件

| 组件 | 用途 | 许可证 |
| --- | --- | --- |
| [ttyd](https://github.com/tsl0922/ttyd) 1.7.7 | 浏览器终端 | MIT（ttyd-LICENSE.txt） |
| libwebsockets、libuv、json-c、zlib | ttyd 依赖（静态链接） | MIT / zlib |
| Mbed TLS | ttyd 依赖（静态链接） | Apache-2.0 |
| [tmux](https://github.com/tmux/tmux) 3.7c（Linux / macOS，改名 looklook-mux） | 让终端里的任务在关掉网页后继续运行 | ISC（tmux-LICENSE.txt） |
| libevent、ncurses | tmux 依赖（静态链接） | BSD-3-Clause / X11（libevent-LICENSE.txt、ncurses-LICENSE.txt） |
| [psmux](https://github.com/psmux/psmux) 3.3.8（Windows，psmux.exe） | 同上（兼容 tmux 命令的 Windows 原生程序） | MIT（psmux-LICENSE.txt） |
| [rathole](https://github.com/rathole-org/rathole) 0.5 | 远程访问通道（编入 looklook） | Apache-2.0（rathole-LICENSE.txt） |
| [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) 2.304 | 终端英文字体 | SIL OFL 1.1 |
| [霞鹜文楷等宽 LXGW WenKai Mono](https://github.com/lxgw/LxgwWenKai) 1.522 | 终端中文字体（按字频分片） | SIL OFL 1.1 |
| [trzsz-go](https://github.com/trzsz/trzsz-go) 1.2.0（trzsz/trz、tsz） | 终端里上传下载文件（也能经 SSH） | MIT（trzsz-LICENSE.txt） |
EOF
}

# Windows 安装脚本是 .bat 外壳 + 内嵌 PowerShell（双击就能运行，.ps1 双击只会问用什么程序打开）：
# cmd 要求 CRLF，且文件开头不能有 BOM（否则第一行 @echo off 会报错）；内嵌部分由 PowerShell 按 UTF-8 读取。
bat() { sed 's/\r$//; s/$/\r/' "$1" >"$2"; }

# 可选的 UPX 压缩壳（详细设计任务 3），默认关闭：设置 LOOKLOOK_PACK=1 打开。
# - 没装 upx：打印警告直接跳过，不算打包失败。
# - macOS 二进制默认不压：UPX 压完的 Mach-O 会破坏后续代码签名（`codesign` 校验失败/
#   Gatekeeper 拒绝），除非你清楚自己在干什么并且确认不需要签名，见 docs/FAQ.md。
#   真要在 macOS 上压，设置 LOOKLOOK_PACK_MACOS=1（和 LOOKLOOK_PACK=1 一起）强制打开。
# - 诚实说明：压完更小、反汇编更麻烦一点，仅此而已；不是防破解手段。而且 UPX 壳本身
#   是不少真实恶意软件也用的手法，容易被杀毒软件/SmartScreen 误报，见 FAQ 里的说明。
pack_upx() { # pack_upx <平台> <可执行文件路径>
  [[ ${LOOKLOOK_PACK:-0} == 1 ]] || return 0
  if [[ $1 == darwin-* && ${LOOKLOOK_PACK_MACOS:-0} != 1 ]]; then
    echo "  （跳过 UPX：macOS 二进制默认不压，会影响代码签名；要强制压缩设置 LOOKLOOK_PACK_MACOS=1）" >&2
    return 0
  fi
  if ! command -v upx >/dev/null 2>&1; then
    echo "  （跳过 UPX：没有装 upx，设置 LOOKLOOK_PACK=1 打包前请先安装 upx）" >&2
    return 0
  fi
  echo "  === upx 压缩 $2" >&2
  upx --best "$2" || echo "  upx 压缩失败，继续用未压缩的可执行文件" >&2
}

tauri_cli() {
  if [[ -x node_modules/.bin/tauri ]]; then node_modules/.bin/tauri "$@"
  elif command -v cargo-tauri >/dev/null 2>&1; then cargo tauri "$@"
  else echo "缺少 Tauri CLI：先 npm ci（@tauri-apps/cli）或 cargo install tauri-cli" >&2; return 1
  fi
}

# 桌面应用（Windows / macOS）：编译程序并生成安装程序。随包资源取自已经整理好的 stage 目录，
# 只在打包时用 --config 传给 Tauri（tauri.conf.json 里不写：普通 cargo build 时还没有这些文件）。
desktop() { # desktop <平台> <三元组> <stage 目录>
  local t=$1 triple=$2 stage=$3 conf bundles runner=()
  conf=$(realpath "$DIST/stage/$t")/tauri.resources.json
  python3 - "$stage" "$conf" <<'PY'
import json, os, sys
stage, out = sys.argv[1], sys.argv[2]
skip = {"looklook", "looklook.exe", "install.sh", "uninstall.sh", "install.bat", "uninstall.bat"}
res = {os.path.realpath(os.path.join(stage, n)): n for n in sorted(os.listdir(stage)) if n not in skip}
json.dump({"bundle": {"resources": res}}, open(out, "w"), ensure_ascii=False, indent=2)
PY
  if [[ $t == windows-* ]]; then
    bundles=nsis
    # Tauri CLI 调 `<runner> build …`；cargo-zigbuild 的子命令叫 zigbuild，用小脚本转一下
    runner=(--runner "$PWD/scripts/zigbuild-runner.sh")
    # windres 要 C 预处理器；没装 mingw 的 gcc 时用 zig cc
    command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || export RC_x86_64_pc_windows_gnu="$PWD/scripts/windres-zig.sh"
  else
    # .dmg 不用 Tauri 的 bundle_dmg.sh：它在 GitHub 的 macOS 机器上偶尔失败（hdiutil 资源忙），下面自己做并重试
    bundles=app
  fi
  tauri_cli build --ci --target "$triple" --bundles "$bundles" --config "$conf" "${runner[@]}" -- --locked
  local out=target/$triple/release/bundle
  if [[ $t == windows-* ]]; then
    cp "$(ls "$out"/nsis/*-setup.exe | head -1)" "$DIST/looklook-$VERSION-$t-setup.exe"
  else
    # 打开 .dmg 看到 Looklook.app 和“应用程序”快捷方式，拖过去即可安装
    local dmgdir i
    dmgdir=$(mktemp -d)
    cp -R "$out"/macos/*.app "$dmgdir/"
    ln -s /Applications "$dmgdir/Applications"
    for i in 1 2 3; do
      hdiutil create -volname Looklook -srcfolder "$dmgdir" -ov -format UDZO "$DIST/looklook-$VERSION-$t.dmg" && break
      [[ $i == 3 ]] && { echo "生成 .dmg 失败" >&2; return 1; }
      echo "  hdiutil 失败，10 秒后重试（$i/3）" >&2
      sleep 10
    done
    rm -rf "$dmgdir"
  fi
}

for t in "$@"; do
  triple=${TRIPLE[$t]:-}
  [[ -n $triple ]] || { echo "未知平台：$t" >&2; exit 2; }
  echo "=== ${t}（${triple}）" >&2
  # 随包的 ttyd 改名为 looklook-term（与 src/paths.rs 的 TERM_EXE 一致）；vendor/ 里保留原名
  exe=looklook; ttyd=ttyd; term=looklook-term; mux=mux; muxout=looklook-mux
  [[ $t == windows-* ]] && { exe=looklook.exe; ttyd=ttyd.exe; term=looklook-term.exe; mux=psmux.exe; muxout=psmux.exe; }
  stage=$DIST/stage/$t/looklook
  mkdir -p "$stage"
  cp "vendor/$t/$ttyd" "$stage/$term"
  cp "vendor/$t/$mux" "$stage/$muxout"
  cp -R vendor/fonts "$stage/fonts"
  # trz / tsz（src/paths.rs 的 TRZSZ_DIR）。Windows 上放在顶层、客户端启动时再挪进 trzsz\：
  # 1.5.0 及以前的更新程序替换新目录时会被杀毒软件挡住（拒绝访问），单个文件不会
  if [[ $t == windows-* ]]; then cp "vendor/$t"/trzsz/*.exe "$stage/"; else cp -R "vendor/$t/trzsz" "$stage/trzsz"; fi
  rm -f "$stage/fonts/.version"
  licenses "$stage" "$t"
  case $t in
    linux-*) cp packaging/install-linux.sh "$stage/install.sh"; cp packaging/uninstall-linux.sh "$stage/uninstall.sh" ;;
    darwin-*) cp packaging/install-macos.sh "$stage/install.sh"; cp packaging/uninstall-macos.sh "$stage/uninstall.sh" ;;
    windows-*) bat packaging/install-windows.bat "$stage/install.bat"; bat packaging/uninstall-windows.bat "$stage/uninstall.bat" ;;
  esac
  chmod 0755 "$stage/$term" "$stage/$muxout" "$stage"/*.sh 2>/dev/null || true
  case $t in
    # macOS 在 macOS 上原生编译（要链接 Apple 系统框架，zig 交叉编译需要 macOS SDK，Linux 上没有）
    windows-* | darwin-*) desktop "$t" "$triple" "$stage" ;;
    *) cargo zigbuild --release --locked --target "$triple" ;;
  esac
  cp "target/$triple/release/$exe" "$stage/$exe"
  chmod 0755 "$stage/$exe"
  pack_upx "$t" "$stage/$exe"
  name=looklook-$VERSION-$t
  if [[ $t == windows-* ]]; then
    (cd "$DIST/stage/$t" && python3 -c 'import os,sys,zipfile
with zipfile.ZipFile(sys.argv[1],"w",zipfile.ZIP_DEFLATED,compresslevel=9) as z:
    for root,_,files in os.walk("looklook"):
        for f in sorted(files): z.write(os.path.join(root,f))' "../../$name.zip")
  else
    tar -C "$DIST/stage/$t" --owner=0 --group=0 -czf "$DIST/$name.tar.gz" looklook
  fi
done

(cd "$DIST" && sha256sum looklook-"$VERSION"-*.{tar.gz,zip,exe,dmg} 2>/dev/null >SHA256SUMS || true)
echo "✓ 安装包：" >&2
ls -lh "$DIST"/looklook-"$VERSION"-* >&2
