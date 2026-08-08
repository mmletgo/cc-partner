//! portable_service — Owner 面 portable inventory / action 门面
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面 GUI 与 control 客户端只能经 owner 读写本机 portable 资产；
//!     inspect 是实际状态真相；mutation 必须 preview → apply → rescan + ledger。
//!
//! Code Logic（这个模块做什么）:
//!     组合 B1–B4：inspect / preview(with inventory) / apply / get-by-request；
//!     供 Tauri command 与 control op 共用，禁止第二写路径。

use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::portable_actions::{
    apply_portable_asset_action, get_portable_asset_action_by_request,
    preview_portable_asset_action_with_inventory, ApplyPortableAssetActionRequest,
    PortableAssetActionPlanDto, PortableAssetActionResultDto, PreviewPortableAssetActionRequest,
};
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory, PortableInventorySnapshotDto,
};
use crate::agent_hub::replication::pull::{
    apply_portable_pull as apply_portable_pull_impl, get_portable_pull as get_portable_pull_impl,
    list_remote_portable_inventory as list_remote_portable_inventory_impl,
    preview_portable_pull as preview_portable_pull_impl, ApplyPortablePullRequest,
    ListRemotePortableInventoryRequest, PortablePullPlanDto, PortablePullResultDto,
    PreviewPortablePullRequest, RemotePortableInventoryDto,
};
use crate::error::AppError;
use crate::state::AppState;

/// Portable 资产 owner 服务门面。
///
/// Business Logic（为什么需要这个结构体）:
///     HeadlessOwner 与 control dispatch 共享同一套四操作入口，避免 command 层直接拼装 B1–B4。
///
/// Code Logic（这个结构体做什么）:
///     纯静态方法命名空间；无内部状态。
pub struct PortableService;

impl PortableService {
    /// 扫描并返回本机 portable inventory 快照（发现即管理账本）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     UI 必须以实际目标状态为真相；inspect 不写目标磁盘内容，但会幂等 ensure
    ///     Hub asset/binding/materialization，避免稳定 unmanaged。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `inspect_portable_inventory`（scan → ensure_managed → reconcile）。
    pub async fn inspect_portable_inventory(
        state: &AppState,
    ) -> Result<PortableInventorySnapshotDto, AppError> {
        inspect_portable_inventory(state).await
    }

    /// 基于当前 inventory 生成短期 action plan（写路径合同，零目标文件写入）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Apply 前必须绑定 inventory hash 与 plan token；计划有 TTL。
    ///
    /// Code Logic（这个函数做什么）:
    ///     inspect → preview_with_inventory + owner fingerprint。
    pub async fn preview_portable_asset_action(
        state: &AppState,
        request: PreviewPortableAssetActionRequest,
    ) -> Result<PortableAssetActionPlanDto, AppError> {
        let snapshot = inspect_portable_inventory(state).await?;
        let owner_fp = portable_owner_fingerprint(state, &snapshot);
        preview_portable_asset_action_with_inventory(
            &state.agent_hub_repo,
            request,
            &snapshot,
            &owner_fp,
        )
        .await
    }

    /// 原子 claim 并执行 plan，rescan 验证后写入 ledger。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     唯一本机 mutation 执行入口；同 clientRequestId 可回放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `apply_portable_asset_action`（B4 executor）。
    pub async fn apply_portable_asset_action(
        state: &AppState,
        request: ApplyPortableAssetActionRequest,
    ) -> Result<PortableAssetActionResultDto, AppError> {
        // mutation 前失效，避免 apply 过程中并发 inspect 命中脏缓存
        crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
        let result = apply_portable_asset_action(state, request).await;
        crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
        result
    }

    /// 按 clientRequestId 读取动作结果（含 outcomeUnknown）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     传输不确定后必须以 ledger 对账，禁止盲重放 apply。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `get_portable_asset_action_by_request`。
    pub async fn get_portable_asset_action(
        state: &AppState,
        client_request_id: &str,
    ) -> Result<PortableAssetActionResultDto, AppError> {
        get_portable_asset_action_by_request(&state.agent_hub_repo, client_request_id).await
    }

    /// 列出远端同类 Agent portable inventory（metadata only）。
    ///
    /// Business Logic: capability 缺失零请求；无 secret 原文。
    /// Code Logic: 委托 `list_remote_portable_inventory`。
    pub async fn list_remote_portable_inventory(
        state: &AppState,
        request: ListRemotePortableInventoryRequest,
    ) -> Result<RemotePortableInventoryDto, AppError> {
        list_remote_portable_inventory_impl(state, request).await
    }

    /// 预览同类 Agent pull（零目标写入）。
    ///
    /// Business Logic: sourceTarget == destinationTarget；映射/opt-in 决定 install 模式。
    /// Code Logic: 委托 `preview_portable_pull`。
    pub async fn preview_portable_pull(
        state: &AppState,
        request: PreviewPortablePullRequest,
    ) -> Result<PortablePullPlanDto, AppError> {
        preview_portable_pull_impl(state, request).await
    }

    /// 执行 pull（objects → canonical import → 同类 install）。
    ///
    /// Business Logic: clientRequestId 幂等；partial 逐项报告。
    /// Code Logic: 委托 `apply_portable_pull`。
    pub async fn apply_portable_pull(
        state: &AppState,
        request: ApplyPortablePullRequest,
    ) -> Result<PortablePullResultDto, AppError> {
        crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
        let result = apply_portable_pull_impl(state, request).await;
        crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
        result
    }

    /// 按 clientRequestId 查询 pull 结果。
    ///
    /// Business Logic: outcomeUnknown/partial 对账入口。
    /// Code Logic: 委托 `get_portable_pull`。
    pub async fn get_portable_pull(
        state: &AppState,
        client_request_id: &str,
    ) -> Result<PortablePullResultDto, AppError> {
        get_portable_pull_impl(state, client_request_id).await
    }
}

/// 构造 plan 绑定的 owner fingerprint。
///
/// Business Logic（为什么需要这个函数）:
///     plan 必须绑定当前 owner 与目标 config roots，防止跨 owner 重放。
///
/// Code Logic（这个函数做什么）:
///     sha256(device_id|target=config_root|...)。
fn portable_owner_fingerprint(state: &AppState, snapshot: &PortableInventorySnapshotDto) -> String {
    let roots = snapshot
        .targets
        .iter()
        .map(|t| format!("{}={}", t.target.as_str(), t.config_root))
        .collect::<Vec<_>>()
        .join("|");
    sha256_hex(format!("{}|{}", state.device_id.as_str(), roots).as_bytes())
}

#[cfg(test)]
mod tests {
    /// Business Logic: 四操作是 UI/control 合同入口。
    /// Code Logic: 源文件含四个 PortableService 方法签名。
    #[test]
    fn portable_service_exposes_four_owner_methods() {
        let src = include_str!("portable_service.rs");
        for sig in [
            "pub async fn inspect_portable_inventory(",
            "pub async fn preview_portable_asset_action(",
            "pub async fn apply_portable_asset_action(",
            "pub async fn get_portable_asset_action(",
            "pub async fn list_remote_portable_inventory(",
            "pub async fn preview_portable_pull(",
            "pub async fn apply_portable_pull(",
            "pub async fn get_portable_pull(",
        ] {
            assert!(src.contains(sig), "missing {sig}");
        }
        assert!(src.contains("inspect_portable_inventory(state)"));
        assert!(src.contains("preview_portable_asset_action_with_inventory"));
        assert!(src.contains("apply_portable_asset_action(state, request)"));
        assert!(src.contains("get_portable_asset_action_by_request"));
        assert!(src.contains("list_remote_portable_inventory_impl(state, request)"));
        assert!(src.contains("preview_portable_pull_impl(state, request)"));
        assert!(src.contains("apply_portable_pull_impl(state, request)"));
        assert!(src.contains("get_portable_pull_impl(state, client_request_id)"));
    }
}
