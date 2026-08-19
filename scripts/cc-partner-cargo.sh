#!/usr/bin/env bash
# cc-partner-cargo.sh — 带本机缓存约定的 cargo 入口。
#
# 用法（在仓库根）:
#   ./scripts/cc-partner-cargo.sh test --locked --lib parse_simple_frontmatter
#
# 会启用 sccache（若已安装），并拒绝把 CARGO_TARGET_DIR 指到空的 /tmp，
# 以免从 unicode-ident 起重编整棵依赖图。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

info() { printf '\033[1;34m[INFO]\033[0m %s\n' "$*"; }
error() { printf '\033[1;31m[ERR ]\033[0m %s\n' "$*" >&2; }

# shellcheck source=scripts/cargo-dev-env.sh
source "$ROOT/scripts/cargo-dev-env.sh"
cc_partner_drop_ephemeral_cargo_target_dir
configure_sccache

cd "$ROOT/src-tauri"
exec cargo "$@"
