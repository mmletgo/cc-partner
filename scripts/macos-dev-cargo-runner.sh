#!/usr/bin/env bash
# macos-dev-cargo-runner.sh
#
# Business Logic（为什么需要这个脚本）:
#   `tauri dev` 默认 cargo run 裸 binary；macOS TCC 对「直接 exec Contents/MacOS 内二进制」
#   不生效（权限 API 假绿、系统设置无条目）。必须经 LaunchServices（open）启动 .app。
#
# Code Logic（这个脚本做什么）:
#   - 非 `run`：原样转发给 cargo
#   - `run`：cargo build → prepare-macos-dev-app → open -n 启动 .app → 等待进程退出
#   - 禁止 open --stdout=/dev/stdout（tauri 管道下会 LS 错误 -10810）；日志写文件并可 tail
#   - EXIT/TERM 时杀掉开发 App，便于热重载
#
# 用法:
#   tauri dev --runner scripts/macos-dev-cargo-runner.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEBUG_DIR="$REPO_ROOT/src-tauri/target/debug"
if [[ -n "${CC_PARTNER_INTERNAL_SIGNING_IDENTITY:-}" ]]; then
  DEV_APP="$DEBUG_DIR/cc-partner-internal-dev.app"
  DEV_DISPLAY_NAME="cc-partner Internal (Dev)"
else
  DEV_APP="$DEBUG_DIR/cc-partner-dev.app"
  DEV_DISPLAY_NAME="cc-partner (Dev)"
fi
DEV_BIN="$DEV_APP/Contents/MacOS/cc-partner"
PREPARE_JS="$SCRIPT_DIR/prepare-macos-dev-app.mjs"
LOG_DIR="$DEBUG_DIR"
STDOUT_LOG="$LOG_DIR/cc-partner-dev.stdout.log"
STDERR_LOG="$LOG_DIR/cc-partner-dev.stderr.log"

if [[ "${1:-}" != "run" ]]; then
  exec cargo "$@"
fi

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
  echo "[macos-dev-cargo-runner] dev app binary missing: $DEV_BIN" >&2
  exit 1
fi

APP_PIDS=()
TAIL_PIDS=()
cleanup() {
  local pid
  for pid in "${TAIL_PIDS[@]+"${TAIL_PIDS[@]}"}"; do
    if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  for pid in "${APP_PIDS[@]+"${APP_PIDS[@]}"}"; do
    if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
  done
  pkill -f "$DEV_BIN" 2>/dev/null || true
}
trap cleanup EXIT TERM INT HUP

# 截断旧日志，方便本轮对照
: >"$STDOUT_LOG"
: >"$STDERR_LOG"

echo "[macos-dev-cargo-runner] launching via LaunchServices (open): $DEV_DISPLAY_NAME" >&2
echo "[macos-dev-cargo-runner] $DEV_APP" >&2
echo "[macos-dev-cargo-runner] logs: $STDOUT_LOG | $STDERR_LOG" >&2
echo "[macos-dev-cargo-runner] NOTE: open --stdout=/dev/stdout fails under tauri pipes (-10810); using log files" >&2

# -n 新实例；-F 不恢复窗口
# stdout/stderr 必须是真实文件路径（不能是 /dev/stdout 管道）
open_args=(-n -F --stdout "$STDOUT_LOG" --stderr "$STDERR_LOG" "$DEV_APP")
if [[ ${#app_args[@]} -gt 0 ]]; then
  open_args+=(--args "${app_args[@]}")
fi

if ! open "${open_args[@]}"; then
  echo "[macos-dev-cargo-runner] ERROR: open failed (LS). Try: open -n \"$DEV_APP\"" >&2
  exit 1
fi

# 把日志 tail 到当前 runner 的 stderr/stdout，方便终端查看
tail -n +1 -f "$STDOUT_LOG" &
TAIL_PIDS+=("$!")
tail -n +1 -f "$STDERR_LOG" >&2 &
TAIL_PIDS+=("$!")

# 等待主进程出现
app_pid=""
for _ in $(seq 1 100); do
  app_pid="$(pgrep -f "$DEV_BIN" 2>/dev/null | head -1 || true)"
  if [[ -n "$app_pid" ]]; then
    break
  fi
  sleep 0.1
done

if [[ -z "$app_pid" ]]; then
  echo "[macos-dev-cargo-runner] ERROR: dev app process did not start within 10s" >&2
  echo "[macos-dev-cargo-runner] stderr log:" >&2
  cat "$STDERR_LOG" >&2 || true
  exit 1
fi

APP_PIDS+=("$app_pid")
echo "[macos-dev-cargo-runner] dev app pid=$app_pid (LaunchServices → TCC applies)" >&2

while kill -0 "$app_pid" 2>/dev/null; do
  sleep 0.4
done

exit 0
