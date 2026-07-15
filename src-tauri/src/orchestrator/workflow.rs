//! Orchestrator 项目工作流策略解析。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator 需要一个稳定内置工作流，允许项目根目录用 WORKFLOW.md 轻量覆盖 Prompt、
//!     验证命令和 Runner 限额，但不能让项目文件控制自动交付开关。
//!
//! Code Logic（这个模块做什么）:
//!     解析项目根 WORKFLOW.md 的可选 YAML front matter 与正文模板，校验工作流状态、Runner
//!     限额和验证命令，并提供任务 Prompt 模板渲染 helper。

use crate::error::AppError;
use crate::orchestrator::config::normalize_verification_command_items;
use crate::orchestrator::models::OrchestratorWorkflowState;
use crate::orchestrator::prompt::{
    contains_standalone_dev_done_sentinel, render_user_block, DEV_DONE_SENTINEL,
};
use crate::workbench::file_content::{save_text_file_atomic, sha256_file_hex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_STALL_TIMEOUT_MS: i64 = 30 * 60 * 1000;

/// 文件相对审阅快照发生漂移时的稳定 Conflict code。
///
/// Business Logic（为什么需要这个常量）:
///     前端 WORKFLOW 向导在 CAS 保存失败时需要稳定 token 提示用户重新加载，不能依赖本地化文案匹配。
///
/// Code Logic（这个常量做什么）:
///     作为 `AppError::conflict` 的消息体 / domain 稳定 code 使用。
pub const WORKFLOW_DOCUMENT_CHANGED_CODE: &str = "workflow_document_changed";

/// 项目级 workflow 配置文件名。
///
/// Business Logic（为什么需要这个常量）:
///     用户需要在项目根目录用一个固定文件名声明 Orchestrator 工作流覆盖项，便于跨项目复用。
///
/// Code Logic（这个常量做什么）:
///     保存 resolver 查找的固定文件名，避免调用点硬编码。
pub const WORKFLOW_FILE_NAME: &str = "WORKFLOW.md";

/// 内置任务 Prompt 模板。
///
/// Business Logic（为什么需要这个常量）:
///     没有项目 WORKFLOW.md 时，Runner 仍需拿到明确的任务标题、目标、验收标准和完成哨兵协议。
///
/// Code Logic（这个常量做什么）:
///     使用轻量 `{{ ... }}` 占位符描述默认 Prompt，渲染时替换任务上下文和 attempt。
pub const DEFAULT_PROMPT_TEMPLATE: &str = "请在当前项目中完成 Orchestrator 任务。\n\n\
任务标题：\n{{ task.title }}\n\n\
任务目标：\n{{ task.goal }}\n\n\
验收标准：\n{{ task.acceptance_criteria }}\n\n\
当前尝试轮次：{{ attempt }}\n\n\
执行要求：\n\
1. 先阅读并遵守项目根目录 AGENTS.md；进入子目录时继续遵守该目录的 AGENTS.md，若没有 AGENTS.md 但有 CLAUDE.md，则遵守 CLAUDE.md。\n\
2. 严格围绕本任务目标和验收标准实现，不要扩大到未要求的功能或无关变更。\n\
3. 完成后说明你运行过的验证方式、仍未验证的风险和需要人工关注的风险。\n\
4. 不要自行清理、删除、合并当前 worktree，也不要自动提交或推送；保留现场供 Orchestrator/Workbench 接管。\n\
5. 只有在你已经完成代码、更改过的相关测试/验证、并给出必要证据说明后，最后单独输出 {{ dev_done_sentinel }}。\n\
6. 未完成代码、未运行必要测试/验证或还没有证据说明时，绝对不要输出 {{ dev_done_sentinel }}。\n";

/// Workflow 解析来源。
///
/// Business Logic（为什么需要这个枚举）:
///     前端和后续调度逻辑需要知道当前策略来自内置默认值还是项目根覆盖，便于诊断项目行为。
///
/// Code Logic（这个枚举做什么）:
///     用强类型标记 resolver 的来源，避免用裸字符串比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowSource {
    BuiltInDefault,
    ProjectOverride,
}

/// Runner 限额配置。
///
/// Business Logic（为什么需要这个结构体）:
///     项目可以收紧或调整可见 Runner 的安全上限，但当前只允许 claudeCodeVisible provider。
///
/// Code Logic（这个结构体做什么）:
///     保存 Runner provider、最大轮次和 stall timeout，resolver 会在应用覆盖项时做边界校验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunnerConfig {
    pub provider: String,
    pub max_turns: i64,
    pub stall_timeout_ms: i64,
}

/// 已解析工作流策略。
///
/// Business Logic（为什么需要这个结构体）:
///     Orchestrator 需要把内置默认值和项目覆盖合并成一个可直接消费的策略对象，避免运行时到处读取文件。
///
/// Code Logic（这个结构体做什么）:
///     聚合任务默认创建状态、活跃/审核/终态集合、Runner 限额、验证命令和 Prompt 模板。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkflow {
    pub source: WorkflowSource,
    pub default_create_state: OrchestratorWorkflowState,
    pub active_states: Vec<OrchestratorWorkflowState>,
    pub review_state: OrchestratorWorkflowState,
    pub terminal_states: Vec<OrchestratorWorkflowState>,
    pub runner: WorkflowRunnerConfig,
    pub validation_commands: Vec<String>,
    pub prompt_template: String,
}

/// Prompt 渲染任务上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     workflow Prompt 模板只需要任务标题、目标和验收标准，避免把完整数据库 Row 暴露给模板层。
///
/// Code Logic（这个结构体做什么）:
///     保存可替换到模板中的任务字段，render_prompt 根据字段名替换占位符。
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTaskContext {
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
}

/// WORKFLOW.md front matter 根结构。
///
/// Business Logic（为什么需要这个结构体）:
///     项目覆盖文件需要把工作流、Runner 和验证命令分区声明，降低用户配置歧义。
///
/// Code Logic（这个结构体做什么）:
///     用 serde_yaml 反序列化可选 section，缺失 section 表示沿用内置默认值。
#[derive(Debug, Default, Deserialize)]
struct WorkflowFrontMatter {
    workflow: Option<WorkflowSection>,
    runner: Option<RunnerSection>,
    validation: Option<ValidationSection>,
}

/// WORKFLOW.md workflow section。
///
/// Business Logic（为什么需要这个结构体）:
///     项目可调整任务看板状态集合，适配不同团队的工作流命名和终态边界。
///
/// Code Logic（这个结构体做什么）:
///     接收状态字符串列表，后续由 workflow_state_from_config 转换为强类型枚举。
#[derive(Debug, Default, Deserialize)]
struct WorkflowSection {
    default_create_state: Option<String>,
    active_states: Option<Vec<String>>,
    review_state: Option<String>,
    terminal_states: Option<Vec<String>>,
}

/// WORKFLOW.md runner section。
///
/// Business Logic（为什么需要这个结构体）:
///     项目可调整单任务 Runner 最大轮次和无输出超时，以匹配项目复杂度和风险偏好。
///
/// Code Logic（这个结构体做什么）:
///     接收 provider、max_turns、stall_timeout_ms 的可选覆盖值，由 resolver 做支持范围校验。
#[derive(Debug, Default, Deserialize)]
struct RunnerSection {
    provider: Option<String>,
    max_turns: Option<i64>,
    stall_timeout_ms: Option<i64>,
}

/// WORKFLOW.md validation section。
///
/// Business Logic（为什么需要这个结构体）:
///     项目可声明推荐验证命令，后续验证层可在 Settings 全局命令之外识别项目策略来源。
///
/// Code Logic（这个结构体做什么）:
///     接收命令字符串列表，resolver 会 trim 并过滤空白项。
#[derive(Debug, Default, Deserialize)]
struct ValidationSection {
    commands: Option<Vec<String>>,
}

impl ResolvedWorkflow {
    /// Business Logic（为什么需要这个函数）:
    ///     没有项目 WORKFLOW.md 时，Orchestrator 仍必须有可用、可预测且不启用交付控制的默认策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造内置默认工作流：Backlog 创建、Todo/Rework 活跃、HumanReview 审核、Done/Canceled 终态，
    ///     Runner 固定 claudeCodeVisible，验证命令为空，并使用 DEFAULT_PROMPT_TEMPLATE。
    pub fn built_in_default() -> Self {
        Self {
            source: WorkflowSource::BuiltInDefault,
            default_create_state: OrchestratorWorkflowState::Backlog,
            active_states: vec![
                OrchestratorWorkflowState::Todo,
                OrchestratorWorkflowState::Rework,
            ],
            review_state: OrchestratorWorkflowState::HumanReview,
            terminal_states: vec![
                OrchestratorWorkflowState::Done,
                OrchestratorWorkflowState::Canceled,
            ],
            runner: WorkflowRunnerConfig {
                provider: "claudeCodeVisible".to_string(),
                max_turns: 1,
                stall_timeout_ms: 300_000,
            },
            validation_commands: Vec::new(),
            prompt_template: DEFAULT_PROMPT_TEMPLATE.to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     可见 Runner 启动前需要把已解析 workflow 模板渲染成具体任务 Prompt，确保项目覆盖生效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 render_prompt 替换任务字段和 attempt，占位符未知时返回 AppError。
    #[allow(dead_code)]
    pub fn render_task_prompt(
        &self,
        task: &PromptTaskContext,
        attempt: i64,
    ) -> Result<String, AppError> {
        render_prompt(&self.prompt_template, task, attempt)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     可见 Runner 执行项目自定义 Prompt 时，仍必须携带 worktree 路径和完成哨兵协议，避免模板漏写导致无法验证或误操作主工作区。
    ///
    /// Code Logic（这个函数做什么）:
    ///     渲染 workflow prompt_template，支持 worktree_path 变量；若渲染结果缺少 worktree 路径或 DEV_DONE_SENTINEL，
    ///     追加标准 Runner guardrail，并最终拒绝裸独立哨兵行。
    pub fn render_runner_prompt(
        &self,
        task: &PromptTaskContext,
        attempt: i64,
        worktree_path: &str,
    ) -> Result<String, AppError> {
        let mut rendered =
            render_prompt_with_worktree(&self.prompt_template, task, attempt, Some(worktree_path))?;
        let trimmed_path = worktree_path.trim();
        let has_worktree_path = !trimmed_path.is_empty() && rendered.contains(trimmed_path);
        let has_dev_done_guardrail = rendered.contains(DEV_DONE_SENTINEL);
        if !has_worktree_path || !has_dev_done_guardrail {
            if !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push_str(&standard_runner_guardrail(trimmed_path));
        }
        if contains_standalone_dev_done_sentinel(&rendered) {
            return Err(AppError::generic(
                "Prompt 模板不能包含独立完成哨兵行，请把哨兵写在说明句中",
            ));
        }
        Ok(rendered)
    }
}

/// WORKFLOW.md 文档检测状态。
///
/// Business Logic（为什么需要这个枚举）:
///     向导需要区分缺失、合法、非法与读失败四种状态，以决定创建模板、打开编辑或提示重试。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化为 `missing|valid|invalid|readError`，供 Tauri/P2P/前端共享。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowDocumentStatus {
    Missing,
    Valid,
    Invalid,
    ReadError,
}

/// 单条 WORKFLOW 诊断信息。
///
/// Business Logic（为什么需要这个结构体）:
///     权威 validator 必须返回可定位的诊断，供向导高亮 front matter / 模板错误。
///
/// Code Logic（这个结构体做什么）:
///     序列化 path/line/column/code/message；未知位置时 line/column 为 null。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDiagnostic {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub code: String,
    pub message: String,
}

/// 权威 WORKFLOW 文档快照 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     get/validate/save 共享同一文档视图：状态、正文、CAS hash、诊断与规范化 preview。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化；missing 时 content/content_hash 为空，preview 可携带默认模板建议。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDocument {
    pub status: WorkflowDocumentStatus,
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Business Logic（为什么需要这个函数）:
///     缺失 WORKFLOW.md 时向导需要一份与内置策略一致的可保存模板，避免用户手写 front matter。
///
/// Code Logic（这个函数做什么）:
///     生成含固定 workflow/runner/validation section 与 DEFAULT_PROMPT_TEMPLATE 正文的 YAML front matter 文档。
pub fn default_workflow_template() -> String {
    format!(
        "---\n\
workflow:\n\
  default_create_state: backlog\n\
  active_states:\n\
    - todo\n\
    - rework\n\
  review_state: humanReview\n\
  terminal_states:\n\
    - done\n\
    - canceled\n\
runner:\n\
  provider: claudeCodeVisible\n\
  max_turns: 1\n\
  stall_timeout_ms: 300000\n\
validation:\n\
  commands: []\n\
---\n\
{DEFAULT_PROMPT_TEMPLATE}"
    )
}

/// Business Logic（为什么需要这个函数）:
///     CAS 保存与前端乐观锁需要稳定的内容哈希，且 validate 响应也要返回与磁盘一致的算法。
///
/// Code Logic（这个函数做什么）:
///     对 UTF-8 正文字节计算 SHA-256，返回小写十六进制字符串。
pub fn content_sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Business Logic（为什么需要这个函数）:
///     权威 validator 与默认模板 round-trip 需要在不读盘的情况下解析 WORKFLOW.md 正文。
///
/// Code Logic（这个函数做什么）:
///     拆分 front matter/正文，合并到 built_in_default 覆盖项，source 固定 ProjectOverride。
pub fn parse_project_workflow(content: &str) -> Result<ResolvedWorkflow, AppError> {
    let (front_matter, body) = split_workflow_document(content)?;
    let mut workflow = ResolvedWorkflow::built_in_default();
    workflow.source = WorkflowSource::ProjectOverride;

    if let Some(yaml) = front_matter {
        let parsed = if yaml.trim().is_empty() {
            WorkflowFrontMatter::default()
        } else {
            serde_yaml::from_str::<WorkflowFrontMatter>(yaml).map_err(|error| {
                AppError::generic(format!(
                    "{WORKFLOW_FILE_NAME} front matter 解析失败: {error}"
                ))
            })?
        };
        apply_front_matter(&mut workflow, parsed)?;
    }

    let prompt_body = body.trim();
    if !prompt_body.is_empty() {
        workflow.prompt_template = prompt_body.to_string();
    }

    Ok(workflow)
}

/// Business Logic（为什么需要这个函数）:
///     Orchestrator 需要按项目根目录解析 workflow 策略，让没有配置的项目使用内置默认值，有配置的项目可覆盖安全范围内的行为。
///
/// Code Logic（这个函数做什么）:
///     查找 WORKFLOW.md；不存在返回 built_in_default；存在则读文件并委托 `parse_project_workflow`。
pub fn resolve_project_workflow(project_path: &Path) -> Result<ResolvedWorkflow, AppError> {
    let workflow_path = project_path.join(WORKFLOW_FILE_NAME);
    if !workflow_path.exists() {
        return Ok(ResolvedWorkflow::built_in_default());
    }

    let content = fs::read_to_string(&workflow_path)
        .map_err(|error| AppError::generic(format!("读取 {WORKFLOW_FILE_NAME} 失败: {error}")))?;
    parse_project_workflow(&content)
}

/// Business Logic（为什么需要这个函数）:
///     向导打开时需要检测项目 WORKFLOW.md 的 missing/valid/invalid/readError 状态与 CAS hash。
///
/// Code Logic（这个函数做什么）:
///     解析项目根下固定文件名；拒绝 symlink；读失败→readError；解析失败→invalid+diagnostics；成功→valid。
pub fn load_workflow_document(project_path: &Path) -> WorkflowDocument {
    let workflow_path = match resolve_workflow_document_path(project_path) {
        Ok(path) => path,
        Err(error) => {
            return WorkflowDocument {
                status: WorkflowDocumentStatus::ReadError,
                content: None,
                content_hash: None,
                diagnostics: vec![diagnostic_from_message(
                    "workflow_path_error",
                    &error.to_string(),
                )],
                preview: None,
            };
        }
    };

    match fs::symlink_metadata(&workflow_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return WorkflowDocument {
                status: WorkflowDocumentStatus::Missing,
                content: None,
                content_hash: None,
                diagnostics: Vec::new(),
                preview: Some(default_workflow_template()),
            };
        }
        Err(error) => {
            return WorkflowDocument {
                status: WorkflowDocumentStatus::ReadError,
                content: None,
                content_hash: None,
                diagnostics: vec![diagnostic_from_message(
                    "workflow_read_error",
                    &format!("读取 {WORKFLOW_FILE_NAME} 元数据失败: {error}"),
                )],
                preview: None,
            };
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return WorkflowDocument {
                status: WorkflowDocumentStatus::ReadError,
                content: None,
                content_hash: None,
                diagnostics: vec![diagnostic_from_message(
                    "workflow_symlink_rejected",
                    &format!("{WORKFLOW_FILE_NAME} 不能是符号链接"),
                )],
                preview: None,
            };
        }
        Ok(metadata) if !metadata.is_file() => {
            return WorkflowDocument {
                status: WorkflowDocumentStatus::ReadError,
                content: None,
                content_hash: None,
                diagnostics: vec![diagnostic_from_message(
                    "workflow_not_file",
                    &format!("{WORKFLOW_FILE_NAME} 必须是普通文件"),
                )],
                preview: None,
            };
        }
        Ok(_) => {}
    }

    match fs::read_to_string(&workflow_path) {
        Ok(content) => validate_workflow_content(&content),
        Err(error) => WorkflowDocument {
            status: WorkflowDocumentStatus::ReadError,
            content: None,
            content_hash: None,
            diagnostics: vec![diagnostic_from_message(
                "workflow_read_error",
                &format!("读取 {WORKFLOW_FILE_NAME} 失败: {error}"),
            )],
            preview: None,
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     保存前与编辑中的实时校验必须走后端权威 parser，前端 YAML 提示不能作为最终判定。
///
/// Code Logic（这个函数做什么）:
///     解析 content；成功返回 valid + preview=规范化 content；失败返回 invalid + 带位置的 diagnostics。
pub fn validate_workflow_content(content: &str) -> WorkflowDocument {
    let content_hash = content_sha256_hex(content);
    match parse_project_workflow(content) {
        Ok(_parsed) => WorkflowDocument {
            status: WorkflowDocumentStatus::Valid,
            content: Some(content.to_string()),
            content_hash: Some(content_hash),
            diagnostics: Vec::new(),
            preview: Some(content.to_string()),
        },
        Err(error) => WorkflowDocument {
            status: WorkflowDocumentStatus::Invalid,
            content: Some(content.to_string()),
            content_hash: Some(content_hash),
            diagnostics: vec![diagnostic_from_app_error(&error)],
            preview: None,
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     向导保存必须 CAS 拒绝并发覆盖，原子落盘，且不得 dispatch 或改动 delivery 配置。
///     空 hash 创建路径必须排他：并发第二次创建不得静默 last-write-wins。
///
/// Code Logic（这个函数做什么）:
///     1) 权威校验 content；2) 解析项目根固定 WORKFLOW.md 路径并拒绝 symlink/越界；
///     3) missing 仅允许 expected_hash 为空串创建（`write_new_workflow_file_atomic` 排他落盘）；
///        已有文件比对 SHA-256 后 atomic rename（update-path CAS，不变）；
///     4) 返回保存后的文档快照。本函数不触发 scheduler。
pub fn save_workflow_document_at_project_root(
    project_path: &Path,
    expected_hash: &str,
    content: &str,
) -> Result<WorkflowDocument, AppError> {
    let validated = validate_workflow_content(content);
    if validated.status != WorkflowDocumentStatus::Valid {
        let message = validated
            .diagnostics
            .first()
            .map(|item| item.message.clone())
            .unwrap_or_else(|| format!("{WORKFLOW_FILE_NAME} 内容无效，拒绝保存"));
        return Err(AppError::validation(message));
    }

    let workflow_path = resolve_workflow_document_path(project_path)?;
    ensure_workflow_path_is_safe(&workflow_path)?;

    let expected_hash = expected_hash.trim();
    match fs::symlink_metadata(&workflow_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if !expected_hash.is_empty() {
                return Err(AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE));
            }
            write_new_workflow_file_atomic(&workflow_path, content)?;
        }
        Err(error) => {
            return Err(AppError::generic(format!(
                "读取 {WORKFLOW_FILE_NAME} 元数据失败: {error}"
            )));
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(AppError::validation(format!(
                "{WORKFLOW_FILE_NAME} 不能是符号链接"
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(AppError::validation(format!(
                "{WORKFLOW_FILE_NAME} 必须是普通文件"
            )));
        }
        Ok(_) => {
            if expected_hash.is_empty() {
                return Err(AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE));
            }
            save_text_file_atomic(&workflow_path, content, expected_hash).map_err(|error| {
                if error.to_string().contains("文件已被修改") {
                    AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE)
                } else {
                    error
                }
            })?;
        }
    }

    Ok(load_workflow_document(project_path))
}

/// Business Logic（为什么需要这个函数）:
///     所有文档 API 只能操作项目根下固定的 WORKFLOW.md，禁止任意路径入参导致路径逃逸。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 项目根目录后 join 固定文件名；不接受用户相对路径组件。
fn resolve_workflow_document_path(project_path: &Path) -> Result<PathBuf, AppError> {
    let project_root = fs::canonicalize(project_path).map_err(|error| {
        AppError::validation(format!("项目路径无效，无法解析 WORKFLOW.md: {error}"))
    })?;
    if !project_root.is_dir() {
        return Err(AppError::validation("项目路径必须是目录"));
    }
    Ok(project_root.join(WORKFLOW_FILE_NAME))
}

/// Business Logic（为什么需要这个函数）:
///     即便固定文件名，也要拒绝把 WORKFLOW.md 做成指向项目外的 symlink 后被读取/覆盖。
///
/// Code Logic（这个函数做什么）:
///     若路径存在且是 symlink，返回 validation 错误；否则 Ok。
fn ensure_workflow_path_is_safe(workflow_path: &Path) -> Result<(), AppError> {
    match fs::symlink_metadata(workflow_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AppError::validation(format!(
            "{WORKFLOW_FILE_NAME} 不能是符号链接"
        ))),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// Business Logic（为什么需要这个函数）:
///     首次创建 WORKFLOW.md 要原子写入，避免半截文件被 scheduler 读到；
///     并发 empty-hash 创建必须排他，禁止 Unix rename 静默覆盖已有文件。
///
/// Code Logic（这个函数做什么）:
///     1) 同目录 create_new 写唯一临时文件并 fsync；
///     2) 落盘前 recheck 目标不存在，已存在 → cleanup temp + `workflow_document_changed`；
///     3) 优先 `hard_link(temp, target)` 排他放置（AlreadyExists → conflict），成功后删 temp；
///     4) hard_link 非 AlreadyExists 失败时回退 `create_new` 直写目标（同样 AlreadyExists → conflict）；
///     5) 任意失败路径清理本次 temp（回退写失败时不删除竞争者文件）。
fn write_new_workflow_file_atomic(path: &Path, content: &str) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::generic("WORKFLOW.md 路径缺少父目录"))?;
    let temporary_path = parent.join(format!(
        ".{}.{}.tmp",
        WORKFLOW_FILE_NAME,
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| -> Result<(), AppError> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    // TOCTOU recheck：目标已出现则本创建失败（随后 hard_link/create_new 仍作最终排他屏障）。
    if path.exists() {
        let _ = fs::remove_file(&temporary_path);
        return Err(AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE));
    }

    match fs::hard_link(&temporary_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&temporary_path);
            let _ = sha256_file_hex(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&temporary_path);
            Err(AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE))
        }
        Err(_link_err) => {
            // 跨卷/受限 FS 等 hard_link 不可用时，用 create_new 直写目标保持排他语义。
            match write_new_workflow_file_exclusive_create(path, content) {
                Ok(()) => {
                    let _ = fs::remove_file(&temporary_path);
                    let _ = sha256_file_hex(path)?;
                    Ok(())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&temporary_path);
                    Err(AppError::conflict(WORKFLOW_DOCUMENT_CHANGED_CODE))
                }
                Err(error) => {
                    let _ = fs::remove_file(&temporary_path);
                    Err(AppError::from(error))
                }
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     当 hard_link 在目标 FS 上不可用时，仍需在“目标必须不存在”语义下创建 WORKFLOW.md，
///     避免回退到可覆盖的 rename。
///
/// Code Logic（这个函数做什么）:
///     `OpenOptions::create_new(true)` 排他打开目标并写入 content；
///     中途写失败删除本次创建的目标文件（不碰 AlreadyExists 竞争者）。
fn write_new_workflow_file_exclusive_create(
    path: &Path,
    content: &str,
) -> Result<(), std::io::Error> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    let write_result = (|| -> Result<(), std::io::Error> {
        file.write_all(content.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    write_result
}

/// Business Logic（为什么需要这个函数）:
///     诊断数组需要统一的 path=WORKFLOW.md 与稳定 code，便于前端 i18n 与定位。
///
/// Code Logic（这个函数做什么）:
///     构造无行列号的 diagnostic。
fn diagnostic_from_message(code: &str, message: &str) -> WorkflowDiagnostic {
    WorkflowDiagnostic {
        path: WORKFLOW_FILE_NAME.to_string(),
        line: None,
        column: None,
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     parser 错误需要尽量带上行号，帮助用户定位 front matter 问题。
///
/// Code Logic（这个函数做什么）:
///     从错误文案推断 code；YAML 位置信息当前依赖消息文本，line/column 尽力提取。
fn diagnostic_from_app_error(error: &AppError) -> WorkflowDiagnostic {
    let message = error.to_string();
    let code = if message.contains("front matter 解析失败") {
        "workflow_yaml_parse_error"
    } else if message.contains("结束分隔符") {
        "workflow_front_matter_unclosed"
    } else if message.contains("未知工作流状态") {
        "workflow_unknown_state"
    } else if message.contains("runner.provider") {
        "workflow_runner_provider"
    } else if message.contains("runner.max_turns") {
        "workflow_runner_max_turns"
    } else if message.contains("runner.stall_timeout_ms") {
        "workflow_runner_stall_timeout"
    } else if message.contains("validation.commands") {
        "workflow_validation_commands"
    } else {
        "workflow_invalid"
    };
    WorkflowDiagnostic {
        path: WORKFLOW_FILE_NAME.to_string(),
        line: None,
        column: None,
        code: code.to_string(),
        message,
    }
}

/// Business Logic（为什么需要这个函数）:
///     WORKFLOW.md 需要兼容“只有正文模板”和“YAML front matter + 正文模板”两种写法，降低项目接入成本。
///
/// Code Logic（这个函数做什么）:
///     如果文件首行是 `---`，查找下一行 `---` 作为 front matter 结束；否则整份文件都视为正文。
fn split_workflow_document(content: &str) -> Result<(Option<&str>, &str), AppError> {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = normalized.split_inclusive('\n');
    let Some(first_line) = lines.next() else {
        return Ok((None, ""));
    };

    if first_line.trim_end_matches(['\r', '\n']) != "---" {
        return Ok((None, normalized));
    }

    let front_matter_start = first_line.len();
    let mut cursor = front_matter_start;
    for line in lines {
        let line_end = cursor + line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok((
                Some(&normalized[front_matter_start..cursor]),
                &normalized[line_end..],
            ));
        }
        cursor = line_end;
    }

    Err(AppError::generic(format!(
        "{WORKFLOW_FILE_NAME} 缺少 front matter 结束分隔符 ---"
    )))
}

/// Business Logic（为什么需要这个函数）:
///     项目 WORKFLOW.md 只能覆盖明确允许的策略字段，避免项目文件越权影响自动交付。
///
/// Code Logic（这个函数做什么）:
///     按 section 应用 workflow、runner 和 validation 覆盖项，并对状态、provider 与数值范围做校验。
fn apply_front_matter(
    workflow: &mut ResolvedWorkflow,
    front_matter: WorkflowFrontMatter,
) -> Result<(), AppError> {
    if let Some(workflow_section) = front_matter.workflow {
        apply_workflow_section(workflow, workflow_section)?;
    }
    if let Some(runner_section) = front_matter.runner {
        apply_runner_section(&mut workflow.runner, runner_section)?;
    }
    if let Some(validation_section) = front_matter.validation {
        workflow.validation_commands = normalize_verification_command_items(
            validation_section.commands.unwrap_or_default(),
            &format!("{WORKFLOW_FILE_NAME} validation.commands"),
        )?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     工作流状态覆盖会影响任务看板的创建、活跃、审核和终态分组，必须逐项验证成已知状态。
///
/// Code Logic（这个函数做什么）:
///     将 workflow section 中存在的状态字段转成 OrchestratorWorkflowState 并写入 ResolvedWorkflow。
fn apply_workflow_section(
    workflow: &mut ResolvedWorkflow,
    section: WorkflowSection,
) -> Result<(), AppError> {
    if let Some(default_create_state) = section.default_create_state {
        workflow.default_create_state = workflow_state_from_config(&default_create_state)?;
    }
    if let Some(active_states) = section.active_states {
        workflow.active_states = parse_workflow_states(active_states)?;
    }
    if let Some(review_state) = section.review_state {
        workflow.review_state = workflow_state_from_config(&review_state)?;
    }
    if let Some(terminal_states) = section.terminal_states {
        workflow.terminal_states = parse_workflow_states(terminal_states)?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Runner 覆盖项会影响自动化资源消耗，只能允许当前已实现的可见 Claude Code provider 和受控上限。
///
/// Code Logic（这个函数做什么）:
///     校验 provider 必须为 claudeCodeVisible，max_turns 必须在 1..=20，stall_timeout_ms 必须在安全范围内。
fn apply_runner_section(
    runner: &mut WorkflowRunnerConfig,
    section: RunnerSection,
) -> Result<(), AppError> {
    if let Some(provider) = section.provider {
        let provider = provider.trim();
        if provider != "claudeCodeVisible" {
            return Err(AppError::generic(format!(
                "{WORKFLOW_FILE_NAME} runner.provider 只支持 claudeCodeVisible"
            )));
        }
        runner.provider = provider.to_string();
    }
    if let Some(max_turns) = section.max_turns {
        if !(1..=20).contains(&max_turns) {
            return Err(AppError::generic(format!(
                "{WORKFLOW_FILE_NAME} runner.max_turns 必须在 1..=20"
            )));
        }
        runner.max_turns = max_turns;
    }
    if let Some(stall_timeout_ms) = section.stall_timeout_ms {
        if !(30_000..=MAX_STALL_TIMEOUT_MS).contains(&stall_timeout_ms) {
            return Err(AppError::generic(format!(
                "{WORKFLOW_FILE_NAME} runner.stall_timeout_ms 必须在 30000..={MAX_STALL_TIMEOUT_MS}，收到 {stall_timeout_ms}"
            )));
        }
        runner.stall_timeout_ms = stall_timeout_ms;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     WORKFLOW.md 中的状态列表可能来自用户手写配置，必须支持常见 camelCase/snake_case 别名并拒绝未知状态。
///
/// Code Logic（这个函数做什么）:
///     批量调用 workflow_state_from_config，把字符串列表转换为强类型状态列表。
fn parse_workflow_states(values: Vec<String>) -> Result<Vec<OrchestratorWorkflowState>, AppError> {
    values
        .into_iter()
        .map(|value| workflow_state_from_config(&value))
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     项目配置里用户会用 backlog、inProgress、in_progress 等不同写法表达同一工作流状态，需要统一解析。
///
/// Code Logic（这个函数做什么）:
///     规范化大小写、下划线和连字符后匹配 OrchestratorWorkflowState，未知值返回业务错误。
pub fn workflow_state_from_config(value: &str) -> Result<OrchestratorWorkflowState, AppError> {
    let normalized = value
        .trim()
        .chars()
        .filter(|character| *character != '_' && *character != '-')
        .flat_map(char::to_lowercase)
        .collect::<String>();

    match normalized.as_str() {
        "backlog" => Ok(OrchestratorWorkflowState::Backlog),
        "todo" => Ok(OrchestratorWorkflowState::Todo),
        "inprogress" => Ok(OrchestratorWorkflowState::InProgress),
        "humanreview" => Ok(OrchestratorWorkflowState::HumanReview),
        "rework" => Ok(OrchestratorWorkflowState::Rework),
        "merging" => Ok(OrchestratorWorkflowState::Merging),
        "done" => Ok(OrchestratorWorkflowState::Done),
        "canceled" | "cancelled" => Ok(OrchestratorWorkflowState::Canceled),
        _ => Err(AppError::generic(format!(
            "{WORKFLOW_FILE_NAME} 包含未知工作流状态: {value}"
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     WORKFLOW.md 正文模板需要把任务字段和当前尝试轮次渲染进 Prompt，同时拒绝拼错的变量避免静默执行错误指令。
///
/// Code Logic（这个函数做什么）:
///     逐段扫描 `{{ ... }}` 占位符，支持带空格和紧凑写法；任务字段逐行引用后替换，未知变量返回 AppError。
#[allow(dead_code)]
pub fn render_prompt(
    template: &str,
    task: &PromptTaskContext,
    attempt: i64,
) -> Result<String, AppError> {
    render_prompt_with_worktree(template, task, attempt, None)
}

/// Business Logic（为什么需要这个函数）:
///     普通模板渲染和 Runner 模板渲染共享同一套安全替换逻辑，但只有 Runner 知道具体 worktree 路径。
///
/// Code Logic（这个函数做什么）:
///     扫描 `{{ ... }}` 占位符，替换任务字段、attempt、dev_done_sentinel 和可选 worktree_path；
///     任务字段逐行引用，未知变量或裸独立哨兵行返回 AppError。
fn render_prompt_with_worktree(
    template: &str,
    task: &PromptTaskContext,
    attempt: i64,
    worktree_path: Option<&str>,
) -> Result<String, AppError> {
    let mut rendered = String::with_capacity(template.len() + task.title.len() + task.goal.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        let (before, after_start) = rest.split_at(start);
        rendered.push_str(before);
        let after_open = &after_start[2..];
        let Some(end) = after_open.find("}}") else {
            return Err(AppError::generic("Prompt 模板变量缺少结束分隔符 }}"));
        };
        let variable = after_open[..end].trim();
        let value = match variable {
            "task.title" => render_user_block(&task.title),
            "task.goal" => render_user_block(&task.goal),
            "task.acceptance_criteria" => render_user_block(&task.acceptance_criteria),
            "attempt" => attempt.to_string(),
            "dev_done_sentinel" => DEV_DONE_SENTINEL.to_string(),
            "worktree_path" => sanitize_worktree_path(worktree_path.unwrap_or_default()),
            _ => return Err(AppError::generic(format!("未知模板变量: {variable}"))),
        };
        rendered.push_str(&value);
        rest = &after_open[end + 2..];
    }

    rendered.push_str(rest);
    if contains_standalone_dev_done_sentinel(&rendered) {
        return Err(AppError::generic(
            "Prompt 模板不能包含独立完成哨兵行，请把哨兵写在说明句中",
        ));
    }
    Ok(rendered)
}

/// Business Logic（为什么需要这个函数）:
///     worktree 路径会被写入终端 Prompt，不能因为少见换行路径破坏 guardrail 的行级语义。
///
/// Code Logic（这个函数做什么）:
///     trim 路径并把 CR/LF 压成空格，返回适合嵌入单行 Prompt 的路径文本。
fn sanitize_worktree_path(worktree_path: &str) -> String {
    worktree_path.trim().replace(['\r', '\n'], " ")
}

/// Business Logic（为什么需要这个函数）:
///     项目自定义模板可能只声明业务上下文，Runner 仍需补齐固定执行边界和完成信号协议。
///
/// Code Logic（这个函数做什么）:
///     生成包含 worktree 路径、禁止清理/提交/推送和 ORCHESTRATOR_DEV_DONE 输出条件的标准 guardrail。
fn standard_runner_guardrail(worktree_path: &str) -> String {
    format!(
        "\nRunner 固定执行约束：\n\
1. 当前任务 worktree 路径：`{}`。所有代码修改、测试和验证都必须在这个 worktree 内完成。\n\
2. 不要自行清理、删除、合并当前 worktree，也不要自动提交或推送；保留现场供 Orchestrator/Workbench 接管。\n\
3. 只有在你已经完成代码、更改过的相关测试/验证、并给出必要证据说明后，最后单独输出 `{}`。\n\
4. 未完成代码、未运行必要测试/验证或还没有证据说明时，绝对不要输出 `{}`。\n",
        sanitize_worktree_path(worktree_path),
        DEV_DONE_SENTINEL,
        DEV_DONE_SENTINEL
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Business Logic（为什么需要这个函数）:
    ///     没有 WORKFLOW.md 的项目也必须能创建和运行 Orchestrator 任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在临时空目录解析 workflow，断言来源和关键默认状态符合内置策略。
    #[test]
    fn built_in_workflow_is_valid_without_project_file() {
        let dir = tempdir().expect("创建临时目录成功");
        let resolved = resolve_project_workflow(dir.path()).expect("内置 workflow 可用");
        assert_eq!(resolved.source, WorkflowSource::BuiltInDefault);
        assert_eq!(
            resolved.default_create_state,
            OrchestratorWorkflowState::Backlog
        );
        assert!(resolved
            .active_states
            .contains(&OrchestratorWorkflowState::Todo));
        assert!(resolved
            .active_states
            .contains(&OrchestratorWorkflowState::Rework));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目根 WORKFLOW.md 需要能覆盖验证命令、Runner 限额和 Prompt 正文模板。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入包含 front matter 和正文的 WORKFLOW.md，断言 resolver 合并覆盖项。
    #[test]
    fn project_workflow_overrides_validation_commands_and_prompt() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\nvalidation:\n  commands:\n    - cd web && npx tsc --noEmit\nrunner:\n  stall_timeout_ms: 120000\n---\nCustom prompt for {{ task.title }}",
        )
        .expect("写入 WORKFLOW.md 成功");

        let resolved = resolve_project_workflow(dir.path()).expect("项目 workflow 解析成功");
        assert_eq!(resolved.source, WorkflowSource::ProjectOverride);
        assert_eq!(
            resolved.validation_commands,
            vec!["cd web && npx tsc --noEmit"]
        );
        assert_eq!(resolved.runner.stall_timeout_ms, 120000);
        assert_eq!(
            resolved.prompt_template,
            "Custom prompt for {{ task.title }}"
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW.md 配置写错时必须及时阻断，不能静默落回默认策略导致用户误判自动化行为。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入非法 YAML front matter，断言错误信息指向 WORKFLOW.md。
    #[test]
    fn invalid_front_matter_returns_validation_error() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(dir.path().join("WORKFLOW.md"), "---\n[\n---\nBody")
            .expect("写入 WORKFLOW.md 成功");
        let error = resolve_project_workflow(dir.path()).expect_err("非法 yaml 必须报错");
        assert!(error.to_string().contains("WORKFLOW.md"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW.md 缺少 front matter 结束分隔符时必须明确报错，避免用户以为覆盖策略已生效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入只有开头 `---` 的文件，断言 resolver 返回包含结束分隔符提示的错误。
    #[test]
    fn missing_front_matter_closing_delimiter_returns_error() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\nvalidation:\n  commands: []",
        )
        .expect("写入 WORKFLOW.md 成功");

        let error = resolve_project_workflow(dir.path()).expect_err("缺少结束分隔符必须报错");

        assert!(error.to_string().contains("结束分隔符"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     某些编辑器会在 Markdown 文件开头写入 UTF-8 BOM，resolver 不能因此误判 front matter。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入带 BOM 的 WORKFLOW.md，断言 front matter 和正文照常解析。
    #[test]
    fn bom_prefixed_workflow_file_is_supported() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "\u{feff}---\nvalidation:\n  commands:\n    - cargo check\n---\nBody {{ attempt }}",
        )
        .expect("写入 WORKFLOW.md 成功");

        let resolved = resolve_project_workflow(dir.path()).expect("BOM 文件可解析");

        assert_eq!(resolved.validation_commands, vec!["cargo check"]);
        assert_eq!(resolved.prompt_template, "Body {{ attempt }}");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW.md 可只作为 Prompt 模板使用，不写 front matter 时应保持内置策略并采用正文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入纯正文模板，断言 resolver 不尝试 YAML 解析且替换 prompt_template。
    #[test]
    fn body_only_workflow_file_overrides_prompt_template() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(dir.path().join("WORKFLOW.md"), "Body only {{ task.title }}")
            .expect("写入 WORKFLOW.md 成功");

        let resolved = resolve_project_workflow(dir.path()).expect("纯正文 WORKFLOW.md 可解析");

        assert_eq!(resolved.source, WorkflowSource::ProjectOverride);
        assert_eq!(resolved.prompt_template, "Body only {{ task.title }}");
        assert_eq!(
            resolved.default_create_state,
            OrchestratorWorkflowState::Backlog
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可能先创建只有分隔符和正文的 WORKFLOW.md 草稿，空 front matter 不应阻断内置默认策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入空 front matter 和正文模板，断言 resolver 使用默认配置并采用正文模板。
    #[test]
    fn empty_front_matter_uses_defaults_and_body_prompt() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\n---\nOnly body {{ task.goal }}",
        )
        .expect("写入 WORKFLOW.md 成功");

        let resolved = resolve_project_workflow(dir.path()).expect("空 front matter 可解析");

        assert_eq!(resolved.source, WorkflowSource::ProjectOverride);
        assert_eq!(
            resolved.default_create_state,
            OrchestratorWorkflowState::Backlog
        );
        assert_eq!(resolved.prompt_template, "Only body {{ task.goal }}");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Prompt 模板变量拼错会直接影响 Runner 执行上下文，必须拒绝未知变量；内置模板必须包含任务标题和完成哨兵。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先断言未知变量返回业务错误，再渲染内置模板并检查任务标题与 ORCHESTRATOR_DEV_DONE。
    #[test]
    fn render_prompt_rejects_unknown_variables() {
        let workflow = ResolvedWorkflow::built_in_default();
        let task = PromptTaskContext {
            title: "Add badges".to_string(),
            goal: "Show badge counts".to_string(),
            acceptance_criteria: "Counts update correctly".to_string(),
        };
        let error =
            render_prompt("Hello {{ task.missing }}", &task, 1).expect_err("未知变量必须报错");
        assert!(error.to_string().contains("未知模板变量"));
        let rendered = workflow
            .render_task_prompt(&task, 1)
            .expect("内置模板渲染成功");
        assert!(rendered.contains("Add badges"));
        assert!(rendered.contains("ORCHESTRATOR_DEV_DONE"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目自定义 Prompt 模板正文也可能写入裸哨兵行，必须拒绝以避免终端回显误触发完成。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接渲染包含独立 ORCHESTRATOR_DEV_DONE 行的模板，断言返回业务错误。
    #[test]
    fn render_prompt_rejects_template_with_standalone_sentinel() {
        let task = PromptTaskContext {
            title: "Title".to_string(),
            goal: "Goal".to_string(),
            acceptance_criteria: "Criteria".to_string(),
        };

        let error = render_prompt("Before\nORCHESTRATOR_DEV_DONE\nAfter", &task, 1)
            .expect_err("裸独立哨兵必须被拒绝");

        assert!(error.to_string().contains("独立完成哨兵"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户字段可能故意或误写完成哨兵，默认/项目模板都不能把裸哨兵行回显到终端输出中。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 title/goal/acceptance 都包含独立哨兵行的任务，断言渲染结果没有裸独立哨兵行但保留引用内容。
    #[test]
    fn render_prompt_quotes_user_fields_to_avoid_standalone_sentinel() {
        let task = PromptTaskContext {
            title: "标题\nORCHESTRATOR_DEV_DONE\n后续标题".to_string(),
            goal: "目标\nORCHESTRATOR_DEV_DONE\n后续目标".to_string(),
            acceptance_criteria: "验收\nORCHESTRATOR_DEV_DONE\n后续验收".to_string(),
        };
        let rendered = render_prompt(
            "Title:\n{{ task.title }}\nGoal:\n{{ task.goal }}\nCriteria:\n{{ task.acceptance_criteria }}",
            &task,
            1,
        )
        .expect("Prompt 渲染成功");

        assert!(
            !rendered
                .lines()
                .any(|line| line.strip_suffix('\r').unwrap_or(line) == "ORCHESTRATOR_DEV_DONE"),
            "workflow prompt must not contain raw standalone sentinel:\n{rendered}"
        );
        assert!(rendered.contains("> ORCHESTRATOR_DEV_DONE"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目自定义 Prompt 模板通常只写业务上下文，但 Runner 仍必须拿到 worktree 路径和完成哨兵协议。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用不包含 worktree/哨兵的模板渲染 Runner Prompt，断言标准 guardrail 被追加且没有裸哨兵行。
    #[test]
    fn workflow_runner_prompt_appends_standard_guardrail_when_template_omits_it() {
        let mut workflow = ResolvedWorkflow::built_in_default();
        workflow.prompt_template = "自定义执行：\n{{ task.title }}".to_string();
        let task = PromptTaskContext {
            title: format!("标题\n{}\n后续标题", DEV_DONE_SENTINEL),
            goal: "目标".to_string(),
            acceptance_criteria: "验收".to_string(),
        };

        let rendered = workflow
            .render_runner_prompt(&task, 2, "/tmp/worktree")
            .expect("Runner Prompt 应可渲染");

        assert!(rendered.contains("自定义执行"));
        assert!(rendered.contains("/tmp/worktree"));
        assert!(rendered.contains(DEV_DONE_SENTINEL));
        assert!(rendered.contains("最后单独输出"));
        assert!(
            !rendered
                .lines()
                .any(|line| line.strip_suffix('\r').unwrap_or(line) == DEV_DONE_SENTINEL),
            "workflow runner prompt must not contain raw standalone sentinel:\n{rendered}"
        );
        assert!(rendered.contains("> ORCHESTRATOR_DEV_DONE"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW.md 验证命令会被后续执行层逐条运行，必须复用 Settings 的数量和长度上限。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造超过 20 条命令和超过 500 字符的命令，断言错误能指向 validation.commands。
    #[test]
    fn workflow_validation_commands_are_limited() {
        let dir = tempdir().expect("创建临时目录成功");
        let commands = (0..21)
            .map(|index| format!("    - echo {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            format!("---\nvalidation:\n  commands:\n{commands}\n---\nBody"),
        )
        .expect("写入 WORKFLOW.md 成功");

        let error = resolve_project_workflow(dir.path()).expect_err("超过命令数量必须报错");

        assert!(error
            .to_string()
            .contains("WORKFLOW.md validation.commands"));

        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            format!(
                "---\nvalidation:\n  commands:\n    - {}\n---\nBody",
                "x".repeat(501)
            ),
        )
        .expect("写入 WORKFLOW.md 成功");

        let error = resolve_project_workflow(dir.path()).expect_err("超长命令必须报错");

        assert!(error.to_string().contains("validation.commands[0]"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner stall timeout 是安全限额，项目配置不能设置到极大值导致任务长期悬挂。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入超过上限的 stall_timeout_ms，断言 resolver 返回包含实际值的错误。
    #[test]
    fn workflow_runner_stall_timeout_has_upper_bound() {
        let dir = tempdir().expect("创建临时目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\nrunner:\n  stall_timeout_ms: 1800001\n---\nBody",
        )
        .expect("写入 WORKFLOW.md 成功");

        let error = resolve_project_workflow(dir.path()).expect_err("超大 timeout 必须报错");

        assert!(error.to_string().contains("1800001"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     内置默认模板必须可被权威 parser round-trip，否则向导“从模板创建”会生成不可调度策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 default_workflow_template，断言 active_states 为 Todo/Rework。
    #[test]
    fn generated_template_round_trips_through_authoritative_parser() {
        let template = default_workflow_template();
        let parsed = parse_project_workflow(&template).expect("默认模板必须可解析");
        assert_eq!(
            parsed.active_states,
            vec![
                OrchestratorWorkflowState::Todo,
                OrchestratorWorkflowState::Rework
            ]
        );
        assert_eq!(parsed.source, WorkflowSource::ProjectOverride);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     get 文档 API 必须把缺失、合法、非法三种状态报告给向导。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空目录→missing；合法文件→valid；非法 yaml→invalid。
    #[test]
    fn load_workflow_document_reports_missing_valid_and_invalid() {
        let dir = tempdir().expect("创建临时目录成功");
        let missing = load_workflow_document(dir.path());
        assert_eq!(missing.status, WorkflowDocumentStatus::Missing);
        assert!(missing.preview.is_some());

        fs::write(dir.path().join("WORKFLOW.md"), default_workflow_template())
            .expect("写入合法 WORKFLOW.md");
        let valid = load_workflow_document(dir.path());
        assert_eq!(valid.status, WorkflowDocumentStatus::Valid);
        assert!(valid.content_hash.is_some());
        assert!(valid.diagnostics.is_empty());

        fs::write(dir.path().join("WORKFLOW.md"), "---\n[\n---\nBody").expect("写入非法");
        let invalid = load_workflow_document(dir.path());
        assert_eq!(invalid.status, WorkflowDocumentStatus::Invalid);
        assert!(!invalid.diagnostics.is_empty());
        assert_eq!(invalid.diagnostics[0].path, "WORKFLOW.md");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     CAS 保存必须拒绝 expected hash 漂移，防止向导覆盖外部编辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先用空 hash 创建，再用错误 hash 保存，断言 conflict code。
    #[test]
    fn save_workflow_document_rejects_hash_conflict() {
        let dir = tempdir().expect("创建临时目录成功");
        let template = default_workflow_template();
        save_workflow_document_at_project_root(dir.path(), "", &template).expect("首次创建");

        let err = save_workflow_document_at_project_root(dir.path(), "deadbeef", &template)
            .expect_err("错误 hash 必须冲突");
        assert_eq!(err.to_string(), WORKFLOW_DOCUMENT_CHANGED_CODE);

        let current = load_workflow_document(dir.path());
        let hash = current.content_hash.expect("hash");
        let updated = template.replace("300000", "120000");
        let saved = save_workflow_document_at_project_root(dir.path(), &hash, &updated)
            .expect("正确 hash 应保存");
        assert_eq!(saved.status, WorkflowDocumentStatus::Valid);
        assert_ne!(saved.content_hash.as_deref(), Some(hash.as_str()));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     并发 empty-hash 创建不得静默 last-write-wins，第二次创建必须稳定 Conflict。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 barrier 并发两次合法但不同的模板 empty-hash save；
    ///     断言恰好 1 次 Ok、至少 1 次 `workflow_document_changed`，磁盘内容是两者之一。
    #[test]
    fn save_workflow_document_concurrent_empty_hash_create_is_exclusive() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = tempdir().expect("创建临时目录成功");
        let project = dir.path().to_path_buf();
        let template_a = default_workflow_template();
        let template_b = template_a.replace("300000", "120000");
        parse_project_workflow(&template_a).expect("模板 A 必须可解析");
        parse_project_workflow(&template_b).expect("模板 B 必须可解析");
        assert_ne!(template_a, template_b, "两份模板内容应不同");

        let barrier = Arc::new(Barrier::new(2));
        let project_a = project.clone();
        let project_b = project.clone();
        let content_a = template_a.clone();
        let content_b = template_b.clone();
        let barrier_a = Arc::clone(&barrier);
        let barrier_b = Arc::clone(&barrier);

        let handle_a = thread::spawn(move || {
            barrier_a.wait();
            save_workflow_document_at_project_root(&project_a, "", &content_a)
        });
        let handle_b = thread::spawn(move || {
            barrier_b.wait();
            save_workflow_document_at_project_root(&project_b, "", &content_b)
        });

        let result_a = handle_a.join().expect("线程 A 不得 panic");
        let result_b = handle_b.join().expect("线程 B 不得 panic");
        let results = [result_a, result_b];

        let ok_count = results.iter().filter(|result| result.is_ok()).count();
        let conflict_count = results
            .iter()
            .filter(|result| {
                result
                    .as_ref()
                    .err()
                    .is_some_and(|error| error.to_string() == WORKFLOW_DOCUMENT_CHANGED_CODE)
            })
            .count();

        assert_eq!(
            ok_count, 1,
            "并发 empty-hash 创建必须恰好成功一次: {results:?}"
        );
        assert!(
            conflict_count >= 1,
            "至少一次必须返回 workflow_document_changed: {results:?}"
        );

        let on_disk =
            fs::read_to_string(project.join(WORKFLOW_FILE_NAME)).expect("读取 WORKFLOW.md");
        assert!(
            on_disk == template_a || on_disk == template_b,
            "落盘内容必须是两份合法模板之一"
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW.md 若为 symlink 必须拒绝读写，避免路径逃逸到项目外。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建指向外部文件的 symlink WORKFLOW.md，断言 load 为 readError、save 失败。
    #[test]
    fn workflow_document_rejects_symlink() {
        let dir = tempdir().expect("创建临时目录成功");
        let outside = tempdir().expect("外部目录");
        let target = outside.path().join("external.md");
        fs::write(&target, default_workflow_template()).expect("写外部文件");
        let link = dir.path().join("WORKFLOW.md");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("创建 symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).expect("创建 symlink");

        let loaded = load_workflow_document(dir.path());
        assert_eq!(loaded.status, WorkflowDocumentStatus::ReadError);
        assert!(loaded
            .diagnostics
            .iter()
            .any(|item| item.code == "workflow_symlink_rejected"));

        let err =
            save_workflow_document_at_project_root(dir.path(), "", &default_workflow_template())
                .expect_err("symlink 必须拒绝保存");
        assert!(err.to_string().contains("符号链接"));
    }
}
