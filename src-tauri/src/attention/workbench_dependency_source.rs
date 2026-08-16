//! attention/workbench_dependency_source.rs — Workbench tmux 依赖的 Attention 投影。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 Inbox 需要在存在 Workbench 项目且 tmux 依赖为 missing/failed/unsupported 时，
//!     投影一条可导航到 Settings 依赖页的环境条目；无项目时即使依赖缺失也不制造待办。
//!
//! Code Logic（这个模块做什么）:
//!     纯函数按项目数量与依赖状态投影；AttentionSource 只读项目仓储与 dependency 缓存，
//!     不触发探测/安装；稳定 ID 固定为 `workbench:dependency:tmux`。

use crate::attention::models::{
    AttentionCategory, AttentionFreshness, AttentionItemDto, AttentionSettingsTab,
    AttentionSourceKind, AttentionTargetDto,
};
use crate::attention::source::AttentionSource;
use crate::commands::workbench_dependencies::get_workbench_dependency_status_for_state;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::dependencies::{WorkbenchDependencyState, WorkbenchDependencyStatusDto};
use futures_util::future::BoxFuture;

/// Workbench tmux 依赖 Attention 投影源。
///
/// Business Logic（为什么需要这个结构体）:
///     聚合器通过统一 AttentionSource 接口收集环境依赖待办，避免页面散落业务判断。
///
/// Code Logic（这个结构体做什么）:
///     无状态 source；collect 读取 workbench 项目数量与 dependency 缓存状态。
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkbenchDependencyAttentionSource;

impl AttentionSource for WorkbenchDependencyAttentionSource {
    /// Business Logic（为什么需要这个函数）:
    ///     聚合器需要一次性拿到本机 Workbench tmux 依赖是否构成 Inbox 条目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取项目列表长度与 dependency 缓存；调用纯投影函数；仓库错误上抛使整次 source 失败。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>> {
        Box::pin(async move { collect_workbench_dependency_attention_items(state).await })
    }
}

/// Business Logic（为什么需要这个函数）:
///     桌面与 Mobile 共用同一依赖投影入口，避免 command/route 各自拼装。
///
/// Code Logic（这个函数做什么）:
///     列出 Workbench 项目数量，读取 dependency 缓存，投影 0 或 1 条 environment 条目。
pub async fn collect_workbench_dependency_attention_items(
    state: &AppState,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let projects = state.workbench_project_repo.list().await?;
    let status = get_workbench_dependency_status_for_state(state);
    Ok(project_workbench_dependency(projects.len(), &status)
        .into_iter()
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     无 Workbench 项目时 tmux 缺失不阻塞任何项目工作；有项目时仅 missing/failed/unsupported
///     才需要用户到 Settings 处理，ready/checking/installing 不制造待办。
///
/// Code Logic（这个函数做什么）:
///     project_count==0 恒返回 None；否则按状态投影固定 ID `workbench:dependency:tmux`，
///     updatedAt 取 status_changed_at，target 为 settings/dependencies。
pub(crate) fn project_workbench_dependency(
    project_count: usize,
    status: &WorkbenchDependencyStatusDto,
) -> Option<AttentionItemDto> {
    if project_count == 0 {
        return None;
    }
    let (title, summary) = match status.status {
        WorkbenchDependencyState::Missing => (
            "tmux 依赖缺失".to_string(),
            "Workbench 需要 tmux 才能恢复终端会话，请前往依赖设置处理".to_string(),
        ),
        WorkbenchDependencyState::Failed => {
            let summary = status
                .error
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or("tmux 安装失败，请前往依赖设置重试或排查")
                .to_string();
            ("tmux 安装失败".to_string(), summary)
        }
        WorkbenchDependencyState::Unsupported => (
            "当前平台不支持自动管理 tmux".to_string(),
            "Workbench 依赖在当前平台不可自动安装，请手动配置后重新检测".to_string(),
        ),
        // ready / installing：非阻塞；未探测完成的状态也绝不能投影为环境待办。
        WorkbenchDependencyState::Ready | WorkbenchDependencyState::Installing => return None,
    };

    Some(AttentionItemDto {
        id: "workbench:dependency:tmux".to_string(),
        category: AttentionCategory::Environment,
        source_kind: AttentionSourceKind::WorkbenchDependency,
        title,
        summary,
        updated_at: status.status_changed_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: None,
        device: None,
        target: AttentionTargetDto::Settings {
            tab: AttentionSettingsTab::Dependencies,
        },
        read_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: 构造仅覆盖投影所需字段的依赖状态 DTO。
    /// Code Logic: 按 status/status_changed_at/error 填最小字段。
    fn dep_status(
        status: WorkbenchDependencyState,
        status_changed_at: &str,
        error: Option<&str>,
    ) -> WorkbenchDependencyStatusDto {
        WorkbenchDependencyStatusDto {
            status,
            available: status == WorkbenchDependencyState::Ready,
            version: None,
            backend: "native".to_string(),
            path: None,
            installable: status == WorkbenchDependencyState::Missing
                || status == WorkbenchDependencyState::Failed,
            install_command_preview: Vec::new(),
            error: error.map(str::to_string),
            output: Vec::new(),
            status_changed_at: status_changed_at.to_string(),
        }
    }

    #[test]
    fn zero_projects_never_produces_dependency_item() {
        for status in [
            WorkbenchDependencyState::Missing,
            WorkbenchDependencyState::Failed,
            WorkbenchDependencyState::Unsupported,
            WorkbenchDependencyState::Ready,
            WorkbenchDependencyState::Installing,
        ] {
            let dto = dep_status(status, "2026-07-12T10:00:00Z", Some("err"));
            assert!(
                project_workbench_dependency(0, &dto).is_none(),
                "无项目时状态 {status:?} 不得进入 attention"
            );
        }
    }

    #[test]
    fn missing_failed_unsupported_project_to_stable_environment_item() {
        let cases = [
            (WorkbenchDependencyState::Missing, None, "tmux 依赖缺失"),
            (
                WorkbenchDependencyState::Failed,
                Some("exit 1"),
                "tmux 安装失败",
            ),
            (
                WorkbenchDependencyState::Unsupported,
                None,
                "当前平台不支持自动管理 tmux",
            ),
        ];
        for (status, error, title) in cases {
            let dto = dep_status(status, "2026-07-12T11:22:33Z", error);
            let item = project_workbench_dependency(1, &dto).expect("should project");
            assert_eq!(item.id, "workbench:dependency:tmux");
            assert_eq!(item.category, AttentionCategory::Environment);
            assert_eq!(item.source_kind, AttentionSourceKind::WorkbenchDependency);
            assert_eq!(item.title, title);
            assert_eq!(item.updated_at, "2026-07-12T11:22:33Z");
            assert_eq!(item.freshness, AttentionFreshness::Live);
            assert_eq!(item.cached_at, None);
            assert_eq!(item.project, None);
            assert_eq!(item.device, None);
            assert_eq!(
                item.target,
                AttentionTargetDto::Settings {
                    tab: AttentionSettingsTab::Dependencies,
                }
            );
            if status == WorkbenchDependencyState::Failed {
                assert_eq!(item.summary, "exit 1");
            }
        }
    }

    #[test]
    fn ready_and_installing_are_excluded_even_with_projects() {
        for status in [
            WorkbenchDependencyState::Ready,
            WorkbenchDependencyState::Installing,
        ] {
            let dto = dep_status(status, "2026-07-12T12:00:00Z", None);
            assert!(
                project_workbench_dependency(3, &dto).is_none(),
                "状态 {status:?} 不应进入 attention"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     冷启动有项目且 tmux 实际 ready 时，Inbox 不得出现虚假环境阻塞。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 ready 状态 + project_count>0 投影，断言返回 None。
    #[test]
    fn cold_start_ready_with_projects_does_not_project_dependency_item() {
        let dto = dep_status(
            WorkbenchDependencyState::Ready,
            "2026-07-12T12:00:00Z",
            None,
        );
        assert!(
            project_workbench_dependency(2, &dto).is_none(),
            "tmux ready 时即使有项目也不得投影依赖条目"
        );
    }

    #[test]
    fn updated_at_equals_status_changed_at() {
        let dto = dep_status(
            WorkbenchDependencyState::Missing,
            "2026-07-11T08:09:10Z",
            None,
        );
        let item = project_workbench_dependency(2, &dto).expect("missing with projects");
        assert_eq!(item.updated_at, dto.status_changed_at);
        assert_eq!(item.updated_at, "2026-07-11T08:09:10Z");
    }
}
