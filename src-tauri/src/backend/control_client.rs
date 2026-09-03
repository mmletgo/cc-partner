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

use crate::agent_hub::cross_agent::{
    ApplyCrossAgentInstructionRequest, CrossAgentApplyTargetResult, CrossAgentPreviewReport,
    PreviewCrossAgentInstructionRequest,
};
use crate::agent_hub::cross_agent_full::{
    ApplyCrossAgentFullRequest, CrossAgentFullApplyItemResult, CrossAgentFullPlan,
    PreviewCrossAgentFullRequest,
};
use crate::agent_hub::project_scope::{AgentHubProjectPreview, AgentHubProjectStatus};
use crate::agent_hub::service::{
    AgentHubAssetDetailDto, AgentHubAssetSummaryDto, AgentHubStatusDto, InstructionBlockDto,
    ListAssetsRequest, PairInstructionVariantsRequest, ResolveConflictRequest,
    SetTargetBindingRequest, UpdateInstructionBlockRequest, UpdateInstructionRequest,
};
use crate::agent_hub::user_instructions::{
    AdaptInstructionToOtherAgentsRequest, AdaptInstructionToOtherAgentsResult,
    AnalyzeInstructionOriginalRequest, AnalyzeInstructionOriginalResult,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, ReadUserNativeInstructionFileRequest,
    ReviseInstructionSlotRequest, ReviseInstructionSlotResult, SaveUserInstructionBlocksRequest,
    UserInstructionCanonicalDto, UserInstructionPlanDto, UserInstructionWorkspaceDto,
    UserNativeInstructionFileDto, WriteUserNativeInstructionFileRequest,
};
use crate::backend::authority::{classify_control_descriptor, CONTROL_SCHEMA_VERSION};
use crate::backend::control::{self, BackendControlFile};
use crate::backend::control_api::WorkbenchLaunchSummaryDto;
use crate::backend::event_bus::{BackendRuntimeCursor, RuntimeRelayMessage};
use crate::commands::orchestrator::{
    AppendOrchestratorTaskBlockMemberRequest, CreateOrchestratorTaskBlockRequest,
    OrchestratorRuntimeSnapshotDto, OrchestratorTaskBlockViewCreatedDto, OrchestratorTaskViewDto,
    ReorderOrchestratorTaskBlockMembersRequest,
};
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
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

/// control 查询超时。
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// Portable inventory 会扫描三个 CLI、配置树与项目映射，不能套用轻量查询预算。
const PORTABLE_INVENTORY_TIMEOUT: Duration = Duration::from_secs(30);
/// control 事件 NDJSON 单行最大字节（1 MiB）。
const CONTROL_EVENT_STREAM_MAX_LINE_BYTES: usize = 1024 * 1024;
/// control mutation 超时（配置落盘可能稍慢）。
const MUTATE_TIMEOUT: Duration = Duration::from_secs(15);
/// Cloud Sync mutation 超时（覆盖 Wait{300s} 门闸 + git 网络操作）。
const CLOUD_SYNC_MUTATE_TIMEOUT: Duration = Duration::from_secs(360);
/// 备份创建/恢复/回退超时（ZIP 读写 + exclusive maintenance_gate + 领域 bulk）。
const BACKUP_MUTATE_TIMEOUT: Duration = Duration::from_secs(360);
/// Orchestrator deliver 超时（git commit/push/merge 可能很长）。
const ORCHESTRATOR_DELIVER_TIMEOUT: Duration = Duration::from_secs(360);
/// 用户级镜像 preview：本机全 Agent 扫描 + 对端 inventory，墙钟 120s。
/// 不得套用 15s MUTATE_TIMEOUT：两侧 inventory 各自可接近 portable 的 30s 扫描预算。
const USER_MIRROR_PREVIEW_TIMEOUT: Duration = Duration::from_secs(120);
/// 用户级镜像 apply：全 Agent 写盘 + 对端 objects，墙钟 900s。
const USER_MIRROR_APPLY_TIMEOUT: Duration = Duration::from_secs(900);

/// 选择 portable read control 操作的响应预算。
///
/// Business Logic（为什么需要这个函数）:
///     inventory 会执行三 Agent 的 CLI/配置树扫描，实测可稳定超过轻量 ledger 查询的 3 秒预算；
///     本机与远端 inventory 都必须等待扫描完成，get-by-request 则仍应快速失败。
///
/// Code Logic（这个函数做什么）:
///     两个 inventory 操作返回 30 秒预算，其余 portable read 操作返回 QUERY_TIMEOUT。
fn portable_control_read_timeout(op: &str) -> Duration {
    match op {
        "agent_hub.inspect_portable_inventory" | "agent_hub.list_remote_portable_inventory" => {
            PORTABLE_INVENTORY_TIMEOUT
        }
        _ => QUERY_TIMEOUT,
    }
}

/// 把可选 deviceId 写入 control payload，供 owner 决定本机或 P2P。
///
/// Business Logic: Preview 等请求 deny_unknown_fields，owner 会先剥离 deviceId。
/// Code Logic: 空/缺省不写字段；非空插入 camelCase `deviceId`。
fn merge_device_id(
    mut payload: serde_json::Value,
    device_id: Option<String>,
) -> Result<serde_json::Value, AppError> {
    if let Some(id) = device_id.filter(|s| !s.trim().is_empty()) {
        let obj = payload.as_object_mut().ok_or_else(|| {
            AppError::generic("agent hub control payload 必须是对象才能附加 deviceId")
        })?;
        obj.insert("deviceId".to_string(), serde_json::Value::String(id));
    }
    Ok(payload)
}

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
        | "sessions.write"
        | "sessions.pasteImage"
        | "notes.save" => "workbench/data",
        _ => "workbench",
    }
}

/// Workbench control 超时选择。
///
/// Business Logic（为什么需要这个函数）:
///     commit/resume 等长操作不能用默认 15s mutation 超时；merge 会跟随 Claude 输出，
///     GUI→sidecar HTTP 不能用墙钟提前掐断。
///
/// Code Logic（这个函数做什么）:
///     merge 返回 None（不设 request timeout）；Claude/Codex session 搜索/preview 用 60s；
///     其它长 Git/Claude op 用 360s，其余用 MUTATE_TIMEOUT。
fn workbench_control_timeout(op: &str) -> Option<Duration> {
    match op {
        "worktrees.merge" => None,
        "worktrees.commit"
        | "worktrees.push"
        | "worktrees.create"
        | "claude.resume"
        | "files.open"
        | "files.save_text"
        | "agent_ledger.export_token_stats"
        | "sessions.pasteImage" => Some(Duration::from_secs(360)),
        "claude.search" | "claude.preview" => Some(Duration::from_secs(60)),
        _ => Some(MUTATE_TIMEOUT),
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorTaskBlockCreateBody {
    control_token: String,
    #[serde(flatten)]
    request: CreateOrchestratorTaskBlockRequest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorTaskBlockAppendBody {
    control_token: String,
    #[serde(flatten)]
    request: AppendOrchestratorTaskBlockMemberRequest,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlOrchestratorTaskBlockReorderBody {
    control_token: String,
    #[serde(flatten)]
    request: ReorderOrchestratorTaskBlockMembersRequest,
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

/// 有界 NDJSON 行解码器：跨 chunk 重组完整事件行。
///
/// Business Logic（为什么需要这个结构）:
///     control events/stream 以 NDJSON 推送 terminal/runtime 消息；网络 chunk 可能切开 UTF-8
///     或多行，客户端必须按完整换行边界解析，并对超大/损坏行给出稳定错误码。
///
/// Code Logic（这个结构做什么）:
///     维护 pending 字节缓冲；`push` 按 `\n`（可选 `\r`）切行并反序列化为 `RuntimeRelayMessage`；
///     单行或 pending 超 1 MiB → `control_event_stream_line_too_large`；非法 JSON →
///     `control_event_stream_malformed`；`finish` 遇非空白残留 → `control_event_stream_truncated`。
#[derive(Debug, Default)]
struct ControlEventStreamDecoder {
    pending: Vec<u8>,
}

impl ControlEventStreamDecoder {
    /// 向解码器追加字节并吐出完整 NDJSON 消息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     流式 body 以任意大小 chunk 到达，调用方需要增量解析而不阻塞整响应。
    ///
    /// Code Logic（这个函数做什么）:
    ///     追加到 pending，扫描完整行，反序列化为 `RuntimeRelayMessage`；错误消息只用稳定 code，
    ///     禁止拼接原始 line 内容。
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeRelayMessage>, AppError> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        let mut line_start = 0usize;
        for newline in 0..self.pending.len() {
            if self.pending[newline] != b'\n' {
                continue;
            }
            if newline - line_start > CONTROL_EVENT_STREAM_MAX_LINE_BYTES {
                self.pending.clear();
                return Err(AppError::validation("control_event_stream_line_too_large"));
            }
            let mut line = &self.pending[line_start..newline];
            line_start = newline + 1;
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.is_empty() {
                continue;
            }
            let message = serde_json::from_slice::<RuntimeRelayMessage>(line)
                .map_err(|_| AppError::generic("control_event_stream_malformed"))?;
            messages.push(message);
        }
        if line_start > 0 {
            self.pending.drain(..line_start);
        }
        if self.pending.len() > CONTROL_EVENT_STREAM_MAX_LINE_BYTES {
            self.pending.clear();
            return Err(AppError::validation("control_event_stream_line_too_large"));
        }
        Ok(messages)
    }

    /// 流结束后冲刷解码器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     连接正常结束时若还有半行 JSON，必须报 truncated 以便上层 resync，而不是静默丢弃。
    ///
    /// Code Logic（这个函数做什么）:
    ///     pending 全空白则清空返回空；否则清空并返回 `control_event_stream_truncated`。
    fn finish(&mut self) -> Result<Vec<RuntimeRelayMessage>, AppError> {
        if self.pending.iter().all(|byte| byte.is_ascii_whitespace()) {
            self.pending.clear();
            return Ok(Vec::new());
        }
        self.pending.clear();
        Err(AppError::generic("control_event_stream_truncated"))
    }
}

/// control events/stream 的 live NDJSON 读取器。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 需要可取消的长连接 relay：先读响应头建立流，再按消息粒度消费 `RuntimeRelayMessage`。
///
/// Code Logic（这个结构做什么）:
///     持有无全局 timeout 的 `reqwest::Response` + decoder + 就绪队列；`next_message` 拉取 chunk
///     并解码，EOF 时 finish；网络错误映射为 `control_event_stream_network`。
pub struct ControlEventsStream {
    response: reqwest::Response,
    decoder: ControlEventStreamDecoder,
    ready: VecDeque<RuntimeRelayMessage>,
    ended: bool,
}

impl ControlEventsStream {
    /// 读取下一条 relay 消息；流结束返回 `None`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     上层 relay 循环需要逐条处理 Event/Gap，而不是自己管理 NDJSON 缓冲。
    ///
    /// Code Logic（这个函数做什么）:
    ///     优先弹出 ready；否则 `response.chunk()` → decoder.push；EOF → finish 并标记 ended。
    pub async fn next_message(&mut self) -> Result<Option<RuntimeRelayMessage>, AppError> {
        loop {
            if let Some(message) = self.ready.pop_front() {
                return Ok(Some(message));
            }
            if self.ended {
                return Ok(None);
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.ready.extend(self.decoder.push(&bytes)?),
                Ok(None) => {
                    self.ready.extend(self.decoder.finish()?);
                    self.ended = true;
                }
                Err(_) => return Err(AppError::unavailable("control_event_stream_network")),
            }
        }
    }
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
    agent_hub_api_version: u32,
    http: reqwest::Client,
}

impl BackendControlClient {
    /// 从本机 control file 构造客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产 GUI 命令在 sidecar 已 ensure 后读取 control file 获得 port/token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 control file；缺失或非权威描述符返回 conflict/unavailable；构造无全局 timeout 的 reqwest client
    ///     （逐请求 timeout 由 send_once 的 RequestBuilder 设置；stream body 不得被 client 级 timeout 截断）。
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
    ///     校验 schema/owner 权威性后填充字段；http client 无 overall timeout。
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
            .build()
            .map_err(|e| AppError::generic(format!("构造 control client 失败: {e}")))?;
        Ok(Self {
            port: control.port,
            control_token: control.control_token.clone(),
            owner_instance_id: control.owner_instance_id.clone(),
            control_schema_version: control.control_schema_version,
            agent_hub_api_version: control.agent_hub_api_version,
            http,
        })
    }

    /// 测试用：直接注入 port/token（不读磁盘）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元/ smoke harness 启动临时 owner HTTP 后，不依赖真实 control 文件路径竞争。
    ///
    /// Code Logic（这个函数做什么）:
    ///     填充权威 schema 与 owner id，构造无全局 timeout 的 client；
    ///     agent_hub_api_version 默认 `AGENT_HUB_API_VERSION`。
    pub fn for_test(
        port: u16,
        control_token: &str,
        owner_instance_id: &str,
    ) -> Result<Self, AppError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| AppError::generic(format!("构造 control client 失败: {e}")))?;
        Ok(Self {
            port,
            control_token: control_token.to_string(),
            owner_instance_id: Some(owner_instance_id.to_string()),
            control_schema_version: CONTROL_SCHEMA_VERSION,
            agent_hub_api_version: crate::backend::control::AGENT_HUB_API_VERSION,
            http,
        })
    }

    /// 测试用：注入指定 Agent Hub API 版本。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     握手单测需要模拟旧 backend（0）与更高不兼容 major。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `for_test` 后覆盖 `agent_hub_api_version`。
    #[cfg(test)]
    pub fn for_test_with_agent_hub_version(
        port: u16,
        control_token: &str,
        owner_instance_id: &str,
        agent_hub_api_version: u32,
    ) -> Result<Self, AppError> {
        let mut client = Self::for_test(port, control_token, owner_instance_id)?;
        client.agent_hub_api_version = agent_hub_api_version;
        Ok(client)
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

    /// 返回 Agent Hub API 主版本（来自 control 文件）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 状态条/只读 gate 需要展示 backend 宣告的 Hub 协议版本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回缓存的 `agent_hub_api_version`（legacy 为 0）。
    pub fn agent_hub_api_version(&self) -> u32 {
        self.agent_hub_api_version
    }

    /// 校验 Agent Hub 写路径兼容性。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧/缺失 `agentHubApiVersion` 允许 status/preview，但 mutation 必须拒绝并提示 upgrade；
    ///     backend 宣告更高不兼容 major 时同样只读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     要求 `self.agent_hub_api_version == required_version`；
    ///     不匹配返回 `AppError::conflict("upgradeRequired")`（稳定 code 供前端分支）。
    ///     调用方须在**每一个** Agent Hub mutation 路径前调用本 helper。
    pub fn require_agent_hub_write_compatibility(
        &self,
        required_version: u32,
    ) -> Result<(), AppError> {
        if self.agent_hub_api_version == required_version {
            return Ok(());
        }
        Err(AppError::conflict("upgradeRequired"))
    }

    /// 比较两个客户端是否绑定同一 control descriptor。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     runtime cache 在失效时必须只清掉“当前仍是同一 descriptor”的缓存，
    ///     避免误清后来者新加载的 client。
    ///
    /// Code Logic（这个函数做什么）:
    ///     比较 port、control_token、owner_instance_id、control_schema_version 与
    ///     agent_hub_api_version；不打印这些字段，也不比较 http client 句柄。
    pub fn same_descriptor(&self, other: &Self) -> bool {
        self.port == other.port
            && self.control_token == other.control_token
            && self.owner_instance_id == other.owner_instance_id
            && self.control_schema_version == other.control_schema_version
            && self.agent_hub_api_version == other.agent_hub_api_version
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
        let resp: ControlWorkbenchResponseBody = match self
            .send_once_with_optional_timeout(path, &body, timeout)
            .await
        {
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

    /// 经 control API 代理 Agent Hub 操作并反序列化 result。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得自建 Agent Hub 写路径；全部 op 代理到 sidecar owner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/backend/control/agent-hub`；body={controlToken,op,payload}；
    ///     send_once MUTATE_TIMEOUT；解包 result 为 T。
    pub async fn agent_hub_op<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<T, AppError> {
        let value = self.agent_hub_op_value(op, payload).await?;
        serde_json::from_value(value).map_err(|e| {
            AppError::generic(format!("agent hub control result 解析失败 ({op}): {e}"))
        })
    }

    /// 经 control API 代理 Agent Hub 操作，返回原始 JSON result。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     与 workbench_op_value 对齐，供不强制 DTO 的调用方使用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST agent-hub；send_once；校验 ownerInstanceId 非空后返回 result。
    pub async fn agent_hub_op_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<serde_json::Value, AppError> {
        self.agent_hub_op_value_with_timeout(op, payload, MUTATE_TIMEOUT)
            .await
    }

    /// 带自定义超时的 Agent Hub control op（raw JSON）。
    ///
    /// Business Logic: LAN multi-target push 需要长于默认 mutation 的预算。
    /// Code Logic: send_once(path, body, timeout)。
    pub async fn agent_hub_op_value_with_timeout(
        &self,
        op: &str,
        payload: impl Serialize,
        timeout: Duration,
    ) -> Result<serde_json::Value, AppError> {
        let body = ControlWorkbenchRequestBody {
            control_token: self.control_token.clone(),
            op: op.to_string(),
            payload: serde_json::to_value(payload)
                .map_err(|e| AppError::generic(format!("序列化 agent hub payload 失败: {e}")))?,
        };
        let resp: ControlWorkbenchResponseBody =
            match self.send_once("agent-hub", &body, timeout).await {
                ControlCallOutcome::Ok(v) => v,
                ControlCallOutcome::Failed(e) => return Err(e),
                ControlCallOutcome::Uncertain(e) => {
                    return Err(AppError::unavailable(format!(
                        "control_response_uncertain: {e}"
                    )));
                }
            };
        if resp.owner_instance_id.trim().is_empty() {
            return Err(AppError::generic(
                "agent hub control 响应缺少 ownerInstanceId",
            ));
        }
        Ok(resp.result)
    }

    /// 带自定义超时的 Agent Hub control op（typed）。
    ///
    /// Business Logic: push_selection 等长操作。
    /// Code Logic: op_value_with_timeout + from_value。
    pub async fn agent_hub_op_with_timeout<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
        timeout: Duration,
    ) -> Result<T, AppError> {
        let value = self
            .agent_hub_op_value_with_timeout(op, payload, timeout)
            .await?;
        serde_json::from_value(value).map_err(|e| {
            AppError::generic(format!("agent hub control result 解析失败 ({op}): {e}"))
        })
    }

    /// Business Logic: 首屏 status。
    /// Code Logic: agent_hub.get_status 查询（非 mutation）。
    pub async fn agent_hub_get_status(&self) -> Result<AgentHubStatusDto, AppError> {
        self.agent_hub_op("agent_hub.get_status", serde_json::json!({}))
            .await
    }

    /// Business Logic: 资产列表。
    /// Code Logic: agent_hub.list_assets。
    pub async fn agent_hub_list_assets(
        &self,
        req: ListAssetsRequest,
    ) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
        self.agent_hub_op("agent_hub.list_assets", req).await
    }

    /// Business Logic: 资产详情。
    /// Code Logic: agent_hub.get_asset。
    pub async fn agent_hub_get_asset(
        &self,
        asset_id: &str,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        self.agent_hub_op(
            "agent_hub.get_asset",
            serde_json::json!({ "assetId": asset_id }),
        )
        .await
    }

    /// Business Logic: GuiClient 必须读取 sidecar owner 的用户级 source chain。
    /// Code Logic: 透传可选 deviceId；owner 再决定本机或 P2P。
    pub async fn agent_hub_inspect_user_instruction_workspace(
        &self,
        device_id: Option<String>,
    ) -> Result<UserInstructionWorkspaceDto, AppError> {
        self.agent_hub_op(
            "agent_hub.inspect_user_instruction_workspace",
            merge_device_id(serde_json::json!({}), device_id)?,
        )
        .await
    }

    /// Business Logic: 读取各 CLI 配置目录里的真实 AGENTS.md / CLAUDE.md / GEMINI.md。
    /// Code Logic: 透传 deviceId；owner 再决定本机或 P2P。
    pub async fn agent_hub_read_user_native_instruction_file(
        &self,
        req: ReadUserNativeInstructionFileRequest,
        device_id: Option<String>,
    ) -> Result<UserNativeInstructionFileDto, AppError> {
        self.agent_hub_op(
            "agent_hub.read_user_native_instruction_file",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 用户保存原生提示词文件是 mutation，必须阻断旧 sidecar。
    /// Code Logic: 版本门闩后 CAS 写白名单路径。
    pub async fn agent_hub_write_user_native_instruction_file(
        &self,
        req: WriteUserNativeInstructionFileRequest,
        device_id: Option<String>,
    ) -> Result<UserNativeInstructionFileDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.write_user_native_instruction_file",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 首次设置 preview 属于 V2 合同，旧 sidecar 不得静默降级。
    /// Code Logic: 版本门闩后调用 setup preview；deviceId 写入 payload 供 owner 路由。
    pub async fn agent_hub_preview_user_instruction_setup(
        &self,
        req: PreviewUserInstructionRequest,
        device_id: Option<String>,
    ) -> Result<UserInstructionPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.preview_user_instruction_setup",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 日常更新 preview 也必须由 owner 绑定计划。
    /// Code Logic: 版本门闩后调用 update preview。
    pub async fn agent_hub_preview_user_instruction_update(
        &self,
        req: PreviewUserInstructionRequest,
        device_id: Option<String>,
    ) -> Result<UserInstructionPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.preview_user_instruction_update",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 应用 plan 是 V2 mutation，必须阻断旧 sidecar。
    /// Code Logic: 版本门闩后调用 owner 原子 apply。
    pub async fn agent_hub_apply_user_instruction_plan(
        &self,
        req: ApplyUserInstructionPlanRequest,
        device_id: Option<String>,
    ) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.apply_user_instruction_plan",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 保存块文档是 V2 mutation，必须阻断旧 sidecar（独立于 CLI 写入门禁）。
    /// Code Logic: 版本门闩后调用 owner canonical CAS。
    pub async fn agent_hub_save_user_instruction_blocks(
        &self,
        req: SaveUserInstructionBlocksRequest,
        device_id: Option<String>,
    ) -> Result<UserInstructionCanonicalDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.save_user_instruction_blocks",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 三槽历史只读查询。
    /// Code Logic: agent_hub.list_user_instruction_slot_versions 是 owner read op。
    pub async fn agent_hub_list_user_instruction_slot_versions(
        &self,
        req: crate::agent_hub::user_instructions::ListUserInstructionSlotVersionsRequest,
        device_id: Option<String>,
    ) -> Result<Vec<crate::commands::prompts::ContentVersionDto>, AppError> {
        self.agent_hub_op(
            "agent_hub.list_user_instruction_slot_versions",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 三槽历史恢复是 V2 mutation，必须阻断旧 sidecar。
    /// Code Logic: 版本门闩后调用 owner canonical CAS。
    pub async fn agent_hub_restore_user_instruction_slot_version(
        &self,
        req: crate::agent_hub::user_instructions::RestoreUserInstructionSlotRequest,
        device_id: Option<String>,
    ) -> Result<UserInstructionCanonicalDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.restore_user_instruction_slot_version",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: 远端 analyze 必须经 sidecar 再 P2P，GuiClient 不得直连 peer。
    /// Code Logic: 长超时对齐 HeadlessCompletion 180s。
    pub async fn agent_hub_analyze_instruction_original(
        &self,
        req: AnalyzeInstructionOriginalRequest,
        device_id: Option<String>,
    ) -> Result<AnalyzeInstructionOriginalResult, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.analyze_instruction_original",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
            Duration::from_secs(200),
        )
        .await
    }

    /// Business Logic: 远端 adapt 在 owning device 跑 HeadlessCompletion。
    /// Code Logic: 长超时 control op。
    pub async fn agent_hub_adapt_instruction_to_other_agents(
        &self,
        req: AdaptInstructionToOtherAgentsRequest,
        device_id: Option<String>,
    ) -> Result<AdaptInstructionToOtherAgentsResult, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.adapt_instruction_to_other_agents",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
            Duration::from_secs(200),
        )
        .await
    }

    /// Business Logic: 远端 revise 在 owning device 跑 HeadlessCompletion。
    /// Code Logic: 长超时 control op。
    pub async fn agent_hub_revise_instruction_slot(
        &self,
        req: ReviseInstructionSlotRequest,
        device_id: Option<String>,
    ) -> Result<ReviseInstructionSlotResult, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.revise_instruction_slot",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
            Duration::from_secs(200),
        )
        .await
    }

    /// Business Logic: 保存整份指令（mutation）。
    /// Code Logic: 调用方须先 require_agent_hub_write_compatibility；再 agent_hub.update_instruction。
    pub async fn agent_hub_update_instruction(
        &self,
        req: UpdateInstructionRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.update_instruction", req).await
    }

    /// Business Logic: 更新指令块（mutation）。
    /// Code Logic: agent_hub.update_instruction_block。
    pub async fn agent_hub_update_instruction_block(
        &self,
        req: UpdateInstructionBlockRequest,
    ) -> Result<InstructionBlockDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.update_instruction_block", req)
            .await
    }

    /// Business Logic: 配对变体（mutation）。
    /// Code Logic: agent_hub.pair_instruction_variants。
    pub async fn agent_hub_pair_instruction_variants(
        &self,
        req: PairInstructionVariantsRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.pair_instruction_variants", req)
            .await
    }

    /// Business Logic: 项目启用预览（只读）。
    /// Code Logic: agent_hub.preview_project。
    pub async fn agent_hub_preview_project(
        &self,
        project_id: &str,
    ) -> Result<AgentHubProjectPreview, AppError> {
        self.agent_hub_op(
            "agent_hub.preview_project",
            serde_json::json!({ "projectId": project_id }),
        )
        .await
    }

    /// Business Logic: 启用项目（mutation）。
    /// Code Logic: agent_hub.enable_project + confirm。
    pub async fn agent_hub_enable_project(
        &self,
        project_id: &str,
        confirm: bool,
    ) -> Result<AgentHubProjectStatus, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.enable_project",
            serde_json::json!({ "projectId": project_id, "confirm": confirm }),
        )
        .await
    }

    /// Business Logic: 解决冲突（mutation）。
    /// Code Logic: agent_hub.resolve_conflict。
    pub async fn agent_hub_resolve_conflict(
        &self,
        req: ResolveConflictRequest,
    ) -> Result<AgentHubAssetDetailDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.resolve_conflict", req).await
    }

    /// Business Logic: 设置 target binding（mutation）。
    /// Code Logic: agent_hub.set_target_binding。
    pub async fn agent_hub_set_target_binding(
        &self,
        req: SetTargetBindingRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.set_target_binding", req).await
    }

    /// Business Logic: 设置 target presence（mutation）。
    /// Code Logic: agent_hub.set_target_presence。
    pub async fn agent_hub_set_target_presence(
        &self,
        req: crate::agent_hub::service::SetTargetPresenceRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.set_target_presence", req)
            .await
    }

    /// Business Logic: 设置 target enabled（mutation）。
    /// Code Logic: agent_hub.set_target_enabled。
    pub async fn agent_hub_set_target_enabled(
        &self,
        req: crate::agent_hub::service::SetTargetEnabledRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.set_target_enabled", req).await
    }

    /// Business Logic: 恢复 detached target（mutation）。
    /// Code Logic: agent_hub.restore_detached_target。
    pub async fn agent_hub_restore_detached_target(
        &self,
        req: crate::agent_hub::service::RestoreDetachedTargetRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.restore_detached_target", req)
            .await
    }

    /// Business Logic: 全 target 删除（mutation）。
    /// Code Logic: agent_hub.delete_asset_everywhere。
    pub async fn agent_hub_delete_asset_everywhere(
        &self,
        req: crate::agent_hub::service::DeleteAssetEverywhereRequest,
    ) -> Result<AgentHubAssetSummaryDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.delete_asset_everywhere", req)
            .await
    }

    /// Business Logic: 源侧 multi-target LAN push（mutation，无 pull）。
    /// Code Logic: agent_hub.push_selection；长超时覆盖多 peer chunk 传输。
    pub async fn agent_hub_push_selection(
        &self,
        req: crate::agent_hub::replication::sender::PushAgentHubSelectionRequest,
    ) -> Result<crate::agent_hub::replication::sender::MultiTargetPushReport, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        // LAN multi-peer 可能超过默认 mutation 15s。
        self.agent_hub_op_with_timeout("agent_hub.push_selection", req, Duration::from_secs(360))
            .await
    }

    /// Business Logic: 读取源侧 push 进度（只读）。
    /// Code Logic: agent_hub.get_push_report。
    pub async fn agent_hub_get_push_report(
        &self,
        request_id: &str,
    ) -> Result<Option<crate::agent_hub::replication::sender::MultiTargetPushReport>, AppError>
    {
        self.agent_hub_op(
            "agent_hub.get_push_report",
            serde_json::json!({ "requestId": request_id }),
        )
        .await
    }

    /// Business Logic: LAN push 前预览 selection（只读）。
    /// Code Logic: agent_hub.preview_lan_push。
    pub async fn agent_hub_preview_lan_push(
        &self,
        req: crate::agent_hub::replication::sender::PushAgentHubSelectionRequest,
    ) -> Result<serde_json::Value, AppError> {
        self.agent_hub_op("agent_hub.preview_lan_push", req).await
    }

    /// Business Logic: 启动源侧 multi-target LAN push（mutation）。
    /// Code Logic: agent_hub.start_lan_push；长超时。
    pub async fn agent_hub_start_lan_push(
        &self,
        req: crate::agent_hub::replication::sender::PushAgentHubSelectionRequest,
    ) -> Result<crate::agent_hub::replication::sender::MultiTargetPushReport, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout("agent_hub.start_lan_push", req, Duration::from_secs(360))
            .await
    }

    /// Business Logic: 读取 LAN push 进度（只读）。
    /// Code Logic: agent_hub.get_lan_push。
    pub async fn agent_hub_get_lan_push(
        &self,
        request_id: &str,
    ) -> Result<Option<crate::agent_hub::replication::sender::MultiTargetPushReport>, AppError>
    {
        self.agent_hub_op(
            "agent_hub.get_lan_push",
            serde_json::json!({ "requestId": request_id }),
        )
        .await
    }

    /// Business Logic: 只读枚举 Git device lanes。
    /// Code Logic: agent_hub.inspect_git_lanes。
    pub async fn agent_hub_inspect_git_lanes(
        &self,
    ) -> Result<crate::agent_hub::git::preview::GitLaneInspectReport, AppError> {
        self.agent_hub_op("agent_hub.inspect_git_lanes", serde_json::json!({}))
            .await
    }

    /// Business Logic: Git import 预览（只读，零写入）。
    /// Code Logic: agent_hub.preview_git_import。
    pub async fn agent_hub_preview_git_import(
        &self,
        lane_device_id: &str,
    ) -> Result<crate::agent_hub::git::preview::GitImportPreview, AppError> {
        self.agent_hub_op(
            "agent_hub.preview_git_import",
            serde_json::json!({ "laneDeviceId": lane_device_id }),
        )
        .await
    }

    /// Business Logic: 确认 Git import（mutation）。
    /// Code Logic: agent_hub.confirm_git_import。
    pub async fn agent_hub_confirm_git_import(
        &self,
        req: crate::agent_hub::git::preview::ConfirmGitImportRequest,
    ) -> Result<crate::agent_hub::git::preview::ConfirmGitImportOutcome, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.confirm_git_import", req).await
    }

    /// Business Logic: 保存 project mapping（mutation，默认 not opted-in）。
    /// Code Logic: agent_hub.confirm_project_mapping。
    pub async fn agent_hub_confirm_project_mapping(
        &self,
        req: crate::agent_hub::git::preview::ConfirmProjectMappingRequest,
    ) -> Result<crate::agent_hub::snapshot::importer::ResolvedProjectMapping, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.confirm_project_mapping", req)
            .await
    }

    /// Business Logic: GuiClient 通过 owner 预览 remote shortcut 项目。
    pub async fn agent_hub_preview_remote_project(
        &self,
        req: crate::agent_hub::replication::pull::RemoteProjectRefRequest,
    ) -> Result<crate::agent_hub::project_scope::AgentHubProjectPreview, AppError> {
        self.agent_hub_op("agent_hub.preview_remote_project", req)
            .await
    }

    /// Business Logic: GuiClient 通过 owner 在 owning peer 启用 remote shortcut 项目。
    pub async fn agent_hub_enable_remote_project(
        &self,
        req: crate::agent_hub::replication::pull::RemoteProjectRefRequest,
    ) -> Result<crate::agent_hub::project_scope::AgentHubProjectStatus, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.enable_remote_project", req)
            .await
    }

    /// Business Logic: GuiClient 读取 sidecar owner 的 portable inventory（只读）；deviceId 透传到 owner。
    /// Code Logic: agent_hub.inspect_portable_inventory；PORTABLE_INVENTORY_TIMEOUT。
    pub async fn agent_hub_inspect_portable_inventory(
        &self,
        query: crate::agent_hub::PortableInventoryQuery,
        device_id: Option<String>,
    ) -> Result<crate::agent_hub::PortableInventorySnapshotDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.inspect_portable_inventory",
            merge_device_id(serde_json::to_value(query)?, device_id)?,
            portable_control_read_timeout("agent_hub.inspect_portable_inventory"),
        )
        .await
    }

    /// Business Logic: preview 属 v3 mutation 合同，旧 sidecar 不得静默降级。
    /// Code Logic: 写兼容门闩 + agent_hub.preview_portable_asset_action；deviceId 供 owner 路由。
    pub async fn agent_hub_preview_portable_asset_action(
        &self,
        req: crate::agent_hub::PreviewPortableAssetActionRequest,
        device_id: Option<String>,
    ) -> Result<crate::agent_hub::PortableAssetActionPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op(
            "agent_hub.preview_portable_asset_action",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
        )
        .await
    }

    /// Business Logic: apply 是长 mutation（目标文件 + rescan），需长超时。
    /// Code Logic: 写兼容门闩 + agent_hub_op_with_timeout 360s；deviceId 供 owner 路由。
    pub async fn agent_hub_apply_portable_asset_action(
        &self,
        req: crate::agent_hub::ApplyPortableAssetActionRequest,
        device_id: Option<String>,
    ) -> Result<crate::agent_hub::PortableAssetActionResultDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout(
            "agent_hub.apply_portable_asset_action",
            merge_device_id(serde_json::to_value(req)?, device_id)?,
            Duration::from_secs(360),
        )
        .await
    }

    /// Business Logic: 按 clientRequestId 对账 apply 结果（只读）。
    /// Code Logic: agent_hub.get_portable_asset_action；QUERY_TIMEOUT；deviceId 供 owner 路由。
    pub async fn agent_hub_get_portable_asset_action(
        &self,
        client_request_id: &str,
        device_id: Option<String>,
    ) -> Result<crate::agent_hub::PortableAssetActionResultDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.get_portable_asset_action",
            merge_device_id(
                serde_json::json!({ "clientRequestId": client_request_id }),
                device_id,
            )?,
            portable_control_read_timeout("agent_hub.get_portable_asset_action"),
        )
        .await
    }

    /// Business Logic: GuiClient 通过 owner 解析 remote shortcut 并读取对端项目库存。
    pub async fn agent_hub_inspect_remote_project_portable_inventory(
        &self,
        req: crate::agent_hub::replication::pull::InspectRemoteProjectPortableInventoryRequest,
    ) -> Result<crate::agent_hub::PortableInventorySnapshotDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.inspect_remote_project_portable_inventory",
            req,
            portable_control_read_timeout("agent_hub.inspect_remote_project_portable_inventory"),
        )
        .await
    }

    /// Business Logic: GuiClient 在 owning peer 生成远端项目动作计划。
    pub async fn agent_hub_preview_remote_project_portable_action(
        &self,
        req: crate::agent_hub::replication::pull::PreviewRemoteProjectPortableActionRequest,
    ) -> Result<crate::agent_hub::PortableAssetActionPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.preview_remote_project_portable_action", req)
            .await
    }

    /// Business Logic: GuiClient 在 owning peer 执行远端项目动作计划。
    pub async fn agent_hub_apply_remote_project_portable_action(
        &self,
        req: crate::agent_hub::replication::pull::ApplyRemoteProjectPortableActionRequest,
    ) -> Result<crate::agent_hub::PortableAssetActionResultDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout(
            "agent_hub.apply_remote_project_portable_action",
            req,
            Duration::from_secs(360),
        )
        .await
    }

    /// Business Logic: GuiClient 对账远端项目动作结果。
    pub async fn agent_hub_get_remote_project_portable_action(
        &self,
        req: crate::agent_hub::replication::pull::GetRemoteProjectPortableActionRequest,
    ) -> Result<crate::agent_hub::PortableAssetActionResultDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.get_remote_project_portable_action",
            req,
            portable_control_read_timeout("agent_hub.get_remote_project_portable_action"),
        )
        .await
    }

    /// Business Logic: 远端 portable inventory 只读 metadata；capability 缺失时 owner 会 fail closed。
    /// Code Logic: agent_hub.list_remote_portable_inventory；PORTABLE_INVENTORY_TIMEOUT。
    pub async fn agent_hub_list_remote_portable_inventory(
        &self,
        req: crate::agent_hub::replication::pull::ListRemotePortableInventoryRequest,
    ) -> Result<crate::agent_hub::replication::pull::RemotePortableInventoryDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.list_remote_portable_inventory",
            req,
            portable_control_read_timeout("agent_hub.list_remote_portable_inventory"),
        )
        .await
    }

    /// Business Logic: pull preview 属 v3 mutation 合同（计划绑定），旧 sidecar 不得静默降级。
    /// Code Logic: 写兼容门闩 + agent_hub.preview_portable_pull。
    pub async fn agent_hub_preview_portable_pull(
        &self,
        req: crate::agent_hub::replication::pull::PreviewPortablePullRequest,
    ) -> Result<crate::agent_hub::replication::pull::PortablePullPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.preview_portable_pull", req)
            .await
    }

    /// Business Logic: apply pull 是长 mutation（objects + import + install）。
    /// Code Logic: 写兼容门闩 + agent_hub_op_with_timeout 360s。
    pub async fn agent_hub_apply_portable_pull(
        &self,
        req: crate::agent_hub::replication::pull::ApplyPortablePullRequest,
    ) -> Result<crate::agent_hub::replication::pull::PortablePullResultDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout(
            "agent_hub.apply_portable_pull",
            req,
            Duration::from_secs(360),
        )
        .await
    }

    /// Business Logic: 按 clientRequestId 对账 pull 结果（只读）。
    /// Code Logic: agent_hub.get_portable_pull；QUERY_TIMEOUT。
    pub async fn agent_hub_get_portable_pull(
        &self,
        client_request_id: &str,
    ) -> Result<crate::agent_hub::replication::pull::PortablePullResultDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.get_portable_pull",
            serde_json::json!({ "clientRequestId": client_request_id }),
            portable_control_read_timeout("agent_hub.get_portable_pull"),
        )
        .await
    }

    /// Business Logic: 用户级镜像 preview 写 plan，旧 sidecar 不得静默降级。
    /// Code Logic: 写兼容门闩 + agent_hub_op_with_timeout 120s（本机扫描 + 对端 inventory）。
    pub async fn agent_hub_preview_user_mirror(
        &self,
        req: crate::agent_hub::user_mirror::PreviewUserMirrorRequest,
    ) -> Result<crate::agent_hub::user_mirror::UserMirrorPlanDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout(
            "agent_hub.preview_user_mirror",
            req,
            USER_MIRROR_PREVIEW_TIMEOUT,
        )
        .await
    }

    /// Business Logic: apply 是全 Agent 写盘长 mutation（墙钟 900s）。
    /// Code Logic: 写兼容门闩 + agent_hub_op_with_timeout 900s。
    pub async fn agent_hub_apply_user_mirror(
        &self,
        req: crate::agent_hub::user_mirror::ApplyUserMirrorRequest,
    ) -> Result<crate::agent_hub::user_mirror::UserMirrorResultDto, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op_with_timeout(
            "agent_hub.apply_user_mirror",
            req,
            USER_MIRROR_APPLY_TIMEOUT,
        )
        .await
    }

    /// Business Logic: 按 clientRequestId 对账镜像结果（只读）。
    /// Code Logic: agent_hub.get_user_mirror；QUERY_TIMEOUT。
    pub async fn agent_hub_get_user_mirror(
        &self,
        client_request_id: &str,
    ) -> Result<crate::agent_hub::user_mirror::UserMirrorResultDto, AppError> {
        self.agent_hub_op_with_timeout(
            "agent_hub.get_user_mirror",
            serde_json::json!({ "clientRequestId": client_request_id }),
            QUERY_TIMEOUT,
        )
        .await
    }

    /// Business Logic: selective 跨 Agent 预览也必须由唯一 owner 读取目标文件。
    /// Code Logic: 版本门闩后代理 preview control op。
    pub async fn agent_hub_preview_cross_agent_instruction(
        &self,
        req: PreviewCrossAgentInstructionRequest,
    ) -> Result<CrossAgentPreviewReport, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.preview_cross_agent_instruction", req)
            .await
    }

    /// Business Logic: selective apply 只能由 sidecar owner 执行 CAS 写入。
    /// Code Logic: 版本门闩后单次发送 apply control op。
    pub async fn agent_hub_apply_cross_agent_instruction(
        &self,
        req: ApplyCrossAgentInstructionRequest,
    ) -> Result<Vec<CrossAgentApplyTargetResult>, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.apply_cross_agent_instruction", req)
            .await
    }

    /// Business Logic: full preview 与 apply 必须由同一 owner 观察本机目标状态。
    /// Code Logic: 版本门闩后代理 full preview。
    pub async fn agent_hub_preview_cross_agent_full(
        &self,
        req: PreviewCrossAgentFullRequest,
    ) -> Result<CrossAgentFullPlan, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.preview_cross_agent_full", req)
            .await
    }

    /// Business Logic: full apply 只允许 owner 按 preview hash 写入。
    /// Code Logic: 版本门闩后单次发送 full apply。
    pub async fn agent_hub_apply_cross_agent_full(
        &self,
        req: ApplyCrossAgentFullRequest,
    ) -> Result<Vec<CrossAgentFullApplyItemResult>, AppError> {
        self.require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)?;
        self.agent_hub_op("agent_hub.apply_cross_agent_full", req)
            .await
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

    /// 打开 control events/stream 长连接并返回 live NDJSON 读取器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 需要低延迟 live relay；catch-up 批量查询不足以承载持续 terminal 输出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `events/stream`；仅对 `.send()` 套 `QUERY_TIMEOUT`（连接/响应头）；成功后返回
    ///     无 body overall timeout 的 `ControlEventsStream`。404 → unsupported；连接失败/网络 →
    ///     稳定 unavailable code；错误消息禁止拼接 payload/chunk 文本。
    pub async fn open_events_stream(
        &self,
        after: Option<&BackendRuntimeCursor>,
    ) -> Result<ControlEventsStream, AppError> {
        let url = format!(
            "http://127.0.0.1:{}/api/backend/control/events/stream",
            self.port
        );
        let body = ControlEventsBody {
            control_token: self.control_token.clone(),
            after_owner_instance_id: after.map(|cursor| cursor.owner_instance_id.clone()),
            after_sequence: after.map(|cursor| cursor.sequence),
        };
        let send = self.http.post(url).json(&body).send();
        let response = tokio::time::timeout(QUERY_TIMEOUT, send)
            .await
            .map_err(|_| AppError::timeout("control_event_stream_connect_timeout"))?
            .map_err(|_| AppError::unavailable("control_event_stream_connect_failed"))?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(if status == reqwest::StatusCode::NOT_FOUND {
                AppError::validation("control_event_stream_unsupported")
            } else {
                AppError::unavailable(format!("control_event_stream_http_{status}"))
            });
        }
        Ok(ControlEventsStream {
            response,
            decoder: ControlEventStreamDecoder::default(),
            ready: VecDeque::new(),
            ended: false,
        })
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

    /// 经 control API 在 owner 侧创建串行任务块。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 不得写本机空库或双路径 dispatch；建块权威只在 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/task-blocks/create`；mutation 不自动重试。
    pub async fn create_orchestrator_task_block(
        &self,
        request: CreateOrchestratorTaskBlockRequest,
    ) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
        let body = ControlOrchestratorTaskBlockCreateBody {
            control_token: self.control_token.clone(),
            request,
        };
        match self
            .send_once("orchestrator/task-blocks/create", &body, MUTATE_TIMEOUT)
            .await
        {
            ControlCallOutcome::Ok(v) => Ok(v),
            ControlCallOutcome::Failed(e) => Err(e),
            ControlCallOutcome::Uncertain(e) => Err(AppError::unavailable(format!(
                "control_response_uncertain: {e}"
            ))),
        }
    }

    /// 经 control API 在 owner 侧追加任务块成员。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     追加改变 live last-member，必须走 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/task-blocks/append-member`。
    pub async fn append_orchestrator_task_block_member(
        &self,
        request: AppendOrchestratorTaskBlockMemberRequest,
    ) -> Result<OrchestratorTaskViewDto, AppError> {
        let body = ControlOrchestratorTaskBlockAppendBody {
            control_token: self.control_token.clone(),
            request,
        };
        match self
            .send_once(
                "orchestrator/task-blocks/append-member",
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

    /// 经 control API 在 owner 侧重排任务块成员。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     重排只能在 owner 上校验整块仍 backlog/todo 且 idle。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `orchestrator/task-blocks/reorder-members`。
    pub async fn reorder_orchestrator_task_block_members(
        &self,
        request: ReorderOrchestratorTaskBlockMembersRequest,
    ) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
        let body = ControlOrchestratorTaskBlockReorderBody {
            control_token: self.control_token.clone(),
            request,
        };
        match self
            .send_once(
                "orchestrator/task-blocks/reorder-members",
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
        self.send_once_with_optional_timeout(path, body, Some(timeout))
            .await
    }

    /// 发送一次 control POST，timeout=None 时不设墙钟。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     merge 必须等到 sidecar 对端 Claude 结束；其它 mutation 仍要有界超时。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST control 路径；可选 `.timeout`；连接失败 Failed，超时/坏响应 Uncertain。
    async fn send_once_with_optional_timeout<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
        timeout: Option<Duration>,
    ) -> ControlCallOutcome<T> {
        let url = format!("http://127.0.0.1:{}/api/backend/control/{path}", self.port);
        let mut request = self.http.post(&url).json(body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
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

/// control client 加载器类型：生产读 control file，测试可注入固定 client。
type ControlClientLoader = dyn Fn() -> Result<BackendControlClient, AppError> + Send + Sync;

/// GUI 侧 Backend control client 运行时缓存。
///
/// Business Logic（为什么需要这个结构）:
///     Workbench GUI 每次 proxy 若都重新读 control file + 新建 HTTP client，会放大终端写路径延迟；
///     同时 mutation 失败后只允许失效缓存，严禁自动重放同一输入批。
///
/// Code Logic（这个结构做什么）:
///     用 Mutex 缓存最近一次成功加载的 `BackendControlClient`；`client()` 命中缓存直接 clone；
///     `invalidate_if_current` 仅在 descriptor 仍匹配时清空；`workbench_*` 包装失败后失效，
///     不在同一调用内重新加载并重发 mutation。
pub struct BackendControlClientRuntime {
    cached: std::sync::Mutex<Option<BackendControlClient>>,
    loader: Arc<ControlClientLoader>,
    terminal_input: crate::backend::terminal_input_client::TerminalInputClientRuntime,
}

impl BackendControlClientRuntime {
    /// 构造生产用 runtime：从本机 control file 加载 client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState / GUI 启动只需一份 runtime，共享缓存直到显式失效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     loader 绑定 `BackendControlClient::from_control_file`。
    pub fn new() -> Self {
        Self::with_loader(BackendControlClient::from_control_file)
    }

    /// 注入 loader 的构造（测试 / 可替换入口）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试不得依赖真实 `~/.cc-partner` control file。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装任意 `Fn() -> Result<BackendControlClient, AppError>` 为 Arc loader，缓存初始为空。
    pub fn with_loader(
        loader: impl Fn() -> Result<BackendControlClient, AppError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            cached: std::sync::Mutex::new(None),
            loader: Arc::new(loader),
            terminal_input:
                crate::backend::terminal_input_client::TerminalInputClientRuntime::default(),
        }
    }

    /// 将桌面终端输入接纳到常驻 control WS 的本机有界队列。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     xterm 输入不能走通用 control HTTP mutation；invoke 只确认本机队列接纳。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取缓存 descriptor，构造不含 token 的 URL 和内部 descriptor key，再委托输入 actor。
    pub fn enqueue_terminal_input(
        &self,
        ui: Arc<dyn crate::backend::ui::BackendUi>,
        session_id: String,
        data: String,
    ) -> Result<(), AppError> {
        let client = self.client()?;
        let descriptor_key = format!(
            "{}:{}",
            client.port,
            client.owner_instance_id.as_deref().unwrap_or_default()
        );
        let ws_url = format!(
            "ws://127.0.0.1:{}/api/backend/control/workbench/terminal-input-stream",
            client.port
        );
        self.terminal_input.enqueue(
            ui,
            descriptor_key,
            ws_url,
            client.control_token.clone(),
            session_id,
            data,
        )
    }

    /// 获取可复用的 control client（缓存未命中时加载一次）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI proxy 需要低开销拿到当前 sidecar descriptor 对应的 client。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁内若有缓存则 clone 返回；否则调 loader，成功后写入缓存再返回。
    pub fn client(&self) -> Result<BackendControlClient, AppError> {
        let mut cached = self.cached.lock().expect("control client cache 锁中毒");
        if let Some(client) = cached.as_ref() {
            return Ok(client.clone());
        }
        let client = (self.loader)()?;
        *cached = Some(client.clone());
        Ok(client)
    }

    /// 若缓存仍是 observed 同一 descriptor，则清空缓存。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mutation/查询失败后应在下次业务调用重新读 control file，但不能清掉后来者新缓存。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁内比较 `same_descriptor`；匹配才置 None。
    pub fn invalidate_if_current(&self, observed: &BackendControlClient) {
        let mut cached = self.cached.lock().expect("control client cache 锁中毒");
        if cached
            .as_ref()
            .is_some_and(|current| current.same_descriptor(observed))
        {
            *cached = None;
        }
    }

    /// 经缓存 client 执行 workbench 查询/操作（失败时失效缓存，不重放）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient Workbench proxy 应复用缓存 client，错误后让后续调用刷新 descriptor。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `client()` 一次 → `workbench_op`；`Err` 时 `invalidate_if_current`，原样返回错误。
    pub async fn workbench_op<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<T, AppError> {
        let client = self.client()?;
        let result = client.workbench_op(op, payload).await;
        if result.is_err() {
            self.invalidate_if_current(&client);
        }
        result
    }

    /// 经缓存 client 执行 workbench mutation（失败时失效缓存，永不自动重放）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `sessions.write` 等 mutation 在 Failed/Uncertain 后只允许上层构造成 unknown envelope
    ///     或返回错误；runtime 不得因 cache miss 在本调用内二次发送。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `client()` 一次 → `workbench_mutation_op_value`；`Err` 时 `invalidate_if_current`；
    ///     不在错误后重新 loader 或再次 POST。
    pub async fn workbench_mutation_op_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<serde_json::Value, MutationControlError> {
        let client = self.client().map_err(MutationControlError::Failed)?;
        let result = client.workbench_mutation_op_value(op, payload).await;
        if result.is_err() {
            self.invalidate_if_current(&client);
        }
        result
    }
}

impl Default for BackendControlClientRuntime {
    /// 默认与 `new()` 相同：生产 control-file loader。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     满足 `Default` 约束，便于测试/结构体字面量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `Self::new()`。
    fn default() -> Self {
        Self::new()
    }
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
            AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig,
            OrchestratorAutomationConfig,
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
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: "/tmp/d.db".into(),
            screenshot_hotkey: "<ctrl>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_optimizer_provider: "claude".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
            relay: crate::config::RelayConfig::default(),
            experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
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
        assert_eq!(
            workbench_control_path("sessions.pasteImage"),
            "workbench/data"
        );
        assert_eq!(workbench_control_path("notes.save"), "workbench/data");
        assert_eq!(workbench_control_path("notes.get"), "workbench");
        assert_eq!(
            workbench_control_path("agent_ledger.export_token_stats"),
            "workbench"
        );
    }

    /// 验证 NDJSON decoder 可跨 chunk 重组 UTF-8 与多行消息。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     流式 body 可能把多字节汉字拆在不同 chunk，decoder 不得丢行或误报截断。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分三次 push 拆开的 event JSON（含半截 UTF-8）与 gap 行，断言 2 条消息且 finish 为空。
    ///     字段名与既有 `RuntimeRelayMessage` wire 对齐（enum rename_all 只改 variant，字段为 snake_case）。
    #[test]
    fn control_event_decoder_preserves_split_utf8_and_multiple_lines() {
        let mut decoder = ControlEventStreamDecoder::default();
        let first = br#"{"kind":"event","owner_instance_id":"o1","sequence":1,"event":"workbench:terminal-output","payload":{"chunk":""#;
        assert!(decoder.push(first).unwrap().is_empty());
        assert!(decoder.push(&[0xE4, 0xBD]).unwrap().is_empty());
        let mut tail = vec![0xA0];
        tail.extend_from_slice(
            br#""}}
{"kind":"gap","owner_instance_id":"o1","oldest_available":2,"latest":9}
"#,
        );
        let messages = decoder.push(&tail).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[1],
            RuntimeRelayMessage::Gap { latest: 9, .. }
        ));
        assert!(decoder.finish().unwrap().is_empty());
    }

    /// 验证 NDJSON decoder 对超大行 / 非法 JSON / 半行 EOF 使用稳定错误码。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     客户端必须拒绝异常流并暴露稳定 code，禁止把原始 line 拼进错误消息。
    ///
    /// Code Logic（这个测试做什么）:
    ///     oversize → line_too_large；malformed → malformed；partial finish → truncated。
    #[test]
    fn control_event_decoder_rejects_oversize_malformed_and_partial_eof() {
        let mut oversize = ControlEventStreamDecoder::default();
        let error = oversize
            .push(&vec![b'x'; CONTROL_EVENT_STREAM_MAX_LINE_BYTES + 1])
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("control_event_stream_line_too_large"));

        let mut malformed = ControlEventStreamDecoder::default();
        let error = malformed.push(b"{not-json}\n").unwrap_err();
        assert!(error.to_string().contains("control_event_stream_malformed"));

        let mut partial = ControlEventStreamDecoder::default();
        partial.push(br#"{"kind":"event"}"#).unwrap();
        let error = partial.finish().unwrap_err();
        assert!(error.to_string().contains("control_event_stream_truncated"));
    }

    /// 启动 mock control events/stream：立即返回响应头，延迟 16s 再写第一条 NDJSON。
    ///
    /// Business Logic（为什么需要这个 helper）:
    ///     paused-time 测试需证明 client 无 overall timeout 时 body 可在旧 15s 边界后仍可读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     bind 127.0.0.1:0；捕获请求 body 中 afterOwner/afterSequence；流首行 event sequence=8。
    async fn spawn_control_event_stream_fixture() -> (
        u16,
        std::sync::Arc<std::sync::Mutex<Option<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::extract::State as AxumState;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::{Json, Router};
        use futures_util::stream;
        use std::convert::Infallible;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct StreamState {
            captured: Arc<Mutex<Option<serde_json::Value>>>,
        }

        let captured = Arc::new(Mutex::new(None));
        let state = StreamState {
            captured: Arc::clone(&captured),
        };
        let app = Router::new()
            .route(
                "/api/backend/control/events/stream",
                post(
                    |AxumState(s): AxumState<StreamState>, body: Json<serde_json::Value>| async move {
                        *s.captured.lock().unwrap() = Some(body.0);
                        // 与 RuntimeRelayMessage 既有 wire 对齐：字段 snake_case。
                        let line = r#"{"kind":"event","owner_instance_id":"owner-1","sequence":8,"event":"workbench:terminal-output","payload":{"chunk":"x"}}"#;
                        let stream = stream::once(async move {
                            tokio::time::sleep(Duration::from_secs(16)).await;
                            Ok::<_, Infallible>(format!("{line}\n"))
                        });
                        let mut response = axum::response::Response::new(
                            axum::body::Body::from_stream(stream),
                        );
                        *response.status_mut() = axum::http::StatusCode::OK;
                        response.headers_mut().insert(
                            axum::http::header::CONTENT_TYPE,
                            axum::http::HeaderValue::from_static("application/x-ndjson"),
                        );
                        response.into_response()
                    },
                ),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        // 真实时间短暂等待，确保 accept 已注册（connect 阶段不用 paused clock）。
        tokio::time::sleep(Duration::from_millis(20)).await;
        (port, captured, server)
    }

    /// 验证 open_events_stream 发送 cursor 且 body 不被 client overall timeout 杀死。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     live relay 可能长时间空闲；旧 builder 15s overall timeout 会截断 body 导致误断开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先用真实时间完成 connect（避免 start_paused auto-advance 误触 QUERY_TIMEOUT）；
    ///     建流后 `pause()`，advance 15s 越过旧 overall timeout，再 advance 到第 16s 读到 sequence=8；
    ///     并断言请求 body 含 afterOwnerInstanceId/afterSequence。
    #[tokio::test]
    async fn events_stream_sends_cursor_and_reads_live_message_without_overall_timeout() {
        let (port, captured, _server) = spawn_control_event_stream_fixture().await;
        let client = BackendControlClient::for_test(port, "token", "owner-1").unwrap();
        let cursor = BackendRuntimeCursor {
            owner_instance_id: "owner-1".into(),
            sequence: 7,
        };
        let mut stream = client.open_events_stream(Some(&cursor)).await.unwrap();
        // 响应头已返回；后续 body 延迟用虚拟时钟推进，避免真实等待 16s。
        tokio::time::pause();
        let next = tokio::spawn(async move { stream.next_message().await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let message = next.await.unwrap().unwrap().unwrap();
        assert!(matches!(
            message,
            RuntimeRelayMessage::Event { sequence: 8, .. }
        ));
        let body = captured.lock().unwrap().clone().unwrap();
        assert_eq!(body["afterOwnerInstanceId"], "owner-1");
        assert_eq!(body["afterSequence"], 7);
    }

    /// 验证 runtime 在显式失效前复用同一 client 加载结果。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 热路径必须复用 control client，避免每次 proxy 重读 control file。
    ///
    /// Code Logic（这个测试做什么）:
    ///     loader 计数：两次 client() 只加载 1 次；invalidate_if_current 后再 client() 加载第 2 次。
    #[test]
    fn control_runtime_reuses_client_until_explicit_invalidation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let loads = Arc::new(AtomicUsize::new(0));
        let runtime = BackendControlClientRuntime::with_loader({
            let loads = Arc::clone(&loads);
            move || {
                loads.fetch_add(1, Ordering::SeqCst);
                BackendControlClient::for_test(62116, "token", "owner-1")
            }
        });
        let first = runtime.client().unwrap();
        let second = runtime.client().unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        runtime.invalidate_if_current(&first);
        let third = runtime.client().unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert!(first.same_descriptor(&second));
        assert!(second.same_descriptor(&third));
    }

    /// 启动始终失败的 control workbench fixture，计数 sessions.write 调用。
    ///
    /// Business Logic（为什么需要这个 helper）:
    ///     证明 mutation 失败路径不会自动重放同一输入批。
    ///
    /// Code Logic（这个函数做什么）:
    ///     bind 127.0.0.1:0；workbench/data 对 sessions.write 返回 500 并 AtomicUsize++。
    async fn spawn_failing_control_workbench_fixture() -> (
        u16,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::extract::State as AxumState;
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        #[derive(Clone)]
        struct FailState {
            calls: Arc<AtomicUsize>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            #[allow(dead_code)]
            control_token: String,
            op: String,
            #[serde(default)]
            #[allow(dead_code)]
            payload: serde_json::Value,
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let state = FailState {
            calls: Arc::clone(&calls),
        };
        let app = Router::new()
            .route(
                "/api/backend/control/workbench/data",
                post(
                    |AxumState(s): AxumState<FailState>, Json(body): Json<WbReq>| async move {
                        if body.op == "sessions.write" {
                            s.calls.fetch_add(1, Ordering::SeqCst);
                        }
                        Err::<Json<serde_json::Value>, _>((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({
                                "error": "inject: sessions.write failed",
                                "code": "internal"
                            })),
                        ))
                    },
                ),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        (port, calls, server)
    }

    /// 验证 workbench mutation 失败会失效缓存但绝不自动重放。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     sessions.write 失败后重放会把同一输入批写两次，破坏终端语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     failing fixture 上调用 workbench_mutation_op_value；断言 Err 且 HTTP 调用数恰好 1。
    #[tokio::test]
    async fn workbench_mutation_failure_invalidates_but_never_replays() {
        use std::sync::atomic::Ordering;

        let (port, calls, _server) = spawn_failing_control_workbench_fixture().await;
        let runtime = BackendControlClientRuntime::with_loader(move || {
            BackendControlClient::for_test(port, "token", "owner-1")
        });
        let result = runtime
            .workbench_mutation_op_value(
                "sessions.write",
                serde_json::json!({"sessionId":"s1","data":"x"}),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// 旧/缺失 agentHubApiVersion 拒绝 mutation，允许读路径不调用本 helper。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 较新而后端未宣告写协议时，status/preview 可读，写路径必须 upgradeRequired。
    ///
    /// Code Logic（这个测试做什么）:
    ///     version=0 的 client 对 required=1 返回 conflict upgradeRequired。
    #[test]
    fn agent_hub_write_compat_rejects_missing_or_zero_version() {
        let client =
            BackendControlClient::for_test_with_agent_hub_version(1, "tok", "owner", 0).unwrap();
        let err = client
            .require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)
            .unwrap_err();
        assert_eq!(err.code(), "upgradeRequired");
        assert_eq!(err.classify(), crate::error::AppErrorCategory::Conflict);
    }

    /// 同 major 当前版本写兼容通过。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     新 GUI ↔ 当前 backend 必须允许 mutation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     for_test 默认当前 AGENT_HUB_API_VERSION，require(current) → Ok。
    #[test]
    fn agent_hub_write_compat_accepts_matching_current_version() {
        let client = BackendControlClient::for_test(1, "tok", "owner").unwrap();
        assert_eq!(
            client.agent_hub_api_version(),
            crate::backend::control::AGENT_HUB_API_VERSION
        );
        client
            .require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)
            .unwrap();
    }

    /// 更高不兼容 major → 只读（upgradeRequired）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     backend 宣告更高 major 时，旧 GUI 不得盲目写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     version=current+1、required=current → upgradeRequired。
    #[test]
    fn agent_hub_write_compat_rejects_higher_incompatible_major() {
        let higher = crate::backend::control::AGENT_HUB_API_VERSION.saturating_add(1);
        let client =
            BackendControlClient::for_test_with_agent_hub_version(1, "tok", "owner", higher)
                .unwrap();
        let err = client
            .require_agent_hub_write_compatibility(crate::backend::control::AGENT_HUB_API_VERSION)
            .unwrap_err();
        assert_eq!(err.code(), "upgradeRequired");
    }

    /// Business Logic: portable control client 必须给真实库存扫描留出高于轻量查询的预算。
    /// Code Logic: 生产 fn 签名；inventory 使用独立预算，ledger get 保持 QUERY_TIMEOUT，apply 使用长 mutation。
    #[test]
    fn portable_control_client_methods_and_timeouts() {
        let src = include_str!("control_client.rs");
        for sig in [
            "pub async fn agent_hub_inspect_portable_inventory(",
            "pub async fn agent_hub_preview_portable_asset_action(",
            "pub async fn agent_hub_apply_portable_asset_action(",
            "pub async fn agent_hub_get_portable_asset_action(",
            "pub async fn agent_hub_list_remote_portable_inventory(",
            "pub async fn agent_hub_preview_portable_pull(",
            "pub async fn agent_hub_apply_portable_pull(",
            "pub async fn agent_hub_get_portable_pull(",
            "pub async fn agent_hub_preview_user_mirror(",
            "pub async fn agent_hub_apply_user_mirror(",
            "pub async fn agent_hub_get_user_mirror(",
        ] {
            assert!(src.contains(sig), "missing client method {sig}");
        }
        assert!(src.contains("\"agent_hub.inspect_portable_inventory\""));
        assert!(src.contains("\"agent_hub.preview_portable_asset_action\""));
        assert!(src.contains("\"agent_hub.apply_portable_asset_action\""));
        assert!(src.contains("\"agent_hub.get_portable_asset_action\""));
        assert!(src.contains("\"agent_hub.list_remote_portable_inventory\""));
        assert!(src.contains("\"agent_hub.preview_portable_pull\""));
        assert!(src.contains("\"agent_hub.apply_portable_pull\""));
        assert!(src.contains("\"agent_hub.get_portable_pull\""));
        assert!(src.contains("\"agent_hub.preview_user_mirror\""));
        assert!(src.contains("\"agent_hub.apply_user_mirror\""));
        assert!(src.contains("\"agent_hub.get_user_mirror\""));
        assert_eq!(PORTABLE_INVENTORY_TIMEOUT, Duration::from_secs(30));
        assert!(PORTABLE_INVENTORY_TIMEOUT > QUERY_TIMEOUT);
        assert_eq!(
            portable_control_read_timeout("agent_hub.inspect_portable_inventory"),
            PORTABLE_INVENTORY_TIMEOUT
        );
        assert_eq!(
            portable_control_read_timeout("agent_hub.list_remote_portable_inventory"),
            PORTABLE_INVENTORY_TIMEOUT
        );
        assert_eq!(
            portable_control_read_timeout("agent_hub.get_portable_asset_action"),
            QUERY_TIMEOUT
        );
        assert_eq!(
            portable_control_read_timeout("agent_hub.get_portable_pull"),
            QUERY_TIMEOUT
        );
        assert!(
            src.contains("\"agent_hub.apply_portable_asset_action\"")
                && src.contains("Duration::from_secs(360)"),
            "apply should use long-mutation timeout"
        );
        assert!(
            src.contains("\"agent_hub.apply_portable_pull\"")
                && src.contains("Duration::from_secs(360)"),
            "apply pull should use long-mutation timeout"
        );
        assert_eq!(USER_MIRROR_PREVIEW_TIMEOUT, Duration::from_secs(120));
        assert!(USER_MIRROR_PREVIEW_TIMEOUT > MUTATE_TIMEOUT);
        assert!(
            src.contains("\"agent_hub.preview_user_mirror\"")
                && src.contains("USER_MIRROR_PREVIEW_TIMEOUT"),
            "preview user-mirror should use 120s inventory timeout, not MUTATE_TIMEOUT"
        );
        assert_eq!(USER_MIRROR_APPLY_TIMEOUT, Duration::from_secs(900));
        assert!(
            src.contains("\"agent_hub.apply_user_mirror\"")
                && src.contains("USER_MIRROR_APPLY_TIMEOUT"),
            "apply user-mirror should use 900s long-mutation timeout"
        );
        let forbidden_peer_path = concat!("/api/agent-hub", "/user-mirror");
        assert!(
            !src.contains(forbidden_peer_path),
            "control client 不得直连 peer HTTP"
        );
        assert_eq!(crate::backend::control::AGENT_HUB_API_VERSION, 5);
    }
}

#[cfg(test)]
#[path = "control_client_timeout_test.rs"]
mod timeout_tests;
