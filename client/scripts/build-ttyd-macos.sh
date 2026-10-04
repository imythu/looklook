#!/usr/bin/env bash
# 在 Linux 上交叉编译 macOS Apple 芯片（arm64）版 ttyd 1.7.7。
#
# ttyd 官方只发布 Linux 与 Windows 版本；Homebrew 版依赖 /opt/homebrew 下的动态库，不能随包分发。
# 这里用 zig 作为交叉编译器，把依赖（zlib、json-c、mbedtls、libuv、libwebsockets）全部静态链接，
# 版本与 ttyd 官方 scripts/cross-build.sh 一致；libwebsockets 用同一稳定分支的修复版 4.3.5
# （取自 Debian 源码池的上游原始包，可断点续传，SHA-256 与 Debian .dsc 一致）。
# 产物只依赖系统自带的 libSystem，由 zig 做 ad-hoc 签名。
#
# 用法：scripts/build-ttyd-macos.sh      （需要 zig 0.15、cmake、make；ZIG 环境变量可指定 zig 路径）
# 产物：vendor/darwin-aarch64/ttyd
set -euo pipefail
cd "$(dirname "$0")/.."

ROOT=$PWD
CACHE=$ROOT/.cache/downloads
BUILD=$ROOT/.cache/build-macos
STAGE=$BUILD/stage
OUT=$ROOT/vendor/darwin-aarch64
TARGET=aarch64-macos.11.0
ZIG=${ZIG:-$(command -v zig || echo /opt/zig/zig-x86_64-linux-0.15.2/zig)}
[[ -x $ZIG ]] || { echo "需要 zig（https://ziglang.org/download/），或用 ZIG=/path/to/zig 指定" >&2; exit 1; }

TTYD_VERSION=1.7.7
ZLIB_VERSION=1.3.1
JSON_C_VERSION=0.17
MBEDTLS_VERSION=2.28.5
LIBUV_VERSION=1.44.2
LWS_VERSION=4.3.5

# 名称 地址 SHA-256
SOURCES=(
  "ttyd-$TTYD_VERSION-src.tar.gz https://github.com/tsl0922/ttyd/archive/refs/tags/$TTYD_VERSION.tar.gz 039dd995229377caee919898b7bd54484accec3bba49c118e2d5cd6ec51e3650"
  "zlib-$ZLIB_VERSION.tar.gz https://zlib.net/fossils/zlib-$ZLIB_VERSION.tar.gz 9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23"
  "json-c-$JSON_C_VERSION.tar.gz https://s3.amazonaws.com/json-c_releases/releases/json-c-$JSON_C_VERSION.tar.gz 7550914d58fb63b2c3546f3ccfbe11f1c094147bd31a69dcd23714d7956159e6"
  "mbedtls-$MBEDTLS_VERSION.tar.gz https://github.com/Mbed-TLS/mbedtls/archive/refs/tags/v$MBEDTLS_VERSION.tar.gz 849e86b626e42ded6bf67197b64aa771daa54e2a7e2868dc67e1e4711959e5e3"
  "libuv-v$LIBUV_VERSION.tar.gz https://dist.libuv.org/dist/v$LIBUV_VERSION/libuv-v$LIBUV_VERSION.tar.gz ccfcdc968c55673c6526d8270a9c8655a806ea92468afcbcabc2b16040f03cb4"
  "libwebsockets-$LWS_VERSION.tar.gz https://deb.debian.org/debian/pool/main/libw/libwebsockets/libwebsockets_$LWS_VERSION.orig.tar.gz 87f99ad32803ed325fceac5327aae1f5c1b417d54ee61ad36cffc8df5f5ab276"
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

rm -rf "$BUILD" && mkdir -p "$BUILD/bin" "$STAGE"
# 编译器包装：CMake 需要单个可执行文件路径。
printf '#!/bin/sh\nexec "%s" cc -target %s "$@"\n' "$ZIG" "$TARGET" >"$BUILD/bin/zcc"
printf '#!/bin/sh\nexec "%s" ar "$@"\n' "$ZIG" >"$BUILD/bin/zar"
# CMake 在 Darwin 上会给 ranlib 加 Apple 专用参数（-no_warning_for_no_symbols、-c），zig ranlib 不认识。
cat >"$BUILD/bin/zranlib" <<EOF
#!/bin/sh
for a in "\$@"; do shift; case "\$a" in -no_warning_for_no_symbols|-c) ;; *) set -- "\$@" "\$a" ;; esac; done
exec "$ZIG" ranlib "\$@"
EOF
chmod +x "$BUILD"/bin/*
cat >"$BUILD/toolchain.cmake" <<EOF
set(CMAKE_SYSTEM_NAME Darwin)
set(CMAKE_SYSTEM_PROCESSOR arm64)
set(CMAKE_C_COMPILER "$BUILD/bin/zcc")
set(CMAKE_AR "$BUILD/bin/zar")
set(CMAKE_RANLIB "$BUILD/bin/zranlib")
set(CMAKE_OSX_SYSROOT "")
set(CMAKE_OSX_ARCHITECTURES "")
set(CMAKE_OSX_DEPLOYMENT_TARGET "")
set(CMAKE_FIND_ROOT_PATH "$STAGE")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(CMAKE_FIND_LIBRARY_SUFFIXES ".a")
EOF

unpack() { tar xzf "$CACHE/$1" -C "$BUILD"; }
cmake_build() { # cmake_build <源码目录> [参数...]
  local src=$1; shift
  cmake -S "$src" -B "$src/build" -DCMAKE_TOOLCHAIN_FILE="$BUILD/toolchain.cmake" -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$STAGE" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 "$@" >"$src/cmake.log" 2>&1 \
    || { tail -40 "$src/cmake.log" >&2; exit 1; }
  cmake --build "$src/build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)" >"$src/build.log" 2>&1 || { tail -40 "$src/build.log" >&2; exit 1; }
  cmake --install "$src/build" >>"$src/build.log" 2>&1
}

echo "=== zlib $ZLIB_VERSION" >&2
unpack "zlib-$ZLIB_VERSION.tar.gz"
cmake_build "$BUILD/zlib-$ZLIB_VERSION" -DZLIB_BUILD_EXAMPLES=OFF
rm -f "$STAGE"/lib/libz*.dylib

echo "=== json-c $JSON_C_VERSION" >&2
unpack "json-c-$JSON_C_VERSION.tar.gz"
cmake_build "$BUILD/json-c-$JSON_C_VERSION" -DBUILD_SHARED_LIBS=OFF -DBUILD_TESTING=OFF -DDISABLE_THREAD_LOCAL_STORAGE=ON -DBUILD_APPS=OFF

echo "=== mbedtls $MBEDTLS_VERSION" >&2
unpack "mbedtls-$MBEDTLS_VERSION.tar.gz"
cmake_build "$BUILD/mbedtls-$MBEDTLS_VERSION" -DENABLE_TESTING=OFF -DENABLE_PROGRAMS=OFF -DUSE_SHARED_MBEDTLS_LIBRARY=OFF

echo "=== libuv $LIBUV_VERSION" >&2
unpack "libuv-v$LIBUV_VERSION.tar.gz"
cmake_build "$BUILD/libuv-v$LIBUV_VERSION" -DLIBUV_BUILD_TESTS=OFF -DLIBUV_BUILD_BENCH=OFF
# 只保留静态库，ttyd 按名字 uv 查找。
rm -f "$STAGE"/lib/libuv*.dylib
[[ -f $STAGE/lib/libuv_a.a ]] && mv "$STAGE/lib/libuv_a.a" "$STAGE/lib/libuv.a"

echo "=== libwebsockets $LWS_VERSION" >&2
unpack "libwebsockets-$LWS_VERSION.tar.gz"
LWS=$BUILD/libwebsockets-$LWS_VERSION
sed -i.bak 's/ websockets_shared//g' "$LWS/cmake/libwebsockets-config.cmake.in"
sed -i.bak 's/ OR PC_OPENSSL_FOUND//g; /PC_OPENSSL/d' "$LWS/lib/tls/CMakeLists.txt"
cmake_build "$LWS" \
  -DLWS_WITHOUT_TESTAPPS=ON -DLWS_WITH_MBEDTLS=ON -DLWS_WITH_LIBUV=ON -DLWS_STATIC_PIC=ON -DLWS_WITH_SHARED=OFF \
  -DLWS_UNIX_SOCK=ON -DLWS_IPV6=ON -DLWS_ROLE_RAW_FILE=OFF -DLWS_WITH_HTTP2=ON -DLWS_WITH_HTTP_BASIC_AUTH=OFF \
  -DLWS_WITH_UDP=OFF -DLWS_WITHOUT_CLIENT=ON -DLWS_WITHOUT_EXTENSIONS=OFF -DLWS_WITH_LEJP=OFF -DLWS_WITH_LEJP_CONF=OFF \
  -DLWS_WITH_LWSAC=OFF -DLWS_WITH_SEQUENCER=OFF -DLWS_WITH_MINIMAL_EXAMPLES=OFF -DDISABLE_WERROR=ON \
  -DLWS_MBEDTLS_INCLUDE_DIRS="$STAGE/include" \
  -DMBEDTLS_LIBRARY="$STAGE/lib/libmbedtls.a" -DMBEDX509_LIBRARY="$STAGE/lib/libmbedx509.a" -DMBEDCRYPTO_LIBRARY="$STAGE/lib/libmbedcrypto.a" \
  -DLIBUV_INCLUDE_DIRS="$STAGE/include" -DLIBUV_LIBRARIES="$STAGE/lib/libuv.a"

echo "=== ttyd $TTYD_VERSION" >&2
unpack "ttyd-$TTYD_VERSION-src.tar.gz"
# zig 自带的 macOS 头文件没有 <util.h>；forkpty/openpty 本身在 libSystem 里，补上声明即可。
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
cmake_build "$BUILD/ttyd-$TTYD_VERSION" \
  -DCMAKE_C_FLAGS="-I$BUILD/shim" \
  -DCMAKE_EXE_LINKER_FLAGS="-Wl,-dead_strip" \
  -DZLIB_LIBRARY="$STAGE/lib/libz.a" -DZLIB_INCLUDE_DIR="$STAGE/include"

mkdir -p "$OUT"
install -m 0755 "$BUILD/ttyd-$TTYD_VERSION/build/ttyd" "$OUT/ttyd"
file "$OUT/ttyd" 2>/dev/null || true
echo "✓ $OUT/ttyd（ttyd ${TTYD_VERSION}，macOS arm64，静态链接依赖）" >&2
