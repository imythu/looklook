#!/usr/bin/env bash
# 准备随包资源（ttyd 与终端字体），放到 vendor/：
#
#   vendor/linux-x86_64/ttyd        ttyd 1.7.7 官方静态版
#   vendor/linux-aarch64/ttyd       ttyd 1.7.7 官方静态版
#   vendor/windows-x86_64/ttyd.exe  ttyd 1.7.7 官方 Windows 版（x86-64）
#   vendor/darwin-aarch64/ttyd      ttyd 1.7.7 源码交叉编译（官方不提供 macOS 版，见 build-ttyd-macos.sh）
#   vendor/linux-*/mux, vendor/darwin-aarch64/mux
#                                   tmux 3.7c 从官方源码静态编译（build-mux.sh；安装包里改名 looklook-mux）
#   vendor/windows-x86_64/psmux.exe psmux 3.3.8（兼容 tmux 命令的原生 Windows 程序，官方发布版）
#   vendor/fonts/                   JetBrains Mono + 霞鹜文楷等宽（build-fonts.py 生成）
#   vendor/*/trzsz/trz、tsz[.exe]   trzsz-go 1.2.0 官方发布版（终端里 trz / tsz 上传下载，见 docs/FILE_TRANSFER.md §4.5）
#
# 所有下载都校验 SHA-256，缓存在 .cache/downloads/，重复执行不会重复下载。
# 用法：scripts/fetch-vendor.sh [all|fonts|linux-x86_64|linux-aarch64|windows-x86_64|darwin-aarch64]...
set -euo pipefail
cd "$(dirname "$0")/.."

CACHE=.cache/downloads
VENDOR=vendor
TTYD_VERSION=1.7.7
PSMUX_VERSION=3.3.8
JB_VERSION=2.304
WENKAI_VERSION=1.522
mkdir -p "$CACHE" "$VENDOR"

# 文件名 → SHA-256（ttyd 的值与官方 SHA256SUMS 一致）
declare -A SHA=(
  [ttyd-$TTYD_VERSION-ttyd.x86_64]=8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55
  [ttyd-$TTYD_VERSION-ttyd.aarch64]=b38acadd89d1d396a0f5649aa52c539edbad07f4bc7348b27b4f4b7219dd4165
  [ttyd-$TTYD_VERSION-ttyd.win32.exe]=e33a27501b10b96981335bcba938b1145c7f52551a343e72160f00ab71832b37
  [ttyd-$TTYD_VERSION-src.tar.gz]=039dd995229377caee919898b7bd54484accec3bba49c118e2d5cd6ec51e3650
  [psmux-v$PSMUX_VERSION-windows-x64.zip]=1ad127ba937194a890b933a73d9b023e297bd73dc742abd841bf159984c2effe
  [JetBrainsMono-$JB_VERSION.zip]=6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf
  [LXGWWenKaiMono-$WENKAI_VERSION-Regular.ttf]=bc068e4e395c396f2909ffdfac3a3751578b73ed3d64a79c9c31bfa84e43debe
  [LXGWWenKaiMono-$WENKAI_VERSION-Medium.ttf]=7a674f448b15a1b3df781c3498973d77f71d270788f7f921080c1344e9d739e1
)
declare -A URL=(
  [ttyd-$TTYD_VERSION-ttyd.x86_64]=https://github.com/tsl0922/ttyd/releases/download/$TTYD_VERSION/ttyd.x86_64
  [ttyd-$TTYD_VERSION-ttyd.aarch64]=https://github.com/tsl0922/ttyd/releases/download/$TTYD_VERSION/ttyd.aarch64
  [ttyd-$TTYD_VERSION-ttyd.win32.exe]=https://github.com/tsl0922/ttyd/releases/download/$TTYD_VERSION/ttyd.win32.exe
  [ttyd-$TTYD_VERSION-src.tar.gz]=https://github.com/tsl0922/ttyd/archive/refs/tags/$TTYD_VERSION.tar.gz
  [psmux-v$PSMUX_VERSION-windows-x64.zip]=https://github.com/psmux/psmux/releases/download/v$PSMUX_VERSION/psmux-v$PSMUX_VERSION-windows-x64.zip
  [JetBrainsMono-$JB_VERSION.zip]=https://github.com/JetBrains/JetBrainsMono/releases/download/v$JB_VERSION/JetBrainsMono-$JB_VERSION.zip
  [LXGWWenKaiMono-$WENKAI_VERSION-Regular.ttf]=https://github.com/lxgw/LxgwWenKai/releases/download/v$WENKAI_VERSION/LXGWWenKaiMono-Regular.ttf
  [LXGWWenKaiMono-$WENKAI_VERSION-Medium.ttf]=https://github.com/lxgw/LxgwWenKai/releases/download/v$WENKAI_VERSION/LXGWWenKaiMono-Medium.ttf
)

# trzsz-go：每个平台一个官方发布包（值与官方 checksums.txt 一致），许可证单独取。不交叉编译。
TRZSZ_VERSION=1.2.0
declare -A TRZSZ_ASSET=(
  [linux-x86_64]=linux_x86_64.tar.gz
  [linux-aarch64]=linux_aarch64.tar.gz
  [darwin-aarch64]=macos_aarch64.tar.gz
  [windows-x86_64]=windows_x86_64.zip
)
SHA+=(
  [trzsz_${TRZSZ_VERSION}_linux_x86_64.tar.gz]=70e3e0847177d4c7b681a8ec19fa00092e422a6c628ef9d8a5db6dfbf4612add
  [trzsz_${TRZSZ_VERSION}_linux_aarch64.tar.gz]=9a73c237b6b12af267e878591ff22a01c97ca9d1cd8125f9ff4ffd6df4fea97c
  [trzsz_${TRZSZ_VERSION}_macos_aarch64.tar.gz]=b6f290e2b6f4d70783797d2fd96eabef9ec789eee402f05e7aa20253cb09b592
  [trzsz_${TRZSZ_VERSION}_windows_x86_64.zip]=0b46318a0fc7e39beaf97e6f07ab257f5b36fdffc3304118b6f72412dbbb52ca
  [trzsz-$TRZSZ_VERSION-LICENSE]=30fbfa725e8534e0f14891463caa18acf797242ed834801b74d2fdb8476b7eda
)
for a in "${TRZSZ_ASSET[@]}"; do URL[trzsz_${TRZSZ_VERSION}_$a]=https://github.com/trzsz/trzsz-go/releases/download/v$TRZSZ_VERSION/trzsz_${TRZSZ_VERSION}_$a; done
URL[trzsz-$TRZSZ_VERSION-LICENSE]=https://raw.githubusercontent.com/trzsz/trzsz-go/v$TRZSZ_VERSION/LICENSE

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# fetch <缓存文件名>：下载（失败重试）并校验
fetch() {
  local name=$1 path="$CACHE/$1"
  if [[ -f $path && $(sha256 "$path") == "${SHA[$name]}" ]]; then return 0; fi
  echo "下载 $name ..." >&2
  for i in 1 2 3 4 5; do
    if curl -fL --retry 3 --connect-timeout 20 -o "$path.part" "${URL[$name]}"; then
      if [[ $(sha256 "$path.part") == "${SHA[$name]}" ]]; then mv "$path.part" "$path"; return 0; fi
      echo "校验失败，重新下载（${i}）" >&2
    fi
    sleep 2
  done
  rm -f "$path.part"
  echo "无法下载 $name" >&2
  exit 1
}

place_ttyd() { # place_ttyd <target> <缓存文件名> <目标文件名>
  fetch "$2"
  fetch "ttyd-$TTYD_VERSION-src.tar.gz"   # 打包时取 ttyd 的 LICENSE
  mkdir -p "$VENDOR/$1"
  install -m 0755 "$CACHE/$2" "$VENDOR/$1/$3"
  echo "✓ $VENDOR/$1/$3（ttyd ${TTYD_VERSION}）" >&2
}

# psmux 发布包里 psmux.exe / pmux.exe / tmux.exe 是同一个程序的三份拷贝，只取一份。必须保留 psmux.exe 这个名字（见 src/paths.rs 的 MUX_EXE）。
place_mux_windows() {
  fetch "psmux-v$PSMUX_VERSION-windows-x64.zip"
  mkdir -p "$VENDOR/windows-x86_64"
  python3 - "$CACHE/psmux-v$PSMUX_VERSION-windows-x64.zip" "$VENDOR/windows-x86_64/psmux.exe" <<'PY'
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
open(sys.argv[2], "wb").write(z.read("psmux.exe"))
PY
  chmod 0755 "$VENDOR/windows-x86_64/psmux.exe"
  echo "✓ $VENDOR/windows-x86_64/psmux.exe（psmux ${PSMUX_VERSION}）" >&2
}

place_mux() { # place_mux <target>：没有就从源码编译 tmux
  if [[ -x $VENDOR/$1/mux ]]; then echo "✓ $VENDOR/$1/mux（已存在）" >&2; else scripts/build-mux.sh "$1"; fi
}

place_trzsz() { # place_trzsz <target>：只取 trz / tsz（包里另有 trzsz 包装程序，用不到）
  local t=$1 a=trzsz_${TRZSZ_VERSION}_${TRZSZ_ASSET[$1]} x=""
  [[ $t == windows-* ]] && x=.exe
  fetch "$a"
  fetch "trzsz-$TRZSZ_VERSION-LICENSE"   # 打包时放进 LICENSES
  local tmp d=$VENDOR/$t/trzsz
  tmp=$(mktemp -d)
  if [[ $a == *.zip ]]; then
    python3 -c 'import sys,zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$CACHE/$a" "$tmp"
  else
    tar -xzf "$CACHE/$a" -C "$tmp"
  fi
  mkdir -p "$d"
  local base=${a%.tar.gz}
  base=${base%.zip}   # 包里的顶层目录与包同名
  install -m 0755 "$tmp/$base/trz$x" "$tmp/$base/tsz$x" "$d/"
  rm -rf "$tmp"
  echo "✓ ${d}/trz${x}、tsz${x}（trzsz-go ${TRZSZ_VERSION}）" >&2
}

fonts() {
  fetch "JetBrainsMono-$JB_VERSION.zip"
  fetch "LXGWWenKaiMono-$WENKAI_VERSION-Regular.ttf"
  fetch "LXGWWenKaiMono-$WENKAI_VERSION-Medium.ttf"
  local stamp="$VENDOR/fonts/.version" want="jb-$JB_VERSION wenkai-$WENKAI_VERSION $(sha256 scripts/build-fonts.py | cut -c1-12) $(sha256 scripts/data/cjk-slices.txt | cut -c1-12)"
  if [[ -f $stamp && $(cat "$stamp") == "$want" ]]; then echo "✓ $VENDOR/fonts（已是最新）" >&2; return 0; fi
  python3 scripts/build-fonts.py \
    --jetbrains "$CACHE/JetBrainsMono-$JB_VERSION.zip" \
    --wenkai-regular "$CACHE/LXGWWenKaiMono-$WENKAI_VERSION-Regular.ttf" \
    --wenkai-medium "$CACHE/LXGWWenKaiMono-$WENKAI_VERSION-Medium.ttf" \
    --out "$VENDOR/fonts"
  echo "$want" >"$stamp"
}

one() {
  [[ -n ${TRZSZ_ASSET[$1]:-} ]] && place_trzsz "$1"
  case $1 in
    fonts) fonts ;;
    linux-x86_64) place_ttyd linux-x86_64 "ttyd-$TTYD_VERSION-ttyd.x86_64" ttyd; place_mux linux-x86_64 ;;
    linux-aarch64) place_ttyd linux-aarch64 "ttyd-$TTYD_VERSION-ttyd.aarch64" ttyd; place_mux linux-aarch64 ;;
    windows-x86_64) place_ttyd windows-x86_64 "ttyd-$TTYD_VERSION-ttyd.win32.exe" ttyd.exe; place_mux_windows ;;
    darwin-aarch64)
      if [[ -x $VENDOR/darwin-aarch64/ttyd ]]; then echo "✓ $VENDOR/darwin-aarch64/ttyd（已存在）" >&2
      else scripts/build-ttyd-macos.sh; fi
      place_mux darwin-aarch64 ;;
    all) for t in fonts linux-x86_64 linux-aarch64 windows-x86_64 darwin-aarch64; do one "$t"; done ;;
    *) echo "未知目标：$1" >&2; exit 2 ;;
  esac
}

if [[ $# -eq 0 ]]; then set -- all; fi
for t in "$@"; do one "$t"; done
