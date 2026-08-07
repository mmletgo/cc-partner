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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
///     并发写同一 asset head 时需要 expected parent CAS，避免后写静默覆盖先写。
///
/// Code Logic（这个结构体做什么）:
///     不含 generation；`expected_parent_id` 为 None 时按 parents 推导 CAS 条件。
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
    /// 期望当前 head（单 parent upsert CAS）；None 表示按 parents 推导
    /// （migration 首 revision / multi-parent merge 等）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_parent_id: Option<RevisionId>,
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

/// 投影 job 持久状态。
///
/// Business Logic（为什么需要这个枚举）:
///     文件系统与 DB 无法单事务，必须用 job ledger 对账 crash recovery。
///     Gate B package 激活扩展：prepared → packageWritten → activationRequested
///     → activationVerified → committed；ActivationRequired/Unsupported 永不 committed/full。
///
/// Code Logic（这个枚举做什么）:
///     prepared/writing/packageWritten/activationRequested/activationVerified/
///     committed/failed/blocked/drifted。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectionJobState {
    /// 已入队，尚未写盘
    Prepared,
    /// 正在写临时文件/原子替换
    Writing,
    /// managed package 已物化到 materialized-packages（Gate B）
    PackageWritten,
    /// 已请求 CLI 激活（marketplace install / native verify）
    ActivationRequested,
    /// 已 inspect CLI 状态并确认激活结果
    ActivationVerified,
    /// materialization 已提交
    Committed,
    /// 可重试失败
    Failed,
    /// 冲突/前置条件阻塞
    Blocked,
    /// 目标漂移，需 reconcile
    Drifted,
}

impl ProjectionJobState {
    /// 稳定 wire/DB 字符串。
    ///
    /// Business Logic: job ledger 与 crash recovery 依赖稳定 token。
    /// Code Logic: camelCase as_str。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Writing => "writing",
            Self::PackageWritten => "packageWritten",
            Self::ActivationRequested => "activationRequested",
            Self::ActivationVerified => "activationVerified",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Drifted => "drifted",
        }
    }

    /// 解析 wire/DB 字符串。
    ///
    /// Business Logic: 未知状态 fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prepared" => Some(Self::Prepared),
            "writing" => Some(Self::Writing),
            "packageWritten" => Some(Self::PackageWritten),
            "activationRequested" => Some(Self::ActivationRequested),
            "activationVerified" => Some(Self::ActivationVerified),
            "committed" => Some(Self::Committed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            "drifted" => Some(Self::Drifted),
            _ => None,
        }
    }

    /// 是否仍处于未完成、可恢复状态。
    ///
    /// Business Logic: owner 启动时对账 prepared/writing 与 Gate B 激活中间态。
    /// Code Logic: prepared|writing|packageWritten|activationRequested|activationVerified。
    pub fn is_recoverable(self) -> bool {
        matches!(
            self,
            Self::Prepared
                | Self::Writing
                | Self::PackageWritten
                | Self::ActivationRequested
                | Self::ActivationVerified
        )
    }

    /// 是否为 package 激活管线中间态（Gate B）。
    ///
    /// Business Logic: recovery 时先 inspect CLI 再决定是否重复命令。
    /// Code Logic: packageWritten|activationRequested|activationVerified。
    pub fn is_package_activation_phase(self) -> bool {
        matches!(
            self,
            Self::PackageWritten | Self::ActivationRequested | Self::ActivationVerified
        )
    }
}

/// 投影 payload 形态。
///
/// Business Logic（为什么需要这个枚举）:
///     单文件与目录（Skill/Plugin）原子替换策略不同。
///
/// Code Logic（这个枚举做什么）:
///     `file` / `directory`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectionPayloadKind {
    /// 单文件原子替换
    File,
    /// 目录 sibling staging + backup rename
    Directory,
}

impl ProjectionPayloadKind {
    /// 稳定 wire/DB 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }

    /// 解析 wire/DB 字符串。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

/// 持久 projection job。
///
/// Business Logic（为什么需要这个结构体）:
///     crash 后必须根据 job + 实际 hash 判定继续/回滚/re-reconcile，不能只信 DB。
///
/// Code Logic（这个结构体做什么）:
///     保存目标路径、expected/rendered hash、CAS 对象引用与状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionJob {
    /// 主键
    pub id: String,
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// target binding id
    pub target_binding_id: String,
    /// 期望 revision
    pub desired_revision_id: Option<RevisionId>,
    /// job 状态
    pub state: ProjectionJobState,
    /// 尝试次数
    pub attempt: u32,
    /// 最近错误
    pub last_error: Option<String>,
    /// 目标绝对路径
    pub target_path: String,
    /// 写前期望的外部 hash（空=目标不存在）
    pub expected_external_hash: Option<String>,
    /// 渲染后内容 hash
    pub rendered_hash: String,
    /// CAS 中渲染正文/树 manifest 的 object hash
    pub rendered_object_hash: String,
    /// 去环 write token
    pub write_token: String,
    /// desired presence
    pub desired_presence: DesiredPresence,
    /// desired enabled
    pub desired_enabled: bool,
    /// file 或 directory
    pub payload_kind: ProjectionPayloadKind,
    /// 目录投影受管相对路径 JSON 数组
    pub managed_paths_json: Option<String>,
    /// 关联 hub project（opt-in 过滤；user scope 为空）
    pub hub_project_id: Option<String>,
    /// 临时 staging 路径
    pub staging_path: Option<String>,
    /// 目录备份路径
    pub backup_path: Option<String>,
    /// 写前 base hash 快照
    pub base_hash: Option<String>,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// 新建 projection job 输入（入队前）。
///
/// Business Logic（为什么需要这个结构体）:
///     scheduler 入队只需业务字段，id/时间戳由仓储生成。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/state/attempt。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProjectionJob {
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// target binding id
    pub target_binding_id: String,
    /// 期望 revision
    pub desired_revision_id: Option<RevisionId>,
    /// 目标绝对路径
    pub target_path: String,
    /// 写前期望外部 hash
    pub expected_external_hash: Option<String>,
    /// 渲染 hash
    pub rendered_hash: String,
    /// CAS object hash
    pub rendered_object_hash: String,
    /// write token
    pub write_token: String,
    /// desired presence
    pub desired_presence: DesiredPresence,
    /// desired enabled
    pub desired_enabled: bool,
    /// payload 形态
    pub payload_kind: ProjectionPayloadKind,
    /// 受管路径 JSON
    pub managed_paths_json: Option<String>,
    /// hub project id
    pub hub_project_id: Option<String>,
    /// base hash
    pub base_hash: Option<String>,
}

/// 纳管事务状态（Gate B Task 6）。
///
/// Business Logic: prepared→activated→archived→committed；失败/碰撞保留唯一发现源。
/// Code Logic: camelCase DB token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdoptionState {
    /// 已写入 prepared 行
    Prepared,
    /// package 已物化并激活
    Activated,
    /// 原树已进 CAS 且 legacy 已 rename 到 staging
    Archived,
    /// DB 已 commit，staging 可删
    Committed,
    /// 外部碰撞
    ExternalCollision,
    /// 阻塞
    Blocked,
    /// 失败（源应仍在）
    Failed,
}

impl AdoptionState {
    /// 稳定 DB token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Activated => "activated",
            Self::Archived => "archived",
            Self::Committed => "committed",
            Self::ExternalCollision => "externalCollision",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }

    /// 解析 DB token。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prepared" => Some(Self::Prepared),
            "activated" => Some(Self::Activated),
            "archived" => Some(Self::Archived),
            "committed" => Some(Self::Committed),
            "externalCollision" => Some(Self::ExternalCollision),
            "blocked" => Some(Self::Blocked),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// DB 中的 adoption 行。
///
/// Business Logic: crash recovery 用 hash 完成或还原源。
/// Code Logic: camelCase 持久化字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptionRecord {
    /// 主键
    pub id: String,
    /// 逻辑资产 id（成功后填）
    pub asset_id: Option<String>,
    /// 目标
    pub target: AgentTarget,
    /// 源路径
    pub origin_path: String,
    /// 源 tree hash（prepared 时）
    pub origin_tree_hash: String,
    /// archive CAS tree hash
    pub archive_tree_hash: Option<String>,
    /// materialization id
    pub materialization_id: Option<String>,
    /// package id
    pub package_id: Option<String>,
    /// staging 绝对路径（rename 目标）
    pub staging_path: Option<String>,
    /// 状态
    pub state: AdoptionState,
    /// 最近错误
    pub last_error: Option<String>,
    /// 是否确认
    pub confirmed: bool,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// 用户级指令文件所有权记录。
///
/// Business Logic: package adoption 与指令文件纳管的恢复/删除合同不同，必须独立持久化。
/// Code Logic: 按 asset+target 记录用户确认路径、hash、revision 和 plan token。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionOwnershipRecord {
    /// 逻辑资产
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// preview/apply 共享的解析绝对路径
    pub resolved_path: String,
    /// 纳管时的外部 hash（create 为 None）
    pub adopted_hash: Option<String>,
    /// 纳管时 canonical revision
    pub adopted_revision_id: Option<RevisionId>,
    /// create/update/adopt 等稳定动作 token
    pub adoption_operation: String,
    /// 用户确认的 V2 plan token
    pub confirmed_plan_token: String,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

/// 用户级指令预览计划持久化记录。
///
/// Business Logic: GuiClient 与 sidecar 跨请求 apply 必须使用 owner 持久的短期计划，不信任客户端回传 diff。
/// Code Logic: token 索引原始计划 JSON，并保留消费/idempotency 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInstructionPlanRecord {
    /// 不可猜短期 token
    pub plan_token: String,
    /// 当前 OS 用户/配置根指纹
    pub owner_fingerprint: String,
    /// 过期时间 RFC3339
    pub expires_at: String,
    /// canonical base revision
    pub base_revision_id: Option<RevisionId>,
    /// inventory 快照 hash
    pub inventory_snapshot_hash: String,
    /// 原始计划 JSON（不记录）
    pub plan_json: String,
    /// 首次 apply 幂等键
    pub client_request_id: Option<String>,
    /// 原子 claim 时间
    pub claimed_at: Option<String>,
    /// 已消费时间
    pub consumed_at: Option<String>,
    /// 幂等返回结果 JSON
    pub result_json: Option<String>,
    /// 创建时间
    pub created_at: String,
}

/// V2 preview plan 原子 claim 结果。
///
/// Business Logic: 同 token 只能有一个 apply 执行者，同 id 重试返回 pending/result。
/// Code Logic: Claimed 携带计划；Pending 表示同 id 执行中；Replay 携带持久化 JSON 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInstructionPlanClaim {
    /// 本请求获得执行权
    Claimed(UserInstructionPlanRecord),
    /// 同 id 请求正在执行
    Pending,
    /// 同 id 已完成
    Replay(String),
}

/// Portable 资产动作预览计划持久化记录。
///
/// Business Logic: owner 持久短期 plan；GuiClient 只回传不可猜 token + clientRequestId。
/// Code Logic: 与 user_instruction_plans 同形字段；result_json 存精确幂等结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAssetActionPlanRecord {
    /// 不可猜短期 token
    pub plan_token: String,
    /// 当前 owner 指纹
    pub owner_fingerprint: String,
    /// 过期时间 RFC3339
    pub expires_at: String,
    /// inventory 快照 hash
    pub inventory_snapshot_hash: String,
    /// 原始计划 JSON
    pub plan_json: String,
    /// 首次 apply 幂等键
    pub client_request_id: Option<String>,
    /// 原子 claim 时间
    pub claimed_at: Option<String>,
    /// 已消费时间
    pub consumed_at: Option<String>,
    /// 幂等返回结果 JSON
    pub result_json: Option<String>,
    /// 创建时间
    pub created_at: String,
}

/// Portable 动作 plan 原子 claim 结果。
///
/// Business Logic: 同 token 只能有一个 apply 执行者；同 id 重试 pending/replay。
/// Code Logic: Claimed/Pending/Replay 三态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortableActionClaim {
    /// 本请求获得执行权
    Claimed(PortableAssetActionPlanRecord),
    /// 同 id 请求正在执行
    Pending,
    /// 同 id 已完成（精确 result JSON）
    Replay(String),
}

/// 新建 materialization 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     投影成功后 upsert materialization 观测状态。
///
/// Code Logic（这个结构体做什么）:
///     不含 id/时间戳。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMaterialization {
    /// 逻辑资产 id
    pub asset_id: String,
    /// 目标 CLI
    pub target: AgentTarget,
    /// target binding id
    pub target_binding_id: String,
    /// 原生路径
    pub native_path: Option<String>,
    /// 上次成功 revision
    pub last_projected_revision_id: Option<RevisionId>,
    /// 渲染 hash
    pub rendered_hash: Option<String>,
    /// 观测外部 hash
    pub observed_external_hash: Option<String>,
    /// 状态
    pub status: MaterializationStatus,
    /// 最近错误
    pub last_error: Option<String>,
}

/// 未解决 conflict 摘要（调度冻结用）。
///
/// Business Logic（为什么需要这个结构体）:
///     canonical conflict 冻结资产全部 target；target conflict 仅冻结该 target。
///
/// Code Logic（这个结构体做什么）:
///     asset_id + optional target。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFreezeKey {
    /// 资产 id
    pub asset_id: String,
    /// None = canonical 级冲突
    pub target: Option<AgentTarget>,
}

/// Target-local 意图（presence / enabled / restore / 全网删除）。
///
/// Business Logic（为什么需要这个枚举）:
///     启停与删除必须是显式意图；单 target 删除不得猜 canonical tombstone。
///
/// Code Logic（这个枚举做什么）:
///     输入到 `TargetBinding::apply_intent` 的意图表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetBindingIntent {
    /// 设置 desiredPresence（present / absent）
    SetPresence(DesiredPresence),
    /// 设置 desiredEnabled（adapter disable 策略）
    SetEnabled(bool),
    /// 从 detached 恢复并调度投影
    RestoreDetached,
    /// 从所有 target 删除（canonical tombstone + fan-out）
    DeleteEverywhere,
}

/// Adapter 声明的 disable 策略。
///
/// Business Logic（为什么需要这个枚举）:
///     desiredEnabled=false 不得一律删除或一律留文件；Codex 用 remove-with-binding-retained。
///
/// Code Logic（这个枚举做什么）:
///     稳定 token 供 activator / 测试断言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetDisableStrategy {
    /// 卸载/移除插件但保留 binding 与 desiredPresence
    RemoveWithBindingRetained,
    /// 仅翻转 enabled 标记（文件可保留，依赖 target 配置）
    ToggleEnabledFlag,
}

impl TargetDisableStrategy {
    /// 稳定 wire/DB 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoveWithBindingRetained => "remove_with_binding_retained",
            Self::ToggleEnabledFlag => "toggle_enabled_flag",
        }
    }

    /// 解析稳定 token。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "remove_with_binding_retained" => Some(Self::RemoveWithBindingRetained),
            "toggle_enabled_flag" => Some(Self::ToggleEnabledFlag),
            _ => None,
        }
    }

    /// 按 target 返回 adapter 声明策略。
    ///
    /// Business Logic: Codex 为 remove-with-binding-retained；Claude/OpenCode 默认同策略。
    /// Code Logic: 静态表。
    pub fn for_target(target: AgentTarget) -> Self {
        match target {
            AgentTarget::Codex => Self::RemoveWithBindingRetained,
            AgentTarget::Claude | AgentTarget::OpenCode => Self::RemoveWithBindingRetained,
        }
    }
}

/// 资产级聚合状态（派生，不可写）。
///
/// Business Logic（为什么需要这个枚举）:
///     UI/API 不能仅凭 package write 成功推断 full；需汇总所有 requested target。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 派生 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssetAggregateStatus {
    /// 尚未选择任何 target；中性状态，不是部分失败
    Unconfigured,
    /// 每个 requested target 均 supported/present/enabled-as-desired/verified
    Full,
    /// 部分 target 未达标
    Partial,
    /// 仅有 source 表示，无可投影 materialization
    SourceOnly,
    /// 需要用户激活
    ActivationRequired,
    /// 与外部同名资产碰撞
    ExternalCollision,
    /// 外部整文件/目录删除后 detached
    Detached,
    /// 写/投影被阻塞
    Blocked,
}

impl AssetAggregateStatus {
    /// 稳定 wire token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Full => "full",
            Self::Partial => "partial",
            Self::SourceOnly => "sourceOnly",
            Self::ActivationRequired => "activationRequired",
            Self::ExternalCollision => "externalCollision",
            Self::Detached => "detached",
            Self::Blocked => "blocked",
        }
    }

    /// 解析 wire token。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unconfigured" => Some(Self::Unconfigured),
            "full" => Some(Self::Full),
            "partial" => Some(Self::Partial),
            "sourceOnly" => Some(Self::SourceOnly),
            "activationRequired" => Some(Self::ActivationRequired),
            "externalCollision" => Some(Self::ExternalCollision),
            "detached" => Some(Self::Detached),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// apply_intent 结果动作（不直接写盘；由 service/scheduler 执行）。
///
/// Business Logic（为什么需要这个枚举）:
///     把允许的状态转移集中成表，避免命令层各自猜测 tombstone/fan-out。
///
/// Code Logic（这个枚举做什么）:
///     描述 binding 更新、投影调度、tombstone 与拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetBindingTransition {
    /// 仅更新 target-local enabled；canonical revision 不变
    UpdateEnabled {
        /// 新 desired_enabled
        desired_enabled: bool,
        /// adapter disable 策略
        disable_strategy: TargetDisableStrategy,
        /// 是否调度投影（disable/enable 需要）
        schedule_projection: bool,
    },
    /// 更新 target-local desiredPresence
    UpdatePresence {
        /// 新 desired_presence
        desired_presence: DesiredPresence,
        /// Absent 时保留 binding，只移除该 target 物化
        remove_owned_materialization_only: bool,
        /// 调度投影
        schedule_projection: bool,
    },
    /// 从 detached 恢复
    RestoreDetached {
        /// 恢复后 desired_presence
        desired_presence: DesiredPresence,
        /// 必须调度投影（禁止静默 no-op）
        schedule_projection: bool,
        /// 清除 detached 观测
        clear_detached_status: bool,
    },
    /// 全 target 删除：一条 canonical tombstone + fan-out Absent
    DeleteEverywhere {
        /// 生成一条 delete revision
        append_canonical_tombstone: bool,
        /// 所有 binding → Absent + disabled
        fan_out_absent: bool,
    },
    /// 拒绝：targetOnly 最后一 target 删除必须显式 everywhere
    RejectLastTargetOnlyRequiresEverywhere {
        /// 稳定错误 token
        code: String,
    },
    /// 拒绝：未知/漂移路径阻塞删除，返回精确 preview
    RejectRemovalBlocked {
        /// 稳定错误 token
        code: String,
        /// 阻塞路径预览
        preview_paths: Vec<String>,
    },
}

/// 单 target 观测输入（聚合状态计算用）。
///
/// Business Logic（为什么需要这个结构体）:
///     full 需要每个 requested target 都 supported/present/enabled/verified。
///
/// Code Logic（这个结构体做什么）:
///     纯输入快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetStatusSnapshot {
    /// 是否在 requested 集合中
    pub requested: bool,
    /// desired presence
    pub desired_presence: DesiredPresence,
    /// desired enabled
    pub desired_enabled: bool,
    /// 是否 supported
    pub supported: bool,
    /// 是否仅 sourceOnly（无可投影表示）
    pub source_only: bool,
    /// materialization 状态
    pub materialization_status: Option<MaterializationStatus>,
    /// 是否 verified（activation/list 通过）
    pub verified: bool,
}

impl TargetBinding {
    /// 应用显式意图，返回允许的状态转移。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     disable 一列不关其它 CLI；Absent 只卸本 target；整文件外部删除 → detached 不自动重建；
    ///     restore 调度投影；delete_everywhere 一条 tombstone；targetOnly 最后一 target 不得猜 everywhere。
    ///
    /// Code Logic（这个函数做什么）:
    ///     纯函数转移表；不写库。
    pub fn apply_intent(
        &self,
        intent: TargetBindingIntent,
        materialization_status: Option<MaterializationStatus>,
        policy: AssetPolicy,
        present_binding_count: usize,
        removal_blocked_paths: &[String],
    ) -> TargetBindingTransition {
        match intent {
            TargetBindingIntent::SetEnabled(enabled) => TargetBindingTransition::UpdateEnabled {
                desired_enabled: enabled,
                disable_strategy: TargetDisableStrategy::for_target(self.target),
                schedule_projection: true,
            },
            TargetBindingIntent::SetPresence(DesiredPresence::Present) => {
                TargetBindingTransition::UpdatePresence {
                    desired_presence: DesiredPresence::Present,
                    remove_owned_materialization_only: false,
                    schedule_projection: true,
                }
            }
            TargetBindingIntent::SetPresence(DesiredPresence::Absent) => {
                // targetOnly 且为最后一 present binding：不得隐式 tombstone
                if policy == AssetPolicy::TargetOnly
                    && self.desired_presence == DesiredPresence::Present
                    && present_binding_count <= 1
                {
                    return TargetBindingTransition::RejectLastTargetOnlyRequiresEverywhere {
                        code: "agent_hub_target_only_last_target_requires_everywhere".into(),
                    };
                }
                if !removal_blocked_paths.is_empty() {
                    return TargetBindingTransition::RejectRemovalBlocked {
                        code: "agent_hub_removal_blocked_unknown_or_changed_paths".into(),
                        preview_paths: removal_blocked_paths.to_vec(),
                    };
                }
                TargetBindingTransition::UpdatePresence {
                    desired_presence: DesiredPresence::Absent,
                    remove_owned_materialization_only: true,
                    schedule_projection: true,
                }
            }
            TargetBindingIntent::RestoreDetached => {
                // 无论当前 materialization 是否已 detached，restore 都强制 present + schedule
                let _ = materialization_status;
                TargetBindingTransition::RestoreDetached {
                    desired_presence: DesiredPresence::Present,
                    schedule_projection: true,
                    clear_detached_status: true,
                }
            }
            TargetBindingIntent::DeleteEverywhere => {
                // 与 Absent 同契约：未知/变更路径必须 fail-closed，禁止 tombstone 或 fan-out。
                if !removal_blocked_paths.is_empty() {
                    return TargetBindingTransition::RejectRemovalBlocked {
                        code: "agent_hub_removal_blocked_unknown_or_changed_paths".into(),
                        preview_paths: removal_blocked_paths.to_vec(),
                    };
                }
                TargetBindingTransition::DeleteEverywhere {
                    append_canonical_tombstone: true,
                    fan_out_absent: true,
                }
            }
        }
    }
}

/// 由各 target 快照计算资产聚合状态。
///
/// Business Logic（为什么需要这个函数）:
///     full 要求全部 requested target supported + present + enabled-as-desired + verified；
///     任一 unsupported/sourceOnly/activationRequired/externalCollision/detached/blocked → 对应/partial。
///
/// Code Logic（这个函数做什么）:
///     优先级：Blocked > ExternalCollision > Detached > ActivationRequired > SourceOnly > Partial > Full。
pub fn compute_asset_aggregate_status(targets: &[TargetStatusSnapshot]) -> AssetAggregateStatus {
    let requested: Vec<&TargetStatusSnapshot> = targets.iter().filter(|t| t.requested).collect();
    if requested.is_empty() {
        return AssetAggregateStatus::Unconfigured;
    }

    let mut any_blocked = false;
    let mut any_collision = false;
    let mut any_detached = false;
    let mut any_activation = false;
    let mut any_source_only = false;
    let mut any_partial = false;

    for t in &requested {
        if t.source_only {
            any_source_only = true;
            continue;
        }
        if !t.supported {
            any_partial = true;
            continue;
        }
        match t.materialization_status {
            Some(MaterializationStatus::Blocked) | Some(MaterializationStatus::Unsupported) => {
                any_blocked = true;
            }
            Some(MaterializationStatus::ExternalCollision) => any_collision = true,
            Some(MaterializationStatus::Detached) => any_detached = true,
            Some(MaterializationStatus::ActivationRequired) => any_activation = true,
            Some(MaterializationStatus::Synced) => {
                if t.desired_presence == DesiredPresence::Present && !t.verified {
                    // package write 成功但未 verified → 不得 full
                    any_partial = true;
                }
                if t.desired_presence == DesiredPresence::Present && !t.desired_enabled {
                    // disabled-as-desired 且 synced 可计为达标；enabled mismatch 走 partial
                }
            }
            Some(MaterializationStatus::Pending)
            | Some(MaterializationStatus::Drift)
            | Some(MaterializationStatus::Conflict)
            | None => {
                if t.desired_presence == DesiredPresence::Present {
                    any_partial = true;
                }
            }
        }
        if t.desired_presence == DesiredPresence::Present
            && t.desired_enabled
            && t.materialization_status == Some(MaterializationStatus::Synced)
            && !t.verified
        {
            any_partial = true;
        }
    }

    if any_blocked {
        return AssetAggregateStatus::Blocked;
    }
    if any_collision {
        return AssetAggregateStatus::ExternalCollision;
    }
    if any_detached {
        return AssetAggregateStatus::Detached;
    }
    if any_activation {
        return AssetAggregateStatus::ActivationRequired;
    }
    if any_source_only && requested.iter().all(|t| t.source_only || !t.supported) {
        return AssetAggregateStatus::SourceOnly;
    }
    if any_source_only || any_partial {
        return AssetAggregateStatus::Partial;
    }

    let all_ok = requested.iter().all(|t| {
        if t.source_only || !t.supported {
            return false;
        }
        match t.desired_presence {
            DesiredPresence::Absent => {
                matches!(
                    t.materialization_status,
                    Some(MaterializationStatus::Synced) | None
                )
            }
            DesiredPresence::Present => {
                t.materialization_status == Some(MaterializationStatus::Synced) && t.verified
            }
        }
    });
    if all_ok {
        AssetAggregateStatus::Full
    } else {
        AssetAggregateStatus::Partial
    }
}

#[cfg(test)]
mod target_presence_tests {
    use super::*;

    fn sample_binding(
        target: AgentTarget,
        presence: DesiredPresence,
        enabled: bool,
    ) -> TargetBinding {
        TargetBinding {
            id: format!("b-{}", target.as_str()),
            asset_id: "asset-1".into(),
            target,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: presence,
            desired_enabled: enabled,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        }
    }

    /// Business Logic: disable 一 target 不得改写其它 binding 的语义结果（仅本 binding intent）。
    /// Code Logic: SetEnabled(false) → UpdateEnabled + disable strategy；presence 保持由调用方保留。
    #[test]
    fn disable_one_target_uses_adapter_strategy_without_presence_change() {
        let binding = sample_binding(AgentTarget::Codex, DesiredPresence::Present, true);
        let transition = binding.apply_intent(
            TargetBindingIntent::SetEnabled(false),
            Some(MaterializationStatus::Synced),
            AssetPolicy::Shared,
            2,
            &[],
        );
        match transition {
            TargetBindingTransition::UpdateEnabled {
                desired_enabled,
                disable_strategy,
                schedule_projection,
            } => {
                assert!(!desired_enabled);
                assert_eq!(
                    disable_strategy,
                    TargetDisableStrategy::RemoveWithBindingRetained
                );
                assert!(schedule_projection);
            }
            other => panic!("unexpected {other:?}"),
        }
        // presence 不变：intent 不包含 UpdatePresence
        assert_eq!(binding.desired_presence, DesiredPresence::Present);
    }

    /// Business Logic: desiredPresence=absent 只卸本 target 物化。
    #[test]
    fn absent_removes_only_owned_materialization() {
        let binding = sample_binding(AgentTarget::Claude, DesiredPresence::Present, true);
        let transition = binding.apply_intent(
            TargetBindingIntent::SetPresence(DesiredPresence::Absent),
            Some(MaterializationStatus::Synced),
            AssetPolicy::Shared,
            2,
            &[],
        );
        match transition {
            TargetBindingTransition::UpdatePresence {
                desired_presence,
                remove_owned_materialization_only,
                schedule_projection,
            } => {
                assert_eq!(desired_presence, DesiredPresence::Absent);
                assert!(remove_owned_materialization_only);
                assert!(schedule_projection);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: 未知/变更路径阻塞删除并返回精确 preview。
    #[test]
    fn unknown_paths_block_removal_with_preview() {
        let binding = sample_binding(AgentTarget::Claude, DesiredPresence::Present, true);
        let blocked = vec!["extra.md".into(), "nested/secret".into()];
        let transition = binding.apply_intent(
            TargetBindingIntent::SetPresence(DesiredPresence::Absent),
            Some(MaterializationStatus::Synced),
            AssetPolicy::Shared,
            2,
            &blocked,
        );
        match transition {
            TargetBindingTransition::RejectRemovalBlocked {
                code,
                preview_paths,
            } => {
                assert_eq!(code, "agent_hub_removal_blocked_unknown_or_changed_paths");
                assert_eq!(preview_paths, blocked);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: restore_detached 必须 schedule projection。
    #[test]
    fn restore_detached_schedules_projection() {
        let binding = sample_binding(AgentTarget::OpenCode, DesiredPresence::Present, true);
        let transition = binding.apply_intent(
            TargetBindingIntent::RestoreDetached,
            Some(MaterializationStatus::Detached),
            AssetPolicy::Shared,
            1,
            &[],
        );
        match transition {
            TargetBindingTransition::RestoreDetached {
                desired_presence,
                schedule_projection,
                clear_detached_status,
            } => {
                assert_eq!(desired_presence, DesiredPresence::Present);
                assert!(schedule_projection);
                assert!(clear_detached_status);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: delete_everywhere 一条 canonical tombstone + fan-out。
    #[test]
    fn delete_everywhere_appends_one_tombstone_and_fans_out() {
        let binding = sample_binding(AgentTarget::Claude, DesiredPresence::Present, true);
        let transition = binding.apply_intent(
            TargetBindingIntent::DeleteEverywhere,
            Some(MaterializationStatus::Synced),
            AssetPolicy::Shared,
            3,
            &[],
        );
        match transition {
            TargetBindingTransition::DeleteEverywhere {
                append_canonical_tombstone,
                fan_out_absent,
            } => {
                assert!(append_canonical_tombstone);
                assert!(fan_out_absent);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: DeleteEverywhere 与 Absent 同 fail-closed，未知路径阻塞 tombstone。
    #[test]
    fn unknown_paths_block_delete_everywhere_with_preview() {
        let binding = sample_binding(AgentTarget::Codex, DesiredPresence::Present, true);
        let blocked = vec!["plugin/extra.toml".into()];
        let transition = binding.apply_intent(
            TargetBindingIntent::DeleteEverywhere,
            Some(MaterializationStatus::Synced),
            AssetPolicy::Shared,
            2,
            &blocked,
        );
        match transition {
            TargetBindingTransition::RejectRemovalBlocked {
                code,
                preview_paths,
            } => {
                assert_eq!(code, "agent_hub_removal_blocked_unknown_or_changed_paths");
                assert_eq!(preview_paths, blocked);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: targetOnly 最后一 target 删除必须显式 everywhere，不得猜测。
    #[test]
    fn target_only_last_target_delete_requires_everywhere() {
        let binding = sample_binding(AgentTarget::Claude, DesiredPresence::Present, true);
        let transition = binding.apply_intent(
            TargetBindingIntent::SetPresence(DesiredPresence::Absent),
            Some(MaterializationStatus::Synced),
            AssetPolicy::TargetOnly,
            1,
            &[],
        );
        match transition {
            TargetBindingTransition::RejectLastTargetOnlyRequiresEverywhere { code } => {
                assert_eq!(
                    code,
                    "agent_hub_target_only_last_target_requires_everywhere"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Business Logic: full 需要 verified；仅 package write 成功不够。
    #[test]
    fn aggregate_full_requires_verified_not_just_package_write() {
        let snaps = vec![TargetStatusSnapshot {
            requested: true,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            supported: true,
            source_only: false,
            materialization_status: Some(MaterializationStatus::Synced),
            verified: false,
        }];
        assert_eq!(
            compute_asset_aggregate_status(&snaps),
            AssetAggregateStatus::Partial
        );

        let snaps_ok = vec![TargetStatusSnapshot {
            requested: true,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
            supported: true,
            source_only: false,
            materialization_status: Some(MaterializationStatus::Synced),
            verified: true,
        }];
        assert_eq!(
            compute_asset_aggregate_status(&snaps_ok),
            AssetAggregateStatus::Full
        );
    }

    /// Business Logic: 用户尚未选择任何 target 是中性未配置，不是 partial 失败。
    /// Code Logic: 空 requested 集合返回 Unconfigured。
    #[test]
    fn aggregate_without_requested_targets_is_unconfigured() {
        assert_eq!(
            compute_asset_aggregate_status(&[]),
            AssetAggregateStatus::Unconfigured
        );
    }

    /// Business Logic: detached / activationRequired / externalCollision 有独立聚合态。
    #[test]
    fn aggregate_priority_for_detached_activation_collision_blocked() {
        assert_eq!(
            compute_asset_aggregate_status(&[TargetStatusSnapshot {
                requested: true,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                supported: true,
                source_only: false,
                materialization_status: Some(MaterializationStatus::Detached),
                verified: false,
            }]),
            AssetAggregateStatus::Detached
        );
        assert_eq!(
            compute_asset_aggregate_status(&[TargetStatusSnapshot {
                requested: true,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                supported: true,
                source_only: false,
                materialization_status: Some(MaterializationStatus::ActivationRequired),
                verified: false,
            }]),
            AssetAggregateStatus::ActivationRequired
        );
        assert_eq!(
            compute_asset_aggregate_status(&[TargetStatusSnapshot {
                requested: true,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                supported: true,
                source_only: false,
                materialization_status: Some(MaterializationStatus::ExternalCollision),
                verified: false,
            }]),
            AssetAggregateStatus::ExternalCollision
        );
        assert_eq!(
            compute_asset_aggregate_status(&[TargetStatusSnapshot {
                requested: true,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                supported: true,
                source_only: false,
                materialization_status: Some(MaterializationStatus::Blocked),
                verified: false,
            }]),
            AssetAggregateStatus::Blocked
        );
        assert_eq!(
            compute_asset_aggregate_status(&[TargetStatusSnapshot {
                requested: true,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
                supported: false,
                source_only: true,
                materialization_status: None,
                verified: false,
            }]),
            AssetAggregateStatus::SourceOnly
        );
    }
}
