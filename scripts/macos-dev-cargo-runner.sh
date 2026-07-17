#!/usr/bin/env bash
# macos-dev-cargo-runner.sh
#
# Business Logic（为什么需要这个脚本）:
#   `tauri dev` 默认用 cargo 直接 run 裸 target/debug/app，macOS 上没有产品级
#   CFBundleIdentifier，TCC 列表名称混乱且应用内输入监控 fail-closed。本 runner
#   替换 cargo：在 `run` 时先 build，再组装 `cc-partner-dev.app`（独立显示名/Bundle ID），
#   最后 exec 包内二进制，使开发版与发布版在系统设置中分开授权。
#
# Code Logic（这个脚本做什么）:
#   - 非 `run` 子命令：原样转发给 cargo
#   - `run`：解析 `--` 前后的 build 参数与 app 参数 → cargo build → prepare-macos-dev-app
#     → exec Contents/MacOS/cc-partner（继承 tauri 注入的全部环境变量）
#
# 用法（由 start.sh / tauri 调用，勿手搓路径）:
#   tauri dev --runner scripts/macos-dev-cargo-runner.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEBUG_DIR="$REPO_ROOT/src-tauri/target/debug"
DEV_APP="$DEBUG_DIR/cc-partner-dev.app"
DEV_BIN="$DEV_APP/Contents/MacOS/cc-partner"
PREPARE_JS="$SCRIPT_DIR/prepare-macos-dev-app.mjs"

if [[ "${1:-}" != "run" ]]; then
  exec cargo "$@"
fi

# cargo run [build_args...] [-- app_args...]
shift # drop `run`
build_args=()
app_args=()
seen_ddash=0
for arg in "$@"; do
  if [[ "$seen_ddash" -eq 1 ]]; then
    app_args+=("$arg")
    continue
  fi
  if [[ "$arg" == "--" ]]; then
    seen_ddash=1
    continue
  fi
  build_args+=("$arg")
done

# 与 cargo run 相同的编译参数，但不自动启动裸二进制
# set -u 下空数组展开会 unbound
if [[ ${#build_args[@]} -gt 0 ]]; then
  cargo build "${build_args[@]}"
else
  cargo build
fi

if [[ ! -f "$PREPARE_JS" ]]; then
  echo "[macos-dev-cargo-runner] missing $PREPARE_JS" >&2
  exit 1
fi

node "$PREPARE_JS"

if [[ ! -x "$DEV_BIN" ]]; then
  echo "[macos-dev-cargo-runner] dev app binary missing or not executable: $DEV_BIN" >&2
  exit 1
fi

echo "[macos-dev-cargo-runner] launching cc-partner (Dev)  com.cc-partner.app.dev" >&2
echo "[macos-dev-cargo-runner] $DEV_BIN" >&2
# set -u 下空数组 "${app_args[@]}" 会 unbound；无 app 参数时直接 exec 二进制
if [[ ${#app_args[@]} -gt 0 ]]; then
  exec "$DEV_BIN" "${app_args[@]}"
else
  exec "$DEV_BIN"
fi
