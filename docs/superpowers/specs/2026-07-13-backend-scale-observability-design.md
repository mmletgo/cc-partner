# 后端规模化与可观测性设计

- 日期：2026-07-13
- 状态：方案已确认，待实施
- 适用范围：Orchestrator scheduler/SQLite、Claude Code History P2P 同步、后端本地性能指标

## 1. 背景与问题

当前后端使用 WAL + `SqlitePoolOptions::max_connections(1)` 保持接近旧版 aiosqlite 的串行语义。这一基线简单可靠，但两个增长路径已经把单连接的等待时间放大为用户可见延迟：

1. `OrchestratorRepo::claim_next_local_queued_tasks_with_global_capacity` 在数据库事务中无上限读取候选、逐项查询项目路径，并同步读取/解析 `WORKFLOW.md`。慢盘或大量排队任务会长期占用唯一连接。
2. CC History 同步发送全量摘要、全量读取历史、逐条 `get(id)`，再逐条无事务 upsert。记录数和正文体积增长后会出现 N+1 查询、大 JSON body、长时间半完成写入和内存峰值。
3. 当前日志能诊断失败，但没有稳定记录连接等待、事务耗时、scheduler tick 延迟和同步批次规模，无法用证据判断是否应该扩大连接池。

本设计采用“先缩短临界区和分页，再以指标决定扩池”的路径。直接把池从 1 扩大到 N 会改变全仓并发语义，并不能消除事务内文件 IO 或全量同步，因此不作为第一步。

## 2. 目标

1. Orchestrator claim 的文件 IO 与 YAML 解析全部发生在 SQLite 事务外。
2. 每次 tick 的候选、文件读取和数据库写入都有硬上限；最终领取继续使用 CAS，不能重复 dispatch。
3. CC History 新协议以游标分页交换摘要、按 ID 批量取正文、分批事务 upsert；旧协议保留一代混合版本兼容。
4. 所有新同步请求都有单条、条数和总字节上限，超限返回稳定、不可重试的错误。
5. 本地记录连接等待、事务耗时、scheduler 延迟和同步批次指标，不上传遥测、不记录正文/路径/凭据。
6. 用压测门槛决定是否把生产池从 1 小幅提升到 2；没有证据时保持 1。

## 3. 非目标

- 不改 Orchestrator 状态机、优先级语义、Preparing lease 或 full-auto 交付语义。
- 不把 `WORKFLOW.md` 内容持久化到 SQLite，也不新增文件 watcher。
- 不引入 Redis、消息队列、外部 metrics 服务或云端 telemetry。
- 不在本轮分页 Prompt、Scratchpad、SSH target 等其它同步协议。
- 不删除 `/api/cc-history/sync/pull|push` legacy 路由。
- 不以扩大连接池替代查询、事务和协议治理。

## 4. 已确认架构决策

### 4.1 Orchestrator 采用三阶段 claim

```text
阶段 A（短 DB 读取，无事务）
  读取 active count + 最多 256 个 Queued/Idle local 候选
  每行一次 JOIN 带出 project path，不再逐候选查项目

阶段 B（DB 外）
  按 project_id 去重；最多解析 64 个项目 WORKFLOW.md
  使用 spawn_blocking 读取/解析文件
  生成 eligible candidate ids + 原 workflow_state

阶段 C（短 DB 写事务）
  重算 active count/remaining
  按原优先级逐项 UPDATE ... WHERE status/workflow_state/run_state/project-kind
  rows_affected == 1 才读取并返回 claimed row
```

常量固定为：

- `CLAIM_CANDIDATE_LIMIT = 256`
- `CLAIM_PROJECT_LIMIT = 64`
- 返回领取数仍不超过 `max_concurrent_tasks - active`

候选排序固定为 `priority DESC, created_at ASC, id ASC`，`id` 是稳定 tie-breaker。若候选窗口被无效 workflow 项目占满，scheduler 记录 `candidate_window_exhausted`，并在下一 tick 使用进程内 `ClaimScanCursor` 从上次扫描边界继续；扫描到尾部后回绕。cursor 不持久化、不参与任务正确性，进程重启仅重置扫描起点。这样既有单 tick 上限，也不会让窗口之后的合法任务永久饥饿。

文件在阶段 B 后变化属于“本 tick workflow 快照”；任务和项目并发变化由阶段 C 的 CAS/`EXISTS(local project)` 守卫处理。无效 workflow 只跳过该项目并记录脱敏项目 ID，不改变任务状态。

### 4.2 连接池先保持 1，扩到 2 必须过门槛

生产默认仍为 `max_connections(1)`。只有同时满足下列条件，才允许独立提交把默认改为 2：

- 10,000 条 CC History + 1,000 个 Orchestrator queued tasks 的混合压测中，连接等待 p95 连续三次超过 50ms；
- 完成 claim/CC History 优化后，扩大到 2 能把 p95 至少降低 30%；
- `cargo test --locked`、两个写事务竞争测试和 crash/retry 测试全部通过；
- `SQLITE_BUSY`/locked 错误率不高于单连接基线；
- 不把池扩大到 3 或以上。

不满足任一条件则明确记录“保持 1”，不做配置开关。SQLite `busy_timeout` 固定 5 秒；WAL 保持不变。

### 4.3 CC History 新协议是 manifest + items + push-batch

新增 capability：`cc-history.paged-sync.v1`。新增三个 P2P 路由，均保持 snake_case：

1. `POST /api/cc-history/sync/manifest-page`
   - 请求 `{cursor: string|null, limit: number}`。
   - 按 `id ASC` 返回 `{summaries, next_cursor, done}`。
   - summary 为 `{id, vector_clock}`；cursor 是不透明 base64url JSON `{v:1,last_id}`，客户端不得解析。
2. `POST /api/cc-history/sync/items`
   - 请求 `{ids: string[]}`。
   - 按请求 ID 顺序返回存在的完整 rows，并返回 `missing_ids`。
3. `POST /api/cc-history/sync/push-batch`
   - 请求 `{items: ClaudeHistoryRow[]}`。
   - 服务端一次批量读取现有 rows、执行既有 `merge_cc_history`、在一个事务中 upsert，返回 `{accepted}`。

客户端先分页读取 remote manifest，批量读取相同 ID 的本地 rows并比较 vector clock；远端领先/并发或本地缺失的 ID 分批调用 items，本地领先/并发或远端缺失的 rows 分批 push。只在内存保留摘要映射，不保留双方全部正文。

分页常量固定为：

- `CC_MANIFEST_PAGE_LIMIT_DEFAULT = 256`
- `CC_MANIFEST_PAGE_LIMIT_MAX = 512`
- `CC_ITEM_BATCH_LIMIT = 128`
- 单条 `content` UTF-8 上限 1 MiB
- 单次 items 响应和 push-batch JSON 估算上限 8 MiB
- 单个 ID UTF-8 上限 256 bytes；空白/重复 ID 拒绝

8 MiB 限制在业务层按序列化前的确定性估算和 axum route body limit 双重执行。超限使用 HTTP 413、错误码 `cc_history.batch_too_large`、`retryable=false`；单条超限使用 422、`cc_history.item_too_large`。cursor 非法使用 400、`cc_history.invalid_cursor`。items 客户端遇到合法 413 时将该批 ID 对半拆分后继续，最小拆到 1；单条仍超限则以 `item_too_large` 结束本轮，禁止把该条静默跳过。

### 4.4 混合版本与回滚

- 新客户端先读取 health；存在 capability 才使用分页路由。
- 缺少 capability 的 v0/v1 对端继续走原 `/pull|push`，行为不变。
- 新服务端继续挂载 legacy 路由，因此旧客户端可与新服务端同步。
- capability 与三个新路由在同一提交原子上线。
- 回滚时先让客户端停止选择 capability，再删除 token/路由；数据库 schema 不变化，无数据回滚。
- 分页中断不保存 remote cursor；下一次同步从头交换摘要。upsert 和 vector-clock merge 幂等，允许重复批次。

## 5. 仓储接口

`ClaudeHistoryRepo` 新增窄接口：

```rust
pub async fn list_sync_manifest_page(
    &self,
    after_id: Option<&str>,
    limit: u32,
) -> Result<Vec<CcSyncSummary>, AppError>;

pub async fn get_many_for_sync(
    &self,
    ids: &[String],
) -> Result<HashMap<String, ClaudeHistoryRow>, AppError>;

pub async fn upsert_merged_batch(
    &self,
    items: &[ClaudeHistoryRow],
) -> Result<usize, AppError>;
```

`get_many_for_sync` 使用最多 128 个绑定参数的动态 `IN`；`upsert_merged_batch` 显式 `begin/commit`，任一行失败整批 rollback。采集用 `bulk_ingest` 同样改为单事务，但仍保持 `INSERT OR IGNORE`，不得与同步 REPLACE 语义混合。

## 6. 本地性能指标

新增 `backend::runtime_metrics`，只在进程内保存有界 rolling snapshot，并通过结构化 tracing 输出；不新增远程 API。指标不包含正文、文件路径、项目名、设备名或 SQL 文本。

| 指标 | 单位/标签 | 告警阈值 |
| --- | --- | --- |
| `db.acquire_wait_ms` | operation | >50ms warning |
| `db.transaction_ms` | operation, outcome | >100ms warning |
| `orchestrator.scheduler_tick_delay_ms` | 无用户字段 | >2× tick interval warning |
| `orchestrator.claim_scan_count` | candidates/projects/claimed | candidate=256 时标 exhausted |
| `cc_history.sync_batch` | direction/items/estimated_bytes/outcome | 超限独立 error code |
| `cc_history.sync_round_ms` | peer capability class，不含 host | >10s warning |

snapshot 保存每项 `count/last/max/ewma`，进程重启清空。`RuntimeMetrics::measure_db_acquire` 和 `RuntimeMetrics::record_duration` 使用 `Instant`，测试注入确定性 duration，不依赖 sleep。现有 doctor JSON 暂不改变 schema；诊断日志已经足够读取 warning。未来若要暴露 snapshot，必须单独设计隐私和 schema 版本。

## 7. 错误与故障语义

- workflow 文件读/解析失败：只跳过该项目；事务未开始，其他项目仍可 claim。
- CAS miss：正常并发结果，不算 error；指标记录 `cas_miss`。
- manifest/items 网络失败：本轮 CC History 同步失败并保留已完成的幂等批次，下轮从头恢复。
- push-batch 任一 DB 写失败：整个 batch rollback，返回 retryable 5xx；不得返回部分 accepted。
- malformed response、重复 ID、非法 cursor：协议错误，不自动重试。
- legacy 路径继续保持当前 best-effort，不把分页错误伪装成空列表成功。

## 8. 验证与性能基线

### 8.1 正确性

- 并发两个 claim 调用，同一任务最多一个拿到 Preparing/token。
- `WORKFLOW.md` 慢读时另一个简单 DB 查询能在 100ms 内完成（单连接也必须通过，因为 IO 已移出事务）。
- cursor 窗口跨 tick 前进、回绕且不漏合法候选。
- manifest 10,001 rows 无重复/遗漏；分页中断后重跑最终收敛。
- mixed new↔new、new client↔legacy server、legacy client↔new server 均收敛。
- push-batch 中途故障整批 rollback；重复提交不改变最终合并结果。

### 8.2 压测 fixture

- 10,000 条 CC History，正文分布 1KiB/64KiB/1MiB。
- 1,000 个 queued tasks，100 个 local projects，其中 10 个无效 workflow。
- 同时运行 scheduler tick、manifest sync、历史页面读取和 Prompt CRUD。
- 每个场景 release profile 预热一次、测量三次，输出 median/p95/max、峰值 RSS 和错误数。

目标不是固定机器上的绝对吞吐 KPI，而是：无无界 body/候选；事务内无文件 IO；无 N+1；分页峰值内存相对 legacy 至少降低 50%；扩池决策遵守 §4.2。

## 9. 文档与完成标准

实施后更新 `docs/p2p-protocol.md`、`src-tauri/CLAUDE.md`、`docs/development/testing.md` 和 PRD 的同步规模行为。完成必须满足：

1. `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked` 通过。
2. P2P route inventory 包含三个新路由及 capability。
3. 压测报告由测试输出/CI artifact 生成，不提交一次性总结 Markdown。
4. 日志和指标敏感字段扫描通过。
5. 没有依据不得把 pool 默认值改为 2。
