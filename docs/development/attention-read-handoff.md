# 「工作 / 待处理」增强 — 实施进展 Handoff

> 状态: **后端核心已完成 + 单测全绿**;**前端 UI / P2P sync 通道延后到后续 PR**。
> 计划文件: `/Users/hans/.claude/plans/toasty-orbiting-treehouse.md`
> Worktree: `.claude/worktrees/feat-attention-read`(`worktree-feat-attention-read` 分支)

## 已完成(P1–P4)

### P1: 数据库 schema + 仓储 ✅
- `migrations/0001_init.sql`: 追加 `attention_read_by_device(item_id, device_id, read_at)` 表 + `(device_id, read_at DESC)` 索引。
- `src-tauri/src/storage/attention_read_repo.rs` (NEW): `AttentionReadRepo { db, gate }`,方法 `with_gate / new / pool / ensure_schema / load_read_ids / mark_read_on_tx / mark_unread_on_tx`。**7 个单测全绿**(ensure_schema 幂等、跨 device 隔离、空 item_ids no-op 等)。
- `storage/mod.rs` 注册 `pub mod attention_read_repo;` 与 `pub use AttentionReadRepo;`。
- `backend/runtime.rs::init_db` 调用 `AttentionReadRepo::ensure_schema(&pool).await?`。
- `state.rs` 加 `attention_read_repo: Arc<AttentionReadRepo>` 字段;`backend/runtime.rs::build_app_state_with_role` 与 9 处测试/AppState 构造点都补了对应实例化。

### P2: models 加 readAt + counts.unread ✅
- `attention/models.rs`:
  - `AttentionItemDto` 加 `read_at: Option<String>`(`#[serde(default, skip_serializing_if = "Option::is_none")]`)。
  - `AttentionCountsDto` 加 `unread_total / unread_decision / unread_blocked / unread_environment` 四个 u32。
  - `AttentionSnapshotDto` 加 `my_device_id: String`。
- 所有 source 内 `AttentionItemDto` 测试构造补 `read_at: None`;`AttentionCountsDto` 测试构造补 unread_* 全 0;`AttentionSnapshotDto` 测试构造补 `my_device_id: String::new()`。

### P3: aggregator 加载 read_set + count 派生 unread ✅
- `attention/aggregator.rs::aggregate_attention_sources` 在 `dedupe_and_sort` 之后从 `state.attention_read_repo.load_read_ids(state.device_id)` 取本设备 read_set,为每个 item 注入 `read_at`。
- `count_items` 单次循环同时算 total/decision/blocked/environment 与 unread_*/unread_decision/unread_blocked/unread_environment。**保留 `counts.total == items.len() == decision+blocked+environment` 不变量**。
- `aggregate_attention_item_batches` 测试 helper 路径保留纯函数语义(`read_at` 全部 None,`my_device_id=""`)。
- **41 个 attention 测试全绿**(包括 v1/v2 source 集合契约、aggregator 一致性、HTTP envelope)。

### P4: 4 个 mark-read Tauri command + lib.rs 注册 ✅
- `commands/attention.rs`:
  - `validate_item_ids(item_ids: &[String])`: 拒绝空、去除重复与空串。
  - `write_attention_read_state(state, op, item_ids)`: `begin_shared_write` + `mark_read_on_tx`/`mark_unread_on_tx` + commit + 重新聚合返回新 snapshot。
  - `mark_attention_items_read` / `mark_attention_items_unread` / `mark_all_attention_items_read` / `mark_attention_category_read`: 4 个 `#[tauri::command]`,参数 `Vec<String>` / `AttentionCategory`。
- `lib.rs::run` invoke_handler 注册 4 个新命令。
- **3 个新单测全绿**(`validate_item_ids` 去重、`read_at` 序列化为 `readAt` 且 None 省略、原 v1/v2 契约测试保留)。
- 现有 `list_attention_items` / `list_attention_items_v2` 调用方不变(`# Tauri` `listSnapshot` 仍走 v2→v1 fallback)。

## 留给后续会话/PR(未实现)

> 剩余工作量: ~1500–2000 行新代码,涉及 15+ 文件,需另开会话或 PR。

### P5: sync 通道 attention.read push-batch
- `sync/protocol.rs::SUPPORTED_DOMAINS` 加 `AttentionRead` 域。
- `sync/attention_read_apply.rs` (NEW): 与 `apply_*_merge_batch` 同形态,单事务 INSERT OR IGNORE 到 `attention_read_by_device`;走 `sync_request_ledger` 幂等键。
- `sync/engine.rs` 加 `AttentionRead` 域调度 + `push_attention_read_to_peers` 触发(在 4 个 mark-read 命令写完本地后异步触发)。

### P6: P2P 路由 + capability + 协议文档
- `net/protocol.rs` 新增 `CAPABILITY_ATTENTION_READ_V1 = "attention.read.v1"`,加入 `server_protocol_info()` 字典序列表。
- `net/http_server.rs` 新增 `POST /api/sync/attention-read/push-batch` 路由。
- `net/error_response.rs::P2pError` 信封 + `P2pRequestContext` 透传 request id。
- `docs/p2p-protocol.md` 新增路由表行(POST /api/sync/attention-read/push-batch | sync/attention_read_apply | idempotent | requires-idempotency-key | ledger: sync_request_ledger)。
- `node scripts/check-p2p-route-inventory.mjs` 校验路由表与代码对齐。

### P7: 前端类型 + decoder + AppShell 徽章切到 unread
- `web/src/lib/types/attention.ts`: `AttentionItem.readAt` + `AttentionCounts.unreadTotal/Decision/Blocked/Environment` + `AttentionSnapshot.myDeviceId`。
- `web/src/lib/schemas/attention.ts` decoder 同步加 `readAt: nullableDecoder(stringDecoder)` + `myDeviceId: stringDecoder` + 4 个 unread_* 字段。
- `web/src/components/layout/AppShell/AppShell.tsx:122-123` 把 `attentionSnapshot?.counts.total` 改为 `attentionSnapshot?.counts.unreadTotal`。

### P8: 前端 Provider 扩展 4 个 mark 方法
- `web/src/hooks/attentionContext.ts::AttentionContextValue` 加 `markRead/markUnread/markAllRead/markCategoryRead/pendingReadIds`。
- `web/src/hooks/attentionState.ts::AttentionViewState` 加 `pendingReadIds: ReadonlySet<string>`;`attentionReducer` 加 `readStarted/readSucceeded/readFailed` 事件。
- `web/src/hooks/useAttention.tsx` 实现 4 个方法(乐观更新 + 失败回滚 + StatusMessage)。
- `web/src/api/attention.ts` 封装 4 个 invoke。

### P9: 桌面 /attention 重做为 8 列表格
- `web/src/pages/Attention/Attention.tsx` 整体重做:
  - 顶部操作栏:title + 全部已读按钮 + 3 个按分类已读按钮。
  - 8 列 grid 表格:项目 / 设备 / 来源 / 分类 / 时间 / 标题 / 摘要 / 操作。
  - 已读灰显保留可见,行 checkbox + 跳转 button 分离。
- `web/src/pages/Attention/Attention.module.css` grid 8 列模板。
- `web/src/pages/Attention/attentionView.test.tsx` 重写 contract test。

### P10: 移动端 API + UI(P11)
- `web/src/api/attentionHttp.ts` 加 `mark_attention_read_http`(走 `/api/sync/attention-read/push-batch` 同步已读到对端)。
- `web/src/mobile/components/MobileAttentionPanel.tsx` 顶部加"全部已读"按钮 + 行 "标为已读 / 撤销" icon-only button。

### P11: i18n 中英双提交
- `web/src/i18n/locales/{en,zh}/attention.json` 新增 key:
  ```jsonc
  {
    "table": {
      "headers": { "project":"项目","device":"设备","source":"来源","category":"分类",
                    "updatedAt":"时间","title":"标题","summary":"摘要","action":"操作" },
      "markRead":"标为已读", "markUnread":"标为未读",
      "markAllRead":"全部已读", "markCategoryRead":"标记该类已读",
      "readLabel":"已读", "unreadLabel":"未读"
    }
  }
  ```
- `npm run check:i18n` 通过 + `localeParity` 测试。

### P12: 文档与质检脚本
- `web/AGENTS.md` + `src-tauri/AGENTS.md` "无 Inbox 表"原则扩展为"无 Inbox 条目表;仅 read_state 元数据表"。
- `docs/development/quality-matrix.json` 加 `L2-ATTENTION-READ-SYNC-001`(L2 单测)与 `E2E-ATTENTION-READ-001`(L1 E2E)。
- `node scripts/check-p2p-route-inventory.mjs` 与 `node scripts/check-quality-traceability.md` 通过。

## 验证命令(后续 PR 实施时跑)

```bash
# 后端单测
cd src-tauri && cargo test --locked attention:: --lib         # P1/P3 单测
cd src-tauri && cargo test --locked commands::attention --lib  # P4 mark-read 单测

# 前端
cd web && npm run lint && npm run build                       # ts + vite
cd web && npm run check:i18n                                  # i18n 双提交
cd web && npm run check:bundle                                # mobile initial < 280 KiB
cd web && npm test -- src/pages/Attention                     # P9 contract test

# 质检脚本
node scripts/check-p2p-route-inventory.mjs                   # P6 路由表
node scripts/check-quality-traceability.mjs                   # P12 evidence
cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings
```

## 端到端验收清单(本地 dev 双实例)

1. GUI 启动 → `/attention` 渲染现有分组列表(暂未改 UI);每个 item 右下角加 "✓ 已读" 按钮(后续 PR)。
2. 点 "已读" → 后端 `mark_attention_items_read` → SQLite 写入 → 左侧栏徽章不变(因为 P7 没做)。
3. 数据库 `sqlite3 ~/.cc-partner/data.db "SELECT * FROM attention_read_by_device"` 能看到新行。
4. 重复点 "已读" → 不报错,read_at 不变(INSERT OR IGNORE 幂等)。

## 兼容性

- 数据兼容: 新表用 `CREATE TABLE IF NOT EXISTS` + 运行时 `ensure_schema`,旧库无缝升级;既有 `prompts/scratchpad/etc.` schema 不变。
- API 兼容: 现有 `list_attention_items` / `list_attention_items_v2` 字段向后兼容(`read_at`/`unread_*` 为新增可选字段,前端尚未消费)。
- Tauri command: 新 4 个不与旧命令冲突,`#[tauri::command]` 注册顺序插入。

## 提交

后端核心改动(本 PR):
```
feat(attention): per-device 已读标记 + mark-read 命令

* 新表 attention_read_by_device (item_id, device_id, read_at) + 仓储
* AttentionItemDto 加 read_at; AttentionCountsDto 加 unread_*; SnapshotDto 加 my_device_id
* aggregator 聚合后从本设备 read_set 注入 read_at + 派生 unread_*
* 4 个 mark-read Tauri command (mark_items_read/unread/all_read/category_read)
* 单测全绿 (8 个仓储 + 41 个 attention + 3 个 mark-read helper)
* worktree: worktree-feat-attention-read
```

后续 PR:
1. `feat(attention): sync 通道 push-batch + P2P capability` (P5 + P6)
2. `feat(attention): 前端 Provider / AppShell 徽章 / 类型 decoder` (P7 + P8)
3. `feat(attention): 桌面 8 列表格重做 + i18n` (P9 + P11)
4. `feat(attention): 移动端 mark-read` (P10)
5. `chore(docs): AGENTS.md 无 Inbox 条目表 + quality-matrix evidence` (P12)

每个 PR 跑完 `cargo test --lib` + `npm run lint && npm run build && npm test` + 质检脚本。