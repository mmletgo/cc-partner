//! Orchestrator 验证与交付入口。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 完成后需要在任务 worktree 中执行项目验证命令，并在验证通过后自动完成提交、推送、
//!     合并到主工作区和推送主分支，把每个阶段作为 evidence 保存。
//!
//! Code Logic（这个模块做什么）:
//!     提供验证命令执行 helper 和 full-auto delivery pipeline；交付失败会写 failed delivery evidence
//!     并把任务置为 Blocked，避免任务永久停留 Delivering。

use crate::commands::workbench::{
    local_commit_workbench_worktree, local_merge_workbench_worktree, local_push_workbench_worktree,
};
use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorTaskDto, OrchestratorTaskStatus, EVIDENCE_KIND_DELIVERY,
};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::storage::{WorkbenchProjectRepo, WorkbenchWorktreeRepo};
use crate::workbench::git as workbench_git;
use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

const DEFAULT_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const PROCESS_REAP_GRACE_TIMEOUT: Duration = Duration::from_millis(200);
const PIPE_READER_JOIN_GRACE_TIMEOUT: Duration = Duration::from_millis(200);
const OUTPUT_TRUNCATED_MARKER: &str = "[output truncated]";
#[cfg(unix)]
const SIGKILL: std::os::raw::c_int = 9;
static DELIVERY_TASK_LOCKS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();

/// 单任务 delivery 执行权守卫。
///
/// Business Logic（为什么需要这个结构体）:
///     同一个 Orchestrator 任务不能同时执行两条自动交付流水线，否则会重复提交、推送、合并并写重复 evidence。
///
/// Code Logic（这个结构体做什么）:
///     记录已领取的 task_id；Drop 时从进程内 HashSet 移除，且不持有 MutexGuard 跨 await。
struct DeliveryTaskGuard {
    task_id: String,
}

impl Drop for DeliveryTaskGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     delivery pipeline 结束后必须释放任务执行权，后续用户重试或再次触发才能继续处理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Drop 中短暂锁定全局 HashSet 并移除 task_id；锁中毒时静默跳过，避免 Drop panic。
    fn drop(&mut self) {
        if let Some(locks) = DELIVERY_TASK_LOCKS.get() {
            if let Ok(mut locked) = locks.lock() {
                locked.remove(&self.task_id);
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     自动交付需要 per-task 执行权，防止重复点击或并发命令同时执行 Git side effect。
///
/// Code Logic（这个函数做什么）:
///     短暂锁定全局 HashSet；首次插入 task_id 返回守卫，已存在返回 None，不持有锁跨 await。
fn try_acquire_delivery_task_guard(task_id: &str) -> Result<Option<DeliveryTaskGuard>, AppError> {
    let locks = DELIVERY_TASK_LOCKS.get_or_init(|| StdMutex::new(HashSet::new()));
    let mut locked = locks
        .lock()
        .map_err(|_| AppError::generic("Orchestrator delivery lock 已损坏"))?;
    if !locked.insert(task_id.to_string()) {
        return Ok(None);
    }
    Ok(Some(DeliveryTaskGuard {
        task_id: task_id.to_string(),
    }))
}

/// Business Logic（为什么需要这个函数）:
///     用户可以在 delivery pipeline 的任意阶段终止任务；一旦任务不再是 Delivering，
///     后续 Git push/merge 这类不可逆副作用必须立即停止。
///
/// Code Logic（这个函数做什么）:
///     重新读取任务当前状态；若仍为 Delivering 返回 None 继续流程，否则返回当前任务和已完成阶段列表。
async fn stop_delivery_if_task_changed(
    repo: &OrchestratorRepo,
    task_id: &str,
    stages: &[String],
) -> Result<Option<DeliverySummary>, AppError> {
    let current = repo.get_task(task_id).await?;
    if current.status == OrchestratorTaskStatus::Delivering {
        return Ok(None);
    }
    Ok(Some(DeliverySummary {
        task: OrchestratorTaskDto::from(current),
        stages: stages.to_vec(),
    }))
}

#[cfg(unix)]
unsafe extern "C" {
    fn killpg(pgrp: std::os::raw::c_int, sig: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// 单条验证命令的 shell 调用规格。
///
/// Business Logic（为什么需要这个结构体）:
///     验证命令需要跨平台构造不同 shell 调用，同时测试要能稳定断言程序和参数。
///
/// Code Logic（这个结构体做什么）:
///     保存将要传给 tokio::process::Command 的 program 与 args。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellCommandSpec {
    program: String,
    args: Vec<String>,
}

/// 单条验证命令的执行结果。
///
/// Business Logic（为什么需要这个结构体）:
///     验证 evidence 需要同时展示命令退出状态、stdout/stderr 和是否截断，失败错误也复用同一份格式化输出。
///
/// Code Logic（这个结构体做什么）:
///     保存已按上限截断并转成 UTF-8 文本的 stdout/stderr、退出状态和截断标记。
struct VerificationCommandOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    truncated: bool,
}

/// 单个输出流的受限读取结果。
///
/// Business Logic（为什么需要这个结构体）:
///     stdout/stderr 需要边读边丢弃超出预算的内容，避免验证命令产生海量输出时撑爆内存或 evidence。
///
/// Code Logic（这个结构体做什么）:
///     保存当前流在共享预算内保留下来的字节，以及该流是否发生截断。
struct LimitedPipeOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Orchestrator 自动交付结果。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层需要知道交付后任务的最终状态，同时测试需要看到交付阶段是否执行。
///
/// Code Logic（这个结构体做什么）:
///     保存最终任务 DTO 和本次 delivery pipeline 追加的阶段标题列表。
#[derive(Debug, Clone)]
pub struct DeliverySummary {
    pub task: OrchestratorTaskDto,
    pub stages: Vec<String>,
}

/// verifier 专用验证命令报告。
///
/// Business Logic（为什么需要这个结构体）:
///     Phase8 中验证命令非零退出不是基础设施失败，而是 verifier Claude 审查任务完成度的输入。
///
/// Code Logic（这个结构体做什么）:
///     保存命令整体是否通过、evidence summary 短值和可落库的格式化输出内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationCommandReport {
    pub passed: bool,
    pub summary: String,
    pub content: String,
}

/// Orchestrator delivery 运行时依赖。
///
/// Business Logic（为什么需要这个 trait）:
///     生产环境需要复用 AppState，测试环境需要在不启动桌面事件循环的情况下验证真实 Git/SQLite 交付语义。
///
/// Code Logic（这个 trait 做什么）:
///     抽象 delivery pipeline 需要的全局配置、仓储与三个 Workbench 阶段动作；生产实现委托 Workbench helper，测试实现委托临时 Git repo。
#[allow(async_fn_in_trait)]
pub(crate) trait DeliveryContext: Sync {
    /// Business Logic（为什么需要这个函数）:
    ///     delivery 运行时策略已迁移为设备级全局配置，自动交付开关不能再读取 legacy 项目配置表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回当前 OrchestratorAutomationConfig；生产从 AppState.config 克隆，测试从可变 harness 配置克隆。
    fn orchestrator_config(&self) -> OrchestratorAutomationConfig;

    /// Business Logic（为什么需要这个函数）:
    ///     delivery pipeline 需要读取任务、配置、写 evidence 并推进任务状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 Orchestrator 仓储引用。
    fn orchestrator_repo(&self) -> &OrchestratorRepo;

    /// Business Logic（为什么需要这个函数）:
    ///     merge main 和 push main 阶段需要知道主工作区路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 Workbench 项目仓储引用。
    fn workbench_project_repo(&self) -> &WorkbenchProjectRepo;

    /// Business Logic（为什么需要这个函数）:
    ///     commit/push/merge task branch 都依赖 task.worktree_id 对应的 Workbench worktree 记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 Workbench worktree 仓储引用。
    fn workbench_worktree_repo(&self) -> &WorkbenchWorktreeRepo;

    /// Business Logic（为什么需要这个函数）:
    ///     full-auto delivery 第一阶段必须把 task worktree 的改动提交到任务分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用确定性提交信息提交 worktree 全部改动；无改动时应视为成功。
    async fn commit_task_worktree(
        &self,
        worktree_id: String,
        message: Option<String>,
    ) -> Result<(), AppError>;

    /// Business Logic（为什么需要这个函数）:
    ///     full-auto delivery 需要先把任务分支推到远端，确保 merge 前已有可审计备份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     推送 task worktree 当前分支，遵循 Workbench 的 upstream/origin 安全规则。
    async fn push_task_worktree(&self, worktree_id: String) -> Result<(), AppError>;

    /// Business Logic（为什么需要这个函数）:
    ///     full-auto delivery 需要把任务分支合并回主工作区，并在成功后清理任务 worktree。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 merge main 阶段，返回可写入 evidence 的阶段结果摘要。
    async fn merge_task_worktree_to_main(&self, worktree_id: String) -> Result<String, AppError>;
}

/// 生产环境 delivery context。
///
/// Business Logic（为什么需要这个结构体）:
///     Orchestrator 命令层需要把 AppState 交给通用 delivery pipeline，
///     同时保留 Workbench merge progress 事件。
///
/// Code Logic（这个结构体做什么）:
///     持有只读 AppState 引用，trait 方法直接委托现有 Workbench 本机 helper。
pub(crate) struct AppDeliveryContext<'a> {
    state: &'a AppState,
}

impl<'a> AppDeliveryContext<'a> {
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次完成验证后需要创建一次短生命周期 delivery context。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 AppState 引用，供 trait 方法复用。
    pub(crate) fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

impl DeliveryContext for AppDeliveryContext<'_> {
    /// Business Logic（为什么需要这个函数）:
    ///     生产 delivery 需要使用 Settings 自动化 tab 保存的全局 Orchestrator 运行偏好。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 AppState.config 读锁中克隆 orchestrator 配置，避免持锁跨 await。
    fn orchestrator_config(&self) -> OrchestratorAutomationConfig {
        self.state
            .config
            .read()
            .expect("config 读锁中毒")
            .orchestrator
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产 delivery 需要读写 Orchestrator 任务和 evidence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 AppState 中返回 OrchestratorRepo 引用。
    fn orchestrator_repo(&self) -> &OrchestratorRepo {
        self.state.orchestrator_repo.as_ref()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产 delivery 需要读取主 Workbench 项目路径来执行 merge 和 push main。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 AppState 中返回 WorkbenchProjectRepo 引用。
    fn workbench_project_repo(&self) -> &WorkbenchProjectRepo {
        self.state.workbench_project_repo.as_ref()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产 delivery 需要读取任务 worktree 元数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 AppState 中返回 WorkbenchWorktreeRepo 引用。
    fn workbench_worktree_repo(&self) -> &WorkbenchWorktreeRepo {
        self.state.workbench_worktree_repo.as_ref()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     full-auto delivery 第一阶段必须复用 Workbench commit 语义，保持用户手动提交和自动提交一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 local_commit_workbench_worktree，并丢弃刷新 DTO，只向 pipeline 暴露成功或错误。
    async fn commit_task_worktree(
        &self,
        worktree_id: String,
        message: Option<String>,
    ) -> Result<(), AppError> {
        local_commit_workbench_worktree(self.state, worktree_id, message)
            .await
            .map(|_| ())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付需要用 Workbench 既有远端选择规则推送任务分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 local_push_workbench_worktree，并丢弃刷新 DTO，只向 pipeline 暴露成功或错误。
    async fn push_task_worktree(&self, worktree_id: String) -> Result<(), AppError> {
        local_push_workbench_worktree(self.state, worktree_id)
            .await
            .map(|_| ())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付合并回主工作区时仍要给 Workbench 前端发送 merge progress。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 local_merge_workbench_worktree，并把返回 DTO 格式化为 evidence 文本。
    async fn merge_task_worktree_to_main(&self, worktree_id: String) -> Result<String, AppError> {
        let result = local_merge_workbench_worktree(self.state, worktree_id).await?;
        Ok(format!("{result:?}"))
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证成功后的 Delivering 任务应自动完成 commit、push 任务分支、merge main 和 push main，
///     让 full-auto 策略无需用户手动收尾。
///
/// Code Logic（这个函数做什么）:
///     校验任务状态与项目 full-auto flags；按顺序复用 Workbench 本机 helper 执行四个阶段；
///     每个阶段成功写 passed delivery evidence，失败写 failed evidence 并把任务置为 Blocked；
///     全局交付开关关闭时写 blocked evidence；全部成功后置 Done。
pub(crate) async fn deliver_task<C>(context: &C, task_id: &str) -> Result<DeliverySummary, AppError>
where
    C: DeliveryContext,
{
    let task = context.orchestrator_repo().get_task(task_id).await?;
    if task.status != OrchestratorTaskStatus::Delivering {
        return Err(AppError::generic(format!(
            "只有 Delivering 任务可以交付，当前状态为 {}",
            task.status.as_str()
        )));
    }
    let Some(_delivery_guard) = try_acquire_delivery_task_guard(&task.id)? else {
        let current = context.orchestrator_repo().get_task(&task.id).await?;
        return Ok(DeliverySummary {
            task: OrchestratorTaskDto::from(current),
            stages: Vec::new(),
        });
    };
    let mut stages = Vec::new();
    let config = context.orchestrator_config();

    if let Some(reason) = disabled_delivery_flag_reason(&config) {
        return block_delivery_task_with_summary(
            context.orchestrator_repo(),
            &task.id,
            &reason,
            "delivery config",
            "blocked",
            &format!("Full-auto delivery requires all delivery flags to be enabled.\n{reason}"),
            &mut stages,
        )
        .await;
    }

    let Some(worktree_id) = task.worktree_id.clone() else {
        return block_delivery_task(
            context.orchestrator_repo(),
            &task.id,
            "task is missing worktree_id",
            "delivery config",
            "Task is Delivering but has no worktree_id, so delivery cannot locate the task branch.",
            &mut stages,
        )
        .await;
    };
    let worktree = match context.workbench_worktree_repo().get(&worktree_id).await {
        Ok(Some(worktree)) => worktree,
        Ok(None) => {
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &format!("task worktree not found: {worktree_id}"),
                "delivery config",
                &format!("Task worktree not found: {worktree_id}"),
                &mut stages,
            )
            .await;
        }
        Err(err) => {
            let reason = format!("read task worktree failed: {err}");
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "delivery config",
                &reason,
                &mut stages,
            )
            .await;
        }
    };
    let project = match context
        .workbench_project_repo()
        .get(&worktree.project_id)
        .await
    {
        Ok(Some(project)) => project,
        Ok(None) => {
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &format!("workbench project not found: {}", worktree.project_id),
                "delivery config",
                &format!("Workbench project not found: {}", worktree.project_id),
                &mut stages,
            )
            .await;
        }
        Err(err) => {
            let reason = format!(
                "read workbench project failed: {}: {err}",
                worktree.project_id
            );
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "delivery config",
                &reason,
                &mut stages,
            )
            .await;
        }
    };
    let task_branch = worktree
        .branch
        .clone()
        .or_else(|| workbench_git::current_branch(Path::new(&worktree.path)))
        .unwrap_or_else(|| "unknown".to_string());
    let task_path = worktree.path.clone();
    let main_path = project.path.clone();

    let commit_message = format!("orchestrator: {}", task.title.trim());
    let before_head = match workbench_git::head_hash(Path::new(&task_path)) {
        Ok(head) => head,
        Err(err) => {
            let reason = format!("commit failed: read task HEAD failed: {err}");
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "commit",
                &reason,
                &mut stages,
            )
            .await;
        }
    };
    if let Err(err) = context
        .commit_task_worktree(worktree_id.clone(), Some(commit_message))
        .await
    {
        let reason = format!("commit failed: {err}");
        return block_delivery_task(
            context.orchestrator_repo(),
            &task.id,
            &reason,
            "commit",
            &reason,
            &mut stages,
        )
        .await;
    }
    let after_head = match workbench_git::head_hash(Path::new(&task_path)) {
        Ok(head) => head,
        Err(err) => {
            let reason = format!("commit failed: read task HEAD after commit failed: {err}");
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "commit",
                &reason,
                &mut stages,
            )
            .await;
        }
    };
    let commit_content = format_commit_evidence(before_head.as_deref(), after_head.as_deref());
    add_delivery_evidence(
        context.orchestrator_repo(),
        &task.id,
        "commit",
        "passed",
        &commit_content,
    )
    .await?;
    stages.push("commit".to_string());

    if let Some(summary) =
        stop_delivery_if_task_changed(context.orchestrator_repo(), &task.id, &stages).await?
    {
        return Ok(summary);
    }

    if let Err(err) = context.push_task_worktree(worktree_id.clone()).await {
        let reason = format!("push branch failed: {err}");
        return block_delivery_task(
            context.orchestrator_repo(),
            &task.id,
            &reason,
            "push branch",
            &format!("branch: {task_branch}\n{reason}"),
            &mut stages,
        )
        .await;
    }
    add_delivery_evidence(
        context.orchestrator_repo(),
        &task.id,
        "push branch",
        "passed",
        &format!("branch: {task_branch}\nTask branch pushed successfully."),
    )
    .await?;
    stages.push("push branch".to_string());

    if let Some(summary) =
        stop_delivery_if_task_changed(context.orchestrator_repo(), &task.id, &stages).await?
    {
        return Ok(summary);
    }

    let main_status = match workbench_git::status(Path::new(&main_path)) {
        Ok(status) => status,
        Err(err) => {
            let reason = format!("merge main failed: {err}");
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "merge main",
                &format!("main path: {main_path}\n{reason}"),
                &mut stages,
            )
            .await;
        }
    };
    if !main_status.clean {
        let reason = "merge main failed: main worktree is dirty; 主工作区有未提交改动，请先提交或清理后再合并";
        return block_delivery_task(
            context.orchestrator_repo(),
            &task.id,
            reason,
            "merge main",
            &format!("main path: {main_path}\n{reason}"),
            &mut stages,
        )
        .await;
    }

    if let Some(summary) =
        stop_delivery_if_task_changed(context.orchestrator_repo(), &task.id, &stages).await?
    {
        return Ok(summary);
    }

    match context
        .merge_task_worktree_to_main(worktree_id.clone())
        .await
    {
        Ok(result) => {
            add_delivery_evidence(
                context.orchestrator_repo(),
                &task.id,
                "merge main",
                "passed",
                &result,
            )
            .await?;
            stages.push("merge main".to_string());
        }
        Err(err) => {
            let reason = normalize_merge_failure_reason(&err);
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "merge main",
                &reason,
                &mut stages,
            )
            .await;
        }
    }

    if let Some(summary) =
        stop_delivery_if_task_changed(context.orchestrator_repo(), &task.id, &stages).await?
    {
        return Ok(summary);
    }

    match workbench_git::push_main_worktree_current_branch(Path::new(&main_path)) {
        Ok(branch) => {
            add_delivery_evidence(
                context.orchestrator_repo(),
                &task.id,
                "push main",
                "passed",
                &format!("branch: {branch}\nMain branch pushed successfully."),
            )
            .await?;
            stages.push("push main".to_string());
        }
        Err(err) => {
            let reason = format!("main merged locally but push main failed: {err}");
            return block_delivery_task(
                context.orchestrator_repo(),
                &task.id,
                &reason,
                "push main",
                &reason,
                &mut stages,
            )
            .await;
        }
    }

    let done = context
        .orchestrator_repo()
        .finish_task_done(&task.id)
        .await?;
    Ok(DeliverySummary {
        task: OrchestratorTaskDto::from(done),
        stages,
    })
}

/// Business Logic（为什么需要这个函数）:
///     Agent 声称完成后，系统需要在对应 worktree 中执行 Settings 全局验证命令，并把输出保存为 evidence。
///
/// Code Logic（这个函数做什么）:
///     逐条用平台 shell 在 cwd 中执行命令，成功时返回包含命令、stdout、stderr 的合并文本；
///     任一命令非零退出时返回包含失败命令和输出的 AppError。
#[cfg(test)]
async fn run_verification_commands(cwd: &Path, commands: &[String]) -> Result<String, AppError> {
    run_verification_commands_with_limits(
        cwd,
        commands,
        DEFAULT_VERIFICATION_TIMEOUT,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     verifier 需要读取完整验证输出；命令非零退出应进入审查上下文，而不是直接阻塞任务。
///
/// Code Logic（这个函数做什么）:
///     使用生产 timeout 和输出上限执行 verifier 专用验证命令，返回 passed/failed/skipped report；启动、读取、timeout 错误仍返回 Err。
pub async fn run_validation_commands_for_verifier(
    cwd: &Path,
    commands: &[String],
) -> Result<ValidationCommandReport, AppError> {
    run_validation_commands_for_verifier_with_limits(
        cwd,
        commands,
        DEFAULT_VERIFICATION_TIMEOUT,
        DEFAULT_MAX_OUTPUT_BYTES,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     单测需要用短 timeout 验证基础设施错误路径，同时生产代码继续使用安全默认限制。
///
/// Code Logic（这个函数做什么）:
///     逐条执行验证命令并累积格式化输出；非零退出只把 report.passed 置为 false，所有命令仍继续执行。
pub async fn run_validation_commands_for_verifier_with_limits(
    cwd: &Path,
    commands: &[String],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<ValidationCommandReport, AppError> {
    if commands.is_empty() {
        return Ok(ValidationCommandReport {
            passed: true,
            summary: "skipped".to_string(),
            content: "未配置验证命令，跳过验证。".to_string(),
        });
    }

    let mut combined = String::new();
    let mut passed = true;
    for command in commands {
        let output = run_shell_command(cwd, command, timeout, max_output_bytes).await?;
        let section = format_verification_output(command, &output);
        combined.push_str(&section);
        combined.push('\n');
        if !output.status.success() {
            passed = false;
        }
    }

    Ok(ValidationCommandReport {
        passed,
        summary: if passed { "passed" } else { "failed" }.to_string(),
        content: combined,
    })
}

/// Business Logic（为什么需要这个函数）:
///     测试和未来配置需要能用短 timeout/小输出上限验证行为，而生产入口仍使用安全默认值。
///
/// Code Logic（这个函数做什么）:
///     逐条执行验证命令；每条命令应用 timeout 和 stdout+stderr 总量截断，非零退出返回包含截断输出的 AppError。
#[cfg(test)]
async fn run_verification_commands_with_limits(
    cwd: &Path,
    commands: &[String],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<String, AppError> {
    let mut combined = String::new();
    for command in commands {
        let output = run_shell_command(cwd, command, timeout, max_output_bytes).await?;
        let section = format_verification_output(command, &output);
        combined.push_str(&section);
        combined.push('\n');
        if !output.status.success() {
            return Err(AppError::generic(format!(
                "验证命令失败: {command}\n{section}"
            )));
        }
    }
    Ok(combined)
}

/// Business Logic（为什么需要这个函数）:
///     用户配置的验证命令需要支持 shell 语法，如重定向、管道和环境变量展开。
///
/// Code Logic（这个函数做什么）:
///     macOS/Linux 优先使用 `$SHELL -lc`，空值回退 `sh -lc`；Windows 使用 `cmd /C`；
///     Unix/macOS 把 shell 放入独立进程组；子进程设置 kill_on_drop 兜底，stdout/stderr 由后台任务流式读取并共享总预算；
///     wait 由 timeout 包裹，超时终止进程树并给 reader 一个短 grace 后 abort。
async fn run_shell_command(
    cwd: &Path,
    command: &str,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<VerificationCommandOutput, AppError> {
    let shell_command = build_shell_command(command);
    let mut child = Command::new(&shell_command.program);
    child.args(&shell_command.args);
    child
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child.kill_on_drop(true);
    #[cfg(unix)]
    child.process_group(0);
    let mut process = child
        .spawn()
        .map_err(|err| AppError::generic(format!("启动验证命令失败: {command}: {err}")))?;
    let process_group_id = process.id();

    let stdout = process
        .stdout
        .take()
        .ok_or_else(|| AppError::generic(format!("捕获验证命令 stdout 失败: {command}")))?;
    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| AppError::generic(format!("捕获验证命令 stderr 失败: {command}")))?;
    let remaining_budget = Arc::new(Mutex::new(max_output_bytes));
    let stdout_task = spawn_limited_pipe_reader(stdout, remaining_budget.clone());
    let stderr_task = spawn_limited_pipe_reader(stderr, remaining_budget);

    let status = match tokio::time::timeout(timeout, process.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(err)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(AppError::generic(format!(
                "执行验证命令失败: {command}: {err}"
            )));
        }
        Err(_) => {
            terminate_shell_process_tree(&mut process, process_group_id).await;
            let _ = join_limited_pipe_output_with_grace(
                "stdout",
                stdout_task,
                PIPE_READER_JOIN_GRACE_TIMEOUT,
            )
            .await;
            let _ = join_limited_pipe_output_with_grace(
                "stderr",
                stderr_task,
                PIPE_READER_JOIN_GRACE_TIMEOUT,
            )
            .await;
            return Err(AppError::generic(format!(
                "验证命令超时: {command}（timeout={}秒）",
                timeout.as_secs_f64()
            )));
        }
    };

    let stdout_output = join_limited_pipe_output("stdout", stdout_task).await?;
    let stderr_output = join_limited_pipe_output("stderr", stderr_task).await?;
    let truncated = stdout_output.truncated || stderr_output.truncated;
    Ok(VerificationCommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout_output.bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_output.bytes).to_string(),
        truncated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     验证命令超时时，watch/dev server 等孙进程可能继续持有 stdout/stderr pipe，必须尽量终止整棵进程树。
///
/// Code Logic（这个函数做什么）:
///     Unix/macOS 先向独立进程组发送 SIGKILL，再对直接子进程执行 start_kill 兜底；所有平台都只等待短 grace 回收进程。
async fn terminate_shell_process_tree(process: &mut Child, process_group_id: Option<u32>) {
    #[cfg(unix)]
    if let Some(process_group_id) = process_group_id {
        let _ = kill_unix_process_group(process_group_id);
    }
    #[cfg(not(unix))]
    let _ = process_group_id;

    let _ = process.start_kill();
    let _ = tokio::time::timeout(PROCESS_REAP_GRACE_TIMEOUT, process.wait()).await;
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS 上验证 shell 使用独立进程组，超时时需要用进程组信号覆盖后台子进程。
///
/// Code Logic（这个函数做什么）:
///     把 tokio 返回的子进程 pid 转成平台 c_int，并调用 POSIX killpg 发送 SIGKILL。
#[cfg(unix)]
fn kill_unix_process_group(process_group_id: u32) -> Result<(), std::io::Error> {
    let pgid: std::os::raw::c_int = process_group_id.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "process group id out of range",
        )
    })?;
    let result = unsafe { killpg(pgid, SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Business Logic（为什么需要这个函数）:
///     stdout 与 stderr 必须并发读取，避免某个 pipe 写满后阻塞子进程，导致验证命令假死。
///
/// Code Logic（这个函数做什么）:
///     为任意 AsyncRead pipe 启动 tokio 任务，调用 read_limited_pipe 按共享预算保存输出。
fn spawn_limited_pipe_reader<R>(
    reader: R,
    remaining_budget: Arc<Mutex<usize>>,
) -> JoinHandle<Result<LimitedPipeOutput, std::io::Error>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(read_limited_pipe(reader, remaining_budget))
}

/// Business Logic（为什么需要这个函数）:
///     验证流程需要把输出读取任务的 panic/IO 错误转换成 AppError，避免后台 join 错误泄漏为不清晰失败。
///
/// Code Logic（这个函数做什么）:
///     await JoinHandle，分别处理任务 join 失败和 pipe 读取 IO 失败，并补充 stdout/stderr 名称。
async fn join_limited_pipe_output(
    stream_name: &str,
    task: JoinHandle<Result<LimitedPipeOutput, std::io::Error>>,
) -> Result<LimitedPipeOutput, AppError> {
    task.await
        .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 任务失败: {err}")))?
        .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 失败: {err}")))
}

/// Business Logic（为什么需要这个函数）:
///     超时分支不能无限等待 stdout/stderr reader；即便管道仍被孙进程持有，也要快速让任务退出 Verifying。
///
/// Code Logic（这个函数做什么）:
///     对 JoinHandle 设置短 grace timeout；reader 正常结束则复用 join 错误映射，grace 超时则 abort 任务并返回 AppError。
async fn join_limited_pipe_output_with_grace(
    stream_name: &str,
    mut task: JoinHandle<Result<LimitedPipeOutput, std::io::Error>>,
    grace_timeout: Duration,
) -> Result<LimitedPipeOutput, AppError> {
    match tokio::time::timeout(grace_timeout, &mut task).await {
        Ok(result) => result
            .map_err(|err| {
                AppError::generic(format!("读取验证命令 {stream_name} 任务失败: {err}"))
            })?
            .map_err(|err| AppError::generic(format!("读取验证命令 {stream_name} 失败: {err}"))),
        Err(_) => {
            task.abort();
            Err(AppError::generic(format!(
                "读取验证命令 {stream_name} 超时"
            )))
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证命令输出需要在读取过程中执行预算限制，而不是等命令结束后再截断完整缓冲。
///
/// Code Logic（这个函数做什么）:
///     循环读取 pipe；每个 chunk 只把共享剩余预算内的字节追加到结果，超出部分继续读取但丢弃并标记 truncated。
async fn read_limited_pipe<R>(
    mut reader: R,
    remaining_budget: Arc<Mutex<usize>>,
) -> Result<LimitedPipeOutput, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let mut remaining = remaining_budget.lock().await;
        let keep = (*remaining).min(read);
        if keep > 0 {
            bytes.extend_from_slice(&buffer[..keep]);
            *remaining -= keep;
        }
        if keep < read {
            truncated = true;
        }
    }

    Ok(LimitedPipeOutput { bytes, truncated })
}

/// Business Logic（为什么需要这个函数）:
///     验证 evidence 和失败错误必须使用同一种文本格式，确保成功、失败、截断时前端展示一致。
///
/// Code Logic（这个函数做什么）:
///     格式化命令、exit、stdout、stderr；若输出被截断，在段落末尾追加固定 marker。
fn format_verification_output(command: &str, output: &VerificationCommandOutput) -> String {
    let mut section = format!(
        "$ {command}\nexit: {}\nstdout:\n{}\nstderr:\n{}\n",
        output.status, output.stdout, output.stderr
    );
    if output.truncated {
        section.push_str(OUTPUT_TRUNCATED_MARKER);
        section.push('\n');
    }
    section
}

/// Business Logic（为什么需要这个函数）:
///     Windows 用户的验证命令应沿用系统 cmd 语义，不受 Unix 用户 shell 逻辑影响。
///
/// Code Logic（这个函数做什么）:
///     构造 `cmd /C <command>` 的执行规格。
#[cfg(windows)]
fn build_shell_command(command: &str) -> ShellCommandSpec {
    ShellCommandSpec {
        program: "cmd".to_string(),
        args: vec!["/C".to_string(), command.to_string()],
    }
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS 验证命令应复用用户登录 shell，以便加载用户 shell 可解析的配置和语法。
///
/// Code Logic（这个函数做什么）:
///     读取 SHELL 环境变量并交给纯 helper 归一化，构造 `<shell> -lc <command>`。
#[cfg(not(windows))]
fn build_shell_command(command: &str) -> ShellCommandSpec {
    build_shell_command_with_shell(command, std::env::var("SHELL").ok().as_deref())
}

/// Business Logic（为什么需要这个函数）:
///     Unix/macOS shell 选择需要可测试，避免单测依赖当前开发机真实 SHELL。
///
/// Code Logic（这个函数做什么）:
///     对传入 shell 环境值 trim；非空使用该 shell，否则回退 `sh`，参数固定为 `-lc <command>`。
#[cfg(not(windows))]
fn build_shell_command_with_shell(command: &str, shell_env: Option<&str>) -> ShellCommandSpec {
    let program = shell_env
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("sh")
        .to_string();
    ShellCommandSpec {
        program,
        args: vec!["-lc".to_string(), command.to_string()],
    }
}

/// Business Logic（为什么需要这个函数）:
///     全局配置允许单独关闭提交、推送、合并或主分支推送，但 full-auto delivery 要求四项全开。
///
/// Code Logic（这个函数做什么）:
///     收集关闭的 delivery flags；全部开启返回 None，否则返回可写入 blocked_reason 的英文说明。
fn disabled_delivery_flag_reason(config: &OrchestratorAutomationConfig) -> Option<String> {
    let mut disabled = Vec::new();
    if !config.auto_commit {
        disabled.push("auto_commit");
    }
    if !config.auto_push_task_branch {
        disabled.push("auto_push_task_branch");
    }
    if !config.auto_merge_to_main {
        disabled.push("auto_merge_to_main");
    }
    if !config.auto_push_main {
        disabled.push("auto_push_main");
    }
    if disabled.is_empty() {
        None
    } else {
        Some(format!(
            "full-auto delivery flags disabled: {}",
            disabled.join(", ")
        ))
    }
}

/// Business Logic（为什么需要这个函数）:
///     delivery evidence 需要让用户分辨“本次有新提交”和“没有改动但当前 HEAD 已可交付”。
///
/// Code Logic（这个函数做什么）:
///     对比 commit 前后 HEAD；相同或缺失时写 no changes/current head，变化时写新 commit hash。
fn format_commit_evidence(before_head: Option<&str>, after_head: Option<&str>) -> String {
    match (before_head, after_head) {
        (Some(before), Some(after)) if before != after => {
            format!("commit: {after}\nprevious: {before}")
        }
        (_, Some(after)) => format!("no changes to commit\ncurrent head: {after}"),
        _ => "no changes to commit\ncurrent head: unavailable".to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     Workbench merge 的中文 dirty main 错误需要在 Orchestrator blocked_reason 中归一为计划指定英文串，
///     方便测试、日志和跨语言 UI 稳定识别。
///
/// Code Logic（这个函数做什么）:
///     识别主工作区 dirty 文案并前置 `main worktree is dirty`；其它错误保留 merge main failed 前缀。
fn normalize_merge_failure_reason(err: &AppError) -> String {
    let message = err.to_string();
    if message.contains("主工作区有未提交改动") {
        format!("merge main failed: main worktree is dirty; {message}")
    } else {
        format!("merge main failed: {message}")
    }
}

/// Business Logic（为什么需要这个函数）:
///     每个交付阶段的结果都要进入 evidence 列表，前端无需理解内部状态机也能展示审计轨迹。
///
/// Code Logic（这个函数做什么）:
///     使用固定 kind=`delivery` 追加 evidence，summary 只传 passed/failed/blocked/skipped 这类前端已支持短值。
async fn add_delivery_evidence(
    orchestrator_repo: &OrchestratorRepo,
    task_id: &str,
    title: &str,
    summary: &str,
    content: &str,
) -> Result<(), AppError> {
    orchestrator_repo
        .add_evidence(task_id, EVIDENCE_KIND_DELIVERY, title, summary, content)
        .await
}

/// Business Logic（为什么需要这个函数）:
///     自动交付任一阶段失败都不能让任务停在 Delivering；用户需要看到 Blocked 状态和失败 evidence。
///
/// Code Logic（这个函数做什么）:
///     先尝试在当前仍为 Delivering 时原子写 Blocked；命中后追加 failed delivery evidence。
///     如果任务已被用户终止或其它流程推进，则直接返回当前任务，不覆盖终止状态。
async fn block_delivery_task(
    orchestrator_repo: &OrchestratorRepo,
    task_id: &str,
    reason: &str,
    evidence_title: &str,
    evidence_content: &str,
    stages: &mut Vec<String>,
) -> Result<DeliverySummary, AppError> {
    block_delivery_task_with_summary(
        orchestrator_repo,
        task_id,
        reason,
        evidence_title,
        "failed",
        evidence_content,
        stages,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     交付失败和交付开关关闭都要阻塞任务，但 evidence summary 语义不同：
///     真实阶段错误是 failed，策略 gate 关闭是 blocked。
///
/// Code Logic（这个函数做什么）:
///     在任务仍为 Delivering 时写 Blocked；命中后按调用方指定 summary 追加 delivery evidence。
async fn block_delivery_task_with_summary(
    orchestrator_repo: &OrchestratorRepo,
    task_id: &str,
    reason: &str,
    evidence_title: &str,
    evidence_summary: &str,
    evidence_content: &str,
    stages: &mut Vec<String>,
) -> Result<DeliverySummary, AppError> {
    let task = orchestrator_repo
        .block_task_if_delivering(task_id, reason)
        .await?;
    if task.status == OrchestratorTaskStatus::Blocked
        && task.blocked_reason.as_deref() == Some(reason)
    {
        add_delivery_evidence(
            orchestrator_repo,
            task_id,
            evidence_title,
            evidence_summary,
            evidence_content,
        )
        .await?;
        stages.push(evidence_title.to_string());
    }
    Ok(DeliverySummary {
        task: OrchestratorTaskDto::from(task),
        stages: stages.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorAutomationConfig;
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::storage::{WorkbenchProjectRepo, WorkbenchWorktreeRepo};
    use crate::workbench::models::{WorkbenchProjectRow, WorkbenchWorktreeRow};
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
    use std::fs;
    use std::path::Path;
    use std::process::Command as StdCommand;
    use std::str::FromStr;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    };
    use tokio::sync::Notify;

    static DELIVERY_TEST_TASK_COUNTER: AtomicUsize = AtomicUsize::new(1);

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令成功时需要返回 stdout/stderr 合并输出，作为任务 evidence 展示给用户。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在临时目录中运行一条输出命令，断言结果包含命令文本和 stdout。
    #[tokio::test]
    async fn successful_command_returns_combined_output() {
        let dir = tempfile::tempdir().expect("tempdir");

        let output = run_verification_commands(dir.path(), &["printf success".to_string()])
            .await
            .expect("verification output");

        assert!(output.contains("$ printf success"));
        assert!(output.contains("stdout"));
        assert!(output.contains("success"));
        assert!(output.contains("stderr"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令失败时必须把失败命令与输出放进错误，方便 blocked UI 告知用户原因。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行一条非零退出命令，断言错误消息包含命令文本与 stderr 输出。
    #[tokio::test]
    async fn failing_command_error_contains_command_and_output() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error =
            run_verification_commands(dir.path(), &["printf failure >&2; exit 7".to_string()])
                .await
                .expect_err("verification should fail");
        let message = error.to_string();

        assert!(message.contains("printf failure"));
        assert!(message.contains("failure"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase8 中验证命令非零退出是 verifier Claude 的业务输入，不能直接作为基础设施错误阻塞任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 verifier 专用验证 helper 执行非零命令，断言返回 failed report 而不是 Err，并保留命令输出。
    #[tokio::test]
    async fn verifier_validation_report_treats_nonzero_exit_as_failed_report() {
        let dir = tempfile::tempdir().expect("tempdir");

        let report = run_validation_commands_for_verifier_with_limits(
            dir.path(),
            &["printf failure >&2; exit 7".to_string()],
            std::time::Duration::from_secs(5),
            4096,
        )
        .await
        .expect("nonzero exit should be report input");

        assert!(!report.passed);
        assert_eq!(report.summary, "failed");
        assert!(report.content.contains("printf failure"));
        assert!(report.content.contains("exit"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     空验证命令仍要产生可审计 skipped evidence，并交给 verifier 结合 diff 判断任务是否满足目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 verifier 专用验证 helper 的空命令分支，断言 report 为 passed/skipped 且内容说明跳过。
    #[tokio::test]
    async fn verifier_validation_report_skips_empty_commands() {
        let dir = tempfile::tempdir().expect("tempdir");

        let report = run_validation_commands_for_verifier(dir.path(), &[])
            .await
            .expect("empty commands");

        assert!(report.passed);
        assert_eq!(report.summary, "skipped");
        assert!(report.content.contains("未配置验证命令"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令超时属于验证基础设施失败，系统不能把不完整输出交给 verifier 继续裁决。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用短 timeout 执行阻塞命令，断言 verifier 专用 helper 仍返回 Err。
    #[tokio::test]
    async fn verifier_validation_report_keeps_timeout_as_error() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error = run_validation_commands_for_verifier_with_limits(
            dir.path(),
            &[sleep_command()],
            std::time::Duration::from_millis(50),
            1024,
        )
        .await
        .expect_err("timeout should be infrastructure error");
        let message = error.to_string();

        assert!(message.contains("timeout") || message.contains("超时"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令可能卡住，任务不能无限停留在 Verifying，超时需要终止子进程并返回可展示错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用极短 timeout 执行跨平台 sleep 命令，断言错误包含原命令和 timeout 信息。
    #[tokio::test]
    async fn verification_command_timeout_returns_error_with_command_and_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let error = run_verification_commands_with_limits(
            dir.path(),
            &[sleep_command()],
            std::time::Duration::from_millis(50),
            1024,
        )
        .await
        .expect_err("sleep should timeout");
        let message = error.to_string();

        assert!(message.contains("timeout") || message.contains("超时"));
        assert!(message.contains("sleep") || message.contains("ping"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令超时时，即使命令启动的后台子进程继续持有 stdout/stderr pipe，任务也不能卡在 Verifying。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Unix/macOS 上启动继承 pipe 的后台 sleep，并用外层短 timeout 断言验证 helper 自身会快速返回超时错误。
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_even_when_child_process_keeps_pipe_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = "sleep 5 & echo child-started; wait".to_string();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(800),
            run_verification_commands_with_limits(
                dir.path(),
                std::slice::from_ref(&command),
                std::time::Duration::from_millis(50),
                1024,
            ),
        )
        .await;

        let error = result
            .expect("timeout branch should not wait for inherited pipe EOF")
            .expect_err("verification should return a timeout error");
        let message = error.to_string();

        assert!(message.contains("timeout") || message.contains("超时"));
        assert!(message.contains(&command));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证命令可能输出大量日志，evidence 需要有大小上限，避免 SQLite 和前端详情页被巨量文本拖垮。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行跨平台大输出命令并设置小输出上限，断言成功输出被截断且带 truncated 标记。
    #[tokio::test]
    async fn verification_command_output_is_truncated_with_marker() {
        let dir = tempfile::tempdir().expect("tempdir");

        let output = run_verification_commands_with_limits(
            dir.path(),
            &[large_output_command()],
            std::time::Duration::from_secs(5),
            64,
        )
        .await
        .expect("large output command should succeed");

        assert!(output.contains("[output truncated]"));
        assert!(output.len() < 512);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     超时测试需要一条不会依赖 Unix 工具的 Windows 等价命令，保证 CI 多平台稳定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Windows 用 ping 本机延迟，Unix/macOS 用 sleep，返回 shell 可执行字符串。
    #[cfg(test)]
    fn sleep_command() -> String {
        if cfg!(windows) {
            "ping 127.0.0.1 -n 3 >NUL".to_string()
        } else {
            "sleep 2".to_string()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     输出截断测试需要稳定制造超过上限的 stdout，且不依赖项目外部文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Windows 用 powershell 输出重复字符，Unix/macOS 用 yes+head 生成有限大输出。
    #[cfg(test)]
    fn large_output_command() -> String {
        if cfg!(windows) {
            "powershell -NoProfile -Command \"Write-Output ('x' * 2048)\"".to_string()
        } else {
            "yes x | head -n 2048".to_string()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Unix/macOS 用户常在 zsh/bash/fish 中配置项目验证所需环境，验证命令应优先复用 `$SHELL`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过纯 helper 注入 shell 环境值，断言构造出的命令使用 trim 后的用户 shell 和 `-lc`。
    #[cfg(not(windows))]
    #[test]
    fn unix_shell_command_prefers_user_shell_env() {
        let command = build_shell_command_with_shell("cargo test", Some("  /bin/zsh  "));

        assert_eq!(command.program, "/bin/zsh");
        assert_eq!(
            command.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户 shell 环境缺失或为空时仍需能运行验证命令，避免后台环境不完整导致验证入口不可用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过纯 helper 注入空白和缺失 shell 值，断言 Unix/macOS 回退到 `sh -lc`。
    #[cfg(not(windows))]
    #[test]
    fn unix_shell_command_falls_back_to_sh_when_shell_env_is_blank_or_missing() {
        let blank = build_shell_command_with_shell("cargo test", Some("  "));
        let missing = build_shell_command_with_shell("cargo test", None);

        assert_eq!(blank.program, "sh");
        assert_eq!(
            blank.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
        assert_eq!(missing.program, "sh");
        assert_eq!(
            missing.args,
            vec!["-lc".to_string(), "cargo test".to_string()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Windows 仍应使用 cmd 执行项目验证命令，避免 Unix shell 选择逻辑影响 Windows 用户。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Windows 条件编译下断言 shell 命令保持 `cmd /C`。
    #[cfg(windows)]
    #[test]
    fn windows_shell_command_uses_cmd_c() {
        let command = build_shell_command("cargo test");

        assert_eq!(command.program, "cmd");
        assert_eq!(
            command.args,
            vec!["/C".to_string(), "cargo test".to_string()]
        );
    }

    /// Orchestrator 交付测试夹具。
    ///
    /// Business Logic（为什么需要这个结构体）:
    ///     自动交付需要同时访问 SQLite 与真实 Git 仓库；测试必须隔离这些资源，避免污染真实用户项目。
    ///
    /// Code Logic（这个结构体做什么）:
    ///     持有最小 Orchestrator/Workbench 仓储、SQLite pool 和可控测试钩子，并实现 DeliveryContext 供 deliver_task 调用。
    #[derive(Clone)]
    struct DeliveryTestHarness {
        pool: SqlitePool,
        orchestrator_repo: Arc<OrchestratorRepo>,
        workbench_project_repo: Arc<WorkbenchProjectRepo>,
        workbench_worktree_repo: Arc<WorkbenchWorktreeRepo>,
        controls: Arc<DeliveryTestControls>,
        orchestrator_config: Arc<StdMutex<OrchestratorAutomationConfig>>,
    }

    impl DeliveryTestHarness {
        /// Business Logic（为什么需要这个函数）:
        ///     delivery Phase 3 运行时读取全局 Orchestrator 配置，单测需要按 case 临时关闭不同交付开关。
        ///
        /// Code Logic（这个函数做什么）:
        ///     覆盖测试 harness 中保存的 OrchestratorAutomationConfig。
        fn set_orchestrator_config(&self, config: OrchestratorAutomationConfig) {
            *self
                .orchestrator_config
                .lock()
                .expect("orchestrator config lock") = config;
        }
    }

    /// Orchestrator 交付测试控制钩子。
    ///
    /// Business Logic（为什么需要这个结构体）:
    ///     并发交付与用户终止都发生在 delivery pipeline 中途，测试需要可控地暂停阶段或模拟用户 Abort。
    ///
    /// Code Logic（这个结构体做什么）:
    ///     用原子计数记录 Git side effect 调用次数，用 Notify 暂停首个 commit，并保存需要在阶段中模拟 Aborted 的任务 id。
    #[derive(Default)]
    struct DeliveryTestControls {
        pause_next_commit: AtomicBool,
        commit_started: Notify,
        release_commit: Notify,
        commit_calls: AtomicUsize,
        push_calls: AtomicUsize,
        merge_calls: AtomicUsize,
        abort_after_merge_task_id: StdMutex<Option<String>>,
        abort_before_push_error_task_id: StdMutex<Option<String>>,
    }

    impl DeliveryContext for DeliveryTestHarness {
        /// Business Logic（为什么需要这个函数）:
        ///     delivery 单测需要按 case 控制全局 Orchestrator 交付开关。
        ///
        /// Code Logic（这个函数做什么）:
        ///     从 harness 的 Mutex 中克隆当前测试配置。
        fn orchestrator_config(&self) -> OrchestratorAutomationConfig {
            self.orchestrator_config
                .lock()
                .expect("orchestrator config lock")
                .clone()
        }

        /// Business Logic（为什么需要这个函数）:
        ///     delivery 单测需要读写 Orchestrator 任务和 evidence。
        ///
        /// Code Logic（这个函数做什么）:
        ///     返回内存 SQLite 对应的 OrchestratorRepo 引用。
        fn orchestrator_repo(&self) -> &OrchestratorRepo {
            self.orchestrator_repo.as_ref()
        }

        /// Business Logic（为什么需要这个函数）:
        ///     delivery 单测需要读取临时 Git main 工作区路径。
        ///
        /// Code Logic（这个函数做什么）:
        ///     返回内存 SQLite 对应的 WorkbenchProjectRepo 引用。
        fn workbench_project_repo(&self) -> &WorkbenchProjectRepo {
            self.workbench_project_repo.as_ref()
        }

        /// Business Logic（为什么需要这个函数）:
        ///     delivery 单测需要读取临时 task worktree 元数据。
        ///
        /// Code Logic（这个函数做什么）:
        ///     返回内存 SQLite 对应的 WorkbenchWorktreeRepo 引用。
        fn workbench_worktree_repo(&self) -> &WorkbenchWorktreeRepo {
            self.workbench_worktree_repo.as_ref()
        }

        /// Business Logic（为什么需要这个函数）:
        ///     单测要验证 delivery commit 阶段真实产生 Git 提交，但不能依赖 Claude CLI 生成消息。
        ///
        /// Code Logic（这个函数做什么）:
        ///     从 worktree row 取路径，使用传入的手写 message 执行 `git add -A && git commit`；无改动视为成功。
        async fn commit_task_worktree(
            &self,
            worktree_id: String,
            message: Option<String>,
        ) -> Result<(), AppError> {
            self.controls.commit_calls.fetch_add(1, Ordering::SeqCst);
            if self
                .controls
                .pause_next_commit
                .swap(false, Ordering::SeqCst)
            {
                self.controls.commit_started.notify_waiters();
                self.controls.release_commit.notified().await;
            }
            let row = self
                .workbench_worktree_repo
                .get(&worktree_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
            let message = message
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::generic("测试交付缺少 commit message"))?;
            workbench_git::commit_all(Path::new(&row.path), message)?;
            Ok(())
        }

        /// Business Logic（为什么需要这个函数）:
        ///     单测要验证任务分支会被真实推送到 bare origin。
        ///
        /// Code Logic（这个函数做什么）:
        ///     从 worktree row 取分支并调用 workbench_git::push_branch。
        async fn push_task_worktree(&self, worktree_id: String) -> Result<(), AppError> {
            self.controls.push_calls.fetch_add(1, Ordering::SeqCst);
            let abort_task_id = self
                .controls
                .abort_before_push_error_task_id
                .lock()
                .expect("abort before push lock")
                .take();
            if let Some(task_id) = abort_task_id {
                self.orchestrator_repo
                    .set_task_status(&task_id, OrchestratorTaskStatus::Aborted, None)
                    .await?;
                return Err(AppError::generic("simulated push failure after abort"));
            }
            let row = self
                .workbench_worktree_repo
                .get(&worktree_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
            let branch = row
                .branch
                .clone()
                .or_else(|| workbench_git::current_branch(Path::new(&row.path)))
                .ok_or_else(|| AppError::generic("当前 worktree 没有可推送的分支"))?;
            workbench_git::push_branch(Path::new(&row.path), &branch)?;
            Ok(())
        }

        /// Business Logic（为什么需要这个函数）:
        ///     单测要验证任务分支能真实 merge 到 main，并在成功后清理 task worktree 元数据。
        ///
        /// Code Logic（这个函数做什么）:
        ///     检查源/主工作区干净状态，执行 git merge，删除临时 worktree 与已合并本地分支，再删除 SQLite row。
        async fn merge_task_worktree_to_main(
            &self,
            worktree_id: String,
        ) -> Result<String, AppError> {
            self.controls.merge_calls.fetch_add(1, Ordering::SeqCst);
            let row = self
                .workbench_worktree_repo
                .get(&worktree_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
            if row.is_main {
                return Err(AppError::generic("主工作区不需要合并到自己"));
            }
            let project = self
                .workbench_project_repo
                .get(&row.project_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
            let source_status = workbench_git::status(Path::new(&row.path))?;
            if !source_status.clean {
                return Err(AppError::generic(
                    "源 worktree 有未提交改动，请先提交或清理后再合并",
                ));
            }
            let branch = row
                .branch
                .clone()
                .or(source_status.branch)
                .ok_or_else(|| AppError::generic("当前 worktree 没有可合并的分支"))?;
            let main_path = Path::new(&project.path);
            let main_status = workbench_git::status(main_path)?;
            if !main_status.clean {
                return Err(AppError::generic(
                    "主工作区有未提交改动，请先提交或清理后再合并",
                ));
            }
            let outcome = workbench_git::merge_branch(main_path, &branch)?;
            if outcome == workbench_git::MergeBranchOutcome::Conflicted {
                return Err(AppError::generic("测试交付不处理 merge 冲突"));
            }
            let repo_root = workbench_git::repo_root(main_path)?;
            workbench_git::remove_worktree(Path::new(&repo_root), Path::new(&row.path), false)?;
            if let Some(branch) = row.branch.as_deref() {
                workbench_git::delete_local_branch_if_merged(
                    Path::new(&repo_root),
                    branch,
                    "HEAD",
                )?;
            }
            self.workbench_worktree_repo.delete(&row.id).await?;
            let abort_task_id = self
                .controls
                .abort_after_merge_task_id
                .lock()
                .expect("abort after merge lock")
                .take();
            if let Some(task_id) = abort_task_id {
                self.orchestrator_repo
                    .set_task_status(&task_id, OrchestratorTaskStatus::Aborted, None)
                    .await?;
            }
            Ok(format!("merge outcome: {outcome:?}"))
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     delivery 单测需要真实仓储，但不能连接用户真实 `~/.cc-partner/data.db`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建内存 SQLite、必要 Workbench/Orchestrator schema 和最小 DeliveryContext。
    async fn setup_delivery_harness() -> DeliveryTestHarness {
        let pool = setup_delivery_pool().await;
        let orchestrator_repo = Arc::new(OrchestratorRepo::new(pool.clone()));
        let workbench_project_repo = Arc::new(WorkbenchProjectRepo::new(pool.clone()));
        let workbench_worktree_repo = Arc::new(WorkbenchWorktreeRepo::new(pool.clone()));
        DeliveryTestHarness {
            pool,
            orchestrator_repo,
            workbench_project_repo,
            workbench_worktree_repo,
            controls: Arc::new(DeliveryTestControls::default()),
            orchestrator_config: Arc::new(StdMutex::new(OrchestratorAutomationConfig::default())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付测试只需要 Orchestrator 与 Workbench 的最小表集合，应避免加载完整应用数据库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接内存 SQLite，初始化 Orchestrator schema，并补建项目、worktree 和 session 表。
    async fn setup_delivery_pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite options")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("sqlite pool");
        OrchestratorRepo::init_schema(&pool)
            .await
            .expect("orchestrator schema");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("workbench projects schema");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_worktrees (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT, \
             base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("workbench worktrees schema");
        pool
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付必须在真实 Git 语义下验证 push、merge 和 main push，而不是 mock 命令输出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 bare origin、clone 到 main 工作区、初始化 main 提交，并创建任务 worktree 分支。
    fn setup_git_delivery_repo() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let origin = dir.path().join("origin.git");
        let repo = dir.path().join("repo");
        let task_worktree = dir.path().join("task-worktree");
        git(
            dir.path(),
            &["init", "--bare", origin.to_string_lossy().as_ref()],
        );
        git(
            dir.path(),
            &[
                "clone",
                origin.to_string_lossy().as_ref(),
                repo.to_string_lossy().as_ref(),
            ],
        );
        configure_git_identity(&repo);
        git(&repo, &["checkout", "-b", "main"]);
        fs::write(repo.join("README.md"), "initial\n").expect("write readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "initial"]);
        git(&repo, &["push", "-u", "origin", "main"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "agent/task-7",
                task_worktree.to_string_lossy().as_ref(),
                "main",
            ],
        );
        (dir, origin, repo, task_worktree)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Git commit 在测试环境中不能依赖开发者全局 user.name/user.email。
    ///
    /// Code Logic（这个函数做什么）:
    ///     为测试仓库写入 local Git 身份。
    fn configure_git_identity(repo: &Path) {
        git(repo, &["config", "user.email", "delivery-test@example.com"]);
        git(repo, &["config", "user.name", "Delivery Test"]);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Git 集成测试失败时需要直接暴露完整 stdout/stderr，便于定位临时仓库状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 cwd 下执行系统 git；非零退出 panic 并打印命令与输出。
    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        if !output.status.success() {
            panic!(
                "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     delivery pipeline 依赖 Workbench 项目、任务 worktree 和 Orchestrator task 三类记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将临时 Git repo 的 main/worktree 路径写入 Workbench 仓储，并创建 Delivering 任务。
    async fn insert_delivery_task(
        harness: &DeliveryTestHarness,
        repo: &Path,
        task_worktree: &Path,
    ) -> String {
        let project_id = "project-delivery";
        let worktree_id = "worktree-delivery";
        let task_id = format!(
            "task-delivery-{}",
            DELIVERY_TEST_TASK_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let now = Utc::now().to_rfc3339();
        harness
            .workbench_project_repo
            .upsert(&WorkbenchProjectRow {
                id: project_id.to_string(),
                name: "Delivery Repo".to_string(),
                kind: "local".to_string(),
                device_id: "device-test".to_string(),
                device_name: "Delivery Test".to_string(),
                path: repo.to_string_lossy().to_string(),
                last_opened_at: now.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .expect("insert project");
        harness
            .workbench_worktree_repo
            .upsert(&WorkbenchWorktreeRow {
                id: worktree_id.to_string(),
                project_id: project_id.to_string(),
                name: "agent/task-7".to_string(),
                branch: Some("agent/task-7".to_string()),
                base_branch: Some("main".to_string()),
                path: task_worktree.to_string_lossy().to_string(),
                is_main: false,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .expect("insert worktree");
        harness
            .orchestrator_repo
            .create_task(&OrchestratorTaskRow {
                id: task_id.to_string(),
                project_id: project_id.to_string(),
                title: "Task 7 delivery".to_string(),
                goal: "Deliver task automatically".to_string(),
                acceptance_criteria: "origin main contains task file".to_string(),
                status: OrchestratorTaskStatus::Delivering,
                priority: 0,
                branch_name: Some("agent/task-7".to_string()),
                worktree_id: Some(worktree_id.to_string()),
                session_id: None,
                prepare_claim_token: None,
                blocked_reason: None,
                attempt: 0,
                created_at: now.clone(),
                updated_at: now,
                started_at: None,
                finished_at: None,
                ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Delivering)
            })
            .await
            .expect("insert task");
        task_id
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Full-auto delivery 必须完成 commit、push task branch、merge main 和 push main，用户无需手动收尾。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在临时 bare origin + local clone + task worktree 上运行 deliver_task，断言远端任务分支和 main 都包含改动，
    ///     任务状态为 Done，并写入四个交付阶段 evidence。
    #[tokio::test]
    async fn full_delivery_pushes_task_branch_and_main() {
        let harness = setup_delivery_harness().await;
        let (_dir, origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(task_worktree.join("task.txt"), "delivered by task 7\n")
            .expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("deliver task");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Done);
        assert_eq!(
            delivered.stages,
            vec!["commit", "push branch", "merge main", "push main"]
        );
        git(
            &origin,
            &["rev-parse", "--verify", "refs/heads/agent/task-7"],
        );
        let main_file = git(&origin, &["show", "refs/heads/main:task.txt"]);
        assert!(main_file.contains("delivered by task 7"));
        let evidence = harness
            .orchestrator_repo
            .list_evidence(&task_id)
            .await
            .expect("delivery evidence");
        let joined = evidence
            .iter()
            .map(|item| format!("{} {}", item.title, item.content))
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_lowercase();
        assert!(joined.contains("commit"));
        assert!(joined.contains("push branch"));
        assert!(joined.contains("merge main"));
        assert!(joined.contains("push main"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     同一个 Delivering 任务被重复点击或并发触发时，只能有一个 delivery pipeline 执行 Git side effect。
    ///
    /// Code Logic（这个函数做什么）:
    ///     暂停首个 commit 阶段并在暂停期间发起第二次 deliver_task，断言第二次快速返回且 commit/push/merge 只执行一次。
    #[tokio::test]
    async fn duplicate_delivery_call_does_not_execute_git_side_effects_twice() {
        let harness = setup_delivery_harness().await;
        let (_dir, _origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(task_worktree.join("task.txt"), "single delivery lock\n")
            .expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
        harness
            .controls
            .pause_next_commit
            .store(true, Ordering::SeqCst);
        let commit_started = harness.controls.commit_started.notified();
        let first_harness = harness.clone();
        let first_task_id = task_id.clone();
        let first = tokio::spawn(async move { deliver_task(&first_harness, &first_task_id).await });
        commit_started.await;

        let second = deliver_task(&harness, &task_id)
            .await
            .expect("second delivery should not run side effects");
        harness.controls.release_commit.notify_one();
        let first = first
            .await
            .expect("first delivery join")
            .expect("first delivery result");
        let evidence = harness
            .orchestrator_repo
            .list_evidence(&task_id)
            .await
            .expect("delivery evidence");

        assert_eq!(second.stages, Vec::<String>::new());
        assert_eq!(first.task.status, OrchestratorTaskStatus::Done);
        assert_eq!(
            harness.controls.commit_calls.load(Ordering::SeqCst),
            1,
            "commit should be claimed by exactly one delivery call"
        );
        assert_eq!(harness.controls.push_calls.load(Ordering::SeqCst), 1);
        assert_eq!(harness.controls.merge_calls.load(Ordering::SeqCst), 1);
        assert_eq!(evidence.len(), 4);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可能在 delivery 已 merge 到本地主工作区但尚未 push main 前点击终止，最终状态不得被 Done 覆盖，
    ///     且不得继续把 main 推送到远端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 merge 阶段成功后模拟用户把任务置为 Aborted，再断言 deliver_task 返回/数据库状态仍是 Aborted，
    ///     并验证 origin/main 没有收到 task 文件。
    #[tokio::test]
    async fn delivery_does_not_mark_done_when_task_aborted_before_finish() {
        let harness = setup_delivery_harness().await;
        let (_dir, origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(task_worktree.join("task.txt"), "abort before done\n").expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
        *harness
            .controls
            .abort_after_merge_task_id
            .lock()
            .expect("abort after merge lock") = Some(task_id.clone());

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("delivery should return current task");
        let persisted = harness
            .orchestrator_repo
            .get_task(&task_id)
            .await
            .expect("persisted task");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
        assert!(persisted.finished_at.is_none());
        assert_eq!(
            delivered.stages,
            vec!["commit", "push branch", "merge main"]
        );
        let main_show = StdCommand::new("git")
            .args(["show", "refs/heads/main:task.txt"])
            .current_dir(&origin)
            .output()
            .expect("git show origin main task file");
        assert!(
            !main_show.status.success(),
            "origin/main must not be pushed after user abort"
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     delivery 失败路径也可能与用户终止并发，失败兜底不得把 Aborted 任务覆盖为 Blocked。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 push branch 阶段先模拟用户终止再返回错误，断言 block_delivery_task 不能覆盖 Aborted。
    #[tokio::test]
    async fn delivery_failure_does_not_block_when_task_was_aborted() {
        let harness = setup_delivery_harness().await;
        let (_dir, _origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(task_worktree.join("task.txt"), "abort before block\n").expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
        *harness
            .controls
            .abort_before_push_error_task_id
            .lock()
            .expect("abort before push lock") = Some(task_id.clone());

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("delivery should return current task");
        let persisted = harness
            .orchestrator_repo
            .get_task(&task_id)
            .await
            .expect("persisted task");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 后 legacy 项目配置只用于兼容/调试，损坏的旧配置 JSON 不能影响全局 delivery 运行时。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入损坏的 verification_commands_json 后执行 delivery，断言任务仍按全局默认配置完成且没有 failed delivery evidence。
    #[tokio::test]
    async fn delivery_ignores_invalid_legacy_project_config() {
        let harness = setup_delivery_harness().await;
        let (_dir, _origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(task_worktree.join("task.txt"), "legacy config ignored\n")
            .expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
        sqlx::query(
            "INSERT INTO orchestrator_project_config \
             (project_id, verification_commands_json, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind("project-delivery")
        .bind("not-json")
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .execute(&harness.pool)
        .await
        .expect("insert invalid config");

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("legacy config should be ignored");
        let evidence = harness
            .orchestrator_repo
            .list_evidence(&task_id)
            .await
            .expect("delivery evidence");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Done);
        assert!(!evidence
            .iter()
            .any(|item| item.kind == EVIDENCE_KIND_DELIVERY && item.summary == "failed"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Delivering 任务缺少 worktree_id 时无法定位任务分支，应阻塞并留下 delivery evidence 供用户处理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接创建无 worktree_id 的 Delivering 任务，断言 delivery 返回 Blocked 和 failed evidence。
    #[tokio::test]
    async fn delivery_blocks_missing_worktree_id_with_failed_evidence() {
        let harness = setup_delivery_harness().await;
        let now = Utc::now().to_rfc3339();
        let task_id = "task-missing-worktree";
        harness
            .orchestrator_repo
            .create_task(&OrchestratorTaskRow {
                id: task_id.to_string(),
                project_id: "project-delivery".to_string(),
                title: "Missing worktree".to_string(),
                goal: "Block gracefully".to_string(),
                acceptance_criteria: "failed evidence".to_string(),
                status: OrchestratorTaskStatus::Delivering,
                priority: 0,
                branch_name: None,
                worktree_id: None,
                session_id: None,
                prepare_claim_token: None,
                blocked_reason: None,
                attempt: 0,
                created_at: now.clone(),
                updated_at: now,
                started_at: None,
                finished_at: None,
                ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Delivering)
            })
            .await
            .expect("insert task");

        let delivered = deliver_task(&harness, task_id)
            .await
            .expect("missing worktree should block");
        let evidence = harness
            .orchestrator_repo
            .list_evidence(task_id)
            .await
            .expect("delivery evidence");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Blocked);
        assert!(delivered
            .task
            .blocked_reason
            .unwrap_or_default()
            .contains("worktree_id"));
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].summary, "failed");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     legacy 项目配置里的 delivery flags 不再控制运行时，旧表关闭 auto_push_main 不能阻塞全局 delivery。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把 legacy 配置 auto_push_main 置为 0 后执行 delivery，断言任务仍按全局默认配置完成。
    #[tokio::test]
    async fn delivery_ignores_disabled_legacy_project_delivery_flags() {
        let harness = setup_delivery_harness().await;
        let (_dir, _origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(
            task_worktree.join("task.txt"),
            "legacy delivery flags ignored\n",
        )
        .expect("write task file");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
        harness
            .orchestrator_repo
            .get_or_create_project_config("project-delivery")
            .await
            .expect("default config");
        sqlx::query(
            "UPDATE orchestrator_project_config SET auto_push_main = 0 WHERE project_id = ?",
        )
        .bind("project-delivery")
        .execute(&harness.pool)
        .await
        .expect("disable push main");

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("legacy disabled flag should be ignored");
        let evidence = harness
            .orchestrator_repo
            .list_evidence(&task_id)
            .await
            .expect("delivery evidence");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Done);
        assert!(!evidence
            .iter()
            .any(|item| item.summary == "failed" && item.content.contains("auto_push_main")));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 delivery flags 已迁移到全局 AppConfig，关闭任一全局开关都必须阻塞任务并写入 evidence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对四个 delivery flag 分别构造关闭配置，执行 delivery 后断言 Blocked、blocked_reason 和 evidence 都指向该 flag。
    #[tokio::test]
    async fn delivery_blocks_each_disabled_global_delivery_flag_with_failed_evidence() {
        for disabled_flag in [
            "auto_commit",
            "auto_push_task_branch",
            "auto_merge_to_main",
            "auto_push_main",
        ] {
            let harness = setup_delivery_harness().await;
            let (_dir, _origin, repo, task_worktree) = setup_git_delivery_repo();
            let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;
            let mut config = OrchestratorAutomationConfig::default();
            match disabled_flag {
                "auto_commit" => config.auto_commit = false,
                "auto_push_task_branch" => config.auto_push_task_branch = false,
                "auto_merge_to_main" => config.auto_merge_to_main = false,
                "auto_push_main" => config.auto_push_main = false,
                _ => unreachable!("unknown delivery flag"),
            }
            harness.set_orchestrator_config(config);

            let delivered = deliver_task(&harness, &task_id)
                .await
                .expect("disabled global flag should block");
            let evidence = harness
                .orchestrator_repo
                .list_evidence(&task_id)
                .await
                .expect("delivery evidence");

            assert_eq!(delivered.task.status, OrchestratorTaskStatus::Blocked);
            assert!(delivered
                .task
                .blocked_reason
                .unwrap_or_default()
                .contains(disabled_flag));
            assert!(evidence
                .iter()
                .any(|item| item.summary == "blocked" && item.content.contains(disabled_flag)));
            assert_eq!(harness.controls.commit_calls.load(Ordering::SeqCst), 0);
            assert_eq!(harness.controls.push_calls.load(Ordering::SeqCst), 0);
            assert_eq!(harness.controls.merge_calls.load(Ordering::SeqCst), 0);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     主工作区存在未提交改动时，自动交付不能强行 merge，必须阻塞并保留可审计原因。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 main 工作区制造 dirty 文件后运行 deliver_task，断言任务 Blocked、原因包含指定英文串，
    ///     且 origin/main 没有收到任务分支改动。
    #[tokio::test]
    async fn delivery_blocks_when_main_worktree_is_dirty() {
        let harness = setup_delivery_harness().await;
        let (_dir, origin, repo, task_worktree) = setup_git_delivery_repo();
        fs::write(
            task_worktree.join("task.txt"),
            "dirty main should block delivery\n",
        )
        .expect("write task file");
        fs::write(repo.join("local-only.txt"), "uncommitted main change\n").expect("dirty main");
        let task_id = insert_delivery_task(&harness, &repo, &task_worktree).await;

        let delivered = deliver_task(&harness, &task_id)
            .await
            .expect("delivery should return blocked task");

        assert_eq!(delivered.task.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(
            delivered.stages,
            vec!["commit", "push branch", "merge main"]
        );
        let reason = delivered.task.blocked_reason.unwrap_or_default();
        assert!(reason.contains("main worktree is dirty"));
        let main_show = StdCommand::new("git")
            .args(["show", "refs/heads/main:task.txt"])
            .current_dir(&origin)
            .output()
            .expect("git show origin main task file");
        assert!(!main_show.status.success());
    }
}
