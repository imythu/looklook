#!/usr/bin/env bash
# 用 zig 从官方源码编译静态链接的 tmux 3.7c，作为随包的会话保持程序（安装包里叫 looklook-mux）。
#
# 为什么要自带：以前要求用户自己装 tmux，没装就“关掉网页，里面的程序也结束”。随包以后装好即用，
# 不依赖系统包管理器，也不依赖系统有没有 libevent / ncurses。
# 依赖（libevent、ncurses）全部静态链接；Linux 用 musl，产物没有任何动态库依赖；macOS 只依赖系统自带的 libSystem。
#
# 用法：scripts/build-mux.sh [linux-x86_64|linux-aarch64|darwin-aarch64]...   （缺省全部；需要 zig 0.15、make、bison）
# 产物：vendor/<target>/mux （vendor/ 里不叫 tmux；package.sh 打包时改名 looklook-mux）
# Windows 没有 tmux，用 psmux（兼容 tmux 命令的原生 Windows 程序），见 fetch-vendor.sh。
set -euo pipefail
cd "$(dirname "$0")/.."

ROOT=$PWD
CACHE=$ROOT/.cache/downloads
ZIG=${ZIG:-$(command -v zig || echo /opt/zig/zig-x86_64-linux-0.15.2/zig)}
[[ -x $ZIG ]] || { echo "需要 zig（https://ziglang.org/download/），或用 ZIG=/path/to/zig 指定" >&2; exit 1; }
command -v yacc >/dev/null || command -v bison >/dev/null || { echo "需要 bison（tmux 的语法分析器由它生成），例如 apt install bison / brew install bison" >&2; exit 1; }

TMUX_VERSION=3.7c
LIBEVENT_VERSION=2.1.12-stable
NCURSES_VERSION=6.5
UTF8PROC_VERSION=2.10.0

# 名称 地址 SHA-256
SOURCES=(
  "tmux-$TMUX_VERSION.tar.gz https://github.com/tmux/tmux/releases/download/$TMUX_VERSION/tmux-$TMUX_VERSION.tar.gz 7c60cae9a0e25288e2e24750aafc9e8800fc7fd4555e447e1b29ee4201cfb3bf"
  "libevent-$LIBEVENT_VERSION.tar.gz https://github.com/libevent/libevent/releases/download/release-$LIBEVENT_VERSION/libevent-$LIBEVENT_VERSION.tar.gz 92e6de1be9ec176428fd2367677e61ceffc2ee1cb119035037a27d346b0403bb"
  "utf8proc-$UTF8PROC_VERSION.tar.gz https://github.com/JuliaStrings/utf8proc/releases/download/v$UTF8PROC_VERSION/utf8proc-$UTF8PROC_VERSION.tar.gz 276a37dc4d1dd24d7896826a579f4439d1e5fe33603add786bb083cab802e23e"
  "ncurses-$NCURSES_VERSION.tar.gz https://ftpmirror.gnu.org/gnu/ncurses/ncurses-$NCURSES_VERSION.tar.gz 136d91bc269a9a5785e5f9e980bc76ab57428f604ce3e5a5a90cebc767971cc6"
)

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

mkdir -p "$CACHE"
for s in "${SOURCES[@]}"; do
  read -r name url sum <<<"$s"
  f=$CACHE/$name
  if [[ ! -f $f || $(sha256 "$f") != "$sum" ]]; then
    echo "下载 $name ..." >&2
    for i in 1 2 3 4 5 6 7 8; do curl -fL -C - --retry 3 -o "$f" "$url" && [[ $(sha256 "$f") == "$sum" ]] && break; sleep 2; done
    [[ $(sha256 "$f") == "$sum" ]] || { echo "校验失败：$name" >&2; exit 1; }
  fi
done

build_one() { # build_one <vendor 目标> <zig 目标> <autoconf host>
  local t=$1 ztarget=$2 host=$3
  local BUILD=$ROOT/.cache/build-mux/$t STAGE=$ROOT/.cache/build-mux/$t/stage OUT=$ROOT/vendor/$t
  echo "=== ${t}（${ztarget}）" >&2
  rm -rf "$BUILD" && mkdir -p "$BUILD/bin" "$STAGE"
  # autoconf 要单个可执行文件路径。
  printf '#!/bin/sh\nexec "%s" cc -target %s "$@"\n' "$ZIG" "$ztarget" >"$BUILD/bin/zcc"
  printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$ZIG" >"$BUILD/bin/zar"
  printf '#!/bin/sh\nexec "%s" ranlib "$@"\n' "$ZIG" >"$BUILD/bin/zranlib"
  # ncurses 编译期要在本机运行的小工具（make_hash 等）：configure 因为 CC 是 clang 系的 zig，给本机编译器也加了 -Qunused-arguments，
  # gcc 不认识，包一层把这个参数去掉。
  printf '#!/bin/sh\nfor a in "$@"; do shift; case "$a" in -Qunused-arguments) ;; *) set -- "$@" "$a" ;; esac; done\nexec cc "$@"\n' >"$BUILD/bin/hostcc"
  chmod +x "$BUILD"/bin/*
  export BUILD_CC="$BUILD/bin/hostcc"
  export CC="$BUILD/bin/zcc" AR="$BUILD/bin/zar" RANLIB="$BUILD/bin/zranlib" 
  local JOBS; JOBS=$(nproc 2>/dev/null || sysctl -n hw.ncpu)
  # 失败时给出各日志的末尾（tail 一次只能带一个文件的 -n）
  logs() { for f in "$@"; do echo "--- $f" >&2; tail -n 40 "$f" >&2; done; }
  unpack() { tar xzf "$CACHE/$1" -C "$BUILD"; }

  echo "--- libevent" >&2
  unpack "libevent-$LIBEVENT_VERSION.tar.gz"
  (cd "$BUILD/libevent-$LIBEVENT_VERSION" &&
    ./configure --host="$host" --prefix="$STAGE" --disable-shared --enable-static --disable-openssl --disable-samples \
      --disable-libevent-regress --disable-debug-mode >configure.log 2>&1 &&
    make -j"$JOBS" >build.log 2>&1 && make install >>build.log 2>&1) || { logs "$BUILD"/libevent-*/configure.log "$BUILD"/libevent-*/build.log; exit 1; }

  # 编进程序的终端描述（fallbacks）要用 tic / infocmp 生成。macOS 自带的是 ncurses 5.7 的老 tic，写不出 6.x 的
  # 数据库目录（“error writing …/tmp_info/6d/mintty”），各系统也不一定装了 tic：先用本机编译器从同一份源码编一套。
  echo "--- ncurses（本机 tic）" >&2
  local HOSTNC=$BUILD/host-ncurses
  mkdir -p "$HOSTNC"
  tar xzf "$CACHE/ncurses-$NCURSES_VERSION.tar.gz" -C "$HOSTNC"
  (cd "$HOSTNC/ncurses-$NCURSES_VERSION" &&
    env CC=cc AR=ar RANLIB=ranlib ./configure --prefix="$HOSTNC/root" --without-shared --without-debug --without-cxx --without-cxx-binding \
      --without-ada --without-manpages --without-tests --enable-widec >configure.log 2>&1 &&
    env CC=cc AR=ar RANLIB=ranlib make -j"$JOBS" >build.log 2>&1 && make install.progs >>build.log 2>&1) || { logs "$HOSTNC"/ncurses-*/configure.log "$HOSTNC"/ncurses-*/build.log; exit 1; }

  echo "--- ncurses" >&2
  unpack "ncurses-$NCURSES_VERSION.tar.gz"
  # 终端描述库：运行时先查系统的 terminfo 目录；再把常用的几个终端编进程序里（fallbacks），
  # 系统缺 terminfo（精简容器、部分 NAS）时 tmux 也能起来。
  (cd "$BUILD/ncurses-$NCURSES_VERSION" &&
    # zig 自带的 macOS 头文件没有 <sys/ttydev.h>，而 lib_baudrate.c 在 __APPLE__ 下会包含它（只是老式波特率兼容，用不到）：去掉这个分支。
    sed -i.bak 's/defined(__APPLE__))/0)/' ncurses/tinfo/lib_baudrate.c &&
    ./configure --host="$host" --prefix="$STAGE" --without-shared --with-normal --enable-widec --without-debug \
      --without-cxx --without-cxx-binding --without-ada --without-manpages --without-tests --without-progs \
      --with-build-cflags=-O2 --with-build-cppflags= --with-build-ldflags= --with-build-libs= \
      --disable-stripping --enable-pc-files --with-pkg-config-libdir="$STAGE/lib/pkgconfig" \
      --with-terminfo-dirs=/etc/terminfo:/lib/terminfo:/usr/share/terminfo:/usr/lib/terminfo:/usr/local/share/terminfo:/opt/homebrew/share/terminfo \
      --with-default-terminfo-dir=/usr/share/terminfo \
      --with-fallbacks=xterm-256color,xterm,screen-256color,screen,tmux-256color,tmux,linux,vt100 \
      --with-tic-path="$HOSTNC/root/bin/tic" --with-infocmp-path="$HOSTNC/root/bin/infocmp" \
      --disable-database --enable-termcap >configure.log 2>&1 &&
    make -j"$JOBS" >build.log 2>&1 && make install.libs install.includes >>build.log 2>&1) || { logs "$BUILD"/ncurses-*/configure.log "$BUILD"/ncurses-*/build.log; exit 1; }

  # widec 版把 terminfo 部分合并在 libncursesw.a 里；tmux 的 configure 还会找 libtinfo / libncurses，指到同一个库即可。
  ln -sf libncursesw.a "$STAGE/lib/libtinfo.a"
  ln -sf libncursesw.a "$STAGE/lib/libncurses.a"

  # macOS 上 tmux 必须用 utf8proc（系统的 wcwidth 对中文、emoji 的宽度不可靠）；Linux 用 musl 自带的即可。
  local utf8proc=()
  if [[ $t == darwin-* ]]; then
    echo "--- utf8proc" >&2
    unpack "utf8proc-$UTF8PROC_VERSION.tar.gz"
    (cd "$BUILD/utf8proc-$UTF8PROC_VERSION" &&
      make -j"$JOBS" libutf8proc.a CC="$CC" AR="$AR" RANLIB="$RANLIB" >build.log 2>&1 &&
      install -m 0644 libutf8proc.a "$STAGE/lib/" && install -m 0644 utf8proc.h "$STAGE/include/") || { logs "$BUILD"/utf8proc-*/build.log; exit 1; }
    utf8proc=(--enable-utf8proc --disable-jemalloc LIBUTF8PROC_CFLAGS="-I$STAGE/include" LIBUTF8PROC_LIBS="$STAGE/lib/libutf8proc.a")
  fi

  # macOS 不允许完全静态的可执行文件：只把 libevent / ncurses 静态链进去，仍然动态链接系统自带的 libSystem。
  local static=-static enable_static=--enable-static extra_cppflags="" extra_cflags=""
  if [[ $t == darwin-* ]]; then
    static=; enable_static=
    # -D_XOPEN_SOURCE 会藏起 u_char 等 BSD 类型，-D_DARWIN_C_SOURCE 把它们找回来；
    # zig 自带的 macOS 头文件没有 <util.h>（forkpty 本身在 libSystem 里），补上声明，做法同 build-ttyd-macos.sh。
    mkdir -p "$BUILD/shim"
    cat >"$BUILD/shim/util.h" <<'EOF2'
#ifndef LOOKLOOK_UTIL_SHIM_H
#define LOOKLOOK_UTIL_SHIM_H
#include <sys/types.h>
#include <sys/ioctl.h>
#include <termios.h>
int openpty(int *, int *, char *, struct termios *, struct winsize *);
pid_t forkpty(int *, char *, struct termios *, struct winsize *);
#endif
EOF2
    # 同样没有 <resolv.h>：tmux 的 input.c 只为 b64_ntop 包含它，而 tmux 自带 compat/base64.c 与声明，留一个空头文件即可。
    : >"$BUILD/shim/resolv.h"
    # -D_XOPEN_SOURCE 也藏起了 getpagesize（recallocarray.c 在用）：隐式声明在 clang 里默认是错误，降成警告（返回 int，与实际一致）。
    extra_cflags="-Wno-error=implicit-function-declaration"
    extra_cppflags="-D_DARWIN_C_SOURCE -I$BUILD/shim"
  fi
  echo "--- tmux" >&2
  unpack "tmux-$TMUX_VERSION.tar.gz"
  # tmux 用 pkg-config 找 libevent/ncurses；这里直接给出静态库的位置与头文件。
  (cd "$BUILD/tmux-$TMUX_VERSION" &&
    ./configure --host="$host" $enable_static "${utf8proc[@]}" \
      LIBEVENT_CFLAGS="-I$STAGE/include" LIBEVENT_LIBS="$STAGE/lib/libevent.a" \
      LIBNCURSES_CFLAGS="-I$STAGE/include -I$STAGE/include/ncursesw" LIBNCURSES_LIBS="$STAGE/lib/libncursesw.a" \
      CPPFLAGS="-I$STAGE/include -I$STAGE/include/ncursesw $extra_cppflags" CFLAGS="-Os $extra_cflags" LDFLAGS="$static -L$STAGE/lib" >configure.log 2>&1 &&
    make -j"$JOBS" >build.log 2>&1) || { logs "$BUILD"/tmux-*/configure.log "$BUILD"/tmux-*/build.log; exit 1; }

  mkdir -p "$OUT"
  install -m 0755 "$BUILD/tmux-$TMUX_VERSION/tmux" "$OUT/mux"
  echo "✓ $OUT/mux（tmux ${TMUX_VERSION}，依赖已静态链接）" >&2
}

one() {
  case $1 in
    linux-x86_64) build_one linux-x86_64 x86_64-linux-musl x86_64-linux-musl ;;
    linux-aarch64) build_one linux-aarch64 aarch64-linux-musl aarch64-linux-musl ;;
    darwin-aarch64) build_one darwin-aarch64 aarch64-macos.11.0 aarch64-apple-darwin ;;
    *) echo "未知目标：$1" >&2; exit 2 ;;
  esac
}

if [[ $# -eq 0 ]]; then set -- linux-x86_64 linux-aarch64 darwin-aarch64; fi
for t in "$@"; do one "$t"; done
