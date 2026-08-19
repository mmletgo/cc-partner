# cargo-dev-env.sh — 本机 cargo/tauri 的 sccache 与 target 目录约定。
# 由 start.sh 与 scripts/cc-partner-cargo.sh source；调用前 cwd 必须是仓库根。
#
# 不写入仓库 .cargo/config.toml，以免 CI 无 sccache 时失败。
# 已有非 sccache 的 RUSTC_WRAPPER 不覆盖。

# shellcheck shell=bash

# 丢掉指向空临时目录的 CARGO_TARGET_DIR。那种目录没有已编译依赖，
# cargo 会从 unicode-ident 起重编整棵图，比等 src-tauri/target 的 artifact lock 更慢。
cc_partner_drop_ephemeral_cargo_target_dir() {
  local dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$dir" ]]; then
    return 0
  fi
  case "$dir" in
    /tmp/* | /private/tmp/* | /var/folders/*)
      if declare -F info >/dev/null 2>&1; then
        info "忽略 CARGO_TARGET_DIR=${dir}（空临时目录会全量重编依赖）。改用 src-tauri/target。"
      else
        printf '[cc-partner] 忽略 CARGO_TARGET_DIR=%s（空临时目录会全量重编依赖）。改用 src-tauri/target。\n' "$dir" >&2
      fi
      unset CARGO_TARGET_DIR
      ;;
  esac
}

# 多 worktree 共享 rustc 缓存。PATH 有 sccache 时启用。
configure_sccache() {
  cc_partner_drop_ephemeral_cargo_target_dir

  local wrapper="${RUSTC_WRAPPER:-}"
  if [[ -n "$wrapper" && "$wrapper" != *sccache* ]]; then
    if declare -F info >/dev/null 2>&1; then
      info "保留已有 RUSTC_WRAPPER=${wrapper}（不改为 sccache）"
    fi
    return 0
  fi

  local sccache_bin=""
  if command -v sccache >/dev/null 2>&1; then
    sccache_bin="$(command -v sccache)"
  fi

  if [[ -z "$wrapper" ]]; then
    if [[ -z "$sccache_bin" ]]; then
      if declare -F info >/dev/null 2>&1; then
        info "未检测到 sccache。并行 git worktree 编译可安装: brew install sccache"
      fi
      return 0
    fi
    export RUSTC_WRAPPER="$sccache_bin"
  fi

  if [[ "${RUSTC_WRAPPER}" != *sccache* ]]; then
    return 0
  fi

  local cache_js="$PWD/scripts/worktree-dev-cache.mjs"
  if [[ -f "$cache_js" ]] && command -v node >/dev/null 2>&1; then
    local basedirs
    if basedirs="$(node "$cache_js" --print-sccache-basedirs)"; then
      if [[ -n "$basedirs" ]]; then
        export SCCACHE_BASEDIRS="$basedirs"
      fi
    fi
  fi
  export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-15G}"
  if declare -F info >/dev/null 2>&1; then
    info "已启用 sccache（CACHE_SIZE=${SCCACHE_CACHE_SIZE}；勿共用 CARGO_TARGET_DIR）"
  fi
}
