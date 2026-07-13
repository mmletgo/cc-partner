# Backend 事务化运行时设计

- 日期：2026-07-13
- 状态：方案已确认，待实施
- 适用范围：Cloud Sync、配置持久化、全局快捷键、Updater、Health 命令边界与故障注入

## 1. 背景与目标

当前多个 backend 子系统存在“内存状态已改变、持久化或外部副作用失败”的分叉风险：Cloud Sync 可并发操作同一 Git 工作区；配置直接覆盖 `config.json`；快捷键先注销旧值；Updater 将同一生命周期拆在多个锁中；Health 对负数、超大数和时间算术缺少统一校验。

本设计把它们收敛到五项不变量：

1. 同一 Cloud Sync 工作区同一时刻最多一个写流程。
2. 配置更新遵循 `clone → mutate → validate → durable atomic replace → swap memory`。
3. 快捷键替换失败时，磁盘、内存和实际注册状态都回到旧值。
4. Updater 的 metadata、bytes、task、cancel 与 phase 处于一个带 generation 的状态机中。
5. Health 输入先校验、时间算术使用 checked operations，非法输入无副作用。

## 2. 方案选择

考虑过：

1. 在每个命令中局部补 rollback；改动小，但规则会继续漂移且难以做故障注入。
2. 引入通用事务框架；抽象过重，外部副作用与文件/内存事务无法真正使用同一数据库事务。
3. 建立三个聚焦 runtime：`ConfigRuntime`、`CloudSyncRuntime`、`UpdateRuntime`，各自公开窄接口。

选择方案 3。它避免伪造“跨文件、OS、Git 的 ACID”，但为每个边界定义可验证的提交点和恢复语义。

## 3. 配置事务

### 3.1 接口

`AppState.config` 保留现有读取接口，新增串行 writer：

```rust
pub struct ConfigRuntime {
    pub value: Arc<RwLock<AppConfig>>,
    update_lock: tokio::sync::Mutex<()>,
    store: Arc<dyn ConfigStore>,
}

pub trait ConfigStore: Send + Sync {
    fn load(&self) -> Result<AppConfig, AppError>;
    fn save_atomic(&self, candidate: &AppConfig) -> Result<(), AppError>;
}

pub async fn update_config_transactionally<T>(
    runtime: &ConfigRuntime,
    mutate: impl FnOnce(&mut AppConfig) -> Result<T, AppError>,
) -> Result<(AppConfig, T), AppError>;
```

所有写入口统一使用该 helper，包括基础配置、Cloud Sync、GitHub Trending、Health 和 Orchestrator config。读入口仍 clone `value`，不持锁跨 await。

### 3.2 提交流程

1. 获取 `update_lock`，保证不会发生 clone/swap 之间的 lost update。
2. clone 当前 `AppConfig` 为 candidate。
3. 应用 patch 并执行完整 `AppConfig::validate()`。
4. 在目标目录创建唯一临时文件，写 UTF-8 JSON、`flush`、`sync_all`。
5. 从临时文件重新读取并反序列化，确认与 candidate 等价。
6. 原子替换 `config.json`；Unix `rename` 后 sync 父目录，Windows 使用 replace-existing/write-through 语义。
7. 只有 durable replace 成功后才把 candidate swap 进内存。

任一步失败时删除本次临时文件；旧 `config.json` 和内存值不变。进程在 rename 前崩溃时旧文件仍权威；rename 后崩溃时新文件权威。启动时清理超过 24 小时的 `.config.json.*.tmp`，不把临时文件当配置加载。

### 3.3 配置校验

`AppConfig::validate()` 至少校验：

- device id/name、receive dir、db path 与 data_dir isolation。
- HTTP 端口保持现有 0=首选默认语义；不把 0 当 OS 随机端口。
- Cloud Sync 间隔不小于 30 秒；repo URL/branch trim 后规范化。
- screenshot/prompt optimizer 快捷键可解析。
- Health 与 Orchestrator 使用各自领域 validator。

`load()` 的旧路径迁移也调用 `save_atomic`。旧 JSON 兼容仍由 serde defaults 与现有字段迁移承担。

## 4. 快捷键替换事务

快捷键是文件事务外的 OS 副作用，使用显式补偿：

```rust
pub fn replace_screenshot_hotkey(
    app: &AppHandle,
    old_value: &str,
    new_value: &str,
) -> Result<RegisteredHotkeyChange, AppError>;

pub struct RegisteredHotkeyChange { /* old/new shortcuts + committed flag */ }
```

流程：

1. 先 parse/validate 新值；无变化直接成功。
2. 在旧快捷键仍注册时先注册新快捷键；失败则完全不动旧值。
3. 注销旧快捷键；失败则注销新快捷键并返回错误。
4. 调用配置事务持久化新值。
5. 配置保存失败则重新注册旧值并注销新值；只有补偿成功才返回原始保存错误。
6. 补偿也失败时返回 `hotkey.rollback_failed`，UI 明确要求重启恢复；磁盘与内存仍保持旧配置。

禁止继续使用 `unregister_all` 做热更新；setup 首次注册仍可走单值注册函数。Prompt optimizer 的前端单键监听不使用 OS global shortcut，不纳入该替换事务。

## 5. Cloud Sync 单飞

`AppState` 新增：

```rust
pub struct CloudSyncRuntime {
    gate: tokio::sync::Mutex<()>,
    status: RwLock<CloudSyncRuntimeStatus>,
}

pub enum CloudSyncTrigger { Manual, Scheduler, ClaudeMdPush }
pub enum CloudSyncBusyPolicy { Wait, ReturnBusy }
```

所有会写 `cloud-sync/` 工作区的入口必须经过 `run_cloud_sync_exclusive`：完整 sync、scheduler、`push_claude_md_to_cloud`。`test_connection` 使用独立临时目录时不需锁；若复用正式 workdir 做 fetch，则也必须取 gate。

策略固定：

- 手动同步与 CLAUDE.md 主动推送：等待当前流程结束，最长 5 分钟；超时返回 `cloud_sync.busy_timeout`。
- scheduler：`try_lock`；忙时跳过本 tick 并记录 `skippedBusy`，不排队。
- 获锁后重读 config，避免等待期间仓库 URL/分支改变。
- gate 覆盖 ensure/clone、fetch/reset、import、export/write、commit 与 push 全流程；不能在 reset 后提前释放。
- panic/取消通过 RAII 释放 gate；取消不得让下一任务把半写工作树当成功，下一次进入先执行 Git worktree integrity/status 检查。

单飞只解决进程内竞争；多进程由 backend CLI 的单实例/serve lifecycle lock 保证。同一 data_dir 不支持两个独立 backend 进程同时同步。

## 6. Updater generation 状态机

### 6.1 单锁状态

移除 AppState 中五个分散的 updater locks，改为：

```rust
pub struct UpdateRuntime {
    inner: Mutex<UpdateRuntimeState>,
}

pub struct UpdateRuntimeState {
    pub generation: u64,
    pub phase: UpdatePhase,
    pub pending: Option<Update>,
    pub bytes: Option<Arc<[u8]>>,
    pub cancel: Option<CancellationToken>,
    pub task: Option<JoinHandle<()>>,
    pub status: UpdateDownloadStatus,
}

pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
    Failed,
    Cancelled,
}
```

所有命令只通过 runtime 方法转移状态；锁内不 await、不执行网络/安装，不持 mutex 进入 callback。

### 6.2 generation 规则

- 每次成功开始 `check_update` 递增 generation，并清理上一代 bytes/error/task。
- Download callback 捕获 generation；仅当当前 generation 相同且 phase=`Downloading` 时才能写进度/完成/失败。
- check 在 Downloading/Installing 时返回 conflict，不静默取消当前代。
- cancel 原子取出 token/handle、把 phase 置 Cancelled，再在锁外 cancel/abort；旧 callback 无权改写。
- 新 check 后旧下载晚到的完成回调因 generation 不匹配被丢弃。

### 6.3 安装重试

安装从 `Arc<[u8]>` clone，不再 `take()`。安装失败后保留同一代 metadata 与 bytes，phase 回到 `Downloaded`，`status.error` 记录失败，允许用户重试。只有安装成功并发出 restart 后才清理；若 restart API 返回错误，仍保留 bytes。重新检查新版本会显式丢弃旧代 bytes。

前端 `UpdateDownloadStatusValue` 增加 `checking` 与 `installing`，其它字段保持 camelCase 兼容。旧前端遇到未知值的 mixed-version 场景只存在本机 GUI/backend 版本不匹配；GUI lifecycle 必须确保 bundled sidecar 版本一致。

## 7. Health 边界校验与安全算术

固定范围：

| 字段/动作 | 范围 |
| --- | --- |
| `workWindowSeconds` | 60..=28800 |
| `breakSeconds` | 30..=7200 |
| `retainDays` | 1..=3650 |
| `waterIntervalSeconds` | 300..=86400 |
| snooze minutes | 1..=1440 |
| habit stats days | 1..=31（维持现有 clamp，但 API 明确返回规范值） |

DND 必须两端同时为空或同时是严格 `HH:MM`，小时 00–23、分钟 00–59；相同起止表示全天 DND，不表示关闭。非法配置返回 `request.validation`，不保存也不改变 runtime。

新增纯函数：

```rust
pub fn validate_health_config(config: &HealthConfigDto) -> Result<HealthConfig, AppError>;
pub fn checked_future_timestamp(now: i64, minutes: i64) -> Result<i64, AppError>;
pub fn checked_water_snooze_origin(now: i64, interval: i64, minutes: i64) -> Result<i64, AppError>;
```

所有 `minutes * 60`、`now +/- duration`、`retain_days * 86400` 使用 `checked_mul/add/sub`。daemon 读取磁盘旧配置时若不合法，记录错误并禁用提醒副作用，不能 panic 或产生立即循环提醒；设置页保存合法值后恢复。

## 8. 磁盘故障注入

`ConfigStore` 的文件操作通过可替换 adapter 执行，测试 fixture 可在以下阶段失败：create temp、write、flush、file fsync、re-read、rename/replace、directory fsync、cleanup。每个测试断言：

- 原配置文件仍是完整合法 JSON 或新配置已完整提交，不出现截断文件。
- 返回错误时内存 config 未 swap。
- 不遗留被 loader 误识别的临时文件。
- 下一次无故障保存可以恢复。

真实磁盘 smoke 在隔离 `CC_PARTNER_DATA_DIR` 下验证只读目录/文件占位、磁盘空间错误可用小型 fake adapter 稳定覆盖；CI 不尝试填满 runner 磁盘。

Updater 另提供 fake driver 注入 check/download/install 的延迟与失败，用 barrier 精确构造旧 generation 晚到、cancel race、install retry。

## 9. 数据库迁移与回滚

本工作流不新增、删除或修改 SQLite 表；Cloud Sync 锁、配置 runtime、Updater bytes/status 都是进程内状态。因此没有数据库迁移步骤，旧数据库可直接使用。

代码回滚时：原子写出的 config 仍是旧字段兼容 JSON；新增字段都必须 `#[serde(default)]`，旧版本忽略未知字段。Updater 新 phase 只存在内存和 IPC，不持久化。回滚不需数据脚本。

## 10. 可观测性与错误语义

- `config.save` 记录 result/stage，不记录完整 JSON、路径中的 home 或 token。
- Cloud Sync status 记录 trigger、startedAt、finishedAt、result、skippedBusy，不记录 repo credential。
- Updater 记录 generation、phase、version、result，不记录签名私钥或安装包内容。
- Health validation 返回字段名与稳定 code，用户文案可本地化；不回显敌意原值。
- 所有锁竞争、回滚失败、stale callback 丢弃都有针对性 trace，但高频 progress 不写文件日志。

## 11. 发布与回滚顺序

1. 先实现 atomic config store 与 fault-injection tests，再迁移各配置命令。
2. 接入 hotkey 补偿事务，完成三平台注册 smoke。
3. 增加 Cloud Sync gate，覆盖三种 trigger 并做并发测试。
4. 替换 Updater runtime，先保持 UI 行为，再增加 installing/checking 展示与安装重试。
5. 最后收紧 Health 校验；发布说明提示历史非法值会被拒绝并要求重新保存。

每一步可独立回滚且保持主分支可运行。禁止先删除旧 updater fields 再等待后续任务补新 runtime。

## 12. 验收标准

- 任意两个 Cloud Sync 写入口并发时，Git 工作区内最多一个流程执行；scheduler 忙时不堆积。
- 任一配置持久化阶段故障后，磁盘 JSON 与内存值保持一致且可再次保存。
- 新快捷键注册失败或配置保存失败时，旧快捷键仍可触发且配置未变化。
- 旧 updater generation 的 progress/completion 无法覆盖新代；取消与安装重试有确定性测试。
- Health 极值、负数、非法 DND 和溢出输入均返回 validation 且无副作用。
- `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、相关 Rust tests、backend smoke 与前端 updater/settings tests 全部通过。

## 13. 非目标

- 不把 Git、OS shortcut、文件系统和 SQLite 包装成虚假的全局 ACID 事务。
- 不更换 Tauri updater、Git CLI、健康状态机或配置文件格式。
- 不持久化更新安装包、Cloud Sync 排队任务或 Health snooze。
- 不借机重构无关的大型模块。
