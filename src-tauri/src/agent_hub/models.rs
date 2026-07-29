//! agent_hub/models — Canonical Hub 领域 DTO 与枚举
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub 需要跨设备可复制的 canonical 身份：Scope / LogicalAsset / Revision DAG /
//!     TargetBinding / Materialization / Conflict。这些类型是 SQLite 与后续 IPC 的共同契约。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase serde 枚举/结构体、RevisionId(UUIDv7) 与 New* 写入输入类型；
//!     枚举提供 as_str / parse 供 SQLite TEXT 列 round-trip。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// CLI 目标运行时。
///
/// Business Logic（为什么需要这个枚举）:
///     投影与适配必须区分 Claude / Codex / OpenCode 的路径与能力差异。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；OpenCode wire 为 `opencode`（与设计文档一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentTarget {
    /// Claude Code
    Claude,
    /// Codex CLI
    Codex,
    /// OpenCode CLI
    #[serde(rename = "opencode")]
    OpenCode,
}

impl AgentTarget {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 持久化与日志需要稳定 token。
    /// Code Logic: 返回 `claude` / `codex` / `opencode`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 读库时必须 fail-closed，禁止静默吞掉未知 target。
    /// Code Logic: 仅匹配 as_str；未知返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }
}

/// 逻辑资产种类。
///
/// Business Logic（为什么需要这个枚举）:
///     不同资产类型（指令/Skill/Command 等）有不同适配与投影语义。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；as_str 与 wire 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    /// 指令文件（CLAUDE.md / AGENTS.md 等）
    Instruction,
    /// Skill 目录资产
    Skill,
    /// 斜杠命令
    Command,
    /// Agent 定义
    Agent,
    /// MCP 配置
    Mcp,
    /// Plugin 包
    Plugin,
    /// Hook 配置
    Hook,
}

impl AssetKind {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 唯一键与 IPC 依赖稳定 kind token。
    /// Code Logic: 返回 camelCase 小写首词。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instruction => "instruction",
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Agent => "agent",
            Self::Mcp => "mcp",
            Self::Plugin => "plugin",
            Self::Hook => "hook",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 kind 不得 silent fallback。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "instruction" => Some(Self::Instruction),
            "skill" => Some(Self::Skill),
            "command" => Some(Self::Command),
            "agent" => Some(Self::Agent),
            "mcp" => Some(Self::Mcp),
            "plugin" => Some(Self::Plugin),
            "hook" => Some(Self::Hook),
            _ => None,
        }
    }
}

/// 资产共享策略。
///
/// Business Logic（为什么需要这个枚举）:
///     shared/adapted/targetOnly 决定 Instruction Compiler 如何在多 target 间分配正文。
///
/// Code Logic（这个枚举做什么）:
///     camelCase；`targetOnly` 保持 camelCase 拼写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetPolicy {
    /// 共同正文，跨 target 共享
    Shared,
    /// 共同正文 + 各 target 适配扩展
    Adapted,
    /// 仅单一 target 持有
    TargetOnly,
}

impl AssetPolicy {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 策略变更影响投影，token 必须稳定。
    /// Code Logic: `shared` / `adapted` / `targetOnly`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Adapted => "adapted",
            Self::TargetOnly => "targetOnly",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知策略 fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "shared" => Some(Self::Shared),
            "adapted" => Some(Self::Adapted),
            "targetOnly" => Some(Self::TargetOnly),
            _ => None,
        }
    }
}

/// Revision 操作类型。
///
/// Business Logic（为什么需要这个枚举）:
///     删除是 tombstone revision，不能物理抹掉历史。
///
/// Code Logic（这个枚举做什么）:
///     `upsert` / `delete`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RevisionOperation {
    /// 写入或更新内容
    Upsert,
    /// 墓碑删除
    Delete,
}

impl RevisionOperation {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: DAG 重放依赖 operation token。
    /// Code Logic: `upsert` / `delete`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 operation 不得默认 upsert。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "upsert" => Some(Self::Upsert),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// Revision 来源种类。
///
/// Business Logic（为什么需要这个枚举）:
///     导入/合并需要知道变更来自文件系统、UI、LAN、Git 还是迁移。
///
/// Code Logic（这个枚举做什么）:
///     camelCase origin tokens。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RevisionOriginKind {
    /// 目标 CLI 本地文件扫描
    Filesystem,
    /// Hub UI 编辑
    Ui,
    /// 局域网 source push
    Lan,
    /// Git snapshot 导入
    Git,
    /// 一次性迁移/纳管
    Migration,
}

impl RevisionOriginKind {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 审计与冲突 UI 需要稳定 origin。
    /// Code Logic: `filesystem` / `ui` / `lan` / `git` / `migration`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Ui => "ui",
            Self::Lan => "lan",
            Self::Git => "git",
            Self::Migration => "migration",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 origin fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "filesystem" => Some(Self::Filesystem),
            "ui" => Some(Self::Ui),
            "lan" => Some(Self::Lan),
            "git" => Some(Self::Git),
            "migration" => Some(Self::Migration),
            _ => None,
        }
    }
}

/// 目标侧期望存在性。
///
/// Business Logic（为什么需要这个枚举）:
///     TargetBinding 用 present/absent 表达是否应投影到该 target，独立于启用开关。
///
/// Code Logic（这个枚举做什么）:
///     `present` / `absent`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DesiredPresence {
    /// 应投影到目标
    Present,
    /// 应不在目标出现
    Absent,
}

impl DesiredPresence {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 投影调度按 presence 决定写入/删除。
    /// Code Logic: `present` / `absent`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 presence fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "present" => Some(Self::Present),
            "absent" => Some(Self::Absent),
            _ => None,
        }
    }
}

/// Materialization 状态。
///
/// Business Logic（为什么需要这个枚举）:
///     UI 与 scheduler 需要细粒度状态解释漂移、冲突、激活与碰撞。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 状态 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaterializationStatus {
    /// 与 desired revision 一致
    Synced,
    /// 等待投影
    Pending,
    /// 外部内容漂移
    Drift,
    /// 已脱离 Hub 管理
    Detached,
    /// 存在未解决 conflict
    Conflict,
    /// 前置条件阻塞
    Blocked,
    /// 目标不支持该资产形态
    Unsupported,
    /// 需要用户激活/确认
    ActivationRequired,
    /// 与外部同名资产碰撞
    ExternalCollision,
}

impl MaterializationStatus {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: 状态机与 Attention 投影依赖稳定 token。
    /// Code Logic: camelCase as_str。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::Pending => "pending",
            Self::Drift => "drift",
            Self::Detached => "detached",
            Self::Conflict => "conflict",
            Self::Blocked => "blocked",
            Self::Unsupported => "unsupported",
            Self::ActivationRequired => "activationRequired",
            Self::ExternalCollision => "externalCollision",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知状态 fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "synced" => Some(Self::Synced),
            "pending" => Some(Self::Pending),
            "drift" => Some(Self::Drift),
            "detached" => Some(Self::Detached),
            "conflict" => Some(Self::Conflict),
            "blocked" => Some(Self::Blocked),
            "unsupported" => Some(Self::Unsupported),
            "activationRequired" => Some(Self::ActivationRequired),
            "externalCollision" => Some(Self::ExternalCollision),
            _ => None,
        }
    }
}

/// 作用域种类。
///
/// Business Logic（为什么需要这个枚举）:
///     user / project / directory 三种节点构成 scope 树，跨设备身份不依赖本机绝对路径。
///
/// Code Logic（这个枚举做什么）:
///     camelCase scope kind tokens。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeKind {
    /// 当前 OS 用户级
    User,
    /// Workbench 项目
    Project,
    /// 项目内相对目录
    Directory,
}

impl ScopeKind {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: scope 身份与 mapping 依赖 kind token。
    /// Code Logic: `user` / `project` / `directory`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Directory => "directory",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知 kind fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "project" => Some(Self::Project),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

/// 跨设备唯一 Revision ID（UUIDv7 字符串）。
///
/// Business Logic（为什么需要这个类型）:
///     Revision 必须跨 Hub 复制且保持原 ID；UUIDv7 提供时间有序与全局唯一。
///
/// Code Logic（这个类型做什么）:
///     透明 newtype String；`new_v7` 包装 `Uuid::now_v7()`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(pub String);

impl RevisionId {
    /// 生成新的 UUIDv7 revision id。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本地接受的新 revision 需要跨设备唯一且时间有序的 ID。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `Uuid::now_v7().to_string()` 包进 newtype。
    pub fn new_v7() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// 以 &str 借用底层 id。
    ///
    /// Business Logic: SQL bind 与日志需要 &str。
    /// Code Logic: 返回内部 String 切片。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for RevisionId {
    /// 从已有字符串构造 RevisionId。
    ///
    /// Business Logic: 导入/读库保留远端原 ID。
    /// Code Logic: 透明包装。
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for RevisionId {
    /// 从 &str 构造 RevisionId。
    ///
    /// Business Logic: 测试与固定 id 场景。
    /// Code Logic: to_string 后包装。
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for RevisionId {
    /// 作为 &str 引用。
    ///
    /// Business Logic: 通用 API 兼容。
    /// Code Logic: 委托 as_str。
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for RevisionId {
    /// 显示底层 id。
    ///
    /// Business Logic: 错误消息与调试输出。
    /// Code Logic: 写内部字符串。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 作用域节点。
///
/// Business Logic（为什么需要这个结构体）:
///     资产必须挂在稳定 scope 下；本机路径只进 mapping 表。
///
/// Code Logic（这个结构体做什么）:
///     保存 id/kind/可选 hub_project_id/相对路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeNode {
    /// 稳定 scope id
    pub id: String,
    /// 作用域种类
    pub kind: ScopeKind,
    /// 项目 scope 的可移植 hub project id（user/directory 可空）
    pub hub_project_id: Option<String>,
    /// directory scope 的规范化相对路径（根为空串）
    pub relative_path: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
}

/// 新建 scope 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     仓储写入需要与完整 ScopeNode 分离的输入形状。
///
/// Code Logic（这个结构体做什么）:
///     不包含 id/created_at（由 repo 生成，或允许调用方指定 id）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewScopeNode {
    /// 可选显式 id；None 时 repo 生成 UUID
    pub id: Option<String>,
    /// 作用域种类
    pub kind: ScopeKind,
    /// 项目 scope 的 hub project id
    pub hub_project_id: Option<String>,
    /// directory 相对路径
    pub relative_path: Option<String>,
}

/// 逻辑资产。
///
/// Business Logic（为什么需要这个结构体）:
///     同一 logical identity 跨设备/lineage 聚合；唯一键为 scope+kind+namespace+key。
///
/// Code Logic（这个结构体做什么）:
///     保存身份字段、策略、当前 head revision 与 tombstone 时间。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogicalAsset {
    /// 本地 LogicalAsset 主键（同时是初始 lineage id）
    pub id: String,
    /// 所属 scope
    pub scope_id: String,
    /// 资产种类
    pub kind: AssetKind,
    /// 来源命名空间（`standalone` 或 `plugin:<id>`）
    pub origin_namespace: String,
    /// 作用域内逻辑键
    pub logical_key: String,
    /// 展示名
    pub display_name: String,
    /// 共享策略
    pub policy: AssetPolicy,
    /// 当前 head revision（无 revision 时为 None）
    pub current_revision_id: Option<RevisionId>,
    /// 删除时间；None 表示未删除
    pub deleted_at: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
    /// 更新时间 RFC3339
    pub updated_at: String,
}

/// 新建逻辑资产输入。
///
/// Business Logic（为什么需要这个结构体）:
///     insert_asset 只需调用方提供身份与策略字段。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/current_revision/deleted_at/时间戳。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLogicalAsset {
    /// 所属 scope
    pub scope_id: String,
    /// 资产种类
    pub kind: AssetKind,
    /// 来源命名空间
    pub origin_namespace: String,
    /// 逻辑键
    pub logical_key: String,
    /// 展示名
    pub display_name: String,
    /// 共享策略
    pub policy: AssetPolicy,
}

/// 不可变 Revision。
///
/// Business Logic（为什么需要这个结构体）:
///     每次接受的编辑形成跨 Hub 可复制 DAG 节点，而非仅本机线性链。
///
/// Code Logic（这个结构体做什么）:
///     保存 parents、generation、operation、origin 与内容 hash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    /// 跨设备唯一 UUIDv7
    pub id: RevisionId,
    /// 创建该历史分支的原始 LogicalAsset id
    pub asset_lineage_id: String,
    /// 父 revision 列表（顺序保留）
    pub parents: Vec<RevisionId>,
    /// max(parent.generation)+1；无 parent 为 0
    pub generation: u64,
    /// upsert 或 delete
    pub operation: RevisionOperation,
    /// 变更来源种类
    pub origin_kind: RevisionOriginKind,
    /// 来源 target（可空）
    pub origin_target: Option<AgentTarget>,
    /// 来源 replica / device id
    pub origin_replica_id: String,
    /// 单文件内容 SHA-256 hex
    pub payload_hash: Option<String>,
    /// 目录 TreeManifest hash
    pub tree_manifest_hash: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
}

/// 追加 revision 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     append_revision 由调用方提供 id/parents/内容元数据；generation 由 repo 计算。
///
/// Code Logic（这个结构体做什么）:
///     不含 generation。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRevision {
    /// 调用方提供的 UUIDv7（导入时保留原 ID）
    pub id: RevisionId,
    /// lineage id（通常为 asset.id）
    pub asset_lineage_id: String,
    /// 父 revision 列表
    pub parents: Vec<RevisionId>,
    /// upsert / delete
    pub operation: RevisionOperation,
    /// 来源种类
    pub origin_kind: RevisionOriginKind,
    /// 来源 target
    pub origin_target: Option<AgentTarget>,
    /// 来源 replica id
    pub origin_replica_id: String,
    /// 内容 hash（delete 必须为 None）
    pub payload_hash: Option<String>,
    /// 目录 manifest hash
    pub tree_manifest_hash: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
}

/// Target 绑定（desired 状态）。
///
/// Business Logic（为什么需要这个结构体）:
///     每个 asset×target×checkout 有独立 desired presence 与 target-local enabled。
///
/// Code Logic（这个结构体做什么）:
///     保存绑定键与 desired 字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetBinding {
    /// 绑定主键
    pub id: String,
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// 本地 scope mapping id（可选，user 级可空）
    pub local_scope_mapping_id: Option<String>,
    /// checkout binding id（可选）
    pub checkout_binding_id: Option<String>,
    /// 期望存在性
    pub desired_presence: DesiredPresence,
    /// target-local 启用开关（false 不跨 target 传播）
    pub desired_enabled: bool,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// 新建 TargetBinding 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     仓储写入 desired 状态时不要求调用方填时间戳。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/时间戳。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTargetBinding {
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// 本地 scope mapping id
    pub local_scope_mapping_id: Option<String>,
    /// checkout binding id
    pub checkout_binding_id: Option<String>,
    /// 期望存在性
    pub desired_presence: DesiredPresence,
    /// target-local 启用
    pub desired_enabled: bool,
}

/// 本机 materialization 观测状态。
///
/// Business Logic（为什么需要这个结构体）:
///     投影成功/漂移/冲突必须可查询；状态不进可移植 snapshot。
///
/// Code Logic（这个结构体做什么）:
///     绑定 asset×target×路径与 hash/status。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Materialization {
    /// 主键
    pub id: String,
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// target binding id
    pub target_binding_id: String,
    /// 原生目标路径或 registry key
    pub native_path: Option<String>,
    /// 上次成功投影的 revision
    pub last_projected_revision_id: Option<RevisionId>,
    /// 渲染后内容 hash
    pub rendered_hash: Option<String>,
    /// 最后观察到的外部 hash
    pub observed_external_hash: Option<String>,
    /// 状态
    pub status: MaterializationStatus,
    /// 最近错误摘要
    pub last_error: Option<String>,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// Agent Hub 冲突记录。
///
/// Business Logic（为什么需要这个结构体）:
///     未解决 conflict 必须冻结受影响投影并进入 Attention。
///
/// Code Logic（这个结构体做什么）:
///     保存 base/current/external 与解决状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHubConflict {
    /// 主键
    pub id: String,
    /// 逻辑资产 id
    pub asset_id: String,
    /// 受影响 target（None 表示 common/canonical 冲突）
    pub target: Option<AgentTarget>,
    /// base revision
    pub base_revision_id: Option<RevisionId>,
    /// Hub current revision
    pub hub_revision_id: Option<RevisionId>,
    /// 外部 revision（若可识别）
    pub external_revision_id: Option<RevisionId>,
    /// 冲突详情 JSON
    pub detail_json: String,
    /// 是否已解决
    pub resolved: bool,
    /// 创建时间
    pub created_at: String,
    /// 解决时间
    pub resolved_at: Option<String>,
}
