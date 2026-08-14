# 修复 Workbench Agent 使用统计 tokens「未提供」

## 根因（已确认）
- Ledger 的 usage 管道（`note_usage` / `usage_cache` / null-fill / 单调合并）完整，但生产代码**没有任何调用者**：唯一调用点 `handle_native_agent_event`（`orchestrator/agent_runtime_bridge.rs:295`）是死代码。
- `claudeCodeVisible` 的状态对账（`claude_status.rs`）只读 busy/idle/pid，不读 usage；OSC wire 也不携带 token 字段。
- 因此所有会话 ledger 行 usage 全 null，前端按规范显示「未提供」。

## 方案：终态时从 Claude 本地 transcript 提取 usage
Claude Code 会在 `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl` 的每条 assistant 消息写入 cumulative `message.usage`（input_tokens / output_tokens / cache_read_input_tokens / cache_creation_input_tokens / costUSD）。runtime 已通过 `claude_status` 绑定 `native_session_id`，按文件名精确查找即可，只读数字、不读正文，符合 ledger 的 metadata-only 隐私边界。

### 改动点（全部在 src-tauri）

1. **新模块 `src-tauri/src/workbench/agent_runtime/claude_usage.rs`**
   - `extract_claude_transcript_usage(native_session_id) -> Option<ReliableUsageSnapshot>`：
     - 在 `~/.claude/projects/*/`（复用 `cc/collector.rs::claude_projects_dir()`）按文件名 `<native_session_id>.jsonl` 有界查找（沿用 `ClaudeIndexBudget` 风格：max files / 64MiB 每文件上限）。
     - 命中后**从文件尾部向前**读（有界，单行 ≤1MiB），解析最后一条含 `message.usage` 的行，映射为 `ReliableUsageSnapshot`（`costUSD` → `cost_major`，currency=`USD`）。
     - 纯函数核心 + 可注入 projects root，便于 tempdir 测试。
   - 每个函数按项目规范添加中文 Business/Code Logic 注释。

2. **接线：`src-tauri/src/workbench/agent_runtime/mod.rs`**
   - 在 `apply_owner_agent_mutation` 终态分支（mod.rs:212-224 附近，spawn `on_agent_runtime_terminal` 处）与启动 reconcile 分支（mod.rs:298-312）：当 `row.provider_id == "claudeCodeVisible"` 且有 `native_session_id` 时，`spawn_blocking` 提取 usage 并调用 `state.agent_ledger_service.note_usage()`（失败仅 debug 日志，不阻断终态写入；`note_usage` 对已 finalize 的行会自动 null-fill）。
   - 加每 session 一次性去重（内存 set），避免重复扫描。

3. **测试**
   - 新增 `claude_usage.rs` 单测（tempdir + `write_jsonl` 风格 fixture，参照 `claude_sessions_test.rs`）：正常提取 / 无 usage 行 / 文件缺失 / 超预算截断。
   - `mod.rs` 接线层已有 ledger 终态测试模式可复用。

4. **前端无改动**：`AgentLedgerDrawer` 对非 null token 正常渲染数值。

### 验证
- `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
- 手动验收：启动 dev 应用 → Workbench 终端跑一次 claude 会话 → 结束后 Agent 使用统计该会话显示输入/输出 tokens（不依赖人工复制日志，直接读 SQLite/界面验证）。

### 流程
- 复杂度 >100 行：按项目规范用 git worktree 新分支开发，subagent 实现，完成后合并回 master，并同步更新相关 AGENTS.md/CLAUDE.md 记忆（功能逻辑层面）。