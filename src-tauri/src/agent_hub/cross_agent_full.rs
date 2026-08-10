//! agent_hub/cross_agent_full — 全量跨 Agent 适配兼容边界
//!
//! Business Logic（为什么需要这个模块）:
//!     当前 full runner 仍是 stub，不能向用户提供可能被误解为可执行方案的预览，
//!     更不能写入真实 CLI 配置。保留类型与入口仅用于混合版本兼容。
//!
//! Code Logic（这个模块做什么）:
//!     生产 preview/apply 入口固定 fail-closed；内部 runner 仅保留确定性单测与未来实现脚手架。

use crate::agent_hub::cross_agent::{
    preview_cross_agent_instruction, preview_cross_agent_plugin_residual, CrossAgentKind,
    PreviewCrossAgentInstructionRequest,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::targets::TargetEnvironment;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// 全量适配 runner 标识（生产日后可切 Claude headless）。
pub const FULL_ADAPT_GENERATOR_STUB: &str = "stub";

/// 源侧 portable 资产引用（扫描或调用方注入）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullPortableRef {
    pub kind: CrossAgentKind,
    pub logical_key: String,
    pub path: String,
}

/// Runner 输入：源 Agent 当前 scope 快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullSnapshot {
    pub source: AgentTarget,
    pub destination: AgentTarget,
    pub scope: String,
    pub source_markdown: String,
    pub portable_assets: Vec<CrossAgentFullPortableRef>,
}

/// 全量 plan 单项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullPlanItem {
    pub kind: CrossAgentKind,
    pub logical_key: String,
    /// create | update | skip
    pub action: String,
    pub path: String,
    pub content: Option<String>,
    /// 指令项保存 selective preview hash，阻止 full apply 在重建后接受新的磁盘状态。
    #[serde(default)]
    pub preview_hash: Option<String>,
    /// 预览时目标内容 hash；用于 full plan 对外部编辑做 CAS 绑定。
    #[serde(default)]
    pub observed_hash: Option<String>,
    pub residual_reason: Option<String>,
    /// 用户可在预览中关闭
    pub included: bool,
}

/// 全量适配方案。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullPlan {
    pub source: AgentTarget,
    pub destination: AgentTarget,
    pub scope: String,
    pub items: Vec<CrossAgentFullPlanItem>,
    pub plan_hash: String,
    /// stub | claude（当前仅 stub）
    pub generator: String,
}

/// preview 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCrossAgentFullRequest {
    pub source: AgentTarget,
    pub destination: AgentTarget,
    /// "user" 或项目标识
    pub scope: String,
    pub source_markdown: String,
    /// 可选：注入 portable 清单（测试 / 前端缓存）；空则本机扫描
    #[serde(default)]
    pub portable_assets: Vec<CrossAgentFullPortableRef>,
    /// 非空 peer device id → 拒绝（同机 only）
    #[serde(default)]
    pub device_id: Option<String>,
}

/// apply 时用户对单项的勾选。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullApplySelection {
    pub logical_key: String,
    pub included: bool,
}

/// apply 请求（必须带 preview 的 plan_hash）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyCrossAgentFullRequest {
    pub source: AgentTarget,
    pub destination: AgentTarget,
    pub scope: String,
    pub source_markdown: String,
    pub plan_hash: String,
    pub client_request_id: String,
    /// 预览后用户勾选；logical_key 须存在于 re-propose plan
    pub items: Vec<CrossAgentFullApplySelection>,
    #[serde(default)]
    pub portable_assets: Vec<CrossAgentFullPortableRef>,
    #[serde(default)]
    pub device_id: Option<String>,
}

/// apply 单项结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentFullApplyItemResult {
    pub kind: CrossAgentKind,
    pub logical_key: String,
    /// applied | skipped | blocked | failed
    pub status: String,
    pub path: String,
    pub error_code: Option<String>,
}

/// Full adapt runner：snapshot → plan。
///
/// Business Logic: 生产可替换为 Claude Code headless；测试与当前默认用 stub。
/// Code Logic: propose 必须确定性（同 snapshot → 同 plan_hash）。
pub trait FullAdaptRunner {
    fn propose(
        &self,
        snapshot: &CrossAgentFullSnapshot,
        env: &TargetEnvironment,
    ) -> Result<CrossAgentFullPlan, AppError>;
}

/// 确定性 stub runner：指令走现有 cross_agent 编译；portable 列清单但 residual/skip。
#[derive(Debug, Default, Clone, Copy)]
pub struct StubFullAdaptRunner;

impl FullAdaptRunner for StubFullAdaptRunner {
    fn propose(
        &self,
        snapshot: &CrossAgentFullSnapshot,
        env: &TargetEnvironment,
    ) -> Result<CrossAgentFullPlan, AppError> {
        validate_source_destination(snapshot.source, snapshot.destination)?;

        let mut items: Vec<CrossAgentFullPlanItem> = Vec::new();

        // 1) Instruction item — reuse selective preview for path/content
        let instr_preview = preview_cross_agent_instruction(
            &PreviewCrossAgentInstructionRequest {
                source: snapshot.source,
                destinations: vec![snapshot.destination],
                source_markdown: snapshot.source_markdown.clone(),
                scope: snapshot.scope.clone(),
                destination_paths: BTreeMap::new(),
            },
            env,
        )?;
        let dest_row = instr_preview
            .destinations
            .first()
            .ok_or_else(|| AppError::validation("CROSS_AGENT_FULL_INSTRUCTION_PREVIEW_EMPTY"))?;
        let dest_path = dest_row.path.clone();
        let dest_exists = PathBuf::from(&dest_path).exists();
        let action = if dest_exists { "update" } else { "create" };
        let content = if dest_row.can_apply {
            // 用 rendered 路径再取编译正文：content 存预览 markdown（hash 确定性）
            let before = std::fs::read_to_string(&dest_path).unwrap_or_default();
            // 从 diff 无法还原；重新 compile 路径在 apply 时执行。此处存 source 适配后标记
            let _ = before;
            Some(snapshot.source_markdown.clone())
        } else {
            None
        };
        let residual = if dest_row.can_apply {
            None
        } else {
            Some(
                dest_row
                    .partial_blockers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "instruction_cannot_apply".into()),
            )
        };
        items.push(CrossAgentFullPlanItem {
            kind: CrossAgentKind::Instruction,
            logical_key: "instruction:user".into(),
            action: if residual.is_some() {
                "skip".into()
            } else {
                action.into()
            },
            path: dest_path,
            content,
            preview_hash: Some(instr_preview.plan_hash.clone()),
            observed_hash: dest_row.observed_hash.clone(),
            residual_reason: residual,
            included: true,
        });

        // 2) Portable assets from snapshot — stub residual skip (copy not wired)
        let mut seen_kinds: BTreeSet<CrossAgentKind> = BTreeSet::new();
        seen_kinds.insert(CrossAgentKind::Instruction);
        for asset in &snapshot.portable_assets {
            if matches!(asset.kind, CrossAgentKind::Instruction) {
                continue;
            }
            seen_kinds.insert(asset.kind);
            let residual_reason = match asset.kind {
                CrossAgentKind::Plugin => {
                    let row =
                        preview_cross_agent_plugin_residual(snapshot.source, snapshot.destination);
                    row.partial_blockers
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "plugin_residual".into())
                }
                CrossAgentKind::Skill => "stub:skill_copy_not_ready".to_string(),
                CrossAgentKind::Command => "stub:command_copy_not_ready".to_string(),
                CrossAgentKind::Mcp => "stub:mcp_copy_not_ready".to_string(),
                CrossAgentKind::Instruction => continue,
            };
            items.push(CrossAgentFullPlanItem {
                kind: asset.kind,
                logical_key: asset.logical_key.clone(),
                action: "skip".into(),
                path: asset.path.clone(),
                content: None,
                preview_hash: None,
                observed_hash: None,
                residual_reason: Some(residual_reason),
                included: true,
            });
        }

        // 3) Ensure five kinds appear in inventory even when source empty
        for kind in [
            CrossAgentKind::Skill,
            CrossAgentKind::Command,
            CrossAgentKind::Mcp,
            CrossAgentKind::Plugin,
        ] {
            if seen_kinds.contains(&kind) {
                continue;
            }
            items.push(CrossAgentFullPlanItem {
                kind,
                logical_key: format!("inventory:empty:{}", kind_wire(kind)),
                action: "skip".into(),
                path: String::new(),
                content: None,
                preview_hash: None,
                observed_hash: None,
                residual_reason: Some(format!("no_{}_on_source", kind_wire(kind))),
                included: false,
            });
            seen_kinds.insert(kind);
        }

        // Stable order: kind ordinal then logical_key
        items.sort_by(|a, b| {
            kind_ord(a.kind)
                .cmp(&kind_ord(b.kind))
                .then_with(|| a.logical_key.cmp(&b.logical_key))
        });

        let plan_hash = compute_plan_hash(
            snapshot.source,
            snapshot.destination,
            &snapshot.scope,
            &items,
        );

        Ok(CrossAgentFullPlan {
            source: snapshot.source,
            destination: snapshot.destination,
            scope: snapshot.scope.clone(),
            items,
            plan_hash,
            generator: FULL_ADAPT_GENERATOR_STUB.into(),
        })
    }
}

/// 预览全量适配 plan（强制预览入口）。
///
/// Business Logic: 无 skip-preview；peer 拒绝。
/// Code Logic: 校验 → 收集 portable → runner.propose。
pub fn preview_cross_agent_full(
    _request: &PreviewCrossAgentFullRequest,
    _env: &TargetEnvironment,
    _runner: &dyn FullAdaptRunner,
) -> Result<CrossAgentFullPlan, AppError> {
    Err(AppError::validation("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"))
}

/// 全量 apply 兼容入口（生产生成器与真实写盘认证前固定阻止）。
///
/// Business Logic: 当前 full runner 是 stub，且真实 CLI 写盘未认证；旧调用方即使持有历史
///     plan hash 也不能把 stub 预览升级为文件 mutation。
/// Code Logic: 保留签名用于 N/N+1 兼容，在任何扫描、re-propose 或 writer 前返回稳定错误码。
pub fn apply_cross_agent_full(
    _request: &ApplyCrossAgentFullRequest,
    _env: &TargetEnvironment,
    _runner: &dyn FullAdaptRunner,
) -> Result<Vec<CrossAgentFullApplyItemResult>, AppError> {
    Err(AppError::validation("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"))
}

/// 使用默认 stub runner 的 preview 便捷入口（Tauri command）。
pub fn preview_cross_agent_full_default(
    request: &PreviewCrossAgentFullRequest,
    env: &TargetEnvironment,
) -> Result<CrossAgentFullPlan, AppError> {
    preview_cross_agent_full(request, env, &StubFullAdaptRunner)
}

/// 使用默认 stub runner 的 apply 便捷入口（Tauri command）。
pub fn apply_cross_agent_full_default(
    request: &ApplyCrossAgentFullRequest,
    env: &TargetEnvironment,
) -> Result<Vec<CrossAgentFullApplyItemResult>, AppError> {
    apply_cross_agent_full(request, env, &StubFullAdaptRunner)
}

// ── helpers ──────────────────────────────────────────────────────────

fn validate_source_destination(
    source: AgentTarget,
    destination: AgentTarget,
) -> Result<(), AppError> {
    if source == destination {
        return Err(AppError::validation("CROSS_AGENT_FULL_DEST_EQUALS_SOURCE"));
    }
    Ok(())
}

fn kind_wire(kind: CrossAgentKind) -> &'static str {
    match kind {
        CrossAgentKind::Instruction => "instruction",
        CrossAgentKind::Skill => "skill",
        CrossAgentKind::Command => "command",
        CrossAgentKind::Mcp => "mcp",
        CrossAgentKind::Plugin => "plugin",
    }
}

fn kind_ord(kind: CrossAgentKind) -> u8 {
    match kind {
        CrossAgentKind::Instruction => 0,
        CrossAgentKind::Skill => 1,
        CrossAgentKind::Command => 2,
        CrossAgentKind::Mcp => 3,
        CrossAgentKind::Plugin => 4,
    }
}

/// plan_hash 不含 `included` / `plan_hash` 本身，保证用户勾选不改 hash。
fn compute_plan_hash(
    source: AgentTarget,
    destination: AgentTarget,
    scope: &str,
    items: &[CrossAgentFullPlanItem],
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "source={}|dest={}|scope={}",
        source.as_str(),
        destination.as_str(),
        scope
    ));
    for item in items {
        lines.push(format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            kind_wire(item.kind),
            item.logical_key,
            item.action,
            item.path,
            item.content.as_deref().unwrap_or(""),
            item.preview_hash.as_deref().unwrap_or(""),
            item.observed_hash.as_deref().unwrap_or("<missing>"),
            item.residual_reason.as_deref().unwrap_or(""),
        ));
    }
    sha256_hex(lines.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_env(home: &Path) -> TargetEnvironment {
        let mut vars = BTreeMap::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into(),
        );
        vars.insert(
            "CODEX_HOME".into(),
            home.join(".codex").to_string_lossy().into(),
        );
        vars.insert(
            "OPENCODE_CONFIG_DIR".into(),
            home.join(".opencode").to_string_lossy().into(),
        );
        TargetEnvironment {
            home: home.to_path_buf(),
            vars,
            path_entries: vec![],
        }
    }

    #[test]
    fn stub_runner_returns_five_kinds_and_stable_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());

        let snapshot = CrossAgentFullSnapshot {
            source: AgentTarget::Claude,
            destination: AgentTarget::Codex,
            scope: "user".into(),
            source_markdown: "Always run tests before commit.\n".into(),
            portable_assets: vec![CrossAgentFullPortableRef {
                kind: CrossAgentKind::Skill,
                logical_key: "skill:demo".into(),
                path: tmp
                    .path()
                    .join(".claude/skills/demo")
                    .to_string_lossy()
                    .into(),
            }],
        };
        let plan1 = StubFullAdaptRunner.propose(&snapshot, &env).unwrap();
        let plan2 = StubFullAdaptRunner.propose(&snapshot, &env).unwrap();
        assert_eq!(plan1.plan_hash, plan2.plan_hash);
        assert_eq!(plan1.generator, FULL_ADAPT_GENERATOR_STUB);
        assert!(plan1
            .items
            .iter()
            .any(|i| i.kind == CrossAgentKind::Instruction));
        assert!(plan1.items.iter().any(|i| i.kind == CrossAgentKind::Skill));
        assert!(plan1
            .items
            .iter()
            .any(|i| i.kind == CrossAgentKind::Command));
        assert!(plan1.items.iter().any(|i| i.kind == CrossAgentKind::Mcp));
        assert!(plan1.items.iter().any(|i| i.kind == CrossAgentKind::Plugin));
        // skill demo is residual skip
        let skill = plan1
            .items
            .iter()
            .find(|i| i.logical_key == "skill:demo")
            .unwrap();
        assert_eq!(skill.action, "skip");
        assert!(skill.residual_reason.is_some());
    }

    #[test]
    fn apply_is_unavailable_even_without_plan_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());

        let err = apply_cross_agent_full(
            &ApplyCrossAgentFullRequest {
                source: AgentTarget::Claude,
                destination: AgentTarget::Codex,
                scope: "user".into(),
                source_markdown: "Always run tests.\n".into(),
                plan_hash: String::new(),
                client_request_id: "req-1".into(),
                items: vec![],
                portable_assets: vec![],
                device_id: None,
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
    }

    #[test]
    fn apply_is_unavailable_even_with_historical_plan_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());

        let err = apply_cross_agent_full(
            &ApplyCrossAgentFullRequest {
                source: AgentTarget::Claude,
                destination: AgentTarget::Codex,
                scope: "user".into(),
                source_markdown: "Always run tests.\n".into(),
                plan_hash: "deadbeef".into(),
                client_request_id: "req-1".into(),
                items: vec![],
                portable_assets: vec![],
                device_id: None,
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
    }

    #[test]
    fn production_preview_is_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());

        let error = preview_cross_agent_full(
            &PreviewCrossAgentFullRequest {
                source: AgentTarget::Claude,
                destination: AgentTarget::Codex,
                scope: "user".into(),
                source_markdown: "Always run tests before commit.\n".into(),
                portable_assets: vec![],
                device_id: None,
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
    }

    #[test]
    fn full_apply_rejects_target_changed_after_preview() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());
        let preview_request = PreviewCrossAgentFullRequest {
            source: AgentTarget::Claude,
            destination: AgentTarget::Codex,
            scope: "user".into(),
            source_markdown: "Always run tests.\n".into(),
            portable_assets: vec![],
            device_id: None,
        };
        let plan = StubFullAdaptRunner
            .propose(
                &CrossAgentFullSnapshot {
                    source: preview_request.source,
                    destination: preview_request.destination,
                    scope: preview_request.scope.clone(),
                    source_markdown: preview_request.source_markdown.clone(),
                    portable_assets: vec![],
                },
                &env,
            )
            .unwrap();
        let instruction = plan
            .items
            .iter()
            .find(|item| item.kind == CrossAgentKind::Instruction)
            .unwrap();
        let instruction_path = instruction.path.clone();
        let instruction_key = instruction.logical_key.clone();
        fs::write(&instruction_path, "external edit\n").unwrap();

        let error = apply_cross_agent_full(
            &ApplyCrossAgentFullRequest {
                source: preview_request.source,
                destination: preview_request.destination,
                scope: preview_request.scope,
                source_markdown: preview_request.source_markdown,
                plan_hash: plan.plan_hash,
                client_request_id: "req-full-stale".into(),
                items: vec![CrossAgentFullApplySelection {
                    logical_key: instruction_key,
                    included: true,
                }],
                portable_assets: vec![],
                device_id: None,
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
        assert_eq!(
            fs::read_to_string(instruction_path).unwrap(),
            "external edit\n"
        );
    }

    #[test]
    fn full_project_scope_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = temp_env(tmp.path());
        let error = preview_cross_agent_full(
            &PreviewCrossAgentFullRequest {
                source: AgentTarget::Claude,
                destination: AgentTarget::Codex,
                scope: "project:demo".into(),
                source_markdown: "Always run tests.\n".into(),
                portable_assets: vec![],
                device_id: None,
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
    }

    #[test]
    fn peer_device_id_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = temp_env(tmp.path());
        let err = preview_cross_agent_full(
            &PreviewCrossAgentFullRequest {
                source: AgentTarget::Claude,
                destination: AgentTarget::Codex,
                scope: "user".into(),
                source_markdown: "hi".into(),
                portable_assets: vec![],
                device_id: Some("peer-1".into()),
            },
            &env,
            &StubFullAdaptRunner,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"));
    }

    #[test]
    fn ipc_request_deserializes_without_optional_fields() {
        let preview_json = r#"{
            "source": "claude",
            "destination": "codex",
            "scope": "user",
            "sourceMarkdown": "Always run tests."
        }"#;
        let preview: PreviewCrossAgentFullRequest =
            serde_json::from_str(preview_json).expect("preview defaults");
        assert!(preview.portable_assets.is_empty());
        assert!(preview.device_id.is_none());

        let apply_json = r#"{
            "source": "claude",
            "destination": "codex",
            "scope": "user",
            "sourceMarkdown": "Always run tests.",
            "planHash": "abc",
            "clientRequestId": "req-1",
            "items": [{"logicalKey": "instruction:user", "included": true}]
        }"#;
        let apply: ApplyCrossAgentFullRequest =
            serde_json::from_str(apply_json).expect("apply defaults");
        assert_eq!(apply.plan_hash, "abc");
        assert_eq!(apply.items.len(), 1);
    }
}
