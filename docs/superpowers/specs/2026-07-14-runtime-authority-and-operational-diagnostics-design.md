# Runtime Authority and Operational Diagnostics 设计

- 日期：2026-07-14
- 状态：已确认
- 依赖：2026-07-13 S1/S3/S6 已落地；基线 `bb980fd`

## 1. 问题

GUI 与独立后端分别通过 `build_app_state` 创建完整 `AppState`。原子配置文件和进程内 singleflight 已存在，但两个进程仍拥有不同的 `ConfigRuntime`、`CloudSyncRuntime`、Workbench terminal registry、Orchestrator telemetry 与 remote event bridge。结果是“GUI 显示保存成功、sidecar 继续旧行为”、手动/自动云同步并发、终端重复恢复、桌面 runtime snapshot 读取空 telemetry，以及 bridge 永久重连。

## 2. 目标

1. sidecar 成为运行态配置、Cloud Sync、terminal、Orchestrator、LAN listener 和 remote bridge 的唯一 owner。
2. GUI 通过受现有 lifecycle control 保护的 loopback 控制面读取/更新权威运行态。
3. 每次应用配置返回稳定 `ownerInstanceId` 与单调递增 `generation`。
4. terminal 由 `RuntimeRole` 强制只在 sidecar 创建/恢复，创建/恢复失败执行 RAII 补偿，禁止两个进程同时拥有同一 attach。
5. desktop runtime snapshot 与诊断页读取 sidecar 权威数据。
6. remote bridge 有按设备取消、TTL、退避抖动、关机回收和资源上限。

## 3. 非目标

- 不改变配置文件原子落盘实现、Updater generation 或固定 LAN 无鉴权业务边界。
- 不让浏览器访问 lifecycle control token，不把 control API 变成公网管理面。
- 不持久化 Prompt、文件内容、终端输入输出或 Orchestrator prompt 到诊断记录。
- 不同时重写 backend CLI lifecycle。

## 4. 所有权模型

```text
GUI React
  → Tauri command adapter
    → BackendControlClient (loopback + control-file token)
      → sidecar BackendControlRouter
        → RuntimeOwner(AppState)
```

GUI 保留窗口、托盘、系统权限、launcher-owned LAN disclosure bootstrap 和 Tauri 事件转发职责；凡会影响 LAN/Workbench/Orchestrator/Cloud Sync 的运行态均由 sidecar 返回结果。所有本机与远端 Workbench projects/files/Git/browser/terminal 请求也先代理到 sidecar，GUI 不创建 `RemoteWorkbenchClient` 或 remote bridge。小控制 DTO 与 Workbench 数据面分路由限额：文件/预览/browser 沿用既有 5 MiB/10 MiB/32 MiB 领域预算并采用流式或二进制 body，不把合法内容塞进 1 MiB control JSON。独立 CLI 本身就是 owner，不经过 GUI。

### 4.1 Owner identity

```rust
pub struct RuntimeOwnerStatus {
    pub owner_instance_id: String,
    pub generation: u64,
    pub started_at: DateTime<Utc>,
    pub config_fingerprint: String,
    pub cloud_sync_phase: RuntimePhase,
    pub terminal_session_count: usize,
    pub bridge_count: usize,
    pub orchestrator: OrchestratorRuntimeSummary,
}
```

- `ownerInstanceId` 每个 sidecar 进程启动生成一次。
- `generation` 仅在权威运行配置成功替换后递增。
- `configFingerprint` 是非敏感字段的规范化摘要，不包含 URL 凭据、token 或路径内容。

### 4.2 配置更新

GUI 提交 `ApplyRuntimeConfigRequest { expectedOwnerInstanceId, expectedGeneration, patch }`；`patch` 是字段 allowlist DTO，不提交完整 `AppConfig`，避免 stale snapshot 覆盖未编辑字段。sidecar 在现有 `update_config_transactionally`/update lock 中校验、用 `spawn_blocking` 原子落盘、同步锁替换内存并递增 generation；generation 不匹配返回 typed conflict 和当前非敏感状态，GUI 刷新后再由用户重试。

Cloud Sync、Orchestrator config、设备名、接收目录和相关运行配置全部走该控制面。只影响 GUI 的主题、窗口偏好与 N4 sidecar 启动前的 LAN disclosure bootstrap 不进入 sidecar。截图快捷键采用两阶段补偿：owner CAS 预检 → GUI 替换 OS shortcut → owner durable patch commit。明确冲突/失败时恢复旧 shortcut；响应丢失先读 owner/generation/config 对账，已提交则保留新 shortcut，确认未提交才回滚，无法判定则进入阻塞式人工 reconcile，不能绕过现有 AppHandle 副作用。

### 4.3 Cloud Sync

所有入口最终调用 owner 内同一个 `CloudSyncRuntime`。GUI 手动同步可等待；sidecar 自动同步忙时沿用现有 skip 语义。禁止 GUI 自建第二个 Git workspace 临界区。状态包含 `idle/running/succeeded/partial/failed`，由 N2 定义领域结果。

### 4.4 Terminal runtime role 与 RAII 补偿

运行时定义 `RuntimeRole::HeadlessOwner | GuiClient`。`local_create/restore/write/resize/close` 仅允许 `HeadlessOwner`；GUI 的 Tauri commands 全部转发到 sidecar。session 表继续保存可恢复元数据，不写短生命周期 PID/owner 字段。sidecar 创建 PTY/tmux 后由 `SessionSpawnGuard` 持有 registry 资源，数据库提交成功才 `commit()`；任何 early return 自动关闭 attach。恢复 claim 使用 `RestoreClaimGuard`，所有错误出口都会释放。

sidecar 的事件总线同时接收 terminal/merge/transfer/runtime 事件，事件游标为 `(ownerInstanceId, sequence)`。GUI 用 `afterSequence` 连接可取消的本机 relay；owner 变化时清旧游标，发现 bounded replay ring gap/lag marker 时先通过 terminal replay/runtime snapshot 恢复，再接 live，避免重启误去重或永久漏事件。

### 4.5 Orchestrator snapshot

桌面本机项目不再直接读取 GUI `AppState` telemetry。Tauri command 通过 control client 读取 sidecar snapshot，并保留现有 display-only cache/stale 语义。snapshot 仍只用于观察，不驱动调度决策。

### 4.6 Remote event bridge 与流限制

- bridge key 为 device id，包含 cancellation token、lastSubscriberAt、attempt 和 lastErrorClass。
- 无订阅者超过 60 秒回收；shutdown 主动 cancel 并等待任务结束。
- 重连使用 bounded exponential backoff + jitter，设备重新发现可提前唤醒。
- NDJSON 单行和 pending buffer 上限均为 1 MiB；错误响应最多读取 8 KiB 前缀。
- 超限返回 typed `ResourceLimit`，不得继续累积内存。

## 5. 运行诊断表面

Settings 的“依赖环境”内新增“运行状态”分区，复用 Card/Pill/Button：

- owner instance、启动时间、generation、配置是否一致；
- Cloud Sync phase 与最近完成时间；
- terminal/bridge 数量；
- Orchestrator 最近 tick 与最近错误类别；
- “刷新”“打开日志目录”“复制脱敏诊断摘要”。

诊断摘要不包含 Prompt、文件名/内容、终端文本、仓库远端 URL、token、SSH key 或 control token。

## 6. 失败与恢复

| 场景 | 行为 |
| --- | --- |
| sidecar 未启动 | GUI 尝试现有 ensure lifecycle；仍失败则展示可重试错误，不在 GUI 本地假保存 |
| generation 冲突 | 保留用户表单，刷新权威配置，要求重新确认 |
| 配置落盘失败 | generation 不变、内存不替换、返回 typed error |
| control 响应丢失 | GUI 读取 owner status 对账，不盲目重复 mutation |
| terminal persist 失败 | `SessionSpawnGuard` 关闭新 attach，删除临时 registry 项，不留下 ghost session |
| restore early return | `RestoreClaimGuard` 释放 claim，后续恢复可再次获取 |
| owner crash | 新 sidecar 从持久 session metadata 恢复；GUI 永远不参与本地 attach |

## 7. 兼容与迁移

- control file/status 增加独立 `controlSchemaVersion`；schema 字段 serde default、`ownerInstanceId: Option` 让 legacy JSON 能先反序列化再分类 stale。GUI 仅在 control 版本存在时使用；旧 sidecar 显示“需要重启后端以应用设置”，不伪装实时成功。
- session 表无需新增 owner/PID 字段；运行时 owner 只由 role 与 control file 判断。
- GUI/sidecar 混合版本不得破坏既有 LAN 业务 API；control version 只存在 control file/status，不进入 `server_protocol_info()` 或任何 LAN 业务 capability 授权。

## 8. 测试与验收

1. L2：两个进程模拟下配置更新后，sidecar generation 和实际接收目录一致。
2. 并发：GUI 手动同步与 sidecar 自动同步只执行一次 Git 临界区。
3. 故障注入：配置写失败、control response 丢失、session DB 写失败均无 split-brain/ghost。
4. terminal：GUI/Mobile 同时 list/restore 只产生一个 sidecar attach；persist/restore 故障无 ghost/claim 泄漏。
5. runtime snapshot：桌面读取到 sidecar scheduler tick，GUI 空 telemetry 不参与补值。
6. bridge：订阅释放、TTL、shutdown、1 MiB 行限制与 8 KiB error prefix 有确定性测试；GUI 进程不创建 remote bridge。
7. relay：owner 重启、断线重连、replay ring lag/gap 后以 `(ownerInstanceId,sequence)` 正确恢复且不丢关键状态。
8. 诊断摘要运行 secret/content scanner，无敏感内容。

## 9. 持久文档

实现时更新 `docs/prd.md`、`src-tauri/CLAUDE.md`、`docs/development/backend-operations.md`、`docs/p2p-protocol.md`（若新增 route inventory）和质量矩阵。

## 10. Spec 自审

- 不重复原子配置或进程内 singleflight；解决的是跨进程 owner。
- control API 与无鉴权 LAN 业务 API 分离，未引入设备身份模型。
- owner、generation、runtime role/RAII claim、resource limit 与失败恢复均有唯一语义。
