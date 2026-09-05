//! agent_hub/targets/portable/render — 投影渲染
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub 侧的 Skill/Command/Agent/MCP payload 需要渲染回各 CLI Agent 可消费的文件
//!     形态（SKILL.md / commands/*.md / agents/*.md / MCP JSON 片段），产出不写盘的
//!     投影计划，由后续 projection 任务统一落盘。
//!
//! Code Logic（这个模块做什么）:
//!     从原 portable.rs 拆出：`render_skill_projection` / `render_command_projection` /
//!     `render_agent_projection` / `render_mcp_projection` 四类渲染与
//!     `render_portable_payload` 分派入口，以及 `claude_user_mcp_config_path`
//!     Claude 用户级 MCP 配置路径解析。

use crate::{
    agent_hub::{
        assets::{
            McpTransport, PortableAgent, PortableAssetPayload, PortableCommand, PortableMcpServer,
            PortableSkill,
        },
        models::AgentTarget,
    },
    error::AppError,
};
use serde_json::Value;
use std::path::PathBuf;

use super::{ProjectedAssetFile, TargetAssetProjection};
use crate::agent_hub::targets::TargetEnvironment;

/// 渲染 Skill 为 `skills/<name>/SKILL.md` 树投影（仅 SKILL.md 正文；supporting 在 CAS）。
///
/// Business Logic: Task 3 只生成主 Markdown 投影计划；完整 tree 物化在后续 package task。
/// Code Logic: 写出 frontmatter + 占位 body（body 由调用方注入时用 context）。
pub fn render_skill_projection(
    target: AgentTarget,
    skill: &PortableSkill,
    skill_markdown: &str,
) -> TargetAssetProjection {
    let body = if skill_markdown.trim().is_empty() {
        format!(
            "---\nname: {}\ndescription: {}\n---\n",
            skill.name, skill.description
        )
    } else {
        skill_markdown.to_string()
    };
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("skills/{}/SKILL.md", skill.name),
            bytes: body.into_bytes(),
        }],
        diagnostics: vec![],
    }
}

/// 渲染 Command Markdown。
pub fn render_command_projection(
    target: AgentTarget,
    command: &PortableCommand,
) -> TargetAssetProjection {
    let mut fm = format!("---\nname: {}\n", command.name);
    if let Some(d) = &command.description {
        fm.push_str(&format!("description: {d}\n"));
    }
    fm.push_str("---\n");
    fm.push_str(&command.prompt_template);
    if !command.prompt_template.ends_with('\n') {
        fm.push('\n');
    }
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("commands/{}.md", command.name),
            bytes: fm.into_bytes(),
        }],
        diagnostics: command.collect_diagnostics(),
    }
}

/// 渲染 Agent Markdown。
pub fn render_agent_projection(
    target: AgentTarget,
    agent: &PortableAgent,
) -> TargetAssetProjection {
    let mut fm = format!("---\nname: {}\n", agent.name);
    if let Some(d) = &agent.description {
        fm.push_str(&format!("description: {d}\n"));
    }
    if let Some(m) = &agent.mode_intent {
        fm.push_str(&format!("mode: {m}\n"));
    }
    if !agent.tool_intents.is_empty() {
        fm.push_str(&format!("tools: {}\n", agent.tool_intents.join(", ")));
    }
    fm.push_str("---\n");
    fm.push_str(&agent.instructions);
    if !agent.instructions.ends_with('\n') {
        fm.push('\n');
    }
    TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("agents/{}.md", agent.name),
            bytes: fm.into_bytes(),
        }],
        diagnostics: agent.collect_diagnostics(),
    }
}

/// 渲染 MCP 为 JSON 片段（server 对象），供后续 config patch 使用。
pub fn render_mcp_projection(
    target: AgentTarget,
    server: &PortableMcpServer,
) -> Result<TargetAssetProjection, AppError> {
    let mut obj = serde_json::Map::new();
    match &server.transport {
        McpTransport::Stdio { command, args, cwd } => {
            obj.insert("type".into(), Value::String("stdio".into()));
            obj.insert("command".into(), Value::String(command.clone()));
            obj.insert(
                "args".into(),
                Value::Array(args.iter().cloned().map(Value::String).collect()),
            );
            if let Some(c) = cwd {
                obj.insert("cwd".into(), Value::String(c.clone()));
            }
        }
        McpTransport::Http { url, headers } => {
            obj.insert("type".into(), Value::String("http".into()));
            obj.insert("url".into(), Value::String(url.clone()));
            let mut h = serde_json::Map::new();
            for (k, v) in headers {
                h.insert(k.clone(), Value::String(v.clone()));
            }
            obj.insert("headers".into(), Value::Object(h));
        }
    }
    if !server.env.is_empty() {
        let mut e = serde_json::Map::new();
        for (k, v) in &server.env {
            e.insert(k.clone(), Value::String(v.clone()));
        }
        obj.insert("env".into(), Value::Object(e));
    }
    obj.insert("enabled".into(), Value::Bool(server.enabled));
    let bytes = serde_json::to_vec_pretty(&Value::Object(obj))
        .map_err(|e| AppError::generic(format!("mcp render: {e}")))?;
    Ok(TargetAssetProjection {
        target,
        files: vec![ProjectedAssetFile {
            relative_path: format!("mcp/{}.json", server.key),
            bytes,
        }],
        diagnostics: server.collect_diagnostics(),
    })
}

/// 分派 render。
pub fn render_portable_payload(
    target: AgentTarget,
    asset: &PortableAssetPayload,
) -> Result<TargetAssetProjection, AppError> {
    match asset {
        PortableAssetPayload::Skill(s) => Ok(render_skill_projection(target, s, "")),
        PortableAssetPayload::Command(c) => Ok(render_command_projection(target, c)),
        PortableAssetPayload::Agent(a) => Ok(render_agent_projection(target, a)),
        PortableAssetPayload::Mcp(m) => render_mcp_projection(target, m),
    }
}

/// Claude user MCP 配置路径（与 legacy 模块一致）。
///
/// Business Logic: CLAUDE_CONFIG_DIR 设置时读 `<dir>/.claude.json`，否则 `~/.claude.json`。
/// Code Logic: 读注入 env。
pub fn claude_user_mcp_config_path(env: &TargetEnvironment) -> PathBuf {
    if let Some(dir) = env.var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join(".claude.json")
    } else {
        env.home.join(".claude.json")
    }
}
