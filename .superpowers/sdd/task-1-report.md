# Task 1 Report

## Status

DONE

## 实现内容

- 新增 `workbench/browser_models.rs`：实现 Workbench browser preview 的 Rust DTO，包括 target source、target、discovery、preview 和 discover/preview request。
- 新增 `workbench/browser.rs`：实现 loopback dev server URL 提取、URL 安全归一化、候选排序、local project/worktree 根解析、terminal replay 发现、package.json 默认端口推断和常见端口探测。
- 新增 `storage/workbench_browser_repo.rs`：实现 `WorkbenchBrowserRepo::new/get_target/upsert_target`，按 `project_id + IFNULL(worktree_id,'')` 隔离主项目与 worktree 目标。
- 接入 `workbench_browser_targets` schema 到 `lib.rs::init_db` 与 `migrations/0001_init.sql`，并把 repo 挂到 `AppState`。
- 更新 `Cargo.toml/Cargo.lock`：按 brief 增加 `axum ws`、`reqwest stream`、`regex`、`futures-util`、`tokio-tungstenite`。
- 更新 `src-tauri/CLAUDE.md` 和 `docs/prd.md`：记录 Workbench browser preview 的 loopback-only 安全边界、发现排序、持久化语义和验证命令。

## TDD RED 证据

- `cargo test workbench::browser::tests --quiet`
  - RED：失败，4 个测试中 3 个失败。
  - 失败点：`extracts_local_dev_server_urls_from_terminal_output` 返回空、`normalizes_allowed_loopback_targets` unwrap 到未实现错误、`ranks_remembered_then_terminal_then_config_then_probe` 排序未实现。
- `cargo test storage::workbench_browser_repo --quiet`
  - RED：失败，2 个测试均失败。
  - 失败点：`get_target` stub 返回 `None`，无法读回 upsert 的项目级/worktree 级目标。

## GREEN / 验证结果

- `cargo test workbench::browser --quiet`：通过，4 passed。
- `cargo test storage::workbench_browser_repo --quiet`：通过，2 passed。
- `cargo check --quiet`：通过。
- `git diff --check`：通过。

## 改动文件

- `src-tauri/Cargo.toml`
- `src-tauri/Cargo.lock`
- `src-tauri/migrations/0001_init.sql`
- `src-tauri/src/workbench/browser_models.rs`
- `src-tauri/src/workbench/browser.rs`
- `src-tauri/src/workbench/mod.rs`
- `src-tauri/src/storage/workbench_browser_repo.rs`
- `src-tauri/src/storage/mod.rs`
- `src-tauri/src/state.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/CLAUDE.md`
- `docs/prd.md`
- `.superpowers/sdd/task-1-report.md`

## 自检

- 未实现 Task 2 的 proxy/routes/commands，也未改前端。
- Discovery 只处理本机 `kind="local"` project；remote shortcut 应由后续 route/client 在 owning device 上执行。
- `normalize_browser_target_url` 拒绝外网、metadata IP、file URL、无端口 URL 和非 http(s) scheme。
- `terminal_output_targets` 对主 worktree 兼容旧 `worktree_id = NULL` session，避免漏掉历史主工作区终端输出。
- `workbench_browser_targets` 使用 generated `worktree_key` 做唯一键，主项目目标和 worktree 目标互不覆盖。

## 疑虑 / 注意事项

- brief 的提取测试要求保留终端输出里的 `http://localhost:5173/`，而归一化函数要求统一到 `127.0.0.1`；当前实现是在 `extract_dev_server_urls` 中保留显式 localhost 展示值，但构造成 `WorkbenchBrowserTarget` 时仍会归一化为可代理 URL。
- 端口探测目前只判断 300ms 内 HTTP 请求是否能建立响应，不校验页面类型；这是 discovery foundation，后续 preview/proxy 层仍需控制目标访问边界。

## Review Fix 2026-07-08

### 修改内容

- 修复 `terminal_output_targets` 的 project-level 范围：`worktree_id=None` 现在只读取旧 `worktree_id=NULL` session 和主 worktree session，不再扫描同项目 feature worktree session，避免主项目浏览器预览误选其他 worktree 的 dev server。
- 补充 `project_level_browser_scope_excludes_feature_worktree_sessions` 回归测试，并为触碰到的 browser 测试函数补齐中文 Business Logic / Code Logic 注释。
- 强化 `discover_workbench_browser_targets` / `resolve_browser_worktree_root` 注释，明确 Task 1 discovery 只能在 owning device 的 local project 上执行；remote shortcut 必须由后续 commands wrapper/P2P route 转发到 owning device。
- 修复 `storage/workbench_browser_repo.rs` 中 `get_target` / `upsert_target` 的 RED 阶段 stale 注释。
- 更新 `src-tauri/CLAUDE.md` 与 `docs/prd.md`，记录 project-level browser discovery 的主工作区 session 边界。

### TDD / 验证结果

- RED：`cargo test workbench::browser --quiet` 失败于 `project_level_browser_scope_excludes_feature_worktree_sessions`，确认旧逻辑会接受 feature worktree session。
- GREEN：`cargo test workbench::browser --quiet` 通过，5 passed。
- `cargo test storage::workbench_browser_repo --quiet` 通过，2 passed。
- `cargo check --quiet` 通过。
