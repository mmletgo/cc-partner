//! models/transfer.rs — 文件传输数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     文件传输需要跟踪每次任务的文件元数据（名称/大小/SHA256）、传输进度、对端设备、
//!     状态（pending/transferring/completed/failed/cancelled），以支持断点续传、取消、
//!     进度展示和传输历史。N5 recovery 需要额外持久化 phase/failure、logical/attempt/protocol
//!     身份与发送端 clientOperationId，同时保留 coarse TransferStatus 兼容旧 GUI。
//!
//! Code Logic（这个模块做什么）:
//!     - `TransferStatus` / `TransferDirection` 枚举（serde lowercase，对照 Python Enum.value）
//!     - `TransferPhase` / `TransferFailureStage` / `TransferFailure`：细粒度阶段与稳定失败契约
//!     - `TransferTask` serde struct：内部用 snake_case（registry 与 transfer_history 表对齐），
//!       对外 DTO 用 `TransferTaskDto`（camelCase + 派生字段 progress，对齐前端 TS 类型）。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 发送端恢复操作类型（retry / resume）。
///
/// Business Logic（为什么需要这个枚举）:
///     同一 clientOperationId 不得把 retry 与 resume、或两个不同 logical task 混为同一操作。
///
/// Code Logic（这个枚举做什么）:
///     参与 canonical payload hash 的 kind 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferRecoveryKind {
    /// 全量重试（可 mint 新 protocol transfer id）
    Retry,
    /// 断点续传（复用稳定 protocol transfer id）
    Resume,
}

impl TransferRecoveryKind {
    /// 稳定小写字符串。
    ///
    /// Business Logic: payload hash 与日志需要稳定 token。
    /// Code Logic: 返回 `retry` / `resume`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Resume => "resume",
        }
    }
}

/// 源文件指纹（size + mtime + sha256）。
///
/// Business Logic（为什么需要这个结构体）:
///     resume/retry 必须拒绝 TOCTOU 下源文件被替换；mtime 不可用时靠 size+SHA。
///
/// Code Logic（这个结构体做什么）:
///     保存 size、可选 mtime_ns、sha256 hex。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFingerprint {
    /// 文件字节大小
    pub size: u64,
    /// 修改时间纳秒（不可用为 None）
    pub mtime_ns: Option<u64>,
    /// 文件 SHA256 hex
    pub sha256: String,
}

/// 计算发送端恢复操作的 canonical payload hash。
///
/// Business Logic（为什么需要这个函数）:
///     same clientOperationId 必须绑定固定语义：kind + logical identity + peer + 预期 protocol id。
///
/// Code Logic（这个函数做什么）:
///     固定 key 顺序的 JSON 字节做 SHA256 hex（不含 clientOperationId 本身）。
pub fn canonical_recovery_payload_hash(
    kind: TransferRecoveryKind,
    logical_transfer_id: &str,
    source_path: &str,
    peer_device_id: &str,
    protocol_transfer_id: &str,
) -> String {
    let payload = serde_json::json!({
        "kind": kind.as_str(),
        "logicalTransferId": logical_transfer_id,
        "peerDeviceId": peer_device_id,
        "protocolTransferId": protocol_transfer_id,
        "sourcePath": source_path,
    });
    // serde_json::Map 插入顺序不稳定；用手写稳定字节。
    let stable = format!(
        "{{\"kind\":\"{}\",\"logicalTransferId\":{},\"peerDeviceId\":{},\"protocolTransferId\":{},\"sourcePath\":{}}}",
        kind.as_str(),
        serde_json::to_string(logical_transfer_id).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(peer_device_id).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(protocol_transfer_id).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(source_path).unwrap_or_else(|_| "\"\"".into()),
    );
    let _ = payload; // 文档意图：字段集合与 stable 一致
    let digest = Sha256::digest(stable.as_bytes());
    format!("{digest:x}")
}

/// 传输任务状态枚举。serde 以 lowercase 序列化，与 Python Enum.value 一致。
///
/// Business Logic: 文件传输是多阶段过程，需精确跟踪当前所处状态以驱动 UI 与断点续传判定。
///     coarse status 保持兼容，不得因 phase 扩展被破坏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    /// 等待中（已创建任务，尚未开始传输）
    Pending,
    /// 传输中
    Transferring,
    /// 已完成（含 SHA256 校验通过）
    Completed,
    /// 失败（网络错误或 SHA256 校验失败）
    Failed,
    /// 已取消（用户主动取消）
    Cancelled,
}

impl TransferStatus {
    /// 从字符串解析状态（用于从 DB 的 status TEXT 列还原）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     历史库 status 以 TEXT 存储；未知值需安全收敛，避免启动崩溃。
    ///
    /// Code Logic（这个函数做什么）:
    ///     识别 pending/transferring/completed/failed/cancelled；其它回落 Failed。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "transferring" => Self::Transferring,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }

    /// 序列化为稳定小写字符串（写入 DB）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仓库层需要把枚举写成与旧库互通的 TEXT。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 lowercase 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Transferring => "transferring",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 传输方向枚举（发送 / 接收）。serde lowercase，对照 Python TransferDirection。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    /// 本机发送给对端
    Send,
    /// 本机接收对端发来的文件
    Receive,
}

impl TransferDirection {
    /// 从字符串解析方向（DB direction TEXT 列还原）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     历史 direction 列是 TEXT；缺省/未知按 Send 处理以兼容旧数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `receive` → Receive，其它 → Send。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "receive" => Self::Receive,
            _ => Self::Send,
        }
    }

    /// 序列化为稳定小写字符串（写入 DB）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仓库写入需要固定文本，避免 Display 漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `send` / `receive`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Receive => "receive",
        }
    }
}

/// 细粒度传输阶段（可观察 pipeline）。
///
/// Business Logic（为什么需要这个枚举）:
///     coarse status 无法区分 connecting/finalizing；UI 与 retry 策略需要更细阶段。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase（单词语与 lowercase 等价）；DB 存稳定小写字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferPhase {
    /// 已 claim，等待 spawn / 计算元数据
    Queued,
    /// 正在连接对端 / 发 init
    Connecting,
    /// 分块传输中
    Transferring,
    /// 收齐后 finalize / 晋升 history
    Finalizing,
    /// 已完成
    Completed,
    /// 已取消
    Cancelled,
    /// 已失败
    Failed,
}

impl TransferPhase {
    /// 解析稳定 phase 字符串；未知返回 None（不得映射 Failed）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧行/未来新 phase 文本不得被误标 Failed，否则 UI 会给出错误动作。
    ///
    /// Code Logic（这个函数做什么）:
    ///     识别已知小写 token；其它返回 None。
    pub fn parse_optional(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "connecting" => Some(Self::Connecting),
            "transferring" => Some(Self::Transferring),
            "finalizing" => Some(Self::Finalizing),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// 由 coarse status 推导 phase（旧行 / 未知 phase 回落）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     legacy 行 phase 为空；UI 仍需可展示阶段，且未知值不能变成 Failed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Pending→Queued；Transferring→Transferring；Completed/Cancelled/Failed 同名映射。
    pub fn from_status(status: TransferStatus) -> Self {
        match status {
            TransferStatus::Pending => Self::Queued,
            TransferStatus::Transferring => Self::Transferring,
            TransferStatus::Completed => Self::Completed,
            TransferStatus::Cancelled => Self::Cancelled,
            TransferStatus::Failed => Self::Failed,
        }
    }

    /// 解析存储 phase；未知或空则回落到 status，绝不因未知文本变成 Failed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     读库时既要保留未来兼容，又要给调用方稳定 phase。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `parse_optional` 成功用自身；否则 `from_status(status)`。
    pub fn resolve(stored: Option<&str>, status: TransferStatus) -> Self {
        match stored {
            Some(s) if !s.is_empty() => Self::parse_optional(s).unwrap_or_else(|| Self::from_status(status)),
            _ => Self::from_status(status),
        }
    }

    /// 稳定小写字符串（DB / wire 内部）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     phase 列需要跨版本稳定 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 lowercase 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Connecting => "connecting",
            Self::Transferring => "transferring",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// 失败发生的稳定阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     retry/resume 决策依赖失败发生在 connect/transfer/finalize/source 等哪一步。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase；DB 存稳定小写字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferFailureStage {
    /// 连接/init 阶段
    Connect,
    /// 分块传输阶段
    Transfer,
    /// finalize / promote 阶段
    Finalize,
    /// 源文件 fingerprint 变化
    Source,
    /// 协议/能力不兼容
    Protocol,
    /// 本机 IO / 路径等本地错误
    Local,
    /// 未分类
    Unknown,
}

impl TransferFailureStage {
    /// 解析稳定 stage 字符串；未知回落 Unknown。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧/损坏 failure_stage 文本仍需可展示，不能丢整条 failure。
    ///
    /// Code Logic（这个函数做什么）:
    ///     识别已知 token；其它 → Unknown。
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "connect" => Self::Connect,
            "transfer" => Self::Transfer,
            "finalize" => Self::Finalize,
            "source" => Self::Source,
            "protocol" => Self::Protocol,
            "local" => Self::Local,
            _ => Self::Unknown,
        }
    }

    /// 稳定小写字符串（DB）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     failure_stage 列需要跨版本稳定 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 lowercase 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Transfer => "transfer",
            Self::Finalize => "finalize",
            Self::Source => "source",
            Self::Protocol => "protocol",
            Self::Local => "local",
            Self::Unknown => "unknown",
        }
    }
}

/// 结构化失败信息。
///
/// Business Logic（为什么需要这个结构体）:
///     UI 与对账需要稳定 code、是否可重试、用户可读 message 与失败阶段。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化；内部与 DB 列一一对应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferFailure {
    /// 失败阶段
    pub stage: TransferFailureStage,
    /// 稳定错误码（如 `source_changed`）
    pub code: String,
    /// 是否允许 retry/resume
    pub retryable: bool,
    /// 用户可读说明
    pub message: String,
}

/// 发送端 clientOperationId 对账结果。
///
/// Business Logic（为什么需要这个枚举）:
///     mutation timeout/断线后 UI 不得盲重试；必须先按全局 clientOperationId 查询发送端
///     ledger 真值（notFound/pending/succeeded/failed），再决定后续动作。
///
/// Code Logic（这个枚举做什么）:
///     内部 tag `status` + camelCase：`{status,taskId?|code?}`。
///     OperationIdConflict 在 claim 边界返回，查询侧通常不需要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TransferOperationStatus {
    /// ledger 无此 clientOperationId
    NotFound,
    /// 已 claim 或仍在飞行中（含 lost-ACK 后的 uncertain Finalizing）
    Pending,
    /// 发送端已提交成功 outcome
    Succeeded {
        /// 绑定的本地 attempt task id
        task_id: String,
    },
    /// 已提交 definitive 失败 outcome
    Failed {
        /// 稳定失败码（failure.code 或 cancelled/failed）
        code: String,
    },
}

/// 传输任务实体（内部用，snake_case）。
///
/// Business Logic: registry 活跃任务表与 transfer_history 表共享同一字段集。
///     created_at / completed_at 用 RFC3339 ISO 字符串透传（兼容 Python isoformat 互通）。
///     recovery 字段：phase/failure 可空；attempt 默认 1；logical/attempt/protocol id 默认同 id。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTask {
    /// 传输任务 ID（UUID）；当前 attempt 的本地主键
    pub id: String,
    /// 文件名（basename）
    pub filename: String,
    /// 本地文件路径（发送端为源文件；接收端为 .tmp 临时文件或最终保存路径）
    pub file_path: String,
    /// 文件总大小（bytes）
    pub size: u64,
    /// 文件 SHA256（hex）
    pub sha256: String,
    /// 块大小（与 Python 一致 960KB）
    pub chunk_size: u64,
    /// 传输方向
    pub direction: TransferDirection,
    /// 对端设备 ID
    pub peer_device_id: String,
    /// 当前 coarse 状态（兼容旧 GUI）
    pub status: TransferStatus,
    /// 已传输字节数
    pub transferred_bytes: u64,
    /// 任务创建时间（RFC3339 ISO 字符串）
    pub created_at: String,
    /// 任务完成时间（RFC3339 ISO 字符串；未完成为 None）
    pub completed_at: Option<String>,
    /// 细粒度阶段；None 表示未知/旧行，展示时由 status 推导
    #[serde(default)]
    pub phase: Option<TransferPhase>,
    /// 结构化失败；非失败任务为 None
    #[serde(default)]
    pub failure: Option<TransferFailure>,
    /// 同一 logical transfer 下的 attempt 序号；legacy 默认 1
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// 逻辑传输 ID（跨 retry 稳定）；legacy = id
    #[serde(default)]
    pub logical_transfer_id: String,
    /// 本 attempt 身份；legacy = id
    #[serde(default)]
    pub attempt_id: String,
    /// 协议层 transfer id（receiver checkpoint 键）；resume 复用，retry 可新发；legacy = id
    #[serde(default)]
    pub protocol_transfer_id: String,
    /// 发送端全局幂等键；receiver 不持有
    #[serde(default)]
    pub client_operation_id: Option<String>,
    /// 操作 payload 规范哈希（同 op id 冲突检测）
    #[serde(default)]
    pub operation_payload_hash: Option<String>,
}

/// serde 默认 attempt=1。
///
/// Business Logic（为什么需要这个函数）:
///     反序列化旧 JSON/行缺省 attempt 时必须等于 1。
///
/// Code Logic（这个函数做什么）:
///     返回常量 1。
fn default_attempt() -> u32 {
    1
}

impl TransferTask {
    /// 进度 0.0~1.0（transferred_bytes / size）。size 为 0 时返回 0.0 避免除零。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端进度条需要 0~1 归一化进度。
    ///
    /// Code Logic（这个函数做什么）:
    ///     transferred_bytes / size；size=0 返回 0.0。
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            self.transferred_bytes as f64 / self.size as f64
        }
    }

    /// 用 task id 填充 recovery 身份默认值的骨架，供 struct update 语法展开。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     现有 send/receive 构造路径尚未 claim clientOperationId；必须默认 attempt=1
    ///     且 logical/attempt/protocol id = id，避免编译破坏与 legacy 语义漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 recovery 字段已填、其余字段占位的 `TransferTask`，供 `..` 覆盖业务字段。
    pub fn recovery_defaults(id: &str) -> Self {
        Self {
            id: id.to_string(),
            filename: String::new(),
            file_path: String::new(),
            size: 0,
            sha256: String::new(),
            chunk_size: 960 * 1024,
            direction: TransferDirection::Send,
            peer_device_id: String::new(),
            status: TransferStatus::Pending,
            transferred_bytes: 0,
            created_at: String::new(),
            completed_at: None,
            phase: None,
            failure: None,
            attempt: 1,
            logical_transfer_id: id.to_string(),
            attempt_id: id.to_string(),
            protocol_transfer_id: id.to_string(),
            client_operation_id: None,
            operation_payload_hash: None,
        }
    }

    /// 归一化 legacy 身份字段（空 id 回落到 task.id；attempt 至少为 1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     读库 coalesce 后仍可能出现空串/0；调用方应得到可比较的稳定身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原地把空 logical/attempt/protocol id 设为 `id`；`attempt==0` 时改为 1。
    pub fn normalize_recovery_identity(&mut self) {
        if self.logical_transfer_id.is_empty() {
            self.logical_transfer_id = self.id.clone();
        }
        if self.attempt_id.is_empty() {
            self.attempt_id = self.id.clone();
        }
        if self.protocol_transfer_id.is_empty() {
            self.protocol_transfer_id = self.id.clone();
        }
        if self.attempt == 0 {
            self.attempt = 1;
        }
    }

    /// 有效 phase：存储值优先，否则由 coarse status 推导。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧行 phase 为空；UI/DTO 需要可展示阶段且未知不得 Failed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `self.phase.unwrap_or_else(|| TransferPhase::from_status(self.status))`。
    pub fn effective_phase(&self) -> TransferPhase {
        self.phase
            .unwrap_or_else(|| TransferPhase::from_status(self.status))
    }

    /// 转为前端 DTO。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     前端 TS 期望 camelCase 字段名、progress 派生与 recovery 字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射内部字段；phase 输出 `Some(effective_phase)`；failure 原样透传。
    pub fn to_dto(&self, error_message: Option<String>) -> TransferTaskDto {
        TransferTaskDto {
            id: self.id.clone(),
            file_name: self.filename.clone(),
            file_path: self.file_path.clone(),
            file_size: self.size,
            direction: self.direction,
            status: self.status,
            progress: self.progress(),
            peer_device_id: Some(self.peer_device_id.clone()),
            peer_device_name: None,
            speed: None,
            error_message,
            started_at: self.created_at.clone(),
            completed_at: self.completed_at.clone(),
            transferred_bytes: self.transferred_bytes,
            phase: Some(self.effective_phase()),
            failure: self.failure.clone(),
            attempt: self.attempt,
            logical_transfer_id: self.logical_transfer_id.clone(),
            attempt_id: self.attempt_id.clone(),
            protocol_transfer_id: self.protocol_transfer_id.clone(),
            client_operation_id: self.client_operation_id.clone(),
            operation_payload_hash: self.operation_payload_hash.clone(),
        }
    }
}

/// 传输任务前端 DTO（camelCase + 派生字段，对照前端 web/src/lib/types.ts 的 TransferTask）。
///
/// Business Logic: 前端 TS 期望 camelCase 字段名与 progress 派生字段。
///     N5 增加 phase/failure/attempt 与 identity 字段供历史动作矩阵使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferTaskDto {
    pub id: String,
    pub file_name: String,
    pub file_path: String,
    pub file_size: u64,
    pub direction: TransferDirection,
    pub status: TransferStatus,
    /// 进度 0.0~1.0（派生）
    pub progress: f64,
    pub peer_device_id: Option<String>,
    /// 对端设备名（registry 中通常未知，置 None；前端可自行回查设备表）
    pub peer_device_name: Option<String>,
    /// 传输速度（暂未实现，置 None，对照 Python）
    pub speed: Option<f64>,
    /// 错误信息（失败时填充）
    pub error_message: Option<String>,
    /// 开始时间（对应内部 created_at）
    pub started_at: String,
    /// 完成时间
    pub completed_at: Option<String>,
    /// 已传输字节数（供前端进度条数字展示）
    pub transferred_bytes: u64,
    /// 细粒度阶段（旧行由 status 推导后填入）
    #[serde(default)]
    pub phase: Option<TransferPhase>,
    /// 结构化失败
    #[serde(default)]
    pub failure: Option<TransferFailure>,
    /// attempt 序号
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// 逻辑传输 ID
    #[serde(default)]
    pub logical_transfer_id: String,
    /// attempt 身份
    #[serde(default)]
    pub attempt_id: String,
    /// 协议 transfer id
    #[serde(default)]
    pub protocol_transfer_id: String,
    /// 发送端幂等键
    #[serde(default)]
    pub client_operation_id: Option<String>,
    /// payload 哈希
    #[serde(default)]
    pub operation_payload_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未知 phase 文本不得映射 Failed，应回落到 coarse status。
    #[test]
    fn unknown_phase_falls_back_to_status_not_failed() {
        assert_eq!(TransferPhase::parse_optional("brand_new_phase"), None);
        assert_eq!(
            TransferPhase::resolve(Some("brand_new_phase"), TransferStatus::Transferring),
            TransferPhase::Transferring
        );
        assert_eq!(
            TransferPhase::resolve(None, TransferStatus::Pending),
            TransferPhase::Queued
        );
        assert_eq!(
            TransferPhase::resolve(Some(""), TransferStatus::Completed),
            TransferPhase::Completed
        );
        assert_ne!(
            TransferPhase::resolve(Some("nope"), TransferStatus::Completed),
            TransferPhase::Failed
        );
    }

    /// to_dto 必须输出 recovery 字段；phase 空时由 status 推导。
    #[test]
    fn to_dto_emits_recovery_fields_and_derived_phase() {
        let task = TransferTask {
            filename: "a.txt".into(),
            file_path: "/tmp/a.txt".into(),
            size: 10,
            sha256: "ab".into(),
            status: TransferStatus::Failed,
            transferred_bytes: 3,
            created_at: "2026-07-14T00:00:00Z".into(),
            failure: Some(TransferFailure {
                stage: TransferFailureStage::Transfer,
                code: "peer_timeout".into(),
                retryable: true,
                message: "超时".into(),
            }),
            client_operation_id: Some("op-1".into()),
            operation_payload_hash: Some("hash-1".into()),
            ..TransferTask::recovery_defaults("task-1")
        };
        let dto = task.to_dto(Some("超时".into()));
        assert_eq!(dto.phase, Some(TransferPhase::Failed));
        assert_eq!(dto.attempt, 1);
        assert_eq!(dto.logical_transfer_id, "task-1");
        assert_eq!(dto.attempt_id, "task-1");
        assert_eq!(dto.protocol_transfer_id, "task-1");
        assert_eq!(dto.client_operation_id.as_deref(), Some("op-1"));
        assert_eq!(dto.operation_payload_hash.as_deref(), Some("hash-1"));
        assert_eq!(
            dto.failure.as_ref().map(|f| f.stage),
            Some(TransferFailureStage::Transfer)
        );
        assert_eq!(dto.error_message.as_deref(), Some("超时"));
    }

    /// TransferOperationStatus 序列化为 camelCase tag 合同。
    #[test]
    fn operation_status_serde_camel_case_tag() {
        let s = TransferOperationStatus::Succeeded {
            task_id: "t1".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["status"], "succeeded");
        assert_eq!(v["taskId"], "t1");
        let p = TransferOperationStatus::Pending;
        assert_eq!(
            serde_json::to_value(&p).unwrap()["status"],
            "pending"
        );
        let n = TransferOperationStatus::NotFound;
        assert_eq!(
            serde_json::to_value(&n).unwrap()["status"],
            "notFound"
        );
        let f = TransferOperationStatus::Failed {
            code: "finalize_rejected".into(),
        };
        let fv = serde_json::to_value(&f).unwrap();
        assert_eq!(fv["status"], "failed");
        assert_eq!(fv["code"], "finalize_rejected");
    }
}
