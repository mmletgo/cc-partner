# Agent Metadata Ledger 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：Agent Session Runtime、Agent Adapter Platform
- 对应计划：`docs/superpowers/plans/2026-07-15-agent-metadata-ledger.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，remote 只缩窄数据面、不制造信任层。

## 1. 问题

当前`RuntimeMetrics`只保存进程内性能计数并在重启后丢失；Orchestrator task虽有Claude runtime字段，却只覆盖自动任务并包含不适合共享的transcript path/last message。普通Workbench Agent没有duration/outcome/provider/model/token/cost历史。

用户需要的是自动形成的轻量运营视图，而不是新的手工台账或同步全文历史。

## 2. 目标

1. 从A1 runtime终态自动生成metadata-only ledger记录。
2. provider/model/token/cost只在adapter提供可靠结构化值时记录，unknown保持null。
3. 本机默认保留30天且最多10,000条，自动清理，无需用户维护。
4. Fleet只读取24h/7d/30d聚合；remote不自动拉取逐session明细。
5. 提供本机可选历史查看与一键清除，但不新增顶层导航。
6. ledger不进入现有Cloud Sync。

## 3. 非目标

- 不保存Prompt、回复、terminal bytes、diff、transcript path、cwd、env或secret。
- 不把token估算冒充provider实际usage。
- 不从CLI输出文本正则猜测cost/model。
- 不做云遥测、排行榜、团队计费或设备信任。
- 不让Ledger成为Agent runtime或Orchestrator状态机真值。

## 4. 数据模型

```rust
pub struct AgentLedgerEntry {
    pub id: String,
    pub agent_session_id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub provider_id: String,
    pub model_id: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub duration_ms: u64,
    pub outcome: AgentLedgerOutcome,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost_minor_units: Option<u64>,
    pub cost_currency: Option<String>,
}
```

新增`agent_session_ledger`：

- `agent_session_id`唯一，保证runtime终态重放幂等；
- project/provider/ended_at索引；
- outcome固定`completed|failed|cancelled|disconnected`；
- usage字段全为nullable；currency只允许ISO 4217三字符代码。

不保存terminal session ID的remote包装值、native transcript path或message。

## 5. 写入与usage归并

- A1 runtime首次进入终态时调用`finalize_agent_ledger_entry`。
- 同一agent session后续更高version只允许填充此前为null的可靠usage或更正endedAt，不创建重复row。
- adapter usage使用单调cumulative snapshot；repo保存每字段max/last可靠值，拒绝负数和counter回退。
- cost只有provider明确返回amount+currency且能按ISO 4217 exponent无损转为整数minor units时写入；需要舍入或currency exponent未知时保持null，不使用静态价目表推算。
- owner restart reconciliation可从终态runtime补写缺失entry。
- ledger写失败不阻断Agent/task完成；记录有界metric并后台重试一次。

## 6. 保留与清理

-默认保留30天；硬上限每device 10,000条。
- owner启动后和每24小时执行一次cleanup：先删`ended_at < now-30d`，再按`ended_at ASC,id`删除超出10,000的最旧记录。
- cleanup单批最多500条，避免长事务；循环由下一次tick继续。
- 设置页只提供“清除Agent元数据历史”明确按钮；不要求用户配置保留期。
-清除不影响runtime、task、evidence、terminal或Fleet当前状态。

## 7. 查询与聚合

本机详情：

- cursor pagination，默认50，最大200；
- filter只允许project/provider/outcome/time range；
- 不提供全文搜索，因为没有正文。

Fleet summary：

```rust
pub struct AgentLedgerSummary {
    pub window: LedgerWindow,
    pub sessions: u64,
    pub completed: u64,
    pub failed: u64,
    pub duration_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_by_currency: Vec<CurrencyAmount>,
    pub usage_coverage: LedgerUsageCoverage,
}
```

- window只允许24h/7d/30d。
- token/cost聚合只在至少一个可靠值时返回；coverage表达complete/partial/unavailable。
- remote capability为`workbench.agent-ledger-summary.v1`，只暴露aggregate，不暴露entry列表。
- owner只汇总请求中的local project IDs；remote shortcut由控制设备映射。

## 8. UI

- Fleet project detail可展开“Agent activity”，显示session数、成功/失败、duration和usage coverage。
- 本机Workbench提供二级历史drawer，默认最近50条，只显示metadata。
- unknown显示“未提供”，不能显示0。
-不新增Sidebar页面，不要求用户维护标签或备注。
-清除按钮复用现有Dialog确认；不提供导出全文，因为不存在全文。

## 9. 隐私、兼容与回滚

- DTO/log/error不得包含Prompt、response、path、terminal bytes、native session ID或provider credential。
- ledger不进入Prompt/SSH/Scratchpad/GitHub sync。
-旧peer无summary capability时Fleet隐藏usage并显示unsupported，不显示0。
- rollback停止writer/cleanup/query即可；保留表不影响runtime。
-降级不要求清空ledger，因为旧版本不会读取新表。

## 10. 测试与验收

1. terminal event重放不重复entry；null usage后补、counter回退、currency validation有repo测试。
2. 30天/10,000/500 batch与虚拟时钟cleanup有测试。
3. cursor/filter/24h-30d边界、coverage和多currency aggregation有测试。
4. remote summary local-project-only、capability与无entry正文有route测试。
5. UI unknown/partial/clear confirmation和无顶层导航有组件测试。
6. privacy fixture扫描DTO/log/sync payload不含禁止字段。

## 11. Spec自审

- Ledger是自动产生的有限metadata，不增加手工记录负担。
- unknown不被估算或显示为0。
- remote只暴露聚合，明细保留在owning device。
