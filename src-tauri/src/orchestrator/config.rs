//! Orchestrator 全局自动化配置领域逻辑。
//!
//! Business Logic（为什么需要这个模块）:
//!     Settings 自动化 tab 需要读写本设备级 Orchestrator 自动化策略，策略不应再建模为
//!     project config。此模块集中处理 DTO、patch 语义、验证命令归一化与参数校验。
//!
//! Code Logic（这个模块做什么）:
//!     从 `AppConfig.orchestrator` 投影 camelCase DTO；接收前端 patch，按未传字段保留当前值
//!     的语义生成下一份 `OrchestratorAutomationConfig`，并在写配置前完成所有校验。

use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use serde::{Deserialize, Serialize};

const MAX_CONCURRENT_TASKS_MIN: i64 = 1;
const MAX_CONCURRENT_TASKS_MAX: i64 = 8;
pub(crate) const MAX_VERIFICATION_COMMANDS: usize = 20;
pub(crate) const MAX_VERIFICATION_COMMAND_CHARS: usize = 500;

/// Orchestrator 自动化配置前端 DTO。
///
/// Business Logic（为什么需要这个结构）:
///     设置页需要用前端 camelCase 字段展示和保存全局自动化策略。
///
/// Code Logic（这个结构做什么）:
///     纯 serde DTO，从 `OrchestratorAutomationConfig` 逐字段投影，字段名通过 serde 转 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorAutomationConfigDto {
    pub enabled: bool,
    pub max_concurrent_tasks: i64,
    pub verification_commands: Vec<String>,
    pub auto_commit: bool,
    pub auto_push_task_branch: bool,
    pub auto_merge_to_main: bool,
    pub auto_push_main: bool,
    pub notify_human_review: bool,
    pub notify_blocked: bool,
    pub notify_remote_outbox_failed: bool,
    pub notify_task_done: bool,
}

/// Orchestrator 自动化配置更新 patch。
///
/// Business Logic（为什么需要这个结构）:
///     前端设置页保存时可能只更新部分字段，未传字段必须保留当前持久化值。
///
/// Code Logic（这个结构做什么）:
///     使用 Option 表示字段是否传入；`verificationCommands` 接收多行文本，由领域函数归一化为 Vec。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorAutomationConfigPatch {
    pub enabled: Option<bool>,
    pub max_concurrent_tasks: Option<i64>,
    pub verification_commands: Option<String>,
    pub auto_commit: Option<bool>,
    pub auto_push_task_branch: Option<bool>,
    pub auto_merge_to_main: Option<bool>,
    pub auto_push_main: Option<bool>,
    pub notify_human_review: Option<bool>,
    pub notify_blocked: Option<bool>,
    pub notify_remote_outbox_failed: Option<bool>,
    pub notify_task_done: Option<bool>,
}

impl From<OrchestratorAutomationConfig> for OrchestratorAutomationConfigDto {
    /// 将磁盘配置转换为前端 DTO。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     IPC 边界需要稳定 camelCase DTO，不能直接暴露磁盘配置结构的内部命名约定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     消费 OrchestratorAutomationConfig 并逐字段投影为 DTO。
    fn from(config: OrchestratorAutomationConfig) -> Self {
        Self {
            enabled: config.enabled,
            max_concurrent_tasks: config.max_concurrent_tasks,
            verification_commands: config.verification_commands,
            auto_commit: config.auto_commit,
            auto_push_task_branch: config.auto_push_task_branch,
            auto_merge_to_main: config.auto_merge_to_main,
            auto_push_main: config.auto_push_main,
            notify_human_review: config.notify_human_review,
            notify_blocked: config.notify_blocked,
            notify_remote_outbox_failed: config.notify_remote_outbox_failed,
            notify_task_done: config.notify_task_done,
        }
    }
}

/// 返回 Orchestrator 自动化配置默认值。
///
/// Business Logic（为什么需要这个函数）:
///     设置页恢复默认和旧 config 升级需要统一使用 full-auto-but-disabled 默认策略。
///
/// Code Logic（这个函数做什么）:
///     直接返回 `OrchestratorAutomationConfig::default()`，避免命令层重复拼装字段。
pub fn default_orchestrator_automation_config() -> OrchestratorAutomationConfig {
    OrchestratorAutomationConfig::default()
}

/// 把前端多行验证命令文本归一化为命令数组。
///
/// Business Logic（为什么需要这个函数）:
///     用户在设置页用 textarea 维护验证命令；空行不应进入运行配置，过多或过长命令会让后续
///     验证 evidence 难以审计，也容易造成误操作。
///
/// Code Logic（这个函数做什么）:
///     按行 trim、过滤空行、保留顺序；限制最多 20 条，每条最长 500 字符。
pub fn normalize_verification_commands(input: &str) -> Result<Vec<String>, AppError> {
    normalize_verification_command_items(input.lines().map(ToOwned::to_owned).collect(), "验证命令")
}

/// 归一化验证命令列表。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 和 WORKFLOW.md 都会提供验证命令，二者必须共享数量和长度限制，避免不同入口行为漂移。
///
/// Code Logic（这个函数做什么）:
///     对命令列表逐项 trim、过滤空白；限制最多 20 条、单条 500 字符，并在错误中包含来源标签和索引。
pub(crate) fn normalize_verification_command_items(
    items: Vec<String>,
    source_label: &str,
) -> Result<Vec<String>, AppError> {
    let commands = items
        .into_iter()
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();

    if commands.len() > MAX_VERIFICATION_COMMANDS {
        return Err(AppError::generic(format!(
            "{source_label} 最多只能配置 {MAX_VERIFICATION_COMMANDS} 条"
        )));
    }

    for (index, command) in commands.iter().enumerate() {
        if command.chars().count() > MAX_VERIFICATION_COMMAND_CHARS {
            return Err(AppError::generic(format!(
                "{source_label}[{index}] 最长不能超过 {MAX_VERIFICATION_COMMAND_CHARS} 字符"
            )));
        }
    }

    Ok(commands)
}

/// 应用 Orchestrator 自动化配置 patch。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 自动化 tab 需要保存用户本次修改，同时保留未编辑字段，且必须在落盘前拒绝非法配置。
///
/// Code Logic（这个函数做什么）:
///     克隆 current 后逐个应用 Option 字段；验证并发上限 1..=8；对多行验证命令做归一化和长度限制。
pub fn apply_orchestrator_config_patch(
    current: &OrchestratorAutomationConfig,
    patch: OrchestratorAutomationConfigPatch,
) -> Result<OrchestratorAutomationConfig, AppError> {
    let mut next = current.clone();

    if let Some(enabled) = patch.enabled {
        next.enabled = enabled;
    }
    if let Some(max_concurrent_tasks) = patch.max_concurrent_tasks {
        if !(MAX_CONCURRENT_TASKS_MIN..=MAX_CONCURRENT_TASKS_MAX).contains(&max_concurrent_tasks) {
            return Err(AppError::generic(format!(
                "Orchestrator 并发上限必须在 {MAX_CONCURRENT_TASKS_MIN}..={MAX_CONCURRENT_TASKS_MAX} 之间"
            )));
        }
        next.max_concurrent_tasks = max_concurrent_tasks;
    }
    if let Some(commands) = patch.verification_commands {
        next.verification_commands = normalize_verification_commands(&commands)?;
    }
    if let Some(auto_commit) = patch.auto_commit {
        next.auto_commit = auto_commit;
    }
    if let Some(auto_push_task_branch) = patch.auto_push_task_branch {
        next.auto_push_task_branch = auto_push_task_branch;
    }
    if let Some(auto_merge_to_main) = patch.auto_merge_to_main {
        next.auto_merge_to_main = auto_merge_to_main;
    }
    if let Some(auto_push_main) = patch.auto_push_main {
        next.auto_push_main = auto_push_main;
    }
    if let Some(v) = patch.notify_human_review {
        next.notify_human_review = v;
    }
    if let Some(v) = patch.notify_blocked {
        next.notify_blocked = v;
    }
    if let Some(v) = patch.notify_remote_outbox_failed {
        next.notify_remote_outbox_failed = v;
    }
    if let Some(v) = patch.notify_task_done {
        next.notify_task_done = v;
    }

    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_orchestrator_config_patch, normalize_verification_commands,
        OrchestratorAutomationConfigDto, OrchestratorAutomationConfigPatch,
    };
    use crate::config::OrchestratorAutomationConfig;

    /// Business Logic（为什么需要这个函数）:
    ///     多个配置测试需要一份带非默认值的当前配置，用来验证 patch 未传字段会保留旧值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 enabled=true、并发=3、带验证命令且 delivery flags 混合开关的配置样本。
    fn sample_current_config() -> OrchestratorAutomationConfig {
        OrchestratorAutomationConfig {
            enabled: true,
            max_concurrent_tasks: 3,
            verification_commands: vec!["cargo test".to_string()],
            auto_commit: false,
            auto_push_task_branch: true,
            auto_merge_to_main: false,
            auto_push_main: true,
            notify_human_review: true,
            notify_blocked: true,
            notify_remote_outbox_failed: true,
            notify_task_done: false,
            generic_terminal: None,
        }
    }

    /// 验证多行命令会 trim 并过滤空行。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     用户在 textarea 中常会输入空行或缩进，保存后不应污染实际验证命令。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造带空行、空格和 tab 的输入，断言输出只保留清理后的两条命令。
    #[test]
    fn normalize_verification_commands_trims_and_filters_blank_lines() {
        let commands = normalize_verification_commands("  cargo test  \n\n\tcargo check\t\n  \n")
            .expect("commands should normalize");

        assert_eq!(commands, vec!["cargo test", "cargo check"]);
    }

    /// 验证 patch 未传字段会保留当前值。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     设置页局部保存不能把用户没有编辑的 delivery flag 或并发上限重置为默认值。
    ///
    /// Code Logic（这个测试做什么）:
    ///     只传 enabled、verificationCommands、autoMergeToMain，断言其它字段沿用 current。
    #[test]
    fn apply_patch_keeps_omitted_fields() {
        let current = sample_current_config();
        let patch = OrchestratorAutomationConfigPatch {
            enabled: Some(false),
            max_concurrent_tasks: None,
            verification_commands: Some("npm test\ncargo check".to_string()),
            auto_commit: None,
            auto_push_task_branch: None,
            auto_merge_to_main: Some(true),
            auto_push_main: None,
            notify_human_review: None,
            notify_blocked: None,
            notify_remote_outbox_failed: None,
            notify_task_done: None,
        };

        let updated = apply_orchestrator_config_patch(&current, patch).expect("patch should pass");

        assert!(!updated.enabled);
        assert_eq!(updated.max_concurrent_tasks, 3);
        assert_eq!(
            updated.verification_commands,
            vec!["npm test".to_string(), "cargo check".to_string()]
        );
        assert!(!updated.auto_commit);
        assert!(updated.auto_push_task_branch);
        assert!(updated.auto_merge_to_main);
        assert!(updated.auto_push_main);
    }

    /// 验证并发上限边界值 1 和 8 可保存。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     设置页允许用户在最小和最大并发边界之间选择，边界值本身必须是合法配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别传入 maxConcurrentTasks=1 和 8，断言 patch 成功并写入目标值。
    #[test]
    fn apply_patch_accepts_min_and_max_concurrent_task_boundaries() {
        let current = sample_current_config();

        let min_updated = apply_orchestrator_config_patch(
            &current,
            OrchestratorAutomationConfigPatch {
                max_concurrent_tasks: Some(1),
                ..Default::default()
            },
        )
        .expect("min boundary should pass");
        let max_updated = apply_orchestrator_config_patch(
            &current,
            OrchestratorAutomationConfigPatch {
                max_concurrent_tasks: Some(8),
                ..Default::default()
            },
        )
        .expect("max boundary should pass");

        assert_eq!(min_updated.max_concurrent_tasks, 1);
        assert_eq!(max_updated.max_concurrent_tasks, 8);
    }

    /// 验证并发上限不能为 0。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     0 并发会让自动调度器永远无法领取任务，应在配置保存阶段明确拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入 maxConcurrentTasks=0，断言返回包含“并发”的业务错误。
    #[test]
    fn apply_patch_rejects_zero_max_concurrent_tasks() {
        let current = sample_current_config();
        let patch = OrchestratorAutomationConfigPatch {
            max_concurrent_tasks: Some(0),
            ..Default::default()
        };

        let err = apply_orchestrator_config_patch(&current, patch).unwrap_err();

        assert!(err.to_string().contains("并发"));
    }

    /// 验证并发上限不能超过 8。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     过高并发可能同时创建多个 worktree/terminal，容易拖垮本机资源。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入 maxConcurrentTasks=9，断言返回说明合法范围 1..=8 的业务错误。
    #[test]
    fn apply_patch_rejects_too_many_max_concurrent_tasks() {
        let current = sample_current_config();
        let patch = OrchestratorAutomationConfigPatch {
            max_concurrent_tasks: Some(9),
            ..Default::default()
        };

        let err = apply_orchestrator_config_patch(&current, patch).unwrap_err();

        assert!(err.to_string().contains("1..=8"));
    }

    /// 验证最多只能配置 20 条验证命令。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     过多命令会让自动验证耗时不可控，也不利于 evidence 审计。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 21 行非空命令，断言归一化函数拒绝并提示上限。
    #[test]
    fn normalize_verification_commands_rejects_more_than_twenty_commands() {
        let input = (0..21)
            .map(|idx| format!("echo {idx}"))
            .collect::<Vec<_>>()
            .join("\n");

        let err = normalize_verification_commands(&input).unwrap_err();

        assert!(err.to_string().contains("20"));
    }

    /// 验证单条命令最长 500 字符。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     超长命令通常是误粘贴或复合脚本，应要求用户放到项目脚本中再调用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 501 字符命令，断言归一化函数拒绝并提示长度。
    #[test]
    fn normalize_verification_commands_rejects_single_command_longer_than_five_hundred_chars() {
        let input = "x".repeat(501);

        let err = normalize_verification_commands(&input).unwrap_err();

        assert!(err.to_string().contains("500"));
    }

    /// 验证 DTO 按 camelCase 序列化。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     前端 Settings 自动化 tab 会按 camelCase 字段读取配置，字段名漂移会直接破坏表单。
    ///
    /// Code Logic（这个测试做什么）:
    ///     将 DTO 序列化成 JSON Value，断言关键字段名为 maxConcurrentTasks、verificationCommands 等。
    #[test]
    fn dto_serializes_camel_case_fields() {
        let dto = OrchestratorAutomationConfigDto::from(OrchestratorAutomationConfig {
            enabled: true,
            max_concurrent_tasks: 2,
            verification_commands: vec!["cargo test".to_string()],
            auto_commit: true,
            auto_push_task_branch: false,
            auto_merge_to_main: true,
            auto_push_main: false,
            notify_human_review: true,
            notify_blocked: false,
            notify_remote_outbox_failed: true,
            notify_task_done: true,
            generic_terminal: None,
        });

        let value = serde_json::to_value(dto).expect("dto should serialize");

        assert_eq!(value["maxConcurrentTasks"], 2);
        assert_eq!(value["verificationCommands"][0], "cargo test");
        assert_eq!(value["autoCommit"], true);
        assert_eq!(value["autoPushTaskBranch"], false);
        assert_eq!(value["autoMergeToMain"], true);
        assert_eq!(value["autoPushMain"], false);
        assert_eq!(value["notifyHumanReview"], true);
        assert_eq!(value["notifyBlocked"], false);
        assert_eq!(value["notifyRemoteOutboxFailed"], true);
        assert_eq!(value["notifyTaskDone"], true);
    }
}
