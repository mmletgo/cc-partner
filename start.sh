#!/usr/bin/env bash
# cc-partner 一键启动脚本
#
# 用法:
#   ./start.sh           开发模式(默认):Tauri + Vite + 热重载
#   ./start.sh build     生产构建(产出 dmg/安装包)
#   ./start.sh web       仅启动前端 Vite(浏览器预览,无 Tauri 外壳,无 invoke 能力)
#   ./start.sh clean     清理构建产物(web/dist + cargo target)
#   ./start.sh help      显示帮助
#
# 说明:仓库根目录无 package.json,前端依赖与 tauri CLI 均在 web/node_modules 下,
# 故开发/构建统一通过 web/node_modules/.bin/tauri 调用。
# 打包/开发会 best-effort 调用 scripts/prune-build-artifacts.mjs：
#   - dev：清陈旧 incremental + debug 超阈值（默认 20GB）整清
#   - build：成功后清 release 的 deps/build/incremental（保留 bundle/bin）
# 多 git worktree：dev 启动若 PATH 有 sccache 则设 RUSTC_WRAPPER + SCCACHE_BASEDIRS
# （禁止共用 CARGO_TARGET_DIR）；并对无 cargo/tauri/rustc 占用的其它 worktree
# cargo clean（CC_PARTNER_IDLE_CARGO_CLEAN=0 关闭）。

set -euo pipefail

# 切到脚本所在目录(仓库根),保证无论从哪里调用都能正确定位
cd "$(dirname "$0")"

WEB_DIR="web"
TAURI_BIN="$WEB_DIR/node_modules/.bin/tauri"

# 彩色输出
info()  { printf "\033[1;34m[INFO]\033[0m %s\n" "$*"; }
error() { printf "\033[1;31m[ERR ]\033[0m %s\n" "$*" >&2; }

# sccache / 拒绝空 /tmp CARGO_TARGET_DIR（与 scripts/cc-partner-cargo.sh 共用）
# shellcheck source=scripts/cargo-dev-env.sh
source "$PWD/scripts/cargo-dev-env.sh"

# 前置依赖检查
check_prereqs() {
  # Node / npm 始终需要
  if ! command -v node >/dev/null 2>&1; then
    error "未检测到 Node.js,请先安装(推荐 Node 20+): https://nodejs.org"
    exit 1
  fi
  # Rust 工具链仅在 dev/build 模式需要(tauri 会 cargo run/build)
  if [[ "$MODE" != "web" ]]; then
    if ! command -v cargo >/dev/null 2>&1; then
      error "未检测到 Rust 工具链,请先安装: https://rustup.rs"
      exit 1
    fi
  fi
}

# 确保前端依赖已安装
ensure_deps() {
  if [[ ! -d "$WEB_DIR/node_modules" ]] || [[ ! -x "$TAURI_BIN" ]]; then
    info "安装前端依赖 (npm install)..."
    (cd "$WEB_DIR" && npm install)
  fi
}

# 确保 cc-partner-backend 独立后端 CLI 的 debug binary 已构建。
# tauri dev 只构建默认 binary(app),不会构建 cc-partner-backend;而 GUI setup 启动时
# 必须能拉起该 sidecar。缺真 binary 时 build.rs 生成的占位 launcher 会被误当真 binary
# 执行,导致 cargo 找不到 manifest、GUI panic。这里提前 cargo build 兜底。
ensure_backend_debug_binary() {
  local backend_bin="src-tauri/target/debug/cc-partner-backend"
  if [[ "$OSTYPE" == msys* ]] || [[ "$OSTYPE" == cygwin* ]] || [[ "${OS:-}" == "Windows_NT" ]]; then
    backend_bin="src-tauri/target/debug/cc-partner-backend.exe"
  fi
  if [[ ! -x "$backend_bin" ]]; then
    info "首次构建独立后端 binary (cargo build --bin cc-partner-backend)..."
    (cd src-tauri && cargo build --bin cc-partner-backend)
  fi
}

# macOS 开发机自动发现固定代码签名 identity。首次成功发现时，检测脚本会把
# 非敏感 SHA-256 指纹写入用户 signing 目录作为 pin；后续同名证书漂移会拒绝启动。
# 没有该 identity 时使用同一产品身份的 ad-hoc Dev 壳，输入监控可手动添加授权。
configure_macos_dev_signing() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    return
  fi

  local identity="${CC_PARTNER_INTERNAL_SIGNING_IDENTITY:-}"
  local fingerprint="${CC_PARTNER_INTERNAL_CERT_SHA256:-}"
  if [[ -n "$identity" && -n "$fingerprint" ]]; then
    info "macOS 固定开发签名: 使用显式环境变量 (${fingerprint:0:12}...)"
    return
  fi
  if [[ -n "$identity" || -n "$fingerprint" ]]; then
    error "macOS 固定开发签名变量必须成对设置: CC_PARTNER_INTERNAL_SIGNING_IDENTITY + CC_PARTNER_INTERNAL_CERT_SHA256"
    exit 1
  fi

  local detected_fingerprint
  if ! detected_fingerprint="$(node scripts/detect-macos-internal-signing.mjs)"; then
    error "自动检测 macOS 固定开发签名失败；拒绝回退 ad-hoc，以免 TCC 身份漂移。"
    exit 1
  fi
  if [[ -z "$detected_fingerprint" ]]; then
    info "未检测到固定签名 identity，使用 cc-partner (Dev) ad-hoc 壳。"
    info "输入监控仍可在系统设置中手动添加；重新构建后可能需要再次授权。"
    return
  fi

  export CC_PARTNER_INTERNAL_SIGNING_IDENTITY='cc-partner Internal Code Signing'
  export CC_PARTNER_INTERNAL_CERT_SHA256="$detected_fingerprint"
  info "已自动启用 macOS 固定 Dev 签名 (${detected_fingerprint:0:12}...)"
}

# 回收其它 git worktree 上无编译进程的 src-tauri/target。失败不阻断。
idle_cargo_clean() {
  if [[ "${CC_PARTNER_IDLE_CARGO_CLEAN:-1}" == "0" ]]; then
    return 0
  fi
  local cache_js="$PWD/scripts/worktree-dev-cache.mjs"
  if [[ ! -f "$cache_js" ]] || ! command -v node >/dev/null 2>&1; then
    return 0
  fi
  info "回收闲置 git worktree 的 Cargo target..."
  node "$cache_js" --mode=idle-clean || {
    error "idle cargo clean 未完全成功（已忽略）"
    return 0
  }
}

# 自动修剪过期/超阈值中间产物。失败不阻断主流程（并发 cargo 占用 target 时会 skip）。
# $1: prune mode（auto|release-intermediates|debug-threshold|stale-incremental）
prune_build_artifacts() {
  local mode="${1:-auto}"
  local prune_js="$PWD/scripts/prune-build-artifacts.mjs"
  if [[ ! -f "$prune_js" ]]; then
    return 0
  fi
  if ! command -v node >/dev/null 2>&1; then
    return 0
  fi
  info "修剪构建中间产物 (mode=${mode})..."
  # dev 启动前不要因 prune 失败而退出；release 打包后同样 best-effort
  node "$prune_js" "--mode=${mode}" || {
    error "prune-build-artifacts 未完全成功（已忽略，可稍后 ./start.sh clean）"
    return 0
  }
}

run_dev() {
  info "启动开发模式 (Tauri dev:Rust 后端 + Vite 前端 + 热重载)..."
  info "首次启动 Rust 编译较慢(数分钟),之后增量编译很快。"
  configure_sccache
  # 先回收其它闲置 worktree 的 target，再修剪本树陈旧 incremental / 超阈值 debug。
  idle_cargo_clean
  prune_build_artifacts auto
  configure_macos_dev_signing
  # tauri dev 内部只 `cargo run` 默认 binary(app),不会自动构建独立后端 CLI。
  # 但 GUI setup 启动时必须拉起 cc-partner-backend sidecar,缺真 binary 会 panic。
  # 故在此预先构建 cc-partner-backend debug binary(cargo 增量编译,之后很快)。
  ensure_backend_debug_binary

  # macOS：固定签名与 ad-hoc 都生成同一个 canonical Dev `.app`；签名只影响授权稳定性。
  # Dev 使用独立 Bundle ID 与发布版分开。
  # 非 Darwin 仍走默认 cargo runner。
  if [[ "$(uname -s)" == "Darwin" ]]; then
    local runner="$PWD/scripts/macos-dev-cargo-runner.sh"
    if [[ ! -x "$runner" ]]; then
      chmod +x "$runner" 2>/dev/null || true
    fi
    if [[ -x "$runner" ]]; then
      info "macOS 开发壳: cc-partner (Dev) / com.cc-partner.app.dev"
      info "固定位置: ~/Applications/cc-partner (Dev).app"
      info "输入监控与正式版分开授权。"
      exec "$TAURI_BIN" dev --runner "$runner"
    fi
    error "缺少可执行 scripts/macos-dev-cargo-runner.sh，回退裸 binary（无法完成标准 .app 权限授权流程）"
  fi
  exec "$TAURI_BIN" dev
}

run_build() {
  info "生产构建 (Tauri build,产出 dmg/安装包)..."
  configure_sccache
  # 不能 exec：打包成功后还要 prune release 中间层（deps/build），否则 target/release 持续膨胀。
  # set -e 下用 if 捕获失败，避免 build 失败时脚本提前退出而跳过状态返回。
  if "$TAURI_BIN" build; then
    prune_build_artifacts release-intermediates
    return 0
  fi
  return 1
}

run_web() {
  info "仅启动前端 (Vite,浏览器预览 http://localhost:5173,无 Tauri 外壳)..."
  (cd "$WEB_DIR" && exec npm run dev)
}

run_clean() {
  info "清理构建产物..."
  rm -rf "$WEB_DIR/dist"
  if command -v cargo >/dev/null 2>&1; then
    (cd src-tauri && cargo clean)
  fi
  idle_cargo_clean
  info "清理完成"
}

show_help() {
  cat <<EOF
cc-partner 启动脚本

用法: ./start.sh [命令]

命令:
  dev       开发模式(默认):Tauri + Vite + 热重载
            macOS 固定生成 ~/Applications/cc-partner (Dev).app；检测到
            固定签名 identity 时使用固定签名，否则使用可手动授权的 ad-hoc 签名
  build     生产构建(产出 dmg/安装包)；成功后自动清 release deps/build
  web       仅前端 Vite(浏览器预览,无 Tauri 外壳)
  clean     全量清理构建产物(web/dist + cargo target)
  help      显示本帮助

说明:
  dev 启动前会 best-effort 修剪陈旧 incremental，以及超过
  CC_PARTNER_DEBUG_TARGET_MAX_GB（默认 20）的 debug target。
  若已安装 sccache，dev/build 会设置 RUSTC_WRAPPER 与 SCCACHE_BASEDIRS
  （每个 git worktree 根单独列出，禁止跨 worktree 共用 CARGO_TARGET_DIR）。
  同树 cargo test 请用 ./scripts/cc-partner-cargo.sh，不要设空的 /tmp CARGO_TARGET_DIR。
  同时会对无 cargo/tauri/rustc 占用的其它 worktree 执行 cargo clean
  （CC_PARTNER_IDLE_CARGO_CLEAN=0 关闭）。
  全量回收本树磁盘请用 clean。
EOF
}

# 解析参数
case "${1:-dev}" in
  dev|""        ) MODE=dev ;;
  build         ) MODE=build ;;
  web|frontend  ) MODE=web ;;
  clean         ) MODE=clean ;;
  -h|--help|help) show_help; exit 0 ;;
  *)
    error "未知命令: $1 (用 ./start.sh help 查看用法)"
    exit 1
    ;;
esac

check_prereqs

# clean 不需要装依赖
if [[ "$MODE" != "clean" ]]; then
  ensure_deps
fi

case "$MODE" in
  dev  ) run_dev ;;
  build) run_build ;;
  web  ) run_web ;;
  clean) run_clean ;;
esac
