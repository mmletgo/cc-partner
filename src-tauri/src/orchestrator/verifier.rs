//! Orchestrator Claude verifier.

use crate::claude_cli;
use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskRow;
use crate::orchestrator::review_diff::{collect_review_diff_for_worktree, render_review_diff_text};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::Path;

const VERIFIER_TIMEOUT_SECS: u64 = 300;
const MAX_DIFF_CONTEXT_BYTES: usize = 96 * 1024;
const DIFF_TRUNCATED_MARKER: &str = "[diff truncated]";
const VERIFIER_REVIEW_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["passed", "reason", "repairPrompt", "riskNotes"],
  "properties": {
    "passed": { "type": "boolean" },
    "reason": { "type": "string" },
    "repairPrompt": { "type": ["string", "null"] },
    "riskNotes": {
      "type": "array",
      "items": { "type": "string" }
    }
  }
}"#;

/// verifier Claude 的结构化审查结果。
///
/// Business Logic（为什么需要这个结构体）:
///     Phase8 需要让 headless Claude 独立裁决任务是否满足目标/验收，并在失败时给出下一轮修复指令。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 对齐 JSON schema；passed 表示是否可交付，reason/riskNotes 用于 evidence，
///     repairPrompt 在 failed 场景必须非空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifierReview {
    pub passed: bool,
    pub reason: String,
    pub repair_prompt: Option<String>,
    pub risk_notes: Vec<String>,
}

/// verifier prompt 输入。
///
/// Business Logic（为什么需要这个结构体）:
///     verifier 需要同时读取任务目标、验收标准、attempt、worktree、验证输出和 diff，才能做交付前裁决。
///
/// Code Logic（这个结构体做什么）:
///     保存 prompt 构造所需的借用字段，避免复制大型验证输出和 diff。
pub struct VerifierPromptInput<'a> {
    pub task: &'a OrchestratorTaskRow,
    pub worktree_path: &'a str,
    pub verification_output: &'a str,
    pub diff: &'a str,
    /// 可选：浏览器验证 evidence 摘要（无 preview 时为 not_applicable 文本）。
    pub browser_verification_note: Option<&'a str>,
}

/// 构造浏览器验证 evidence 摘要供 verifier/落库使用。
///
/// Business Logic（为什么需要这个函数）:
///     验证路径需要把 A5 browser evidence 或 not_applicable 接到 orchestrator，避免 helper 孤岛。
///
/// Code Logic（这个函数做什么）:
///     委托 `prepare_browser_verification_evidence`，返回 content 字符串。
pub fn browser_verification_evidence_note(
    preview_available: bool,
    evidence: Option<&crate::workbench::browser_verification::models::BrowserVerificationEvidence>,
) -> Result<String, AppError> {
    let entry = crate::orchestrator::browser_verification::prepare_browser_verification_evidence(
        preview_available,
        evidence,
    )?;
    Ok(entry.content)
}

/// Business Logic（为什么需要这个函数）:
///     verifier 输出可能来自不同 Claude CLI 版本，既可能是直接 JSON，也可能被 result/structured_output 包装。
///
/// Code Logic（这个函数做什么）:
///     先剥离 markdown fenced code block，再复用 claude_cli 结构化解析；若包装内是 fenced string，则二次剥离并解析；
///     最后校验 passed/reason/repairPrompt 合约。
#[cfg(test)]
fn parse_verifier_review(output: &str) -> Result<VerifierReview, AppError> {
    let stripped = strip_markdown_fenced_code(output);
    match claude_cli::parse_structured_output::<VerifierReview>(&stripped) {
        Ok(review) => validate_verifier_review(review),
        Err(primary_error) => parse_wrapped_fenced_review(&stripped)
            .and_then(validate_verifier_review)
            .map_err(|secondary_error| {
                AppError::generic(format!(
                    "解析 verifier 输出失败: {primary_error}; {secondary_error}"
                ))
            }),
    }
}

/// Business Logic（为什么需要这个函数）:
///     verifier failed 结果如果缺少 repairPrompt，系统无法自动启动下一轮修复，必须转为基础设施失败。
///
/// Code Logic（这个函数做什么）:
///     修剪 reason/riskNotes/repairPrompt；reason 不能为空，failed 时 repairPrompt 必须非空。
fn validate_verifier_review(mut review: VerifierReview) -> Result<VerifierReview, AppError> {
    review.reason = review.reason.trim().to_string();
    if review.reason.is_empty() {
        return Err(AppError::generic("verifier reason 不能为空"));
    }
    review.repair_prompt = review
        .repair_prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    review.risk_notes = review
        .risk_notes
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    if !review.passed && review.repair_prompt.is_none() {
        return Err(AppError::generic(
            "verifier failed review 缺少非空 repairPrompt",
        ));
    }
    Ok(review)
}

/// Business Logic（为什么需要这个函数）:
///     Claude 输出经常把 JSON 放入 ```json fenced code block，直接交给 JSON parser 会失败。
///
/// Code Logic（这个函数做什么）:
///     若整段或首个 fenced code block 存在，则提取 fence 内文本；否则返回 trim 后原文。
#[cfg(test)]
fn strip_markdown_fenced_code(output: &str) -> String {
    let trimmed = output.trim();
    if !trimmed.contains("```") {
        return trimmed.to_string();
    }
    let lines = trimmed.lines().collect::<Vec<_>>();
    if lines
        .first()
        .is_some_and(|line| line.trim_start().starts_with("```"))
    {
        let end = lines
            .iter()
            .rposition(|line| line.trim_start().starts_with("```"))
            .filter(|index| *index > 0)
            .unwrap_or(lines.len());
        return lines[1..end].join("\n").trim().to_string();
    }

    let mut in_fence = false;
    let mut fenced = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            if in_fence {
                break;
            }
            in_fence = true;
            continue;
        }
        if in_fence {
            fenced.push(line);
        }
    }
    if fenced.is_empty() {
        trimmed.to_string()
    } else {
        fenced.join("\n").trim().to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     部分 CLI 包装会把 result/structured_output 写成字符串，且字符串内部仍可能是 fenced JSON。
///
/// Code Logic（这个函数做什么）:
///     手动解析外层 JSON，抽取 result/structured_output 字段；对象直接反序列化，字符串剥 fence 后递归走结构化解析。
#[cfg(test)]
fn parse_wrapped_fenced_review(output: &str) -> Result<VerifierReview, AppError> {
    let value: serde_json::Value = serde_json::from_str(output)?;
    for key in ["structured_output", "result"] {
        let Some(inner) = value.get(key) else {
            continue;
        };
        if inner.is_object() {
            return Ok(serde_json::from_value::<VerifierReview>(inner.clone())?);
        }
        if let Some(text) = inner.as_str() {
            let stripped = strip_markdown_fenced_code(text);
            return claude_cli::parse_structured_output::<VerifierReview>(&stripped);
        }
    }
    Err(AppError::generic(
        "verifier 输出缺少可解析的 structured_output/result",
    ))
}

/// Business Logic（为什么需要这个函数）:
///     verifier prompt 中的任务文本来自用户输入，需要降低 prompt 注入和哨兵回显风险。
///
/// Code Logic（这个函数做什么）:
///     对文本 trim 后逐行加 Markdown 引用前缀；空值输出占位。
fn render_quoted_block(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "> （未填写）".to_string();
    }
    trimmed
        .lines()
        .map(|line| format!("> {}", line.strip_suffix('\r').unwrap_or(line)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Business Logic（为什么需要这个函数）:
///     headless verifier Claude 必须拿到完整任务边界、验证输出和 diff，并被明确要求失败时给下一轮修复 prompt。
///
/// Code Logic（这个函数做什么）:
///     拼接结构化中文审查指令；要求只返回符合 schema 的 JSON，passed=false 时 repairPrompt 必须可直接交给开发 Claude。
pub fn build_verifier_prompt(input: &VerifierPromptInput<'_>) -> String {
    let browser_section = input
        .browser_verification_note
        .map(|note| format!("\n\n浏览器验证 evidence：\n```json\n{}\n```", note.trim()))
        .unwrap_or_default();
    format!(
        "你是 cc-partner Orchestrator 的交付前 verifier。请只根据当前任务目标、验收标准、验证命令输出和 worktree diff 判断任务是否可以交付。\n\n\
任务标题：\n{}\n\n\
任务目标：\n{}\n\n\
验收标准：\n{}\n\n\
Attempt：{}\n\n\
Worktree：{}\n\n\
验证命令输出：\n```text\n{}\n```\n\n\
Worktree diff/context：\n```diff\n{}\n```{}
\n\n\
裁决要求：\n\
1. 如果实现已经满足任务目标和验收标准，即使验证命令被跳过，也可以返回 passed=true，但 reason 必须说明判断依据。\n\
2. 如果验证命令失败、diff 显示实现不完整、缺测试或存在明显风险，返回 passed=false。\n\
3. passed=false 时 repairPrompt 必须非空，且必须是可以直接交给下一轮开发 Claude 的具体修复指令。\n\
4. riskNotes 填写需要人工注意的风险数组；没有则返回 []。\n\
5. 只返回 JSON：{{\"passed\": boolean, \"reason\": string, \"repairPrompt\": string|null, \"riskNotes\": string[]}}。",
        render_quoted_block(&input.task.title),
        render_quoted_block(&input.task.goal),
        render_quoted_block(&input.task.acceptance_criteria),
        input.task.attempt,
        input.worktree_path.trim(),
        input.verification_output.trim(),
        input.diff.trim(),
        browser_section,
    )
}

/// Business Logic（为什么需要这个函数）:
///     completion pipeline 需要在 Verifying 阶段调用 headless Claude 做最终裁决，且复用用户配置的 Claude CLI 路径和模型。
///
/// Code Logic（这个函数做什么）:
///     从 AppConfig.github_trending 读取 claude_cli_path/claude_model，在 worktree cwd 下运行项目上下文 JSON schema 调用，
///     然后校验 VerifierReview 合约。
pub async fn run_verifier_claude(
    state: &AppState,
    task: &OrchestratorTaskRow,
    cwd: &Path,
    verification_output: &str,
    diff: &str,
) -> Result<VerifierReview, AppError> {
    let (cli_path, model) = {
        let config = state.config.read().expect("config 读锁中毒");
        (
            config.github_trending.claude_cli_path.clone(),
            config.github_trending.claude_model.clone(),
        )
    };
    let worktree_path = cwd.to_string_lossy().to_string();
    // 非 Web 任务默认无 preview：记 not_applicable，不阻塞 verifier
    let browser_note = browser_verification_evidence_note(false, None).unwrap_or_default();
    let input = VerifierPromptInput {
        task,
        worktree_path: &worktree_path,
        verification_output,
        diff,
        browser_verification_note: Some(browser_note.as_str()),
    };
    let prompt = build_verifier_prompt(&input);
    let review = claude_cli::run_structured_json_with_cwd::<VerifierReview>(
        &cli_path,
        &model,
        VERIFIER_REVIEW_SCHEMA,
        &prompt,
        Some(cwd),
        VERIFIER_TIMEOUT_SECS,
        "verifier",
    )
    .await?;
    validate_verifier_review(review)
}

/// Business Logic（为什么需要这个函数）:
///     verifier 需要看到 worktree 的真实改动范围，尤其是验证命令失败时用于判断是否应继续修复。
///
/// Code Logic（这个函数做什么）:
///     复用 review_diff 有界 snapshot（staged/unstaged/untracked/unborn 同一语义），渲染为文本后再按
///     verifier prompt 全局字节上限截断并标记。
pub fn collect_worktree_diff(cwd: &Path) -> Result<String, AppError> {
    let snapshot = collect_review_diff_for_worktree("verifier", cwd, None)?;
    let context = render_review_diff_text(&snapshot);
    Ok(truncate_diff_context(&context, MAX_DIFF_CONTEXT_BYTES))
}

/// Business Logic（为什么需要这个函数）:
///     大型 diff 不能无限写入 Claude prompt，否则会拖慢 verifier 或超过上下文限制。
///
/// Code Logic（这个函数做什么）:
///     按 UTF-8 边界截断到 max_bytes，并追加固定 `[diff truncated]` marker。
fn truncate_diff_context(diff: &str, max_bytes: usize) -> String {
    if diff.len() <= max_bytes {
        return diff.to_string();
    }
    let prefix = truncate_utf8_content(diff, max_bytes);
    format!(
        "{}\n{} omitted_bytes={}",
        prefix,
        DIFF_TRUNCATED_MARKER,
        diff.len().saturating_sub(prefix.len())
    )
}

/// Business Logic（为什么需要这个函数）:
///     diff 和未跟踪文件片段都需要按字节预算截断，但不能切断 UTF-8 字符导致 prompt 出现乱码。
///
/// Code Logic（这个函数做什么）:
///     返回不超过 max_bytes 的 UTF-8 前缀；max_bytes 覆盖完整内容时返回原文拷贝。
fn truncate_utf8_content(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (index, ch) in value.char_indices() {
        let next = index + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};

    /// Business Logic（为什么需要这个函数）:
    ///     verifier prompt 测试需要构造完整任务行，确保标题、目标和验收标准都能进入审查上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回字段完整的 Verifying 任务 Row，调用方用它构造 VerifierPromptInput。
    fn verifier_task_row() -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: "task-verifier".to_string(),
            project_id: "project-1".to_string(),
            title: "修复自动交付".to_string(),
            goal: "验证命令失败后应进入自动修复循环".to_string(),
            acceptance_criteria: "失败时生成 repairPrompt；通过时进入 delivery".to_string(),
            status: OrchestratorTaskStatus::Verifying,
            priority: 0,
            branch_name: Some("agent/task-verifier".to_string()),
            worktree_id: Some("worktree-1".to_string()),
            session_id: Some("session-1".to_string()),
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 2,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: Some("2026-07-05T00:00:00Z".to_string()),
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Verifying)
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 通过时系统需要信任结构化 JSON 的 passed=true，并继续进入交付。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析直接 JSON，断言 passed/reason/riskNotes 字段按 camelCase 还原。
    #[test]
    fn parse_verifier_review_accepts_direct_pass() {
        let review = parse_verifier_review(
            r#"{"passed":true,"reason":"满足验收","repairPrompt":null,"riskNotes":["低风险"]}"#,
        )
        .expect("review");

        assert!(review.passed);
        assert_eq!(review.reason, "满足验收");
        assert_eq!(review.risk_notes, vec!["低风险".to_string()]);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude CLI 可能把结构化结果放在 structured_output 或 result 包装中，verifier 解析必须兼容。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 structured_output 包装的 failed JSON，断言 repairPrompt 被保留给下一轮修复 Claude。
    #[test]
    fn parse_verifier_review_accepts_wrapped_failed_review() {
        let review = parse_verifier_review(
            r#"{"structured_output":{"passed":false,"reason":"测试仍失败","repairPrompt":"修复失败测试","riskNotes":[]}}"#,
        )
        .expect("review");

        assert!(!review.passed);
        assert_eq!(review.repair_prompt.as_deref(), Some("修复失败测试"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 判定失败但不给修复指令时，系统无法启动下一轮开发 Claude，必须按基础设施失败阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析缺失 repairPrompt 的 failed JSON，断言返回错误。
    #[test]
    fn parse_verifier_review_rejects_failed_review_without_repair_prompt() {
        let error = parse_verifier_review(
            r#"{"passed":false,"reason":"测试仍失败","repairPrompt":"   ","riskNotes":[]}"#,
        )
        .expect_err("missing repair prompt must fail");

        assert!(error.to_string().contains("repairPrompt"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 输出损坏时不能误判通过或启动修复，任务应进入 Blocked 等人工介入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     传入非 JSON 文本，断言 parser 返回错误。
    #[test]
    fn parse_verifier_review_rejects_malformed_json() {
        let error = parse_verifier_review("not json").expect_err("malformed output must fail");

        assert!(error.to_string().contains("JSON") || error.to_string().contains("expected"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude 模型偶尔会把 JSON 包在 markdown fenced code block 中，解析器应剥掉 fence 后再校验。
    ///
    /// Code Logic（这个函数做什么）:
    ///     传入 ```json fenced JSON，断言仍能解析为 passed review。
    #[test]
    fn parse_verifier_review_strips_markdown_fence() {
        let review = parse_verifier_review(
            "```json\n{\"passed\":true,\"reason\":\"ok\",\"repairPrompt\":null,\"riskNotes\":[]}\n```",
        )
        .expect("fenced review");

        assert!(review.passed);
        assert_eq!(review.reason, "ok");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 必须同时看到任务目标、验收标准、验证输出和 diff，才能判断任务是否真的满足要求。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 prompt input 并断言生成 prompt 包含关键上下文。
    #[test]
    fn verifier_prompt_includes_goal_acceptance_output_and_diff() {
        let task = verifier_task_row();
        let input = VerifierPromptInput {
            task: &task,
            worktree_path: "/repo/worktree",
            verification_output: "$ cargo test\nexit: 101\nfailed",
            browser_verification_note: None,
            diff: "diff --git a/src/lib.rs b/src/lib.rs",
        };

        let prompt = build_verifier_prompt(&input);

        assert!(prompt.contains(&task.goal));
        assert!(prompt.contains(&task.acceptance_criteria));
        assert!(prompt.contains("exit: 101"));
        assert!(prompt.contains("diff --git"));
        assert!(prompt.contains("repairPrompt"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     worktree diff 可能很大，verifier prompt 需要有上限并明确告知内容已截断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用纯截断 helper，断言输出长度被限制且包含 truncation marker。
    #[test]
    fn diff_context_truncation_marks_truncated_content() {
        let diff = "中".repeat(512);

        let truncated = truncate_diff_context(&diff, 64);

        assert!(truncated.len() < diff.len());
        assert!(truncated.contains("[diff truncated]"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 必须能审查已经 staged 的改动和新增未跟踪文件，否则自动修复循环会在常见 Claude 工作流下误判。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建临时 git repo，制造 staged 修改和 untracked 文件，断言 collect_worktree_diff 同时包含两类内容。
    #[test]
    fn collect_worktree_diff_includes_staged_and_untracked_changes() {
        use std::fs;
        use std::process::Command as StdCommand;

        let dir = tempfile::tempdir().expect("tempdir");
        let run_git_test_command = |cwd: &Path, args: &[&str]| {
            let output = StdCommand::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("run git");
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git_test_command(dir.path(), &["init"]);
        fs::write(dir.path().join("README.md"), "base\n").expect("write readme");
        run_git_test_command(dir.path(), &["add", "README.md"]);
        run_git_test_command(
            dir.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );
        fs::write(dir.path().join("README.md"), "base\nstaged line\n").expect("stage change");
        run_git_test_command(dir.path(), &["add", "README.md"]);
        fs::write(
            dir.path().join("generated.rs"),
            "pub fn generated() -> bool { true }\n",
        )
        .expect("write generated");

        let context = collect_worktree_diff(dir.path()).expect("diff context");

        assert!(context.contains("review-diff snapshot"));
        assert!(context.contains("+staged line"));
        assert!(context.contains("generated.rs"));
        assert!(context.contains("pub fn generated() -> bool"));
    }
}
