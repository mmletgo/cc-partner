//! models/transfer.rs — 文件传输数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     文件传输需要跟踪每次任务的文件元数据（名称/大小/SHA256）、传输进度、对端设备、
//!     状态（pending/transferring/completed/failed/cancelled），以支持断点续传、取消、
//!     进度展示和传输历史。对照 Python `models/transfer.py` 的 TransferTask dataclass。
//!
//! Code Logic（这个模块做什么）:
//!     - `TransferStatus` / `TransferDirection` 枚举（serde lowercase，对照 Python Enum.value）
//!     - `TransferTask` serde struct：内部用 snake_case（registry 与 transfer_history 表对齐），
//!       对外 DTO 用 `TransferTaskDto`（camelCase + 派生字段 progress，对齐前端 TS 类型）。

use serde::{Deserialize, Serialize};

/// 传输任务状态枚举。serde 以 lowercase 序列化，与 Python Enum.value 一致。
///
/// Business Logic: 文件传输是多阶段过程，需精确跟踪当前所处状态以驱动 UI 与断点续传判定。
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
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "receive" => Self::Receive,
            _ => Self::Send,
        }
    }
}

/// 传输任务实体（内部用，snake_case）。
///
/// Business Logic: registry 活跃任务表与 transfer_history 表共享同一字段集。
///     created_at / completed_at 用 RFC3339 ISO 字符串透传（兼容 Python isoformat 互通）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferTask {
    /// 传输任务 ID（UUID）
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
    /// 当前状态
    pub status: TransferStatus,
    /// 已传输字节数
    pub transferred_bytes: u64,
    /// 任务创建时间（RFC3339 ISO 字符串）
    pub created_at: String,
    /// 任务完成时间（RFC3339 ISO 字符串；未完成为 None）
    pub completed_at: Option<String>,
}

impl TransferTask {
    /// 进度 0.0~1.0（transferred_bytes / size）。size 为 0 时返回 0.0 避免除零。
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            0.0
        } else {
            self.transferred_bytes as f64 / self.size as f64
        }
    }
}

/// 传输任务前端 DTO（camelCase + 派生字段，对照前端 web/src/lib/types.ts 的 TransferTask）。
///
/// Business Logic: 前端 TS 期望 camelCase 字段名与 progress 派生字段。
///     对照 Python protocol.py 的 `_transfer_to_frontend_dict`。
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
}

impl TransferTask {
    /// 转为前端 DTO。
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
        }
    }
}
