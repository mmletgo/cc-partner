//! workspace_restore — side-effect-free preflight 与幂等 tmux safe attach。
//!
//! Business Logic（为什么需要这个模块）:
//!     恢复工作现场前必须纯读校验 project/worktree/session/tmux/browser，
//!     只允许复用已有资源；禁止创建 shell、写 terminal、spawn/resume Agent。
//!
//! Code Logic（这个模块做什么）:
//!     产出 WorkspaceRestorePlan（select|reuse|safeAttach|skip）；safe_attach 仅 attach 已存在 tmux target。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::workspace_layout::{
    ensure_known_schema_version, InspectorTab, WorkspaceLayout, WorkspaceView,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// restore 计划状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestorePlanStatus {
    /// 全部可安全恢复。
    Complete,
    /// 部分跳过。
    Partial,
    /// 远端 offline 等不可用。
    Offline,
    /// 无可恢复项。
    Empty,
}

/// skip 原因（有界 code，不含绝对远端路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoreSkipReason {
    /// project 不存在。
    ProjectMissing,
    /// worktree 不存在。
    WorktreeMissing,
    /// worktree 不属于 project。
    WorktreeOwnershipMismatch,
    /// session 不存在。
    SessionMissing,
    /// session 归属不一致。
    SessionOwnershipMismatch,
    /// raw PTY 不可 safe attach。
    RawPtySkipped,
    /// tmux target 不存在。
    TmuxTargetMissing,
    /// backend 非 tmux 且非已注册。
    BackendUnsupported,
    /// browser target 非法。
    BrowserTargetInvalid,
    /// layout schema 未知。
    UnknownSchema,
    /// layout revision 已变。
    LayoutRevisionChanged,
    /// remote owner 离线。
    RemoteOffline,
    /// peer 不支持 capability。
    CapabilityUnsupported,
    /// 未指定 session。
    SessionNotRequested,
    /// 未指定 worktree。
    WorktreeNotRequested,
    /// 未指定 browser。
    BrowserNotRequested,
}

impl RestoreSkipReason {
    /// Business Logic（为什么需要这个函数）:
    ///     前端 notice 需要稳定 reason code。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 camelCase token 字符串。
    #[allow(dead_code)] // 前端 notice 稳定 reason code API surface
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProjectMissing => "projectMissing",
            Self::WorktreeMissing => "worktreeMissing",
            Self::WorktreeOwnershipMismatch => "worktreeOwnershipMismatch",
            Self::SessionMissing => "sessionMissing",
            Self::SessionOwnershipMismatch => "sessionOwnershipMismatch",
            Self::RawPtySkipped => "rawPtySkipped",
            Self::TmuxTargetMissing => "tmuxTargetMissing",
            Self::BackendUnsupported => "backendUnsupported",
            Self::BrowserTargetInvalid => "browserTargetInvalid",
            Self::UnknownSchema => "unknownSchema",
            Self::LayoutRevisionChanged => "layoutRevisionChanged",
            Self::RemoteOffline => "remoteOffline",
            Self::CapabilityUnsupported => "capabilityUnsupported",
            Self::SessionNotRequested => "sessionNotRequested",
            Self::WorktreeNotRequested => "worktreeNotRequested",
            Self::BrowserNotRequested => "browserNotRequested",
        }
    }
}

/// 单步恢复动作 outcome。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceRestoreOutcome {
    /// 前端应 select 已有资源。
    Select,
    /// 运行期已存在，直接 reuse。
    Reuse,
    /// 需幂等 tmux safe attach。
    SafeAttach,
    /// 跳过。
    Skip,
}

/// 单步恢复动作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRestoreAction {
    /// 动作目标资源类型。
    pub target: String,
    /// 资源 id（若有）。
    pub resource_id: Option<String>,
    /// outcome。
    pub outcome: WorkspaceRestoreOutcome,
    /// skip 原因。
    pub reason: Option<RestoreSkipReason>,
}

impl WorkspaceRestoreAction {
    /// Business Logic（为什么需要这个函数）:
    ///     测试断言 skip reason。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 reason 字段。
    #[allow(dead_code)] // 测试断言 skip reason API surface
    pub fn reason(&self) -> Option<RestoreSkipReason> {
        self.reason
    }
}

/// preflight 产出的恢复计划。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRestorePlan {
    /// 本次 restore 实例 id。
    pub restore_id: String,
    /// layout id。
    pub layout_id: String,
    /// layout revision（apply 时校验）。
    pub layout_revision: u64,
    /// 计划状态。
    pub status: RestorePlanStatus,
    /// 解析到的 project。
    pub resolved_project_id: Option<String>,
    /// 解析到的 worktree。
    pub resolved_worktree_id: Option<String>,
    /// 解析到的 session。
    pub resolved_session_id: Option<String>,
    /// workspace view（原样或默认）。
    pub workspace_view: WorkspaceView,
    /// inspector tab。
    pub inspector_tab: InspectorTab,
    /// browser target（校验后）。
    pub browser_target_url: Option<String>,
    /// 有序动作列表。
    pub actions: Vec<WorkspaceRestoreAction>,
}

/// safe attach 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeAttachResult {
    /// session id。
    pub session_id: String,
    /// 是否复用已有 registry。
    pub reused: bool,
}

/// preflight 依赖的只读观测接口（测试可注入计数器）。
///
/// Business Logic（为什么需要这个结构体）:
///     证明 preflight/safe_attach 路径零副作用需要可观测计数。
///
/// Code Logic（这个结构体做什么）:
///     原子计数器；生产路径使用真实 tmux 探测函数，测试路径可 mock。
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct RestoreSideEffectCounters {
    /// tmux new-session 调用次数。
    pub tmux_new_session: AtomicU64,
    /// tmux new-window 调用次数。
    pub tmux_new_window: AtomicU64,
    /// terminal write 次数。
    pub terminal_write: AtomicU64,
    /// agent spawn 次数。
    pub agent_spawn: AtomicU64,
    /// attach client 创建次数。
    pub attach_client: AtomicU64,
    /// worktree create 次数。
    pub worktree_create: AtomicU64,
    /// claude/codex resume 次数。
    pub agent_resume: AtomicU64,
}

#[allow(dead_code)] // 安全恢复副作用计数测试 API surface
impl RestoreSideEffectCounters {
    /// Business Logic（为什么需要这个函数）:
    ///     测试断言零副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取各计数器。
    pub fn tmux_new_session_count(&self) -> u64 {
        self.tmux_new_session.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn tmux_new_window_count(&self) -> u64 {
        self.tmux_new_window.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn terminal_write_count(&self) -> u64 {
        self.terminal_write.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn agent_spawn_count(&self) -> u64 {
        self.agent_spawn.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn attach_client_count(&self) -> u64 {
        self.attach_client.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn worktree_create_count(&self) -> u64 {
        self.worktree_create.load(Ordering::SeqCst)
    }

    /// 见上。
    pub fn agent_resume_count(&self) -> u64 {
        self.agent_resume.load(Ordering::SeqCst)
    }
}

/// preflight 用的只读环境适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     将 project/session/tmux 观测与 AppState 解耦，便于测试注入假 tmux 目标集合。
///
/// Code Logic（这个结构体做什么）:
///     包装 AppState 与可选 mock 目标集合、计数器。
pub struct RestoreInspectionContext {
    /// 应用状态。
    pub state: AppState,
    /// 副作用计数器。
    pub counters: Arc<RestoreSideEffectCounters>,
    /// 若 Some，则用此集合判断 tmux target 是否存在（测试 mock）；None 用真实 tmux。
    pub mock_tmux_targets: Option<std::collections::HashSet<String>>,
    /// mock registry 中已有的 session id。
    pub mock_registry_sessions: Option<std::collections::HashSet<String>>,
}

impl RestoreInspectionContext {
    /// Business Logic（为什么需要这个函数）:
    ///     生产路径用真实 AppState。
    ///
    /// Code Logic（这个函数做什么）:
    ///     无 mock，新计数器。
    pub fn from_state(state: AppState) -> Self {
        Self {
            state,
            counters: Arc::new(RestoreSideEffectCounters::default()),
            mock_tmux_targets: None,
            mock_registry_sessions: None,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     探测 tmux target 是否存在且不创建任何 session/window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mock 优先；否则委托 sessions 只读 helper。
    pub fn tmux_target_exists(&self, target: &str) -> bool {
        if let Some(ref set) = self.mock_tmux_targets {
            return set.contains(target);
        }
        crate::workbench::sessions::inspect_tmux_target_exists(target)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     判断 session 是否已在运行期 registry（map 级，含 provisional）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     真实 registry contains；再合并测试 mock 集合。
    #[allow(dead_code)]
    pub fn registry_contains(&self, session_id: &str) -> bool {
        if self.state.workbench_sessions.contains(session_id) {
            return true;
        }
        if let Some(ref set) = self.mock_registry_sessions {
            return set.contains(session_id);
        }
        false
    }

    /// Business Logic（为什么需要这个函数）:
    ///     workspace preflight/safe_attach 必须匹配 runtime_presence：仅 Live 可 Reuse。
    ///     claim-held provisional 不得被当成可复用会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mock set 命中视为 Live；否则委托 `runtime_presence`。
    pub fn runtime_presence(
        &self,
        session_id: &str,
    ) -> crate::workbench::sessions::SessionRuntimePresence {
        use crate::workbench::sessions::SessionRuntimePresence;
        if let Some(ref set) = self.mock_registry_sessions {
            if set.contains(session_id) {
                return SessionRuntimePresence::Live;
            }
        }
        self.state.workbench_sessions.runtime_presence(session_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Reuse 快捷路径仅对 Live Ready 会话成立。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `runtime_presence == Live`。
    pub fn registry_is_live(&self, session_id: &str) -> bool {
        matches!(
            self.runtime_presence(session_id),
            crate::workbench::sessions::SessionRuntimePresence::Live
        )
    }
}

/// Business Logic（为什么需要这个函数）:
///     打开 Workbench 时先纯读 preflight，生成可安全应用的计划。
///
/// Code Logic（这个函数做什么）:
///     校验 schema → project → worktree → session → view/inspector → browser；
///     动作顺序固定；不调用任何 restore/spawn 路径。
pub async fn preflight_workspace_restore(
    ctx: &RestoreInspectionContext,
    layout: &WorkspaceLayout,
) -> Result<WorkspaceRestorePlan, AppError> {
    if let Err(err) = ensure_known_schema_version(layout.schema_version) {
        return Ok(plan_for_unknown_schema(layout, err));
    }

    let restore_id = uuid::Uuid::new_v4().to_string();
    let mut actions = Vec::new();
    let mut resolved_project_id = None;
    let mut resolved_worktree_id = None;
    let mut resolved_session_id = None;
    let mut browser_target_url = None;

    // 1) project
    match ctx
        .state
        .workbench_project_repo
        .get(&layout.project_id)
        .await?
    {
        Some(project) => {
            resolved_project_id = Some(project.id.clone());
            actions.push(WorkspaceRestoreAction {
                target: "project".to_string(),
                resource_id: Some(project.id),
                outcome: WorkspaceRestoreOutcome::Select,
                reason: None,
            });
        }
        None => {
            actions.push(WorkspaceRestoreAction {
                target: "project".to_string(),
                resource_id: Some(layout.project_id.clone()),
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(RestoreSkipReason::ProjectMissing),
            });
            return Ok(finalize_plan(
                restore_id,
                layout,
                RestorePlanStatus::Empty,
                resolved_project_id,
                resolved_worktree_id,
                resolved_session_id,
                browser_target_url,
                actions,
            ));
        }
    }

    // 2) worktree
    match &layout.active_worktree_id {
        None => {
            actions.push(WorkspaceRestoreAction {
                target: "worktree".to_string(),
                resource_id: None,
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(RestoreSkipReason::WorktreeNotRequested),
            });
        }
        Some(worktree_id) => match ctx.state.workbench_worktree_repo.get(worktree_id).await? {
            None => {
                actions.push(WorkspaceRestoreAction {
                    target: "worktree".to_string(),
                    resource_id: Some(worktree_id.clone()),
                    outcome: WorkspaceRestoreOutcome::Skip,
                    reason: Some(RestoreSkipReason::WorktreeMissing),
                });
            }
            Some(wt) => {
                if wt.project_id != layout.project_id {
                    actions.push(WorkspaceRestoreAction {
                        target: "worktree".to_string(),
                        resource_id: Some(worktree_id.clone()),
                        outcome: WorkspaceRestoreOutcome::Skip,
                        reason: Some(RestoreSkipReason::WorktreeOwnershipMismatch),
                    });
                } else {
                    resolved_worktree_id = Some(wt.id.clone());
                    actions.push(WorkspaceRestoreAction {
                        target: "worktree".to_string(),
                        resource_id: Some(wt.id),
                        outcome: WorkspaceRestoreOutcome::Select,
                        reason: None,
                    });
                }
            }
        },
    }

    // 3) session
    match &layout.active_session_id {
        None => {
            actions.push(WorkspaceRestoreAction {
                target: "session".to_string(),
                resource_id: None,
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(RestoreSkipReason::SessionNotRequested),
            });
        }
        Some(session_id) => {
            match ctx.state.workbench_session_repo.get(session_id).await? {
                None => {
                    actions.push(WorkspaceRestoreAction {
                        target: "session".to_string(),
                        resource_id: Some(session_id.clone()),
                        outcome: WorkspaceRestoreOutcome::Skip,
                        reason: Some(RestoreSkipReason::SessionMissing),
                    });
                }
                Some(row) => {
                    let ownership_mismatch = row.project_id != layout.project_id
                        || (resolved_worktree_id.is_some()
                            && row.worktree_id.as_deref() != resolved_worktree_id.as_deref());
                    if ownership_mismatch {
                        actions.push(WorkspaceRestoreAction {
                            target: "session".to_string(),
                            resource_id: Some(session_id.clone()),
                            outcome: WorkspaceRestoreOutcome::Skip,
                            reason: Some(RestoreSkipReason::SessionOwnershipMismatch),
                        });
                    } else if ctx.registry_is_live(session_id) {
                        resolved_session_id = Some(session_id.clone());
                        actions.push(WorkspaceRestoreAction {
                            target: "session".to_string(),
                            resource_id: Some(session_id.clone()),
                            outcome: WorkspaceRestoreOutcome::Reuse,
                            reason: None,
                        });
                    } else if row.backend == "tmux" {
                        let target = crate::workbench::sessions::tmux_target_string_for_row(&row);
                        match target {
                            Ok(target) if ctx.tmux_target_exists(&target) => {
                                resolved_session_id = Some(session_id.clone());
                                actions.push(WorkspaceRestoreAction {
                                    target: "session".to_string(),
                                    resource_id: Some(session_id.clone()),
                                    outcome: WorkspaceRestoreOutcome::SafeAttach,
                                    reason: None,
                                });
                            }
                            Ok(_) => {
                                actions.push(WorkspaceRestoreAction {
                                    target: "session".to_string(),
                                    resource_id: Some(session_id.clone()),
                                    outcome: WorkspaceRestoreOutcome::Skip,
                                    reason: Some(RestoreSkipReason::TmuxTargetMissing),
                                });
                            }
                            Err(_) => {
                                actions.push(WorkspaceRestoreAction {
                                    target: "session".to_string(),
                                    resource_id: Some(session_id.clone()),
                                    outcome: WorkspaceRestoreOutcome::Skip,
                                    reason: Some(RestoreSkipReason::TmuxTargetMissing),
                                });
                            }
                        }
                    } else {
                        // raw PTY 或其它：始终 skip，绝不 spawn
                        actions.push(WorkspaceRestoreAction {
                            target: "session".to_string(),
                            resource_id: Some(session_id.clone()),
                            outcome: WorkspaceRestoreOutcome::Skip,
                            reason: Some(RestoreSkipReason::RawPtySkipped),
                        });
                    }
                }
            }
        }
    }

    // 4) view
    actions.push(WorkspaceRestoreAction {
        target: "workspaceView".to_string(),
        resource_id: Some(layout.workspace_view.as_str().to_string()),
        outcome: WorkspaceRestoreOutcome::Select,
        reason: None,
    });

    // 5) inspector
    actions.push(WorkspaceRestoreAction {
        target: "inspectorTab".to_string(),
        resource_id: Some(layout.inspector_tab.as_str().to_string()),
        outcome: WorkspaceRestoreOutcome::Select,
        reason: None,
    });

    // 6) browser
    match &layout.browser_target_url {
        None => {
            actions.push(WorkspaceRestoreAction {
                target: "browserTarget".to_string(),
                resource_id: None,
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(RestoreSkipReason::BrowserNotRequested),
            });
        }
        Some(url) => match crate::workbench::browser::normalize_browser_target_url(url) {
            Ok(normalized) => {
                browser_target_url = Some(normalized.clone());
                actions.push(WorkspaceRestoreAction {
                    target: "browserTarget".to_string(),
                    resource_id: Some(normalized),
                    outcome: WorkspaceRestoreOutcome::Select,
                    reason: None,
                });
            }
            Err(_) => {
                actions.push(WorkspaceRestoreAction {
                    target: "browserTarget".to_string(),
                    resource_id: Some(url.clone()),
                    outcome: WorkspaceRestoreOutcome::Skip,
                    reason: Some(RestoreSkipReason::BrowserTargetInvalid),
                });
            }
        },
    }

    let has_skip = actions.iter().any(|a| {
        a.outcome == WorkspaceRestoreOutcome::Skip
            && a.reason != Some(RestoreSkipReason::WorktreeNotRequested)
            && a.reason != Some(RestoreSkipReason::SessionNotRequested)
            && a.reason != Some(RestoreSkipReason::BrowserNotRequested)
    });
    // 仅“未请求”类 skip 仍可算 complete；真正资源缺失为 partial
    let status = if has_skip {
        RestorePlanStatus::Partial
    } else {
        RestorePlanStatus::Complete
    };

    Ok(finalize_plan(
        restore_id,
        layout,
        status,
        resolved_project_id,
        resolved_worktree_id,
        resolved_session_id,
        browser_target_url,
        actions,
    ))
}

/// Business Logic（为什么需要这个函数）:
///     apply 阶段仅对 preflight 标记 safeAttach 的 session 做幂等 attach。
///
/// Code Logic（这个函数做什么）:
///     复核 persisted tmux + target 存在；registry 已有则 reuse；否则仅创建 attach client。
///     禁止 new-session/new-window、raw PTY、terminal write、agent resume。
pub async fn safe_attach_workbench_session(
    ctx: &RestoreInspectionContext,
    session_id: &str,
) -> Result<SafeAttachResult, AppError> {
    use crate::workbench::sessions::{RestoreClaimOutcome, SessionRuntimePresence};

    // R19 M2：仅 Live Ready 可 reuse。
    // RestoreInProgress：短自旋等待 holder 结束（Ready→reuse / 释放→可 claim）；
    // 超时仍 in-progress → retryable busy（workspace apply 不得挂满 60s）。
    match ctx.runtime_presence(session_id) {
        SessionRuntimePresence::Live => {
            return Ok(SafeAttachResult {
                session_id: session_id.to_string(),
                reused: true,
            });
        }
        SessionRuntimePresence::RestoreInProgress => {
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                match ctx.runtime_presence(session_id) {
                    SessionRuntimePresence::Live => {
                        return Ok(SafeAttachResult {
                            session_id: session_id.to_string(),
                            reused: true,
                        });
                    }
                    SessionRuntimePresence::Missing => break,
                    SessionRuntimePresence::RestoreInProgress => continue,
                }
            }
            if ctx.registry_is_live(session_id) {
                return Ok(SafeAttachResult {
                    session_id: session_id.to_string(),
                    reused: true,
                });
            }
            if matches!(
                ctx.runtime_presence(session_id),
                SessionRuntimePresence::RestoreInProgress
            ) {
                return Err(AppError::unavailable("safe_attach_claim_busy".to_string()));
            }
            // claim 已释放且非 Live：fallthrough 重新 claim / attach。
        }
        SessionRuntimePresence::Missing => {}
    }

    let row = ctx
        .state
        .workbench_session_repo
        .get(session_id)
        .await?
        .ok_or_else(|| AppError::not_found("session_not_found".to_string()))?;

    if row.backend != "tmux" {
        return Err(AppError::validation(
            "safe_attach_requires_tmux".to_string(),
        ));
    }

    let target = crate::workbench::sessions::tmux_target_string_for_row(&row)?;
    if !ctx.tmux_target_exists(&target) {
        return Err(AppError::unavailable("tmux_target_missing".to_string()));
    }

    // claim 防止并发重复 attach；未拿到 claim 时禁止 fallthrough（否则会无占位 attach，
    // 并可能用 armed RestoreClaimGuard 误释放他人 claim）。
    // R26 H1：Claimed 携带可撤销 generation。
    let mut claim_generation: Option<u64> = match ctx
        .state
        .workbench_sessions
        .try_claim_restore(session_id)
    {
        RestoreClaimOutcome::Claimed { generation } => Some(generation),
        _ => None,
    };
    if claim_generation.is_none() {
        // 另一路正在 attach 或已完成：优先 reuse
        if ctx.registry_is_live(session_id) {
            return Ok(SafeAttachResult {
                session_id: session_id.to_string(),
                reused: true,
            });
        }
        // 短暂自旋等待 holder 完成并进入 registry，或 claim 释放后由本路接手
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            if ctx.registry_is_live(session_id) {
                return Ok(SafeAttachResult {
                    session_id: session_id.to_string(),
                    reused: true,
                });
            }
            if let RestoreClaimOutcome::Claimed { generation } =
                ctx.state.workbench_sessions.try_claim_restore(session_id)
            {
                claim_generation = Some(generation);
                break;
            }
        }
        if claim_generation.is_none() {
            if ctx.registry_is_live(session_id) {
                return Ok(SafeAttachResult {
                    session_id: session_id.to_string(),
                    reused: true,
                });
            }
            // fail-closed：超时仍无 claim 且无 registry → busy，绝不无 claim 继续 attach
            return Err(AppError::unavailable("safe_attach_claim_busy".to_string()));
        }
    }

    let claim_generation = claim_generation.expect("claim generation present after spin");
    // 仅在确认 claimed 后构造 guard（Drop 才安全 generation-scoped 释放本路 claim）
    let mut guard = crate::workbench::sessions::RestoreClaimGuard::new(
        (*ctx.state.workbench_sessions).clone(),
        session_id.to_string(),
        claim_generation,
    );

    // 再次确认 target（preflight 与 apply 之间可能消失）
    if !ctx.tmux_target_exists(&target) {
        return Err(AppError::unavailable("tmux_target_missing".to_string()));
    }

    // 测试 mock：只登记 Fake registry，不创建真实 PTY/tmux window。
    if ctx.mock_tmux_targets.is_some() {
        #[cfg(test)]
        {
            // R27 H5：mock 路径也必须 revalidate claim generation 后才 Ready。
            ctx.state
                .workbench_sessions
                .require_restore_claim_active(&row.id, claim_generation)?;
            ctx.counters.attach_client.fetch_add(1, Ordering::SeqCst);
            let sid = row.id.clone();
            ctx.state
                .workbench_sessions
                .insert_fake_session_row_for_test(row);
            ctx.state
                .workbench_sessions
                .bind_restore_claim_generation_for_test(&sid, Some(claim_generation));
            let gen = ctx
                .state
                .workbench_sessions
                .session_generation_for_test(&sid)
                .expect("generation after fake insert");
            // insert_fake 已 Ready；幂等 Ready 仍会 revalidate claim/project。
            if !ctx
                .state
                .workbench_sessions
                .mark_session_ready_for_generation(&sid, gen, Some(&ctx.state))
            {
                return Err(AppError::unavailable(
                    "session_restore_claim_revoked".to_string(),
                ));
            }
        }
        #[cfg(not(test))]
        {
            let _ = row;
            return Err(AppError::generic(
                "safe_attach mock path is test-only".to_string(),
            ));
        }
    } else {
        crate::workbench::sessions::safe_attach_existing_tmux_session(
            &ctx.state,
            row,
            &ctx.counters,
            Some(claim_generation),
        )?;
    }

    // R16：显式广播 Ready，禁止仅 release 让 waiter 误判。
    guard.finish(crate::workbench::sessions::SharedRestoreNotification::Ready);

    Ok(SafeAttachResult {
        session_id: session_id.to_string(),
        reused: false,
    })
}

/// Business Logic（为什么需要这个函数）:
///     apply 在执行前校验 layout revision 未变。
///
/// Code Logic（这个函数做什么）:
///     比对 plan.layout_revision 与当前 layout。
pub async fn ensure_plan_layout_revision(
    state: &AppState,
    plan: &WorkspaceRestorePlan,
) -> Result<(), AppError> {
    let layout = state
        .workbench_workspace_layout_repo
        .get_by_id(&plan.layout_id)
        .await?
        .ok_or_else(|| AppError::not_found("workspace_layout_not_found".to_string()))?;
    if layout.revision != plan.layout_revision {
        return Err(AppError::conflict(
            "workspace_layout_revision_changed".to_string(),
        ));
    }
    Ok(())
}

fn plan_for_unknown_schema(layout: &WorkspaceLayout, _err: AppError) -> WorkspaceRestorePlan {
    WorkspaceRestorePlan {
        restore_id: uuid::Uuid::new_v4().to_string(),
        layout_id: layout.id.clone(),
        layout_revision: layout.revision,
        status: RestorePlanStatus::Empty,
        resolved_project_id: None,
        resolved_worktree_id: None,
        resolved_session_id: None,
        workspace_view: layout.workspace_view,
        inspector_tab: layout.inspector_tab,
        browser_target_url: None,
        actions: vec![WorkspaceRestoreAction {
            target: "layout".to_string(),
            resource_id: Some(layout.id.clone()),
            outcome: WorkspaceRestoreOutcome::Skip,
            reason: Some(RestoreSkipReason::UnknownSchema),
        }],
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_plan(
    restore_id: String,
    layout: &WorkspaceLayout,
    status: RestorePlanStatus,
    resolved_project_id: Option<String>,
    resolved_worktree_id: Option<String>,
    resolved_session_id: Option<String>,
    browser_target_url: Option<String>,
    actions: Vec<WorkspaceRestoreAction>,
) -> WorkspaceRestorePlan {
    WorkspaceRestorePlan {
        restore_id,
        layout_id: layout.id.clone(),
        layout_revision: layout.revision,
        status,
        resolved_project_id,
        resolved_worktree_id,
        resolved_session_id,
        workspace_view: layout.workspace_view,
        inspector_tab: layout.inspector_tab,
        browser_target_url,
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo,
        WorkbenchWorktreeRepo,
    };
    use crate::workbench::models::{
        WorkbenchProjectRow, WorkbenchSessionRow, WorkbenchWorktreeRow,
    };
    use crate::workbench::workspace_layout::{
        desktop_auto_slot_key, WorkspaceLayoutDraft, WorkspaceLayoutKind,
        WORKSPACE_LAYOUT_SCHEMA_VERSION,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashSet;
    use std::str::FromStr;
    use std::sync::Arc;

    /// 测试 fixture：内存库 + 可控 mock tmux。
    struct RestoreFixture {
        ctx: RestoreInspectionContext,
        layout: WorkspaceLayout,
    }

    impl RestoreFixture {
        async fn base() -> Self {
            let options = SqliteConnectOptions::from_str("sqlite::memory:")
                .unwrap()
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();

            sqlx::query(
                "CREATE TABLE workbench_projects (\
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
                 device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE TABLE workbench_worktrees (\
                 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT, \
                 base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL, \
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE TABLE workbench_sessions (\
                 id TEXT PRIMARY KEY, project_id TEXT NOT NULL, worktree_id TEXT, name TEXT NOT NULL, \
                 command TEXT NOT NULL, cwd TEXT, status TEXT NOT NULL, cols INTEGER NOT NULL, \
                 rows INTEGER NOT NULL, started_at TEXT NOT NULL, exited_at TEXT, exit_code INTEGER, \
                 backend TEXT NOT NULL, backend_id TEXT, backend_window_id TEXT, \
                 created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();

            let project_repo = WorkbenchProjectRepo::new(pool.clone());
            let worktree_repo = WorkbenchWorktreeRepo::new(pool.clone());
            let session_repo = WorkbenchSessionRepo::new(pool.clone());
            let layout_repo = WorkbenchWorkspaceLayoutRepo::new(pool.clone());
            layout_repo.ensure_schema().await.unwrap();

            project_repo
                .upsert(&WorkbenchProjectRow {
                    id: "p1".to_string(),
                    name: "demo".to_string(),
                    kind: "local".to_string(),
                    device_id: "d1".to_string(),
                    device_name: "local".to_string(),
                    path: "/tmp/demo".to_string(),
                    last_opened_at: "t".to_string(),
                    created_at: "t".to_string(),
                    updated_at: "t".to_string(),
                })
                .await
                .unwrap();
            worktree_repo
                .upsert(&WorkbenchWorktreeRow {
                    id: "w1".to_string(),
                    project_id: "p1".to_string(),
                    name: "main".to_string(),
                    branch: Some("main".to_string()),
                    base_branch: None,
                    path: "/tmp/demo".to_string(),
                    is_main: true,
                    created_at: "t".to_string(),
                    updated_at: "t".to_string(),
                })
                .await
                .unwrap();

            let layout = layout_repo
                .save_cas(
                    WorkspaceLayoutDraft {
                        slot_key: desktop_auto_slot_key().to_string(),
                        kind: WorkspaceLayoutKind::Auto,
                        name: None,
                        project_id: "p1".to_string(),
                        active_worktree_id: Some("w1".to_string()),
                        active_session_id: Some("s1".to_string()),
                        workspace_view: WorkspaceView::Terminal,
                        inspector_tab: InspectorTab::Files,
                        browser_target_url: None,
                    },
                    None,
                )
                .await
                .unwrap();

            // 构造最小 AppState 需要很多字段——用 runtime 完整路径过重。
            // 这里用专用测试 state helper。
            let state =
                build_minimal_state(pool, project_repo, worktree_repo, session_repo, layout_repo)
                    .await;

            Self {
                ctx: RestoreInspectionContext {
                    state,
                    counters: Arc::new(RestoreSideEffectCounters::default()),
                    mock_tmux_targets: Some(HashSet::new()),
                    mock_registry_sessions: Some(HashSet::new()),
                },
                layout,
            }
        }

        async fn persisted_tmux(self, session_id: &str) -> Self {
            let row = WorkbenchSessionRow {
                id: session_id.to_string(),
                project_id: "p1".to_string(),
                worktree_id: Some("w1".to_string()),
                name: "demo".to_string(),
                command: "tmux attach".to_string(),
                cwd: "/tmp/demo".to_string(),
                status: "running".to_string(),
                cols: 80,
                rows: 24,
                started_at: "t".to_string(),
                exited_at: None,
                exit_code: None,
                backend: "tmux".to_string(),
                backend_id: Some("sess-p1".to_string()),
                backend_window_id: Some("@1".to_string()),
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            };
            self.ctx
                .state
                .workbench_session_repo
                .upsert(&row)
                .await
                .unwrap();
            self
        }

        async fn persisted_raw_pty(self, session_id: &str) -> Self {
            let row = WorkbenchSessionRow {
                id: session_id.to_string(),
                project_id: "p1".to_string(),
                worktree_id: Some("w1".to_string()),
                name: "demo".to_string(),
                command: "/bin/zsh".to_string(),
                cwd: "/tmp/demo".to_string(),
                status: "running".to_string(),
                cols: 80,
                rows: 24,
                started_at: "t".to_string(),
                exited_at: None,
                exit_code: None,
                backend: "raw_pty".to_string(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            };
            self.ctx
                .state
                .workbench_session_repo
                .upsert(&row)
                .await
                .unwrap();
            self
        }

        fn tmux_target_absent(self) -> Self {
            // mock set 为空 → 不存在
            self
        }

        fn tmux_target_present(mut self) -> Self {
            let mut set = HashSet::new();
            set.insert("sess-p1:@1".to_string());
            self.ctx.mock_tmux_targets = Some(set);
            self
        }

        fn registry_has(mut self, session_id: &str) -> Self {
            let mut set = self.ctx.mock_registry_sessions.take().unwrap_or_default();
            set.insert(session_id.to_string());
            self.ctx.mock_registry_sessions = Some(set);
            self
        }

        async fn preflight(&self) -> Result<WorkspaceRestorePlan, AppError> {
            preflight_workspace_restore(&self.ctx, &self.layout).await
        }

        fn tmux_new_session_count(&self) -> u64 {
            self.ctx.counters.tmux_new_session_count()
        }
        fn tmux_new_window_count(&self) -> u64 {
            self.ctx.counters.tmux_new_window_count()
        }
        fn terminal_write_count(&self) -> u64 {
            self.ctx.counters.terminal_write_count()
        }
        fn agent_spawn_count(&self) -> u64 {
            self.ctx.counters.agent_spawn_count()
        }
        fn attach_client_count(&self) -> u64 {
            self.ctx.counters.attach_client_count()
        }

        async fn safe_attach(&self, session_id: &str) -> Result<SafeAttachResult, AppError> {
            safe_attach_workbench_session(&self.ctx, session_id).await
        }
    }

    async fn build_minimal_state(
        pool: sqlx::SqlitePool,
        project_repo: WorkbenchProjectRepo,
        worktree_repo: WorkbenchWorktreeRepo,
        session_repo: WorkbenchSessionRepo,
        layout_repo: WorkbenchWorkspaceLayoutRepo,
    ) -> AppState {
        use crate::backend::authority::RuntimeRole;
        use crate::backend::event_bus::RuntimeEventBus;
        use crate::backend::runtime_metrics::RuntimeMetrics;
        use crate::backend::ui::HeadlessBackendUi;
        use crate::cloud_sync::CloudSyncRuntime;
        use crate::config::{
            AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
        };
        use crate::config_runtime::ConfigRuntime;
        use crate::config_store::MemoryConfigStore;
        use crate::net::peer_client::PeerClient;
        use crate::orchestrator::repo::OrchestratorRepo;
        use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
        use crate::storage::{
            ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate, PromptRepo, ScratchpadRepo,
            SshTargetRepo, TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo,
        };
        use crate::transfer::registry::TransferRegistry;
        use crate::updater::UpdateRuntime;
        use std::sync::atomic::AtomicU16;
        use std::sync::{Mutex, RwLock};

        let config = AppConfig {
            device_id: "d1".to_string(),
            device_name: "test".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            db_path: ":memory:".to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(config.clone()));
        let config_runtime = Arc::new(ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());
        let owner = uuid::Uuid::new_v4().to_string();
        let event_bus = Arc::new(RuntimeEventBus::new(owner));

        AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: maintenance_gate.clone(),
            prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("d1".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(std::path::PathBuf::from("/tmp"))),
            update_runtime: Arc::new(UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(project_repo),
            workbench_session_repo: Arc::new(session_repo),
            workbench_worktree_repo: Arc::new(worktree_repo),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            workbench_workspace_layout_repo: Arc::new(layout_repo),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::path::PathBuf::from("/tmp/browser-verification-test"),
                    "test-owner".into(),
                )
                .expect("browser verification fixture"),
            ),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: std::sync::Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::default(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            runtime_metrics: Arc::new(RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus,
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    #[tokio::test]
    async fn preflight_skips_missing_tmux_without_spawning_or_writing() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .tmux_target_absent();
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.reason() == Some(RestoreSkipReason::TmuxTargetMissing)));
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.tmux_new_window_count(), 0);
        assert_eq!(fixture.terminal_write_count(), 0);
        assert_eq!(fixture.agent_spawn_count(), 0);
    }

    #[tokio::test]
    async fn preflight_skips_raw_pty() {
        let fixture = RestoreFixture::base().await.persisted_raw_pty("s1").await;
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.reason() == Some(RestoreSkipReason::RawPtySkipped)));
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.agent_spawn_count(), 0);
    }

    #[tokio::test]
    async fn preflight_safe_attach_when_tmux_exists() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .tmux_target_present();
        let plan = fixture.preflight().await.unwrap();
        assert!(plan.actions.iter().any(|a| {
            a.target == "session" && a.outcome == WorkspaceRestoreOutcome::SafeAttach
        }));
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.terminal_write_count(), 0);
    }

    #[tokio::test]
    async fn preflight_reuses_registry_session() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .registry_has("s1");
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.target == "session" && a.outcome == WorkspaceRestoreOutcome::Reuse));
    }

    /// Business Logic（R19 M2: 为什么需要这个测试）:
    ///     claim-held provisional 不得 preflight Reuse。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + provisional fake insert → preflight 不为 Reuse。
    #[tokio::test]
    async fn preflight_does_not_reuse_claim_held_provisional() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await;
        assert!(fixture
            .ctx
            .state
            .workbench_sessions
            .try_claim_restore("s1")
            .is_claimed());
        fixture
            .ctx
            .state
            .workbench_sessions
            .insert_provisional_fake_session_for_test("s1", "p1");
        assert!(fixture.ctx.state.workbench_sessions.contains("s1"));
        assert!(!fixture.ctx.registry_is_live("s1"));

        let plan = fixture.preflight().await.unwrap();
        assert!(
            plan.actions.iter().all(|a| {
                a.target != "session" || a.outcome != WorkspaceRestoreOutcome::Reuse
            }),
            "provisional claim-held session must not be Reuse"
        );
        fixture
            .ctx
            .state
            .workbench_sessions
            .release_restore_claim("s1");
    }

    /// Business Logic（R19 M2: 为什么需要这个测试）:
    ///     provisional 随后 Ready 后，preflight 才可 Reuse。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + provisional → mark Ready + finish Ready → preflight Reuse。
    #[tokio::test]
    async fn preflight_reuses_after_provisional_becomes_ready() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await;
        assert!(fixture
            .ctx
            .state
            .workbench_sessions
            .try_claim_restore("s1")
            .is_claimed());
        fixture
            .ctx
            .state
            .workbench_sessions
            .insert_provisional_fake_session_for_test("s1", "p1");
        fixture
            .ctx
            .state
            .workbench_sessions
            .mark_session_ready("s1", None);
        fixture.ctx.state.workbench_sessions.finish_restore_claim(
            "s1",
            crate::workbench::sessions::SharedRestoreNotification::Ready,
        );
        assert!(fixture.ctx.registry_is_live("s1"));

        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.target == "session" && a.outcome == WorkspaceRestoreOutcome::Reuse));
    }

    #[tokio::test]
    async fn preflight_skips_missing_project() {
        let mut fixture = RestoreFixture::base().await;
        fixture.layout.project_id = "missing".to_string();
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.reason() == Some(RestoreSkipReason::ProjectMissing)));
        assert_eq!(plan.status, RestorePlanStatus::Empty);
    }

    #[tokio::test]
    async fn preflight_skips_worktree_ownership_mismatch() {
        let mut fixture = RestoreFixture::base().await;
        // 插入另一 project 的 worktree
        fixture
            .ctx
            .state
            .workbench_project_repo
            .upsert(&WorkbenchProjectRow {
                id: "p2".to_string(),
                name: "other".to_string(),
                kind: "local".to_string(),
                device_id: "d1".to_string(),
                device_name: "local".to_string(),
                path: "/tmp/other".to_string(),
                last_opened_at: "t".to_string(),
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            })
            .await
            .unwrap();
        fixture
            .ctx
            .state
            .workbench_worktree_repo
            .upsert(&WorkbenchWorktreeRow {
                id: "w2".to_string(),
                project_id: "p2".to_string(),
                name: "x".to_string(),
                branch: None,
                base_branch: None,
                path: "/tmp/other".to_string(),
                is_main: true,
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            })
            .await
            .unwrap();
        fixture.layout.active_worktree_id = Some("w2".to_string());
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.reason() == Some(RestoreSkipReason::WorktreeOwnershipMismatch)));
    }

    #[tokio::test]
    async fn unknown_schema_fails_closed() {
        let mut fixture = RestoreFixture::base().await;
        fixture.layout.schema_version = 99;
        let plan = fixture.preflight().await.unwrap();
        assert!(plan
            .actions
            .iter()
            .any(|a| a.reason() == Some(RestoreSkipReason::UnknownSchema)));
        let _ = WORKSPACE_LAYOUT_SCHEMA_VERSION;
    }

    #[tokio::test]
    async fn concurrent_safe_attach_creates_one_attach_client_only() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .tmux_target_present();
        let (a, b) = tokio::join!(fixture.safe_attach("s1"), fixture.safe_attach("s1"));
        assert!(a.is_ok() && b.is_ok(), "a={a:?} b={b:?}");
        // 至多一次 attach client（另一路 reuse）
        assert!(fixture.attach_client_count() <= 1);
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.terminal_write_count(), 0);
        assert_eq!(fixture.ctx.counters.agent_resume_count(), 0);
    }

    /// holder 持 claim 但不插入 registry，且 attach 慢于自旋窗口 → waiter 必须 busy，不得二次 attach。
    #[tokio::test]
    async fn concurrent_safe_attach_waiter_busy_when_claim_held_without_registry() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .tmux_target_present();
        // 模拟慢 holder：占 claim 但不写 registry、不 attach
        assert!(
            fixture
                .ctx
                .state
                .workbench_sessions
                .try_claim_restore("s1")
                .is_claimed(),
            "holder must own claim"
        );
        let err = fixture
            .safe_attach("s1")
            .await
            .expect_err("waiter must fail closed");
        assert_eq!(err.code(), "safe_attach_claim_busy");
        assert_eq!(
            fixture.attach_client_count(),
            0,
            "waiter must not fallthrough to attach without claim"
        );
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.tmux_new_window_count(), 0);
        // holder 仍持 claim（waiter 不得误 release）
        assert!(
            !fixture
                .ctx
                .state
                .workbench_sessions
                .try_claim_restore("s1")
                .is_claimed(),
            "holder claim must still be held"
        );
        fixture
            .ctx
            .state
            .workbench_sessions
            .release_restore_claim("s1");
    }

    /// preflight skip missing tmux 后，list/restore 路径也不得 create window。
    #[tokio::test]
    async fn preflight_skip_then_list_restore_never_creates_tmux_window() {
        crate::workbench::sessions::reset_create_tmux_window_call_count_for_test();
        // 使用全局唯一 backend_id，避免本机遗留 tmux session 让 restore 误命中
        let unique_backend = format!("cp-a8-missing-{}", uuid::Uuid::new_v4());
        let fixture = RestoreFixture::base().await;
        let row = WorkbenchSessionRow {
            id: "s1".to_string(),
            project_id: "p1".to_string(),
            worktree_id: Some("w1".to_string()),
            name: "demo".to_string(),
            command: "tmux attach".to_string(),
            cwd: "/tmp/demo".to_string(),
            status: "running".to_string(),
            cols: 80,
            rows: 24,
            started_at: "t".to_string(),
            exited_at: None,
            exit_code: None,
            backend: "tmux".to_string(),
            backend_id: Some(unique_backend.clone()),
            backend_window_id: Some("@99991".to_string()),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        };
        fixture
            .ctx
            .state
            .workbench_session_repo
            .upsert(&row)
            .await
            .unwrap();
        // preflight mock：target 不存在 → skip
        let fixture = fixture.tmux_target_absent();
        // layout 指向 s1
        let mut layout = fixture.layout.clone();
        layout.active_session_id = Some("s1".to_string());
        let plan = preflight_workspace_restore(&fixture.ctx, &layout)
            .await
            .unwrap();
        assert!(
            plan.actions.iter().any(|a| {
                a.target == "session"
                    && a.outcome == WorkspaceRestoreOutcome::Skip
                    && a.reason() == Some(RestoreSkipReason::TmuxTargetMissing)
            }),
            "preflight must skip missing tmux session"
        );
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.tmux_new_window_count(), 0);

        // 模拟 project open → sessions.list → restore_persisted_sessions → restore()
        // restore() 走真实 tmux 探测；唯一 backend_id 必不存在 → skip，禁止 create
        let project = fixture
            .ctx
            .state
            .workbench_project_repo
            .get("p1")
            .await
            .unwrap()
            .expect("project");
        let restore_err = fixture
            .ctx
            .state
            .workbench_sessions
            .restore(
                fixture.ctx.state.clone(),
                project,
                row,
                Some("main".to_string()),
                None,
            )
            .expect_err("missing target must skip, not create");
        let code = restore_err.code();
        assert!(
            code == "tmux_target_missing" || code == "tmux_unavailable",
            "unexpected restore error code: {code}"
        );
        assert_eq!(
            crate::workbench::sessions::create_tmux_window_call_count_for_test(),
            0,
            "list restore path must not call create_tmux_window"
        );
        assert_eq!(fixture.attach_client_count(), 0);
        assert!(!fixture.ctx.state.workbench_sessions.contains("s1"));
    }

    /// raw PTY 持久化 session 在 list restore 路径必须 skip，不得 spawn 新 shell。
    #[tokio::test]
    async fn list_restore_skips_raw_pty_without_create() {
        crate::workbench::sessions::reset_create_tmux_window_call_count_for_test();
        let fixture = RestoreFixture::base()
            .await
            .persisted_raw_pty("s-raw")
            .await;
        let row = fixture
            .ctx
            .state
            .workbench_session_repo
            .get("s-raw")
            .await
            .unwrap()
            .expect("row");
        let project = fixture
            .ctx
            .state
            .workbench_project_repo
            .get("p1")
            .await
            .unwrap()
            .expect("project");
        let err = fixture
            .ctx
            .state
            .workbench_sessions
            .restore(
                fixture.ctx.state.clone(),
                project,
                row,
                Some("main".to_string()),
                None,
            )
            .expect_err("raw pty must skip");
        assert_eq!(err.code(), "restore_skips_raw_pty");
        assert_eq!(
            crate::workbench::sessions::create_tmux_window_call_count_for_test(),
            0
        );
        assert!(!fixture.ctx.state.workbench_sessions.contains("s-raw"));
    }

    #[tokio::test]
    async fn safe_attach_rejects_raw_pty() {
        let fixture = RestoreFixture::base().await.persisted_raw_pty("s1").await;
        let err = fixture.safe_attach("s1").await.unwrap_err();
        assert_eq!(err.code(), "safe_attach_requires_tmux");
        assert_eq!(fixture.tmux_new_session_count(), 0);
        assert_eq!(fixture.agent_spawn_count(), 0);
    }

    #[tokio::test]
    async fn safe_attach_rejects_missing_target() {
        let fixture = RestoreFixture::base()
            .await
            .persisted_tmux("s1")
            .await
            .tmux_target_absent();
        let err = fixture.safe_attach("s1").await.unwrap_err();
        assert_eq!(err.code(), "tmux_target_missing");
        assert_eq!(fixture.attach_client_count(), 0);
    }
}
