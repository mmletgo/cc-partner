//! models/ssh_target.rs — SSH 连接目标数据模型
//!
//! Business Logic（为什么需要这个模块）:
//!     SSH 页需要为每个连接目标（局域网设备 IP 或手填 IP）保存用户名/端口，并跨设备同步。
//!     需同时服务两个场景：1) 数据库读写与 P2P 同步（snake_case 互通）；
//!     2) 前端 IPC 返回（camelCase，对齐前端 types.ts）。模式对齐 cc/models.rs。
//!
//! Code Logic（这个模块做什么）:
//!     - `SshTargetRow`：snake_case，直接映射 ssh_targets 表一行，vector_clock 为 HashMap，datetime 用 String 透传。
//!     - `SshTargetDto`：camelCase，给前端用（前端只需 host/port/username/label/updatedAt）。
//!     - 提供 Row→Dto 转换。

use std::collections::HashMap;

/// SSH 连接目标数据库行 / 同步实体（snake_case）。
///
/// Business Logic: 持久化与跨设备同步需保留稳定字段命名，以便向量时钟 JSON 与各端互通。
///     host 为主键（IP 或 hostname）；port 默认 22；username 空串表示用本机默认用户名；
///     label 为可选备注；deleted 软删除参与同步传播。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SshTargetRow {
    /// 主键：IP 或 hostname
    pub host: String,
    /// SSH 端口，默认 22
    pub port: u16,
    /// SSH 用户名（空串 = 用本机默认用户名）
    pub username: String,
    /// 可选备注
    pub label: Option<String>,
    /// 最后修改设备 ID
    pub device_id: String,
    /// 向量时钟 {device_id: counter}（CRDT 同步用）
    pub vector_clock: HashMap<String, u64>,
    /// 入库时间 ISO 字符串
    pub created_at: String,
    /// 更新时间 ISO 字符串（同步合并/删除时推进）
    pub updated_at: String,
    /// 软删除标记
    pub deleted: bool,
}

/// SSH 目标 前端 DTO（camelCase，对照前端 types.ts）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshTargetDto {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub label: Option<String>,
    pub updated_at: String,
}

impl SshTargetRow {
    /// 转换为前端 DTO（snake_case → camelCase，仅保留前端需要的字段）。
    ///
    /// Business Logic: 前端只需 host/port/username/label/updatedAt，同步元数据（device_id/vector_clock 等）不暴露。
    /// Code Logic: 字段克隆/拷贝组装 DTO（camelCase 序列化名）。
    pub fn to_dto(&self) -> SshTargetDto {
        SshTargetDto {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            label: self.label.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}
