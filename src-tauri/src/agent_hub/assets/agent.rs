//! agent_hub/assets/agent — Canonical PortableAgent 载荷
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent 定义跨 Claude/Codex/OpenCode 的 common 字段为 name/instructions/mode/tool intents；
//!     model/provider/权限不做跨 target 自动等价，进入 target_extensions + 诊断。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PortableAgent` 与空名校验；工具/模式字段原样保留。

use crate::agent_hub::assets::diagnostics::PortabilityDiagnostic;
use crate::agent_hub::models::AgentTarget;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Canonical Agent 可移植载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     Hub 以 typed JSON 保存 agent 身份与 instructions；target 专属 config 进 extensions。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；tool_intents 保序；target_extensions 为 BTreeMap。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableAgent {
    /// Agent 名
    pub name: String,
    /// 描述
    pub description: Option<String>,
    /// system/prompt instructions
    pub instructions: String,
    /// 模式意图（如 plan/default）
    pub mode_intent: Option<String>,
    /// 可移植 tool intent 列表
    pub tool_intents: Vec<String>,
    /// 各 target 扩展（含 model/permission 等）
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

impl PortableAgent {
    /// 校验 agent 载荷。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     空名不得进入 revision。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim name 非空。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.name.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_portable_agent_empty_name".to_string(),
            ));
        }
        Ok(())
    }

    /// 从 target_extensions 收集 model/permission 不可移植诊断。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     已有 target 字段可保留回写，但不得暗示跨 target 自动覆盖策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 extensions 对象含 `model`/`permission`/`permissions` 键则记诊断。
    pub fn collect_diagnostics(&self) -> Vec<PortabilityDiagnostic> {
        let mut out = Vec::new();
        for (target, value) in &self.target_extensions {
            if let Some(obj) = value.as_object() {
                if obj.contains_key("model") || obj.contains_key("provider") {
                    out.push(PortabilityDiagnostic::model_not_portable(format!(
                        "/targetExtensions/{}/model",
                        target.as_str()
                    )));
                }
                if obj.contains_key("permission") || obj.contains_key("permissions") {
                    out.push(PortabilityDiagnostic::permission_not_portable(format!(
                        "/targetExtensions/{}/permission",
                        target.as_str()
                    )));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> PortableAgent {
        PortableAgent {
            name: "reviewer".into(),
            description: Some("Reviews PRs".into()),
            instructions: "Be thorough.".into(),
            mode_intent: Some("default".into()),
            tool_intents: vec!["read".into(), "search".into()],
            target_extensions: BTreeMap::from([(
                AgentTarget::Codex,
                json!({"model": "o3", "permissions": {"sandbox": "workspace"}}),
            )]),
        }
    }

    #[test]
    fn rejects_empty_name() {
        let mut a = sample();
        a.name = " ".into();
        assert!(a.validate().is_err());
    }

    #[test]
    fn model_and_permission_diagnostics() {
        let a = sample();
        let diags = a.collect_diagnostics();
        assert!(diags.iter().any(|d| d.code == "modelNotPortable"));
        assert!(diags.iter().any(|d| d.code == "permissionNotPortable"));
    }
}
