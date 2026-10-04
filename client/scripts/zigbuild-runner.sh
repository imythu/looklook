#!/usr/bin/env bash
# Tauri CLI 的 --runner：它执行 `<runner> build <参数…>`，cargo-zigbuild 对应的子命令是 zigbuild。
set -euo pipefail
[[ ${1:-} == build ]] && shift
exec cargo zigbuild "$@"
