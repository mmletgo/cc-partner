//! backend/control_client.rs — GUI 本机 control-file 客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 不得自建第二份运行时权威；配置 / Cloud Sync / backup / Orchestrator deliver 等 mutation
//!     必须代理到 sidecar owner。客户端从本机 control file 读取 port + token，经 loopback
//!     control API 读写权威状态。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `BackendControlClient`：安全查询允许在连接失败时一次性刷新 control file；
//!     mutation 经 `send_once` 只发送一次，响应不确定时绝不自动重放。
//!     查询含 status / get-config / workbench-launch-summary / orchestrator snapshot /
//!     orchestrator review-diff / workflow-document get / events / backup list；
//!     mutation 含 deliver-reviewed / workflow-document validate|save；
//!     截图快捷键走两阶段补偿（CAS 预检 → OS replace → owner durable commit → 响应丢失对账）。

use crate::backend::authority::{classify_control_descriptor, CONTROL_SCHEMA_VERSION};
use crate::backend::control::{self, BackendControlFile};
use crate::backend::control_api::WorkbenchLaunchSummaryDto;
use crate::backend::event_bus::{BackendRuntimeCursor, RuntimeRelayMessage};
use crate::commands::orchestrator::{OrchestratorRuntimeSnapshotDto, OrchestratorTaskViewDto};
use crate::config_runtime::{
    ConfigSnapshot, ConfigUpdateRequest, ConfigUpdateResponse, RuntimeConfigPatch,
    RuntimeOwnerStatus,
};
use crate::error::AppError;
use crate::hotkey::{
    compensate_screenshot_hotkey_os, replace_screenshot_hotkey_os, GlobalShortcutBackend,
};
use crate::models::transfer::{
    LocalTransferOpenTarget, TransferOpenAction, TransferOperationStatus, TransferTaskDto,
};
use crate::orchestrator::experiments::{CreateExperimentRequest, OrchestratorExperimentDto};
use crate::orchestrator::models::{OperationalNotificationSnapshot, OrchestratorTaskDto};
use crate::orchestrator::workflow::WorkflowDocument;
use crate::workbench::operation_ledger::MutationTransportClass;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// control 查询超时。
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// control mutation 超时（配置落盘可能稍慢）。
const MUTATE_TIMEOUT: Duration = Duration::from_secs(15);
/// Cloud Sync mutation 超时（覆盖 Wait{300s} 门闸 + git 网络操作）。
const CLOUD_SYNC_MUTATE_TIMEOUT: Duration = Duration::from_secs(360);
/// 备份创建/恢复/回退超时（ZIP 读写 + exclusive maintenance_gate + 领域 bulk）。
const BACKUP_MUTATE_TIMEOUT: Duration = Duration::from_secs(360);
/// Orchestrator deliver 超时（git commit/push/merge 可能很长）。
const ORCHESTRATOR_DELIVER_TIMEOUT: Duration = Duration::from_secs(360);

/// 包装 get-config 响应（与 control_api::ControlConfigResponse 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlConfigResponseBody {
    snapshot: ConfigSnapshot,
}

/// 仅带 controlToken 的鉴权 body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlAuthBody {
    control_token: String,
}

/// transfer prepare-open control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlTransferPrepareOpenBody {
    control_token: String,
    task_id: String,
    action: TransferOpenAction,
}

/// transfer send control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlTransferSendBody {
    control_token: String,
    device_id: String,
    file_path: String,
    client_operation_id: String,
}

/// transfer retry/resume control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlTransferRecoveryBody {
    control_token: String,
    task_id: String,
    client_operation_id: String,
}

/// transfer get-operation control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlTransferGetOperationBody {
    control_token: String,
    client_operation_id: String,
}

/// transfer cancel control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlTransferCancelBody {
    control_token: String,
    task_id: String,
}

/// Workbench control 请求 body（token + op + payload）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlWorkbenchRequestBody {
    control_token: String,
    op: String,
    payload: serde_json::Value,
}

/// Workbench control 响应（owner + result）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlWorkbenchResponseBody {
    owner_instance_id: String,
    result: serde_json::Value,
}

/// Workbench control 元数据/data 路径选择。
///
/// Business Logic（为什么需要这个函数）:
///     文件内容/预览/browser 预览等可能超过元数据 256 KiB，需走 data 路径避免被 control body limit 截断。
///
/// Code Logic（这个函数做什么）:
///     open/save/preview/browser/replay/write 走 `workbench/data`，其余走 `workbench`。
fn workbench_control_path(op: &str) -> &'static str {
    match op {
        "files.open"
        | "files.save_text"
        | "files.preview_sqlite"
        | "files.preview_html_asset"
        | "browser.discover"
        | "browser.create_preview"
        | "sessions.replay"
        | "sessions.write" => "workbench/data",
        _ => "workbench",
    }
}

/// Workbench control 超时选择。
///
/// Business Logic（为什么需要这个函数）:
///     commit/merge/resume 等长操作不能用默认 15s mutation 超时。
///
/// Code Logic（这个函数做什么）:
///     长 Git/Claude 类 op 用 360s，其余用 MUTATE_TIMEOUT。
fn workbench_control_timeout(op: &str) -> Duration {
    match op {
        "worktrees.commit" | "worktrees.merge" | "worktrees.push" | "worktrees.create"
        | "claude.resume" | "files.open" | "files.save_text" => Duration::from_secs(360),
        _ => MUTATE_TIMEOUT,
    }
}

/// update-config HTTP body（token + CAS）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlConfigUpdateBody {
    control_token: String,
    expected_owner_instance_id: String,
    expected_generation: u64,
    patch: RuntimeConfigPatch,
}

/// runtime-snapshot HTTP body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlRuntimeSnapshotBody {
    control_token: String,
    project_id: String,
}

/// orchestrator deliver-reviewed control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorDeliverReviewedBody {
    control_token: String,
    project_id: String,
    task_id: String,
}

/// orchestrator complete-agent-run control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorCompleteAgentRunBody {
    control_token: String,
    task_id: String,
}

/// orchestrator dispatch-once control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorDispatchOnceBody {
    control_token: String,
}

/// orchestrator abort-task control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorAbortTaskBody {
    control_token: String,
    task_id: String,
}

/// orchestrator cancel-task control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorCancelTaskBody {
    control_token: String,
    task_id: String,
}

/// orchestrator experiment create control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentCreateBody {
    control_token: String,
    #[serde(flatten)]
    request: CreateExperimentRequest,
}

/// orchestrator experiment list control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentListBody {
    control_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
}

/// orchestrator experiment get control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentGetBody {
    control_token: String,
    experiment_id: String,
}

/// orchestrator experiment approve-winner control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentApproveBody {
    control_token: String,
    experiment_id: String,
    winner_task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// orchestrator experiment cancel control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentCancelBody {
    control_token: String,
    experiment_id: String,
}

/// orchestrator experiment prepare-downgrade control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorExperimentPrepareDowngradeBody {
    control_token: String,
}

/// workflow-document/get control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlWorkflowDocumentGetBody {
    control_token: String,
    project_id: String,
}

/// workflow-document/validate control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlWorkflowDocumentValidateBody {
    control_token: String,
    project_id: String,
    content: String,
}

/// workflow-document/save control body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlWorkflowDocumentSaveBody {
    control_token: String,
    project_id: String,
    expected_hash: String,
    content: String,
}

/// events catch-up HTTP body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlEventsBody {
    control_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_owner_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_sequence: Option<u64>,
}

/// CLAUDE.md 云端推送 control body（token + 本机已保存 row 字段）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlClaudeMdPushBody {
    control_token: String,
    content: String,
    updated_at: String,
    device_id: String,
    vector_clock: std::collections::HashMap<String, u64>,
}

/// backup/create body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlBackupCreateBody {
    control_token: String,
    dest_path: String,
}

/// backup/inspect body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlBackupInspectBody {
    control_token: String,
    archive_path: String,
}

/// backup/restore body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlBackupRestoreBody {
    control_token: String,
    archive_path: String,
    mode: crate::backup::RestoreMode,
    domains: Vec<String>,
}

/// backup/list-jobs body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlBackupListJobsBody {
    control_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<i64>,
}

/// backup/rollback body。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlBackupRollbackBody {
    control_token: String,
    job_id: String,
}

/// 刷新 control token 后重建查询 body：保留全部业务字段，仅替换 `controlToken`。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 重启后 port/token 变更时，查询重试必须仍携带 projectId/afterSequence 等字段，
///     否则 runtime-snapshot / events catch-up 会静默变成 auth-only 请求。
///
/// Code Logic（这个函数做什么）:
///     `serde_json::to_value(body)` → 对象插入/覆盖 `controlToken`；非对象体报错。
pub fn rebind_control_token_body(
    body: &impl Serialize,
    new_token: &str,
) -> Result<serde_json::Value, AppError> {
    let mut value = serde_json::to_value(body)
        .map_err(|e| AppError::generic(format!("序列化 control 查询 body 失败: {e}")))?;
    let Some(obj) = value.as_object_mut() else {
        return Err(AppError::generic(
            "control 查询 body 必须是 JSON 对象才能刷新 token",
        ));
    };
    obj.insert(
        "controlToken".to_string(),
        serde_json::Value::String(new_token.to_string()),
    );
    Ok(value)
}

/// events catch-up 响应。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEventsCatchUp {
    pub messages: Vec<RuntimeRelayMessage>,
    pub latest: BackendRuntimeCursor,
}

/// 对账后截图快捷键 OS 侧应保留的状态。
///
/// Business Logic（为什么需要这个枚举）:
///     响应丢失后必须按 owner 权威状态决定 OS 是否回滚，避免 config/OS split-brain。
///
/// Code Logic（这个枚举做什么）:
///     KeepNew / RollbackToOld / ManualReconcile。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyOsReconcileDecision {
    /// owner 已提交新快捷键，OS 保留新值。
    KeepNew,
    /// owner 仍为旧值，OS 回滚到旧快捷键。
    RollbackToOld,
    /// 无法判定，阻塞进一步编辑。
    ManualReconcile,
}

/// 一次 control HTTP 调用的结果分类。
///
/// Business Logic（为什么需要这个枚举）:
///     mutation 在“确定失败”与“响应不确定”下补偿策略不同：后者禁止自动重放。
///
/// Code Logic（这个枚举做什么）:
///     Ok / Failed / Uncertain。
#[derive(Debug)]
enum ControlCallOutcome<T> {
    Ok(T),
    Failed(AppError),
    Uncertain(AppError),
}

/// Workbench mutation control 错误：确定失败 vs 不确定传输。
///
/// Business Logic（为什么需要这个枚举）:
///     workbench mutation 需要把 Uncertain 与 Failed 区分，禁止自动重放；
///     不确定结果必须经 envelope unknown 上抛，不能折叠成普通 AppError 让调用方误重试。
///
/// Code Logic（这个枚举做什么）:
///     Failed 携带确定失败的 AppError；Uncertain 仅携带传输类别（Timeout/Network）。
#[derive(Debug)]
pub enum MutationControlError {
    /// 确定失败：请求未到达 handler 或业务明确拒绝。
    Failed(AppError),
    /// 传输不确定：禁止自动重放，由上层构造成 unknown envelope。
    Uncertain {
        /// 传输分类（timeout / network）。
        transport: MutationTransportClass,
    },
}

/// GUI 侧 Backend control 客户端。
///
/// Business Logic（为什么需要这个结构）:
///     所有 GUI 运行态 mutation 统一走本客户端，避免命令层各自拼 HTTP 导致 token/重试语义分叉。
///
/// Code Logic（这个结构做什么）:
///     持有 port/token/http client；查询可一次刷新 control file；mutation 只发一次。
#[derive(Debug, Clone)]
pub struct BackendControlClient {
    port: u16,
    control_token: String,
    owner_instance_id: Option<String>,
    control_schema_version: u32,
    http: reqwest::Client,
}

impl BackendControlClient {
    /// 从本机 control file 构造客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产 GUI 命令在 sidecar 已 ensure 后读取 control file 获得 port/token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 control file；缺失或非权威描述符返回 conflict/unavailable；构造带超时的 reqwest client。
    pub fn from_control_file() -> Result<Self, AppError> {
        let control = control::read_control_file()?
            .ok_or_else(|| AppError::unavailable("后端控制文件不存在，请先启动 sidecar"))?;
        Self::from_control(&control)
    }

    /// 从已解析的 control file 构造客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     harness/测试可注入内存 control，无需依赖进程全局路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 schema/owner 权威性后填充字段。
    pub fn from_control(control: &BackendControlFile) -> Result<Self, AppError> {
        if !classify_control_descriptor(control).is_authoritative() {
            return Err(AppError::conflict(
                "control_descriptor_stale: 需要重启后端以应用设置",
            ));
        }
        if control.control_token.trim().is_empty() {
            return Err(AppError::unavailable("控制令牌为空"));
        }
        let http = reqwest::Client::builder()
            .timeout(MUTATE_TIMEOUT)
            .build()
            .map_err(|e| AppError::generic(format!("构造 control client 失败: {e}")))?;
        Ok(Self {
            port: control.port,
            control_token: control.control_token.clone(),
            owner_instance_id: control.owner_instance_id.clone(),
            control_schema_version: control.control_schema_version,
            http,
        })
    }

    /// 测试用：直接注入 port/token（不读磁盘）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元/ smoke harness 启动临时 owner HTTP 后，不依赖真实 control 文件路径竞争。
    ///
    /// Code Logic（这个函数做什么）:
    ///     填充权威 schema 与 owner id，构造 client。
    pub fn for_test(
        port: u16,
        control_token: &str,
        owner_instance_id: &str,
    ) -> Result<Self, AppError> {
        let http = reqwest::Client::builder()
            .timeout(MUTATE_TIMEOUT)
            .build()
            .map_err(|e| AppError::generic(format!("构造 control client 失败: {e}")))?;
        Ok(Self {
            port,
            control_token: control_token.to_string(),
            owner_instance_id: Some(owner_instance_id.to_string()),
            control_schema_version: CONTROL_SCHEMA_VERSION,
            http,
        })
    }

    /// 当前 control 中的 owner 实例 id（若有）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     诊断与测试断言需要读取客户端已缓存的 owner 身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回可选 owner_instance_id 切片。
    pub fn owner_instance_id(&self) -> Option<&str> {
        self.owner_instance_id.as_deref()
    }

    /// 返回 control schema 版本。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试/诊断确认客户端绑定的是当前 schema。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回缓存的 control_schema_version。
    pub fn control_schema_version(&self) -> u32 {
        self.control_schema_version
    }

    /// 查询 owner status（安全查询：连接失败可一次性刷新 control file）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 预检与诊断页需要 owner/generation/fingerprint。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/backend/control/status`；查询路径允许一次 control-file 刷新后重试。
    pub async fn status(&self) -> Result<RuntimeOwnerStatus, AppError> {
        self.query_with_optional_refresh(
            "status",
            &ControlAuthBody {
                control_token: self.control_token.clone(),
            },
        )
        .await
    }

    /// 查询 Workbench 启动摘要（五段独立 section outcomes）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI Continue Working 表面必须读 sidecar 权威摘要，不得扫本机空库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `workbench-launch-summary`；查询路径允许一次 control-file 刷新后重试。
    pub async fn workbench_launch_summary(&self) -> Result<WorkbenchLaunchSummaryDto, AppError> {
        self.query_with_optional_refresh(
            "workbench-launch-summary",
            &ControlAuthBody {
                control_token: self.control_token.clone(),
            },
        )
        .await
    }

    /// 查询权威配置快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     generation 冲突后刷新表单、热键对账都需要完整 allowlist 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST get-config；解包 snapshot；查询允许一次 control-file 刷新。
    pub async fn get_config(&self) -> Result<ConfigSnapshot, AppError> {
        let body: ControlConfigResponseBody = self
            .query_with_optional_refresh(
                "get-config",
                &ControlAuthBody {
                    control_token: self.control_token.clone(),
                },
            )
            .await?;
        Ok(body.snapshot)
    }

    /// CAS 更新权威运行配置（mutation：永不自动重放）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 只提交 allowlist patch + expected owner/generation。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST update-config 一次；不确定响应原样上抛，不重试。
    pub async fn update_config(
        &self,
        request: ConfigUpdateRequest,
    ) -> Result<ConfigUpdateResponse, AppError> {
        let body = ControlConfigUpdateBody {
            control_token: self.control_token.clone(),
            expected_owner_instance_id: request.expected_owner_instance_id,
            expected_generation: request.expected_generation,
            patch: request.patch,
        };
        self.mutate("update-config", &body).await
    }

    /// 便捷：用当前 status 更新 device_name。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke/harness 用最小补丁验证 generation 与 runtime 值收敛。
    ///
    /// Code Logic（这个函数做什么）:
    ///     以 before 的 owner/generation 提交 device_name patch。
    pub async fn update_device_name(
        &self,
        before: &RuntimeOwnerStatus,
        device_name: &str,
    ) -> Result<ConfigUpdateResponse, AppError> {
        self.update_config(ConfigUpdateRequest {
            expected_owner_instance_id: before.owner_instance_id.clone(),
            expected_generation: before.generation,
            patch: RuntimeConfigPatch {
                device_name: Some(device_name.to_string()),
                ..Default::default()
            },
        })
        .await
    }

    /// 通用 mutation：只发送一次，不自动重试。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     任意 control mutation 共享“不重放”语义，防止重复触发有副作用操作。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `send_once`，把 Failed/Uncertain 都映射为 AppError（不确定带前缀）。
    pub async fn mutate<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, AppError> {
        match self.send_once(path, body, MUTATE_TIMEOUT).await {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 代理 Workbench 操作并反序列化 `result`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得自建 Workbench runtime / RemoteWorkbenchClient / event bridge；
    ///     全部 projects/files/Git/browser/session 操作必须代理到 sidecar HeadlessOwner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/backend/control/workbench` 或大数据路径 `workbench/data`；
    ///     body = `{controlToken, op, payload}`；解包 `ControlWorkbenchResponse.result` 为 T。
    pub async fn workbench_op<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<T, AppError> {
        let value = self.workbench_op_value(op, payload).await?;
        serde_json::from_value(value).map_err(|e| {
            AppError::generic(format!("workbench control result 解析失败 ({op}): {e}"))
        })
    }

    /// 经 control API 代理 Workbench 操作，返回原始 JSON `result`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     merge/remove 等返回轻量 JSON 的命令需要 Value，不强制 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 op 选择元数据/data 路径，mutation 语义 send_once；校验 owner_instance_id 非空后返回 result。
    pub async fn workbench_op_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<serde_json::Value, AppError> {
        let (_owner, result) = self.workbench_op_with_owner_value(op, payload).await?;
        Ok(result)
    }

    /// 经 control API 代理 Workbench 操作，返回 ownerInstanceId 与强类型 result。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试/对账需要同时确认响应来自当前 owner 实例与业务结果。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 value 路径后把 result 反序列化为 T。
    pub async fn workbench_op_with_owner<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<(String, T), AppError> {
        let (owner, value) = self.workbench_op_with_owner_value(op, payload).await?;
        let result = serde_json::from_value(value).map_err(|e| {
            AppError::generic(format!("workbench control result 解析失败 ({op}): {e}"))
        })?;
        Ok((owner, result))
    }

    /// 经 control API 代理 Workbench mutation，区分 Failed 与 Uncertain。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     workbench mutation 需要把 Uncertain 与 Failed 区分，禁止自动重放；
    ///     不确定传输必须映射为 unknown envelope，不能像旧 API 那样折叠成 AppError。
    ///
    /// Code Logic（这个函数做什么）:
    ///     与 `workbench_op_with_owner_value` 相同 path/body/`send_once`；Ok→result Value；
    ///     Failed→`MutationControlError::Failed`；Uncertain→Timeout 若 AppError 为 Timeout 否则 Network；
    ///     成功路径校验 owner_instance_id 非空。
    pub async fn workbench_mutation_op_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<serde_json::Value, MutationControlError> {
        let (_owner, result) = self.workbench_op_outcome_value(op, payload).await?;
        Ok(result)
    }

    /// 经 control API 代理 Workbench 操作，返回 ownerInstanceId 与原始 result。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     workbench_op / workbench_op_value / with_owner 共享唯一 mutate 发送路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST workbench 或 workbench/data；send_once 不自动重试；校验 owner 非空；
    ///     非 mutation 路径把 Uncertain 折叠为 AppError（禁止调用方自动重放）。
    async fn workbench_op_with_owner_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<(String, serde_json::Value), AppError> {
        match self.workbench_op_outcome_value(op, payload).await {
            Ok(v) => Ok(v),
            Err(MutationControlError::Failed(e)) => Err(e),
            Err(MutationControlError::Uncertain { transport }) => Err(AppError::unavailable(
                format!("control_response_uncertain: {}", transport.as_str()),
            )),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mutation 与普通 workbench_op 共享发送路径，但 mutation 需要保留 Uncertain。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST workbench 路径；解析 ControlWorkbenchResponseBody；校验 owner；
    ///     Uncertain 按 AppError::Timeout 映射 Timeout，否则 Network。
    async fn workbench_op_outcome_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<(String, serde_json::Value), MutationControlError> {
        let path = workbench_control_path(op);
        let timeout = workbench_control_timeout(op);
        let body = ControlWorkbenchRequestBody {
            control_token: self.control_token.clone(),
            op: op.to_string(),
            payload: match serde_json::to_value(payload) {
                Ok(v) => v,
                Err(e) => {
                    return Err(MutationControlError::Failed(AppError::generic(format!(
                        "序列化 workbench payload 失败: {e}"
                    ))));
                }
            },
        };
        let resp: ControlWorkbenchResponseBody = match self.send_once(path, &body, timeout).await {
            ControlCallOutcome::Ok(v) => v,
            ControlCallOutcome::Failed(e) => return Err(MutationControlError::Failed(e)),
            ControlCallOutcome::Uncertain(e) => {
                let transport = match e {
                    AppError::Timeout(_) => MutationTransportClass::Timeout,
                    _ => MutationTransportClass::Network,
                };
                return Err(MutationControlError::Uncertain { transport });
            }
        };
        if resp.owner_instance_id.trim().is_empty() {
            return Err(MutationControlError::Failed(AppError::generic(
                "workbench control 响应缺少 ownerInstanceId",
            )));
        }
        Ok((resp.owner_instance_id, resp.result))
    }

    /// 经 control API 拉取 sidecar Orchestrator runtime snapshot。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     桌面 GUI 不得用本机空 telemetry 填充 owner 字段；必须代理到 sidecar remote-aware 路由。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/runtime-snapshot`；查询允许一次 control-file 刷新。
    pub async fn orchestrator_runtime_snapshot(
        &self,
        project_id: &str,
    ) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
        self.query_with_optional_refresh(
            "orchestrator/runtime-snapshot",
            &ControlRuntimeSnapshotBody {
                control_token: self.control_token.clone(),
                project_id: project_id.to_string(),
            },
        )
        .await
    }

    /// 经 control API 做 afterSequence 事件 catch-up。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 断线重连需要 ring 回放与显式 Gap，以便 terminal/runtime resync。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `events/catch-up`；可选 after owner/sequence；查询允许一次刷新。
    pub async fn events_catch_up(
        &self,
        after: Option<&BackendRuntimeCursor>,
    ) -> Result<ControlEventsCatchUp, AppError> {
        self.query_with_optional_refresh(
            "events/catch-up",
            &ControlEventsBody {
                control_token: self.control_token.clone(),
                after_owner_instance_id: after.map(|c| c.owner_instance_id.clone()),
                after_sequence: after.map(|c| c.sequence),
            },
        )
        .await
    }

    /// 经 control API 拉取运营通知 baseline snapshot。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI handshake 必须从 sidecar owner 拿 opaque 当前态 + asOfCursor，禁止读本机空 repo。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `operational-notifications/snapshot`；查询允许一次 control-file 刷新。
    pub async fn operational_notification_snapshot(
        &self,
    ) -> Result<OperationalNotificationSnapshot, AppError> {
        self.query_with_optional_refresh(
            "operational-notifications/snapshot",
            &ControlAuthBody {
                control_token: self.control_token.clone(),
            },
        )
        .await
    }

    /// 经 control API 在 owner 侧交付人工复核任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得在本进程跑 commit/push/merge 或持有 delivery lock；必须代理到 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/deliver-reviewed`；超时 360s；mutation 不自动重试。
    pub async fn deliver_reviewed_orchestrator_task(
        &self,
        project_id: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskViewDto, AppError> {
        let body = ControlOrchestratorDeliverReviewedBody {
            control_token: self.control_token.clone(),
            project_id: project_id.to_string(),
            task_id: task_id.to_string(),
        };
        match self
            .send_once(
                "orchestrator/deliver-reviewed",
                &body,
                ORCHESTRATOR_DELIVER_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧完成 Agent 运行（验证 + 可能 delivery）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得在本进程跑验证命令、Claude verifier 或 delivery lock；必须代理到 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/complete-agent-run`；超时 360s；mutation 不自动重试。
    pub async fn complete_orchestrator_agent_run(
        &self,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        let body = ControlOrchestratorCompleteAgentRunBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
        };
        match self
            .send_once(
                "orchestrator/complete-agent-run",
                &body,
                ORCHESTRATOR_DELIVER_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧终止任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     abort 必须检查 owner delivery 租约；GuiClient 本机库不可见 sidecar 交付。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/abort-task`；mutation 不自动重试。
    pub async fn abort_orchestrator_task(
        &self,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        let body = ControlOrchestratorAbortTaskBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
        };
        match self
            .send_once("orchestrator/abort-task", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧取消任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     cancel 必须检查 owner delivery 租约。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/cancel-task`；mutation 不自动重试。
    pub async fn cancel_orchestrator_task(
        &self,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        let body = ControlOrchestratorCancelTaskBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
        };
        match self
            .send_once("orchestrator/cancel-task", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧触发一次调度。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 无队列/PTY 权威；手动 dispatch 必须在 sidecar 执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/dispatch-once`；mutation 不自动重试。
    pub async fn dispatch_orchestrator_once(&self) -> Result<serde_json::Value, AppError> {
        let body = ControlOrchestratorDispatchOnceBody {
            control_token: self.control_token.clone(),
        };
        match self
            .send_once("orchestrator/dispatch-once", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧创建实验组。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得写本机空库或双路径 dispatch；创建权威只在 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/create`；mutation 不自动重试。
    pub async fn create_orchestrator_experiment(
        &self,
        request: CreateExperimentRequest,
    ) -> Result<OrchestratorExperimentDto, AppError> {
        let body = ControlOrchestratorExperimentCreateBody {
            control_token: self.control_token.clone(),
            request,
        };
        match self
            .send_once("orchestrator/experiments/create", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 列出 owner 侧实验组。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 本地 DB 无权威 experiment 行，看板必须读 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/list`；查询允许一次刷新。
    pub async fn list_orchestrator_experiments(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
        self.query_with_optional_refresh(
            "orchestrator/experiments/list",
            &ControlOrchestratorExperimentListBody {
                control_token: self.control_token.clone(),
                project_id: project_id.map(str::to_string),
            },
        )
        .await
    }

    /// 经 control API 读取 owner 侧实验组详情。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     详情与 candidates 权威在 owner；禁止 GuiClient 空库 NotFound。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/get`；查询允许一次刷新。
    pub async fn get_orchestrator_experiment(
        &self,
        experiment_id: &str,
    ) -> Result<OrchestratorExperimentDto, AppError> {
        self.query_with_optional_refresh(
            "orchestrator/experiments/get",
            &ControlOrchestratorExperimentGetBody {
                control_token: self.control_token.clone(),
                experiment_id: experiment_id.to_string(),
            },
        )
        .await
    }

    /// 经 control API 在 owner 侧批准实验 winner（可触发 delivery）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     批准可能跑 full-auto commit/push/merge；delivery lock 仅 owner 进程持有。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/approve-winner`；超时 360s；mutation 不自动重试。
    pub async fn approve_orchestrator_experiment_winner(
        &self,
        experiment_id: &str,
        winner_task_id: &str,
        reason: Option<&str>,
    ) -> Result<OrchestratorExperimentDto, AppError> {
        let body = ControlOrchestratorExperimentApproveBody {
            control_token: self.control_token.clone(),
            experiment_id: experiment_id.to_string(),
            winner_task_id: winner_task_id.to_string(),
            reason: reason.map(str::to_string),
        };
        match self
            .send_once(
                "orchestrator/experiments/approve-winner",
                &body,
                ORCHESTRATOR_DELIVER_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧取消实验组。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     组 CAS 与 child abort 只能在 owner 仓储执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/cancel`；mutation 不自动重试。
    pub async fn cancel_orchestrator_experiment(
        &self,
        experiment_id: &str,
    ) -> Result<OrchestratorExperimentDto, AppError> {
        let body = ControlOrchestratorExperimentCancelBody {
            control_token: self.control_token.clone(),
            experiment_id: experiment_id.to_string(),
        };
        match self
            .send_once("orchestrator/experiments/cancel", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧执行 experiment 降级 quiesce。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     关闭能力前 cancel 非终态组必须在 owner 仓储完成；GuiClient 本地扫库无效且危险。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/experiments/prepare-downgrade`；mutation 不自动重试；返回 cancelled 计数。
    pub async fn prepare_experiment_downgrade(&self) -> Result<u32, AppError> {
        let body = ControlOrchestratorExperimentPrepareDowngradeBody {
            control_token: self.control_token.clone(),
        };
        match self
            .send_once(
                "orchestrator/experiments/prepare-downgrade",
                &body,
                MUTATE_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧读取 WORKFLOW 文档。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得猜项目根路径读盘；必须向 sidecar 要权威状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/workflow-document/get`；查询允许一次刷新。
    pub async fn get_workflow_document(
        &self,
        project_id: &str,
    ) -> Result<WorkflowDocument, AppError> {
        self.query_with_optional_refresh(
            "orchestrator/workflow-document/get",
            &ControlWorkflowDocumentGetBody {
                control_token: self.control_token.clone(),
                project_id: project_id.to_string(),
            },
        )
        .await
    }

    /// 经 control API 在 owner 侧权威校验 WORKFLOW 内容。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     validate 与 save 必须同进程，避免前端提示与 owner parser 漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/workflow-document/validate`；mutation 语义 send_once 不自动重试。
    pub async fn validate_workflow_document(
        &self,
        project_id: &str,
        content: &str,
    ) -> Result<WorkflowDocument, AppError> {
        let body = ControlWorkflowDocumentValidateBody {
            control_token: self.control_token.clone(),
            project_id: project_id.to_string(),
            content: content.to_string(),
        };
        match self
            .send_once(
                "orchestrator/workflow-document/validate",
                &body,
                MUTATE_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧 CAS 保存 WORKFLOW 文档。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     排他 create / expected-hash CAS 只能在 owner 执行；GuiClient 不得本进程写盘。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/workflow-document/save`；mutation 不自动重试。
    pub async fn save_workflow_document(
        &self,
        project_id: &str,
        expected_hash: &str,
        content: &str,
    ) -> Result<WorkflowDocument, AppError> {
        let body = ControlWorkflowDocumentSaveBody {
            control_token: self.control_token.clone(),
            project_id: project_id.to_string(),
            expected_hash: expected_hash.to_string(),
            content: content.to_string(),
        };
        match self
            .send_once("orchestrator/workflow-document/save", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 提交字段级 patch：先读 status 再 CAS 一次。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多数 GUI 配置 writer 只需业务 patch，不关心手写 generation 流程。
    ///
    /// Code Logic（这个函数做什么）:
    ///     status → update_config；冲突直接返回，由 UI 刷新后用户重试。
    pub async fn apply_patch(
        &self,
        patch: RuntimeConfigPatch,
    ) -> Result<ConfigUpdateResponse, AppError> {
        let status = self.status().await?;
        self.update_config(ConfigUpdateRequest {
            expected_owner_instance_id: status.owner_instance_id,
            expected_generation: status.generation,
            patch,
        })
        .await
    }

    /// 截图快捷键两阶段补偿更新。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     快捷键 OS 副作用在 GUI 进程；权威配置在 sidecar。必须先 CAS 预检，再 OS 切换，
    ///     最后 durable commit；响应丢失时按 owner 对账，禁止 split-brain。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) get_config 预检；2) OS replace（若热键变化）；3) send_once update-config；
    ///     4) Failed → OS 回滚；Uncertain → reconcile 后 KeepNew/Rollback/ManualReconcile。
    pub async fn update_config_with_hotkey_compensation(
        &self,
        backend: &mut dyn GlobalShortcutBackend,
        patch: RuntimeConfigPatch,
    ) -> Result<ConfigUpdateResponse, AppError> {
        // 预检：owner 可达 + 取 generation/旧热键
        let before = self.get_config().await?;
        let old_hotkey = before.screenshot_hotkey.clone();
        let new_hotkey = patch
            .screenshot_hotkey
            .clone()
            .unwrap_or_else(|| old_hotkey.clone());
        let hotkey_changed = new_hotkey != old_hotkey;

        // 预检冲突：若调用方已带 expected 语义，这里用 status generation 作为 CAS 基线
        // （generation 在 get_config 与 commit 之间可能被抢占，commit 会 409）

        let mut os_replaced = false;
        if hotkey_changed {
            replace_screenshot_hotkey_os(backend, &old_hotkey, &new_hotkey)?;
            os_replaced = true;
        }

        let body = ControlConfigUpdateBody {
            control_token: self.control_token.clone(),
            expected_owner_instance_id: before.owner_instance_id.clone(),
            expected_generation: before.generation,
            patch,
        };

        match self
            .send_once::<ConfigUpdateResponse>("update-config", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(resp) => Ok(resp),
            ControlCallOutcome::Failed(err) => {
                if os_replaced {
                    compensate_screenshot_hotkey_os(backend, &old_hotkey, &new_hotkey)?;
                }
                Err(err)
            }
            ControlCallOutcome::Uncertain(err) => {
                if !os_replaced {
                    return Err(AppError::unavailable(format!(
                        "control_response_uncertain: {err}"
                    )));
                }
                let decision = self
                    .reconcile_hotkey_after_uncertain(&old_hotkey, &new_hotkey, before.generation)
                    .await?;
                match decision {
                    HotkeyOsReconcileDecision::KeepNew => {
                        // owner 已提交新值：保留 OS 新快捷键，再读一次配置返回
                        let snap = self.get_config().await?;
                        Ok(ConfigUpdateResponse {
                            owner_instance_id: snap.owner_instance_id.clone(),
                            generation: snap.generation,
                            snapshot: snap,
                        })
                    }
                    HotkeyOsReconcileDecision::RollbackToOld => {
                        compensate_screenshot_hotkey_os(backend, &old_hotkey, &new_hotkey)?;
                        Err(AppError::unavailable(format!(
                            "control_response_uncertain_rolled_back: {err}"
                        )))
                    }
                    HotkeyOsReconcileDecision::ManualReconcile => Err(AppError::conflict(format!(
                        "hotkey_reconcile_required: OS 与配置可能不一致，请手动确认后重试 ({err})"
                    ))),
                }
            }
        }
    }

    /// 响应丢失后按 owner/generation/config 对账快捷键。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     不能盲目重放 mutation 或盲目回滚 OS；必须以 owner 权威状态为准。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 get_config；generation 前进且热键=new → KeepNew；
    ///     generation 未变且热键=old → RollbackToOld；其它 → ManualReconcile。
    pub async fn reconcile_hotkey_after_uncertain(
        &self,
        old_hotkey: &str,
        new_hotkey: &str,
        preflight_generation: u64,
    ) -> Result<HotkeyOsReconcileDecision, AppError> {
        let snap = match self.get_config().await {
            Ok(s) => s,
            Err(_) => return Ok(HotkeyOsReconcileDecision::ManualReconcile),
        };
        if snap.generation > preflight_generation && snap.screenshot_hotkey == new_hotkey {
            return Ok(HotkeyOsReconcileDecision::KeepNew);
        }
        if snap.generation == preflight_generation && snap.screenshot_hotkey == old_hotkey {
            return Ok(HotkeyOsReconcileDecision::RollbackToOld);
        }
        // generation 前进但热键不是 new，或 generation 未变但热键已变等歧义
        if snap.screenshot_hotkey == new_hotkey && snap.generation >= preflight_generation {
            return Ok(HotkeyOsReconcileDecision::KeepNew);
        }
        if snap.screenshot_hotkey == old_hotkey {
            return Ok(HotkeyOsReconcileDecision::RollbackToOld);
        }
        Ok(HotkeyOsReconcileDecision::ManualReconcile)
    }

    /// 安全查询：失败时最多刷新一次 control file 再试。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     sidecar 重启后 port/token 可能变；查询允许一次刷新，避免 GUI 粘在旧 control。
    ///
    /// Code Logic（这个函数做什么）:
    ///     send_once；若失败且 from_control_file 成功得到新 client，用**保留原业务字段**的 body
    ///     仅替换 `controlToken` 后再发一次查询（不得丢 projectId/afterSequence 等字段）。
    async fn query_with_optional_refresh<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, AppError> {
        match self.send_once(path, body, QUERY_TIMEOUT).await {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(first) | ControlCallOutcome::Uncertain(first) => {
                // 仅查询路径允许一次刷新；刷新失败则返回首次错误
                let refreshed = match control::read_control_file() {
                    Ok(Some(c)) => Self::from_control(&c).ok(),
                    _ => None,
                };
                let Some(new_client) = refreshed else {
                    return Err(first);
                };
                // 若 port/token 未变，不再重试避免放大故障
                if new_client.port == self.port && new_client.control_token == self.control_token {
                    return Err(first);
                }
                let retry_body = match rebind_control_token_body(body, &new_client.control_token) {
                    Ok(v) => v,
                    Err(e) => return Err(e),
                };
                match new_client.send_once(path, &retry_body, QUERY_TIMEOUT).await {
                    ControlCallOutcome::Ok(v) => Ok(v),
                    ControlCallOutcome::Failed(e) | ControlCallOutcome::Uncertain(e) => Err(e),
                }
            }
        }
    }

    /// 经 control API 触发 owner 侧 Cloud Sync 完整同步（mutation，不自动重试）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得在本进程跑 Git workdir 写路径；手动「立即同步」必须进 sidecar 单飞门闸。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `cloud-sync/trigger`；超时 360s（Wait 门闸最长 300s）。
    pub async fn cloud_sync_trigger(
        &self,
    ) -> Result<crate::cloud_sync::engine::CloudSyncResult, AppError> {
        let body = ControlAuthBody {
            control_token: self.control_token.clone(),
        };
        match self
            .send_once("cloud-sync/trigger", &body, CLOUD_SYNC_MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧测试 Cloud Sync 连通性。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     连通性探测可能触达正式 workdir 的 fetch 路径，须走 owner gate，禁止 GUI 本地第二路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `cloud-sync/test`。
    pub async fn cloud_sync_test(
        &self,
    ) -> Result<crate::cloud_sync::engine::TestCloudSyncResult, AppError> {
        let body = ControlAuthBody {
            control_token: self.control_token.clone(),
        };
        match self
            .send_once("cloud-sync/test", &body, CLOUD_SYNC_MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧推送 CLAUDE.md 到 GitHub 工作区。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CLAUDE.md 云推送与完整 sync 共享同一 Git workdir 临界区；GUI 只传已保存 row，不本地写 git。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `cloud-sync/claude-md-push` body = token + ClaudeMdRow 字段；mutation 不自动重试。
    pub async fn cloud_sync_claude_md_push(
        &self,
        row: &crate::models::claude_md::ClaudeMdRow,
    ) -> Result<crate::cloud_sync::engine::CloudClaudeMdPushResultDto, AppError> {
        let body = ControlClaudeMdPushBody {
            control_token: self.control_token.clone(),
            content: row.content.clone(),
            updated_at: row.updated_at.clone(),
            device_id: row.device_id.clone(),
            vector_clock: row.vector_clock.clone(),
        };
        match self
            .send_once(
                "cloud-sync/claude-md-push",
                &body,
                CLOUD_SYNC_MUTATE_TIMEOUT,
            )
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧准备 Open/Reveal local target。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得自读本地空/过期 history 猜路径；必须向 sidecar 校验
    ///     Receive+completed+path exists 后拿 local target，再在 GUI 调 opener。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/prepare-open` body={controlToken,taskId,action}；mutation 不自动重试。
    pub async fn prepare_transfer_open(
        &self,
        task_id: &str,
        action: TransferOpenAction,
    ) -> Result<LocalTransferOpenTarget, AppError> {
        let body = ControlTransferPrepareOpenBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
            action,
        };
        match self
            .send_once("transfer/prepare-open", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧发起发送。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得本进程 spawn send loop；claim/registry 仅 owner 持有。
    ///     稳定 clientOperationId 保证 lost ACK 同意图幂等。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/send`；返回 accepted JSON；mutation 不自动重试。
    pub async fn send_transfer(
        &self,
        device_id: &str,
        file_path: &str,
        client_operation_id: &str,
    ) -> Result<serde_json::Value, AppError> {
        let body = ControlTransferSendBody {
            control_token: self.control_token.clone(),
            device_id: device_id.to_string(),
            file_path: file_path.to_string(),
            client_operation_id: client_operation_id.to_string(),
        };
        match self.send_once("transfer/send", &body, MUTATE_TIMEOUT).await {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧幂等 retry。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     recovery claim 必须与 recover_pending 同进程，避免双 drive。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/retry` → TransferTaskDto。
    pub async fn retry_transfer(
        &self,
        task_id: &str,
        client_operation_id: &str,
    ) -> Result<TransferTaskDto, AppError> {
        let body = ControlTransferRecoveryBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
            client_operation_id: client_operation_id.to_string(),
        };
        match self
            .send_once("transfer/retry", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧幂等 resume。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     resume 能力探测与 claim 只在 owner 执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/resume` → TransferTaskDto。
    pub async fn resume_transfer(
        &self,
        task_id: &str,
        client_operation_id: &str,
    ) -> Result<TransferTaskDto, AppError> {
        let body = ControlTransferRecoveryBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
            client_operation_id: client_operation_id.to_string(),
        };
        match self
            .send_once("transfer/resume", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧查询 clientOperationId 真值。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     lost-ACK 对账与 registry 优先读取必须在 owner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/get-operation` → TransferOperationStatus。
    pub async fn get_transfer_operation(
        &self,
        client_operation_id: &str,
    ) -> Result<TransferOperationStatus, AppError> {
        let body = ControlTransferGetOperationBody {
            control_token: self.control_token.clone(),
            client_operation_id: client_operation_id.to_string(),
        };
        match self
            .send_once("transfer/get-operation", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧取消传输。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     cancel token 只在 owner registry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `transfer/cancel` → `{ok,id}`。
    pub async fn cancel_transfer(&self, task_id: &str) -> Result<serde_json::Value, AppError> {
        let body = ControlTransferCancelBody {
            control_token: self.control_token.clone(),
            task_id: task_id.to_string(),
        };
        match self
            .send_once("transfer/cancel", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧创建导出备份。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 不得直读 sidecar DB 写 ZIP；导出路径由用户选择后代理到 owner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/create`；超时 BACKUP_MUTATE_TIMEOUT；mutation 不自动重试。
    pub async fn create_backup(
        &self,
        dest_path: &str,
    ) -> Result<crate::backup::CreateBackupResult, AppError> {
        let body = ControlBackupCreateBody {
            control_token: self.control_token.clone(),
            dest_path: dest_path.to_string(),
        };
        match self
            .send_once("backup/create", &body, BACKUP_MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧只读 inspect 备份包。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     恢复确认前预览；inspect 不写 DB，但仍走 owner 统一校验入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/inspect`；超时 MUTATE_TIMEOUT；不确定响应不自动重试。
    pub async fn inspect_backup(
        &self,
        archive_path: &str,
    ) -> Result<crate::backup::InspectPreview, AppError> {
        let body = ControlBackupInspectBody {
            control_token: self.control_token.clone(),
            archive_path: archive_path.to_string(),
        };
        match self
            .send_once("backup/inspect", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧事务恢复。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     exclusive maintenance_gate 与 recovery_jobs 仅 sidecar owner 持有。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/restore`；超时 BACKUP_MUTATE_TIMEOUT；mutation 不自动重试。
    pub async fn restore_backup(
        &self,
        archive_path: &str,
        mode: crate::backup::RestoreMode,
        domains: Vec<String>,
    ) -> Result<crate::backup::RestoreResult, AppError> {
        let body = ControlBackupRestoreBody {
            control_token: self.control_token.clone(),
            archive_path: archive_path.to_string(),
            mode,
            domains,
        };
        match self
            .send_once("backup/restore", &body, BACKUP_MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 列出 recovery jobs。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     job 表在 sidecar；GUI 只展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/list-jobs`；query 语义允许一次 token 刷新。
    pub async fn list_recovery_jobs(
        &self,
        limit: Option<i64>,
    ) -> Result<Vec<crate::storage::RecoveryJobRow>, AppError> {
        let body = ControlBackupListJobsBody {
            control_token: self.control_token.clone(),
            limit,
        };
        self.query_with_optional_refresh("backup/list-jobs", &body)
            .await
    }

    /// 经 control API 列出 pre-restore 备份。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     备份目录在 sidecar data_dir；GUI 只读列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/list-backups`；query 语义允许一次 token 刷新。
    pub async fn list_pre_restore_backups(
        &self,
    ) -> Result<Vec<crate::backup::PreRestoreBackupInfo>, AppError> {
        let body = ControlAuthBody {
            control_token: self.control_token.clone(),
        };
        self.query_with_optional_refresh("backup/list-backups", &body)
            .await
    }

    /// 经 control API 按 job 回退。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     回退会再次 replace-domain 写库，必须走 owner exclusive gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `backup/rollback`；超时 BACKUP_MUTATE_TIMEOUT；mutation 不自动重试。
    pub async fn rollback_recovery_job(
        &self,
        job_id: &str,
    ) -> Result<crate::backup::RestoreResult, AppError> {
        let body = ControlBackupRollbackBody {
            control_token: self.control_token.clone(),
            job_id: job_id.to_string(),
        };
        match self
            .send_once("backup/rollback", &body, BACKUP_MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 发送一次 control POST，不做自动重试。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mutation 与查询的底层发送语义统一；mutation 调用方禁止循环调用本方法重放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `http://127.0.0.1:{port}/api/backend/control/{path}`；
    ///     连接级失败→Failed；超时/响应体损坏→Uncertain；HTTP 4xx/5xx 可解析信封→Failed。
    async fn send_once<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
        timeout: Duration,
    ) -> ControlCallOutcome<T> {
        let url = format!("http://127.0.0.1:{}/api/backend/control/{path}", self.port);
        let request = self.http.post(&url).timeout(timeout).json(body);
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    return ControlCallOutcome::Uncertain(AppError::timeout(format!(
                        "control {path} 超时: {e}"
                    )));
                }
                // 连接被拒绝等：请求很可能未到达 handler → Failed（可安全不补偿重放）
                if e.is_connect() {
                    return ControlCallOutcome::Failed(AppError::unavailable(format!(
                        "control {path} 连接失败: {e}"
                    )));
                }
                // 其它传输错误（可能已发送）→ Uncertain
                return ControlCallOutcome::Uncertain(AppError::unavailable(format!(
                    "control {path} 传输失败: {e}"
                )));
            }
        };

        let status = response.status();
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ControlCallOutcome::Uncertain(AppError::unavailable(format!(
                    "control {path} 读取响应失败: {e}"
                )));
            }
        };

        if status.is_success() {
            match serde_json::from_slice::<T>(&bytes) {
                Ok(v) => ControlCallOutcome::Ok(v),
                Err(e) => ControlCallOutcome::Uncertain(AppError::generic(format!(
                    "control {path} 成功响应无法解析: {e}"
                ))),
            }
        } else {
            // 错误响应：尽量解析业务码；解析失败仍视为确定失败（带 HTTP 状态）
            let msg = parse_control_error_message(&bytes)
                .unwrap_or_else(|| format!("control {path} HTTP {status}"));
            let err = if status.as_u16() == 409 {
                AppError::conflict(msg)
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                AppError::unavailable(msg)
            } else if status.as_u16() == 400 {
                AppError::validation(msg)
            } else if status.as_u16() == 404 {
                AppError::not_found(msg)
            } else if status.as_u16() == 503 {
                AppError::unavailable(msg)
            } else if status.as_u16() == 504 {
                AppError::timeout(msg)
            } else {
                AppError::generic(msg)
            };
            ControlCallOutcome::Failed(err)
        }
    }
}

/// 从 control 错误响应 body 提取可读消息。
///
/// Business Logic（为什么需要这个函数）:
///     优先展示服务端 error 信封消息，便于设置页提示 generation 冲突等。
///
/// Code Logic（这个函数做什么）:
///     尝试解析 `{error}` JSON；失败返回 None。
fn parse_control_error_message(bytes: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrBody {
        error: Option<String>,
        code: Option<String>,
    }
    let body: ErrBody = serde_json::from_slice(bytes).ok()?;
    if let Some(ref code) = body.code {
        if code == "conflict" || code.contains("conflict") {
            if let Some(err) = body.error {
                return Some(err);
            }
            return Some(code.clone());
        }
    }
    body.error.or(body.code)
}

/// 纯函数：根据观测到的 owner 快照决定热键 OS 对账动作（供单测无 HTTP）。
///
/// Business Logic（为什么需要这个函数）:
///     对账规则应可单测且不依赖网络。
///
/// Code Logic（这个函数做什么）:
///     比较 preflight_generation 与 snap.generation/hotkey。
pub fn decide_hotkey_reconcile(
    snap_generation: u64,
    snap_hotkey: &str,
    preflight_generation: u64,
    old_hotkey: &str,
    new_hotkey: &str,
) -> HotkeyOsReconcileDecision {
    if snap_generation > preflight_generation && snap_hotkey == new_hotkey {
        return HotkeyOsReconcileDecision::KeepNew;
    }
    if snap_generation == preflight_generation && snap_hotkey == old_hotkey {
        return HotkeyOsReconcileDecision::RollbackToOld;
    }
    if snap_hotkey == new_hotkey && snap_generation >= preflight_generation {
        return HotkeyOsReconcileDecision::KeepNew;
    }
    if snap_hotkey == old_hotkey {
        return HotkeyOsReconcileDecision::RollbackToOld;
    }
    HotkeyOsReconcileDecision::ManualReconcile
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::FakeGlobalShortcutBackend;

    /// 验证已提交新值时对账保留新快捷键。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     响应丢失但 owner 已 commit 时回滚 OS 会造成 split-brain。
    ///
    /// Code Logic（这个测试做什么）:
    ///     generation+1 且 hotkey=new → KeepNew。
    #[test]
    fn reconcile_keeps_new_when_owner_committed() {
        let d = decide_hotkey_reconcile(1, "<ctrl>+n", 0, "<ctrl>+o", "<ctrl>+n");
        assert_eq!(d, HotkeyOsReconcileDecision::KeepNew);
    }

    /// 验证确认未提交时对账回滚旧快捷键。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     owner 仍为旧 generation/hotkey 时 OS 必须恢复旧注册。
    ///
    /// Code Logic（这个测试做什么）:
    ///     generation 不变且 hotkey=old → RollbackToOld。
    #[test]
    fn reconcile_rolls_back_when_owner_still_old() {
        let d = decide_hotkey_reconcile(0, "<ctrl>+o", 0, "<ctrl>+o", "<ctrl>+n");
        assert_eq!(d, HotkeyOsReconcileDecision::RollbackToOld);
    }

    /// 验证歧义状态要求人工 reconcile。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     无法判定时不得擅自改 OS 或重放 mutation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     generation 前进但 hotkey 既非 old 也非 new → ManualReconcile。
    #[test]
    fn reconcile_blocks_when_ambiguous() {
        let d = decide_hotkey_reconcile(2, "<ctrl>+x", 0, "<ctrl>+o", "<ctrl>+n");
        assert_eq!(d, HotkeyOsReconcileDecision::ManualReconcile);
    }

    /// 验证 OS replace 失败时不进入 owner commit（由调用方短路）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     OS 侧失败必须保持旧快捷键，且不得提交配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Fake backend fail_register → replace 返回 Err，registered 仍为旧值。
    #[test]
    fn os_replace_failure_keeps_old_registration() {
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            fail_register: vec!["<ctrl>+<shift>+s".into()],
            ..Default::default()
        };
        let err = replace_screenshot_hotkey_os(&mut fake, "<ctrl>+s", "<ctrl>+<shift>+s")
            .expect_err("register 应失败");
        assert!(err.to_string().contains("注入") || err.to_string().contains("注册失败"));
        assert_eq!(fake.registered(), vec!["<ctrl>+s".to_string()]);
    }

    /// 验证 owner durable 失败时 OS 回滚旧快捷键。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     两阶段路径在 commit 明确失败时必须恢复 OS，避免 split-brain。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动最小 owner HTTP，inject 500 save 失败；OS 先切到新热键，失败后回滚。
    #[tokio::test]
    async fn hotkey_os_rolls_back_when_owner_durable_save_fails() {
        use crate::config::{
            AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
        };
        use crate::config_runtime::ConfigRuntime;
        use crate::config_store::MemoryConfigStore;
        use axum::extract::State as AxumState;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        #[derive(Clone)]
        struct S {
            runtime: Arc<ConfigRuntime>,
            token: String,
            fail: Arc<AtomicBool>,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Auth {
            control_token: String,
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Upd {
            control_token: String,
            expected_owner_instance_id: String,
            expected_generation: u64,
            patch: RuntimeConfigPatch,
        }

        let owner = "owner-hotkey-test";
        let token = "tok-hotkey";
        let initial = AppConfig {
            device_id: "d".into(),
            device_name: "n".into(),
            http_port: 0,
            receive_dir: "/tmp/r".into(),
            db_path: "/tmp/d.db".into(),
            screenshot_hotkey: "<ctrl>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::with_owner(initial, store, owner.into()));
        let fail = Arc::new(AtomicBool::new(true));
        let state = S {
            runtime: Arc::clone(&runtime),
            token: token.into(),
            fail: Arc::clone(&fail),
        };
        let app = Router::new()
            .route(
                "/api/backend/control/get-config",
                post(
                    |AxumState(s): AxumState<S>, Json(b): Json<Auth>| async move {
                        if b.control_token != s.token {
                            return Err((
                                StatusCode::UNAUTHORIZED,
                                Json(
                                    serde_json::json!({"error":"bad token","code":"unauthorized"}),
                                ),
                            ));
                        }
                        let snap = s.runtime.snapshot_with_generation().unwrap();
                        Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(
                            serde_json::json!({"snapshot": snap}),
                        ))
                    },
                ),
            )
            .route(
                "/api/backend/control/update-config",
                post(
                    |AxumState(s): AxumState<S>, Json(b): Json<Upd>| async move {
                        if b.control_token != s.token {
                            return Err((
                                StatusCode::UNAUTHORIZED,
                                Json(
                                    serde_json::json!({"error":"bad token","code":"unauthorized"}),
                                ),
                            ));
                        }
                        if s.fail.swap(false, Ordering::SeqCst) {
                            return Err((
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(serde_json::json!({
                                    "error":"注入: durable save 失败",
                                    "code":"internal"
                                })),
                            ));
                        }
                        match s
                            .runtime
                            .apply_patch_if_generation(
                                &b.expected_owner_instance_id,
                                b.expected_generation,
                                b.patch,
                            )
                            .await
                        {
                            Ok(r) => Ok(Json(r)),
                            Err(e) => Err((
                                StatusCode::CONFLICT,
                                Json(
                                    serde_json::json!({"error": e.to_string(), "code":"conflict"}),
                                ),
                            )),
                        }
                    },
                ),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, token, owner).unwrap();
        let mut fake = FakeGlobalShortcutBackend {
            registered: vec!["<ctrl>+s".into()],
            ..Default::default()
        };
        let err = client
            .update_config_with_hotkey_compensation(
                &mut fake,
                RuntimeConfigPatch {
                    screenshot_hotkey: Some("<ctrl>+<shift>+s".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("durable fail");
        assert!(err.to_string().contains("注入") || err.to_string().contains("save"));
        assert_eq!(
            fake.registered(),
            vec!["<ctrl>+s".to_string()],
            "OS 应回滚旧热键"
        );
        assert_eq!(
            runtime.snapshot().unwrap().screenshot_hotkey,
            "<ctrl>+s",
            "owner 保持旧热键"
        );
    }

    /// 验证 workbench_op 命中 mock control workbench 路由并返回 result。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 代理路径必须真正 POST control workbench 并校验 token/op/owner 信封。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 127.0.0.1:0 mock；断言 controlToken/op=projects.list；返回 owner + 空列表 result。
    #[tokio::test]
    async fn workbench_op_projects_list_hits_control_route() {
        use axum::extract::State as AxumState;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct HitState {
            ops: Arc<Mutex<Vec<String>>>,
            token: String,
            owner: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            control_token: String,
            op: String,
            #[serde(default)]
            #[allow(dead_code)]
            payload: serde_json::Value,
        }

        let ops = Arc::new(Mutex::new(Vec::new()));
        let token = "tok-wb-1".to_string();
        let owner = "owner-sidecar-1".to_string();
        let state = HitState {
            ops: Arc::clone(&ops),
            token: token.clone(),
            owner: owner.clone(),
        };

        let app = Router::new()
            .route(
                "/api/backend/control/workbench",
                post(
                    |AxumState(s): AxumState<HitState>, Json(body): Json<WbReq>| async move {
                        if body.control_token != s.token {
                            return Err((
                                StatusCode::UNAUTHORIZED,
                                Json(
                                    serde_json::json!({"error":"bad token","code":"unauthorized"}),
                                ),
                            ));
                        }
                        s.ops.lock().unwrap().push(body.op.clone());
                        let result = if body.op == "projects.list" {
                            serde_json::json!([])
                        } else {
                            serde_json::json!({"ok": true})
                        };
                        Ok::<_, (StatusCode, Json<serde_json::Value>)>(Json(serde_json::json!({
                            "ownerInstanceId": s.owner,
                            "result": result,
                        })))
                    },
                ),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, &token, &owner).unwrap();
        let (got_owner, items): (String, Vec<serde_json::Value>) = client
            .workbench_op_with_owner("projects.list", serde_json::json!({}))
            .await
            .expect("workbench_op projects.list");
        assert_eq!(got_owner, owner);
        assert!(items.is_empty());
        assert_eq!(
            ops.lock().unwrap().as_slice(),
            &["projects.list".to_string()]
        );
    }

    /// 验证 GuiClient 的 require_owner 冲突码（与 bridge ensure 拒绝路径一致）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 进程不得成为 Workbench owner；require_owner 必须稳定拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 RuntimeRole::GuiClient.require_owner() 并断言 Conflict + runtime_owner_required。
    #[test]
    fn gui_client_require_owner_is_conflict() {
        use crate::backend::authority::RuntimeRole;
        use crate::error::AppErrorCategory;
        let err = RuntimeRole::GuiClient
            .require_owner()
            .expect_err("GuiClient 必须被拒绝");
        assert_eq!(err.classify(), AppErrorCategory::Conflict);
        assert_eq!(err.to_string(), "runtime_owner_required");
    }

    /// 验证 workbench control 路径选择：大 payload op 走 data。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     元数据 256 KiB limit 不能截断 open/save/preview/browser。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 workbench_control_path 对 metadata/data 类 op 的分流。
    #[test]
    fn workbench_control_path_routes_large_ops_to_data() {
        assert_eq!(workbench_control_path("projects.list"), "workbench");
        assert_eq!(workbench_control_path("sessions.create"), "workbench");
        assert_eq!(workbench_control_path("files.open"), "workbench/data");
        assert_eq!(workbench_control_path("files.save_text"), "workbench/data");
        assert_eq!(
            workbench_control_path("files.preview_sqlite"),
            "workbench/data"
        );
        assert_eq!(
            workbench_control_path("browser.create_preview"),
            "workbench/data"
        );
        assert_eq!(workbench_control_path("sessions.write"), "workbench/data");
    }
}
