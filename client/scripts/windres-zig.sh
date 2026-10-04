#!/usr/bin/env bash
# 在 Linux 上交叉编译 Windows 程序时给 embed-resource（Tauri 嵌入图标与清单）用的资源编译器：
# GNU windres（binutils-mingw-w64-x86-64），预处理器换成 zig cc，不用另装 mingw 的 gcc。
# package.sh 在没有 x86_64-w64-mingw32-gcc 时通过 RC_x86_64_pc_windows_gnu 指向这里。
set -euo pipefail
pp=(cc -E -xc -DRC_INVOKED -target x86_64-windows-gnu)
exec x86_64-w64-mingw32-windres --preprocessor=zig "${pp[@]/#/--preprocessor-arg=}" "$@"
