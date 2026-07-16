# LAN Agent Fleet 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：Agent Session Runtime、Agent State Projection
- 对应计划：`docs/superpowers/plans/2026-07-15-lan-agent-fleet.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

Project Rail 当前只展示 project、remote badge、device name 和 terminal window/pane 数。Agent 状态、Attention、Git、browser target、Orchestrator slot 与 device online 状态分散在不同 API；前端逐项目逐设备拼接会形成 N×M 轮询和竞态。

现有 Orchestrator runtime snapshot 的 `maxConcurrentTasks` 是设备全局值，而 `slotsUsed` 常按当前项目统计，不能直接作为 Fleet 的设备剩余容量。

## 2. 目标

1. owner 按设备一次性聚合用户已保存 shortcut范围内的 project/Agent/Attention/Git/Orchestrator/browser摘要。
2. Project Rail 只增加低噪音状态，独立 Fleet 视图提供跨项目详情。
3. 每个 device 独立表达 live/cached/offline/unsupported/error，不因一个远端失败阻塞全部结果。
4. 使用有界并发与事件 invalidation，避免前端 N×M 高频轮询。
5. Fleet 只观察和导航，不自动调度、迁移或复制项目。

## 3. 非目标

- 不枚举对端全部项目，只聚合控制设备已经保存的 remote shortcut。
- 不把 mDNS online/capability称为认证、可信或安全。
- 不自动选择设备、迁移 task、复制 repo或调整max concurrency。
- 不从 Fleet 行内输入Agent、批准delivery或执行Git mutation。
- 不新增设备配对、账号、token或权限矩阵。

## 4. Snapshot模型

```rust
pub struct LanFleetSnapshot {
    pub generated_at: String,
    pub devices: Vec<LanFleetDeviceSummary>,
    pub truncated: bool,
}

pub struct LanFleetDeviceSummary {
    pub device_id: String,
    pub device_name: String,
    pub reachability: FleetReachability,
    pub freshness: FleetFreshness,
    pub scheduler_slots_used: Option<u32>,
    pub scheduler_slots_max: Option<u32>,
    pub projects: Vec<LanFleetProjectSummary>,
    pub error_code: Option<String>,
}

pub struct LanFleetProjectSummary {
    pub project_id: String,
    pub display_name: String,
    pub project_kind: String,
    pub agent_counts: AgentPhaseCounts,
    pub attention_count: u32,
    pub terminal_count: u32,
    pub git_state: FleetGitState,
    pub browser_state: FleetBrowserState,
    pub orchestrator_running: u32,
    pub orchestrator_retrying: u32,
    pub last_activity_at: Option<String>,
}
```

remote project ID继续使用`remote:<deviceId>:<inner>`包装；snapshot不得泄漏remote绝对path。

## 5. Owner聚合

- 当前设备先按owning device对保存shortcut分组；同一device只发一个batch请求。
- owner batch请求只接受请求方已经保存的project path列表，并逐项调用现有open/resolve规则得到local project ID。
- owner从Agent runtime、Attention aggregator、terminal repo、Git status、browser registry和Orchestrator repo读取同一时间窗口内的摘要。
- device scheduler slot必须新增真正的global active count；不能用当前project `slotsUsed`推导。
- 每device最多100 projects；每snapshot最多500 projects，超限稳定截断。
- 不把Fleet snapshot持久化为权威；控制设备只保存last display cache与`capturedAt`。

## 6. 刷新与故障隔离

- owner-local snapshot优先由event invalidation触发，visible页面使用30秒safety reconcile。
- remote fan-out最多3台device并发，每device 5秒timeout；结果独立合并。
- page hidden停止safety polling，恢复可见时立即刷新一次。
- remote offline保留最后cache并标记cached/offline；无cache显示offline空摘要。
- capability为`workbench.lan-fleet.v1`；旧peer显示unsupported。
- 禁止递归调用对端Fleet API；owner只汇总自己的local project。

## 7. UI

### 7.1 Project Rail

- 每project最多显示：Agent状态点、`needsInput/failed`聚合badge、device offline标识。
- working数量等正常状态不形成红色badge；只在hover/辅助文本展示。
- Rail header提供“Fleet”二级入口，不新增全局Sidebar项目。
- 点击project仍进入既有Workbench；点击Attention badge进入`/attention`并定位project。

### 7.2 Fleet视图

- Workbench内路由/子视图`/workbench/fleet`，按owning device分组。
- device header显示reachability、global slots和更新时间。
- project row显示Agent phase counts、Attention、Git clean/dirty/conflict、browser preview和Orchestrator摘要。
- 所有动作仅导航到既有project/terminal/automation/attention authority。
- cached/offline/unsupported使用文本+图标，不只依赖颜色。

## 8. Ledger增强

A9可在不改变Fleet首版合同的前提下提供24h/7d usage摘要：

- capability单独协商；缺失时隐藏usage，不显示0。
- Fleet只显示aggregate duration/session/token/cost，不拉取逐session明细。
- provider未可靠提供usage时保持unknown。

## 9. 失败、兼容与回滚

- 单device timeout只影响该device，不清空其他live数据。
- project被删除/shortcut失效显示unavailable并保留导航到项目管理入口。
- Git/browser/Orchestrator子源失败用field-level unknown，不把整个device标记offline。
- rollback移除Fleet视图和batch route后，原Project Rail和逐project APIs继续工作。
- 不持久化新的设备信任或发现表。

## 10. 测试与验收

1. owner batch聚合、100/500上限、global slots和field-level failure有Rust测试。
2. control device按device去重、3并发、timeout、partial merge和cache freshness有hook测试。
3. remote ID、saved-shortcut scope、unsupported capability和禁止递归有route测试。
4. Rail低噪音badge、offline/cached文本与导航有组件测试。
5. Fleet device/project分组、unknown字段、键盘/屏幕阅读器有组件/E2E。
6. 验证没有自动task placement、repo copy或inline mutation入口。

## 11. Spec自审

- Fleet是聚合投影，不成为scheduler或device权威。
- remote请求按owning device批量化且故障隔离。
- 正常Agent状态低噪音，只有异常状态突出显示。
