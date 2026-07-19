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

set -euo pipefail

# 切到脚本所在目录(仓库根),保证无论从哪里调用都能正确定位
cd "$(dirname "$0")"

WEB_DIR="web"
TAURI_BIN="$WEB_DIR/node_modules/.bin/tauri"

# 彩色输出
info()  { printf "\033[1;34m[INFO]\033[0m %s\n" "$*"; }
error() { printf "\033[1;31m[ERR ]\033[0m %s\n" "$*" >&2; }

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

# macOS 内部开发机自动发现固定代码签名 identity。首次成功发现时，检测脚本会把
# 非敏感 SHA-256 指纹写入用户 signing 目录作为 pin；后续同名证书漂移会拒绝启动。
# 没有该 identity 的开源贡献者继续使用社区 Dev 壳，不需要额外配置。
configure_macos_internal_dev_signing() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    return
  fi

  local identity="${CC_PARTNER_INTERNAL_SIGNING_IDENTITY:-}"
  local fingerprint="${CC_PARTNER_INTERNAL_CERT_SHA256:-}"
  if [[ -n "$identity" && -n "$fingerprint" ]]; then
    info "macOS 内部开发签名: 使用显式环境变量 (${fingerprint:0:12}...)"
    return
  fi
  if [[ -n "$identity" || -n "$fingerprint" ]]; then
    error "macOS 内部开发签名变量必须成对设置: CC_PARTNER_INTERNAL_SIGNING_IDENTITY + CC_PARTNER_INTERNAL_CERT_SHA256"
    exit 1
  fi

  local detected_fingerprint
  if ! detected_fingerprint="$(node scripts/detect-macos-internal-signing.mjs)"; then
    error "自动检测 macOS 内部开发签名失败；拒绝回退 ad-hoc，以免 TCC 身份漂移。"
    exit 1
  fi
  if [[ -z "$detected_fingerprint" ]]; then
    info "未检测到固定内部签名 identity，使用社区 Dev 壳（输入监控不可用）。"
    return
  fi

  export CC_PARTNER_INTERNAL_SIGNING_IDENTITY='cc-partner Internal Code Signing'
  export CC_PARTNER_INTERNAL_CERT_SHA256="$detected_fingerprint"
  info "已自动启用 macOS Internal Dev 签名 (${detected_fingerprint:0:12}...)"
}

run_dev() {
  info "启动开发模式 (Tauri dev:Rust 后端 + Vite 前端 + 热重载)..."
  info "首次启动 Rust 编译较慢(数分钟),之后增量编译很快。"
  configure_macos_internal_dev_signing
  # tauri dev 内部只 `cargo run` 默认 binary(app),不会自动构建独立后端 CLI。
  # 但 GUI setup 启动时必须拉起 cc-partner-backend sidecar,缺真 binary 会 panic。
  # 故在此预先构建 cc-partner-backend debug binary(cargo 增量编译,之后很快)。
  ensure_backend_debug_binary

  # macOS：已配置固定 identity 时生成内部开发 .app；否则生成社区开发 .app。
  # 两者都替换裸 target/debug/app，并使用独立 Bundle ID 与发布版分开。
  # 非 Darwin 仍走默认 cargo runner。
  if [[ "$(uname -s)" == "Darwin" ]]; then
    local runner="$PWD/scripts/macos-dev-cargo-runner.sh"
    if [[ ! -x "$runner" ]]; then
      chmod +x "$runner" 2>/dev/null || true
    fi
    if [[ -x "$runner" ]]; then
      if [[ -n "${CC_PARTNER_INTERNAL_SIGNING_IDENTITY:-}" ]]; then
        info "macOS 开发壳: cc-partner Internal (Dev) / com.cc-partner.app.internal.dev"
        info "固定位置: ~/Applications/cc-partner Internal (Dev).app"
        info "输入监控将登记到固定内部 Dev 主体，与内部稳定版分开授权。"
      else
        info "macOS 开发壳: cc-partner (Dev) / com.cc-partner.app.dev"
      fi
      exec "$TAURI_BIN" dev --runner "$runner"
    fi
    error "缺少可执行 scripts/macos-dev-cargo-runner.sh，回退裸 binary（输入监控可能 fail-closed）"
  fi
  exec "$TAURI_BIN" dev
}

run_build() {
  info "生产构建 (Tauri build,产出 dmg/安装包)..."
  exec "$TAURI_BIN" build
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
  info "清理完成"
}

show_help() {
  cat <<EOF
cc-partner 启动脚本

用法: ./start.sh [命令]

命令:
  dev       开发模式(默认):Tauri + Vite + 热重载
            macOS 已安装固定内部签名 identity 时自动生成
            ~/Applications/cc-partner Internal (Dev).app；未安装时生成社区
            cc-partner-dev.app（输入监控不可用）
  build     生产构建(产出 dmg/安装包)
  web       仅前端 Vite(浏览器预览,无 Tauri 外壳)
  clean     清理构建产物
  help      显示本帮助
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
