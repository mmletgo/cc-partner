//! backend/authority.rs — 运行时 owner 角色与 versioned control descriptor。
//!
//! Business Logic（为什么需要这个模块）:
//!     sidecar 必须成为配置、Workbench runtime 与后台 bridge 的唯一 owner；
//!     GUI 只能作为 client 代理，不能再以第二份本地 runtime 执行 mutation。
//!     控制文件需要 schema 版本与 owner 实例 id，让旧 sidecar 被识别为需重启而非伪装权威。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `RuntimeRole`/`RuntimeOwnerDescriptor`、当前 control schema 版本常量，
//!     以及把控制文件分类为 authoritative / needs_restart 的 helper；
//!     不把 control schema 写入 LAN `server_protocol_info()`。

use crate::backend::control::BackendControlFile;
use crate::error::AppError;

/// 当前 control 文件 schema 版本（独立于 LAN protocol_version）。
///
/// Business Logic（为什么需要这个常量）:
///     GUI 需要知道控制文件是否携带 owner 描述符；旧文件缺字段时必须标记需重启，
///     而不是假装实时配置更新成功。
///
/// Code Logic（这个常量做什么）:
///     新写入的控制文件使用该版本；分类 helper 以它为权威版本比对。
pub const CONTROL_SCHEMA_VERSION: u32 = 2;

/// 运行时角色：sidecar 是唯一 HeadlessOwner，GUI 只做 GuiClient。
///
/// Business Logic（为什么需要这个枚举）:
///     终端创建/恢复、配置 mutation 等只能在 sidecar 执行；GUI 调用必须被拒绝或代理到 owner。
///
/// Code Logic（这个枚举做什么）:
///     提供 `require_owner`：GuiClient 返回 conflict `runtime_owner_required`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRole {
    /// 独立后端 / sidecar：拥有全部运行时权威。
    HeadlessOwner,
    /// 桌面 GUI：仅 UI/OS 集成，运行时 mutation 必须代理到 owner。
    GuiClient,
}

impl RuntimeRole {
    /// 要求当前角色是 HeadlessOwner。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 PTY/配置/编排等 mutation 入口必须在执行前确认自己是 owner，
    ///     防止 GUI 误走本地第二 runtime 造成 split-brain。
    ///
    /// Code Logic（这个函数做什么）:
    ///     HeadlessOwner 返回 Ok；GuiClient 返回 `AppError::conflict("runtime_owner_required")`。
    pub fn require_owner(self) -> Result<(), AppError> {
        match self {
            Self::HeadlessOwner => Ok(()),
            Self::GuiClient => Err(AppError::conflict("runtime_owner_required")),
        }
    }
}

/// 运行时 owner 描述符（schema + 实例 id + generation）。
///
/// Business Logic（为什么需要这个结构）:
///     配置 CAS 与事件游标需要稳定的 owner 身份与单调 generation；
///     后续 control API 会回传该描述符供 GUI 对账。
///
/// Code Logic（这个结构做什么）:
///     纯数据载体：schema_version、owner_instance_id、generation（Task 1 先定义类型，CAS 在后续任务落地）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOwnerDescriptor {
    /// 控制文件 schema 版本。
    pub schema_version: u32,
    /// 本 sidecar 进程启动时生成的唯一 owner 实例 id。
    pub owner_instance_id: String,
    /// 权威运行配置成功替换后递增的 generation（Task 1 仅为类型占位，默认 0）。
    pub generation: u64,
}

/// 控制描述符分类结果。
///
/// Business Logic（为什么需要这个结构）:
///     旧控制文件能反序列化，但不能被当作权威 owner；GUI 必须提示重启后端。
///
/// Code Logic（这个结构做什么）:
///     封装 `needs_restart` 布尔判定，供 status/GUI 读取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlDescriptorClassification {
    needs_restart: bool,
}

impl ControlDescriptorClassification {
    /// 是否需要重启后端才能获得权威 control 描述符。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方只需知道旧 sidecar 是否不可用作 owner，而不是解析 schema 细节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 `needs_restart` 标志。
    pub fn needs_restart(self) -> bool {
        self.needs_restart
    }

    /// 描述符是否可视为当前权威 owner 控制面。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     与 needs_restart 对偶，便于后续 status/client 分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `!needs_restart`。
    pub fn is_authoritative(self) -> bool {
        !self.needs_restart
    }
}

/// 分类控制文件中的 owner 描述符是否权威。
///
/// Business Logic（为什么需要这个函数）:
///     legacy JSON 缺 schema/owner 字段时仍可反序列化，但绝不能伪装成实时权威；
///     分类结果驱动 GUI “需要重启后端以应用设置” 提示。
///
/// Code Logic（这个函数做什么）:
///     当 `control_schema_version != CONTROL_SCHEMA_VERSION` 或 `owner_instance_id` 缺失/空白时
///     标记 `needs_restart`；不读取、不记录 control_token。
pub fn classify_control_descriptor(
    control: &BackendControlFile,
) -> ControlDescriptorClassification {
    let owner_missing = control
        .owner_instance_id
        .as_ref()
        .map(|id| id.trim().is_empty())
        .unwrap_or(true);
    let needs_restart = control.control_schema_version != CONTROL_SCHEMA_VERSION || owner_missing;
    ControlDescriptorClassification { needs_restart }
}

/// 从权威控制文件投影 RuntimeOwnerDescriptor（generation 暂为 0）。
///
/// Business Logic（为什么需要这个函数）:
///     后续 control status 需要从磁盘描述符构造 owner 身份；非权威文件不得投影。
///
/// Code Logic（这个函数做什么）:
///     仅当 `classify_control_descriptor` 权威且 owner id 存在时返回 Some；否则 None。
pub fn owner_descriptor_from_control(
    control: &BackendControlFile,
) -> Option<RuntimeOwnerDescriptor> {
    if !classify_control_descriptor(control).is_authoritative() {
        return None;
    }
    let owner_instance_id = control.owner_instance_id.clone()?;
    Some(RuntimeOwnerDescriptor {
        schema_version: control.control_schema_version,
        owner_instance_id,
        generation: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::control::BackendControlFile;

    /// 验证 HeadlessOwner 通过 require_owner。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     sidecar 角色必须允许执行 owner-only mutation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 `RuntimeRole::HeadlessOwner.require_owner()` 并断言 Ok。
    #[test]
    fn require_owner_accepts_headless_owner() {
        assert!(RuntimeRole::HeadlessOwner.require_owner().is_ok());
    }

    /// 验证 GuiClient 被 require_owner 拒绝为 conflict。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 不得本地 fallback 执行 runtime mutation，必须得到稳定 conflict 码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 `GuiClient.require_owner()`，断言 Conflict 且消息为 `runtime_owner_required`。
    #[test]
    fn require_owner_rejects_gui_client_with_conflict_code() {
        let err = RuntimeRole::GuiClient
            .require_owner()
            .expect_err("GuiClient 必须被拒绝");
        assert_eq!(err.classify(), crate::error::AppErrorCategory::Conflict);
        assert_eq!(err.to_string(), "runtime_owner_required");
    }

    /// 验证当前 schema + 非空 owner 的控制文件被分类为权威。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     新 sidecar 写出的描述符必须可被 GUI 信任。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 schema=CONTROL_SCHEMA_VERSION 且带 owner id 的文件，断言 needs_restart=false。
    #[test]
    fn current_descriptor_is_authoritative() {
        let mut file = BackendControlFile::for_test(1, 62116, "device-a");
        file.control_schema_version = CONTROL_SCHEMA_VERSION;
        file.owner_instance_id = Some("owner-a".to_string());
        let class = classify_control_descriptor(&file);
        assert!(!class.needs_restart());
        assert!(class.is_authoritative());
        let descriptor = owner_descriptor_from_control(&file).expect("authoritative");
        assert_eq!(descriptor.owner_instance_id, "owner-a");
        assert_eq!(descriptor.schema_version, CONTROL_SCHEMA_VERSION);
    }
}
