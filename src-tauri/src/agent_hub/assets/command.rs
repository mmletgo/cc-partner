//! agent_hub/assets/command — Canonical PortableCommand 载荷
//!
//! Business Logic（为什么需要这个模块）:
//!     斜杠 Command 在 Claude/OpenCode 为原生 Markdown，在 Codex 适配为 Plugin Skill；
//!     Hub 保存名称、模板与参数语义，不可移植插值进 target_extensions + 诊断。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `CommandArgument` / `PortableCommand` 与校验（空名、重复参数名）。

use crate::agent_hub::assets::diagnostics::PortabilityDiagnostic;
use crate::agent_hub::models::AgentTarget;
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Command 参数占位语义。
///
/// Business Logic（为什么需要这个结构体）:
///     各 CLI 参数声明形态不同，但 name/required/description 可跨 target 共享。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandArgument {
    /// 参数名
    pub name: String,
    /// 参数描述
    pub description: Option<String>,
    /// 是否必填
    pub required: bool,
}

/// Canonical Command 可移植载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     Hub 以 typed JSON 保存 command common 字段与 target 扩展。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；arguments 保序；target_extensions 为 BTreeMap。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableCommand {
    /// 命令名
    pub name: String,
    /// 描述
    pub description: Option<String>,
    /// Prompt 模板正文
    pub prompt_template: String,
    /// 参数列表（name 唯一）
    pub arguments: Vec<CommandArgument>,
    /// 各 target 扩展
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

impl PortableCommand {
    /// 校验 command 载荷。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     空名与重复参数名会使 target adapter 行为不确定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim name；arguments 名非空且集合唯一。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.name.trim().is_empty() {
            return Err(AppError::validation(
                "agent_hub_portable_command_empty_name".to_string(),
            ));
        }
        let mut seen = BTreeSet::new();
        for (idx, arg) in self.arguments.iter().enumerate() {
            if arg.name.trim().is_empty() {
                return Err(AppError::validation(format!(
                    "agent_hub_portable_command_empty_argument_name:{idx}"
                )));
            }
            if !seen.insert(arg.name.clone()) {
                return Err(AppError::validation(format!(
                    "agent_hub_portable_command_duplicate_argument_name:{}",
                    arg.name
                )));
            }
        }
        Ok(())
    }

    /// 收集模板中的不可移植插值诊断。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     shell/`$ARGUMENTS` 等语法无法跨 CLI 等价映射时标记 partial。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启发式检测 `$ARGUMENTS`、`{{$`、反引号命令替换。
    pub fn collect_diagnostics(&self) -> Vec<PortabilityDiagnostic> {
        let mut out = Vec::new();
        let t = &self.prompt_template;
        if t.contains("$ARGUMENTS") || t.contains("{{$") || t.contains("$(") {
            out.push(PortabilityDiagnostic::unsupported_interpolation(
                "/promptTemplate",
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PortableCommand {
        PortableCommand {
            name: "release".into(),
            description: Some("Cut a release".into()),
            prompt_template: "Prepare release $ARGUMENTS".into(),
            arguments: vec![CommandArgument {
                name: "version".into(),
                description: Some("semver".into()),
                required: true,
            }],
            target_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_empty_name() {
        let mut c = sample();
        c.name = String::new();
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_argument_names() {
        let mut c = sample();
        c.arguments.push(CommandArgument {
            name: "version".into(),
            description: None,
            required: false,
        });
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate_argument_name"));
    }

    #[test]
    fn interpolation_diagnostic() {
        let c = sample();
        let diags = c.collect_diagnostics();
        assert!(diags.iter().any(|d| d.code == "unsupportedInterpolation"));
    }
}
