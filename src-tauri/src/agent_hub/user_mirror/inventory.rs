//! user_mirror/inventory — 本机全 Agent 用户级镜像元数据扫描
//!
//! Business Logic（为什么需要这个模块）:
//!     Pull/Push 预览必须先拿到 catalog 全部 Hub Agent 的用户级槽 hash、原生文件事实
//!     与 Skill/Command/Plugin/MCP 身份；LAN JSON 不得带绝对路径或 MCP secret。
//!
//! Code Logic（这个模块做什么）:
//!     对 `all_hub_targets()` 组装 `UserMirrorInventoryDto`；复用 portable / 用户级
//!     指令 inspect；原生路径只走白名单。

use super::models::{
    UserMirrorAgentInventoryDto, UserMirrorInventoryDto, UserMirrorMcpCredentialFactDto,
    UserMirrorNativeFileFactDto, UserMirrorPortableItemDto, UserMirrorSlotHashesDto,
};
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_with_env_query, PortableInventoryItemDto, PortableInventoryQuery,
};
use crate::agent_hub::service::instruction_document_from_block_dtos;
use crate::agent_hub::snapshot::canonical_json::canonicalize_value;
use crate::agent_hub::targets::{TargetEnvironment, TargetPathResolver};
use crate::agent_hub::user_instructions::{
    extract_slot_text, inspect_user_instruction_workspace_with_env, user_level_mirror_native_paths,
    InstructionSlotKey, UserInstructionCanonicalDto,
};
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use std::fs;
use std::path::Path;

/// 扫描本机全部 Hub Agent 的用户级镜像元数据。
///
/// Business Logic（为什么需要这个函数）:
///     源端必须一次性暴露 catalog 全部 Agent 的用户级槽/原生文件/portable 事实，
///     供 preview 绑定 snapshot hash；缺席不得静默跳过，空库存仍占位。
///
/// Code Logic（这个函数做什么）:
///     用当前进程 `TargetEnvironment` 读用户级 workspace 与 per-target portable inspect；
///     白名单原生/槽文件只记 logical_id+hash+size；MCP 只抄 present/hash。
pub async fn build_local_user_mirror_inventory(
    state: &AppState,
    device_id: &str,
) -> Result<UserMirrorInventoryDto, AppError> {
    let env = TargetEnvironment::from_process();
    build_local_user_mirror_inventory_with_env(state, device_id, &env).await
}

/// 注入环境下的用户级镜像 inventory（测试与生产共用扫描规则）。
///
/// Business Logic: 隔离 HOME 必须与生产走同一白名单与 user-scope 过滤。
/// Code Logic: workspace 一次；catalog 顺序逐 target 扫 portable + 原生文件。
async fn build_local_user_mirror_inventory_with_env(
    state: &AppState,
    device_id: &str,
    env: &TargetEnvironment,
) -> Result<UserMirrorInventoryDto, AppError> {
    let workspace = inspect_user_instruction_workspace_with_env(state, env).await?;
    let homes = TargetPathResolver::resolve_all(env);
    let native_specs = user_level_mirror_native_paths(&homes);
    let mut agents = Vec::new();
    for target in crate::agent_catalog::all_hub_targets() {
        let snapshot = inspect_portable_inventory_with_env_query(
            state,
            env,
            PortableInventoryQuery {
                target: Some(target),
                kind: None,
                scope_kind: Some(ScopeKind::User),
                local_project_id: None,
            },
        )
        .await?;
        let items = snapshot
            .items
            .iter()
            .filter(|item| item.target == target && item.scope_kind == ScopeKind::User)
            .map(map_portable_item)
            .collect();
        let native_files = native_specs
            .iter()
            .filter(|(spec_target, _, _)| *spec_target == target)
            .map(|(_, logical_id, path)| native_file_fact(logical_id.clone(), path))
            .collect();
        agents.push(UserMirrorAgentInventoryDto {
            target,
            slots: slot_hashes_for_target(workspace.canonical.as_ref(), target)?,
            native_files,
            items,
        });
    }
    let credential_bearing_count = agents
        .iter()
        .flat_map(|agent| &agent.items)
        .filter(|item| {
            item.mcp_credential
                .as_ref()
                .is_some_and(|credential| credential.present)
        })
        .count() as u64;
    let inventory_snapshot_hash = hash_agents_snapshot(&agents)?;
    Ok(UserMirrorInventoryDto {
        source_device_id: device_id.to_string(),
        inventory_snapshot_hash,
        refreshed_at: Utc::now().to_rfc3339(),
        agents,
        credential_bearing_count,
    })
}

/// 从 Hub canonical 取该 Agent 三槽 hash；无 workspace 则为 None。
///
/// Business Logic: inventory 只传 hash，空槽与未配置不得伪造占位正文。
/// Code Logic: 块 DTO → InstructionDocument → extract_slot_text；空串不哈希。
fn slot_hashes_for_target(
    canonical: Option<&UserInstructionCanonicalDto>,
    target: AgentTarget,
) -> Result<UserMirrorSlotHashesDto, AppError> {
    let Some(canonical) = canonical else {
        return Ok(UserMirrorSlotHashesDto {
            common: None,
            adapted: None,
            exclusive: None,
        });
    };
    let document = instruction_document_from_block_dtos(&canonical.blocks)?;
    Ok(UserMirrorSlotHashesDto {
        common: hash_nonempty_text(&extract_slot_text(&document, InstructionSlotKey::Shared)),
        adapted: hash_nonempty_text(&extract_slot_text(
            &document,
            InstructionSlotKey::Adapted { agent: target },
        )),
        exclusive: hash_nonempty_text(&extract_slot_text(
            &document,
            InstructionSlotKey::TargetOnly { agent: target },
        )),
    })
}

/// 非空正文的 SHA-256 hex。
fn hash_nonempty_text(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(sha256_hex(text.as_bytes()))
    }
}

/// 白名单文件的元数据事实（无路径）。
///
/// Business Logic: 缺失文件仍要占位，preview 才能对号清空。
/// Code Logic: 非文件 → exists=false/size=0；可读则 hash 全文。
fn native_file_fact(logical_id: String, path: &Path) -> UserMirrorNativeFileFactDto {
    if !path.is_file() {
        return UserMirrorNativeFileFactDto {
            logical_id,
            content_hash: None,
            exists: false,
            size: 0,
        };
    }
    match fs::read(path) {
        Ok(bytes) => UserMirrorNativeFileFactDto {
            logical_id,
            content_hash: Some(sha256_hex(&bytes)),
            exists: true,
            size: bytes.len() as u64,
        },
        Err(_) => UserMirrorNativeFileFactDto {
            logical_id,
            content_hash: None,
            exists: true,
            size: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        },
    }
}

/// portable 库存项映射为镜像 DTO；MCP 只保留 present/hash。
///
/// Business Logic: inventory/UI 不得回显 env/token/path。
/// Code Logic: 拷贝 kind/nativeId/hashes/warnings；凭据字段只抄 fact。
fn map_portable_item(item: &PortableInventoryItemDto) -> UserMirrorPortableItemDto {
    UserMirrorPortableItemDto {
        kind: item.kind,
        native_id: item.native_id.clone(),
        display_name: item.display_name.clone(),
        content_hash: item.content_hash.clone(),
        tree_hash: item.tree_hash.clone(),
        actual_enabled: item.actual_enabled,
        mcp_credential: item.mcp_credential.as_ref().map(|credential| {
            UserMirrorMcpCredentialFactDto {
                present: credential.present,
                hash: credential.hash.clone(),
            }
        }),
        warnings: item.warnings.clone(),
    }
}

/// `inventory_snapshot_hash`：agents 的 canonical JSON SHA-256（不含 refreshed_at）。
///
/// Business Logic: preview/apply 绑定该 hash；刷新时间不得使快照漂移。
/// Code Logic: serde → RFC8785 子集 canonicalize → sha256_hex。
fn hash_agents_snapshot(agents: &[UserMirrorAgentInventoryDto]) -> Result<String, AppError> {
    let value = serde_json::to_value(agents)?;
    let bytes = canonicalize_value(&value).map_err(|error| {
        AppError::validation(format!("user_mirror_inventory_hash_canon:{error}"))
    })?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::build_local_user_mirror_inventory;
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::portable_inventory::PortableAssetKind;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::state::AppState;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    struct UserMirrorHomes {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        app_state: AppState,
        claude_home: PathBuf,
        home: PathBuf,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     镜像 inventory 测试必须隔离 HOME 与 data_dir，避免扫到开发者真实配置或凭据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     tempfile 下建 home/data；注入 `CC_PARTNER_DATA_DIR` 与 `HOME`；构造 AppState。
    async fn seed_user_mirror_homes() -> UserMirrorHomes {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&data).expect("data");
        let claude_home = home.join(".claude");
        fs::create_dir_all(&claude_home).expect("claude home");

        let data_guard = install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let home_guard = install_env_var("HOME", Some(home.to_str().expect("utf8 home")));
        let ui = Arc::new(RecordingBackendUi::default());
        let app_state = build_app_state(ui).await.expect("app state");
        UserMirrorHomes {
            _tmp: tmp,
            _guards: vec![Box::new(data_guard), Box::new(home_guard)],
            app_state,
            claude_home,
            home,
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     镜像 inventory 必须覆盖 catalog 全部 Hub Agent，且不得把 MCP 明文或 home 路径送上 LAN。
    ///
    /// Code Logic（这个测试做什么）:
    ///     隔离 HOME 写入 Claude 原生文件、hello skill 与 `~/.claude.json` 带 TOKEN 的 MCP；
    ///     断言全 target、脱敏、凭据条数与 Claude 原生/技能事实。
    #[tokio::test]
    async fn build_local_user_mirror_inventory_covers_all_hub_targets_and_redacts_secrets() {
        let env = seed_user_mirror_homes().await;
        write(env.claude_home.join("CLAUDE.md").as_path(), "# src claude");
        write(
            env.claude_home.join("skills/hello/SKILL.md").as_path(),
            "---\nname: hello\ndescription: d\n---\n",
        );
        write(
            env.home.join(".claude.json").as_path(),
            r#"{"mcpServers":{"s":{"command":"uvx","args":["srv"],"env":{"TOKEN":"plain-secret-xyz"},"enabled":true}}}"#,
        );

        let dto = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();
        let targets: Vec<_> = dto.agents.iter().map(|a| a.target).collect();
        for t in crate::agent_catalog::all_hub_targets() {
            assert!(targets.contains(&t), "missing {t:?}");
        }
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("plain-secret-xyz"));
        assert!(!json.contains(&env.claude_home.to_string_lossy().to_string()));
        let claude = dto
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Claude)
            .unwrap();
        assert!(claude
            .native_files
            .iter()
            .any(|f| f.logical_id == "claude.native.CLAUDE.md" && f.exists));
        assert!(claude
            .items
            .iter()
            .any(|i| i.kind == PortableAssetKind::Skill && i.native_id == "hello"));
        assert!(
            dto.credential_bearing_count > 0,
            "MCP env TOKEN must count as credential-bearing"
        );
        assert!(
            claude.items.iter().any(|i| {
                i.kind == PortableAssetKind::Mcp
                    && i.native_id == "s"
                    && i.mcp_credential
                        .as_ref()
                        .is_some_and(|credential| credential.present)
            }),
            "Claude MCP server s must be scanned with present credential fact"
        );
        let opencode = dto
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::OpenCode)
            .unwrap();
        assert!(
            !opencode
                .native_files
                .iter()
                .any(|f| f.logical_id.contains("CLAUDE")),
            "OpenCode must not attribute Claude CLAUDE.md as a native file"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Grok/Cursor 公共槽读仓库 AGENTS.md，用户级镜像不得把项目仓库文件当成原生用户文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在非 grok/cursor config_root 的项目目录写入 AGENTS.md；断言不进 native_files 且 JSON 无该路径。
    #[tokio::test]
    async fn grok_and_cursor_do_not_list_repo_workspace_agents_md() {
        let env = seed_user_mirror_homes().await;
        let repo_agents = env.home.join("proj-not-config/AGENTS.md");
        write(&repo_agents, "# repo agents — must not be mirrored\n");

        let dto = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains(&repo_agents.to_string_lossy().to_string()));
        assert!(!json.contains("proj-not-config"));

        let grok = dto
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Grok)
            .expect("grok agent");
        let grok_agents = grok
            .native_files
            .iter()
            .find(|f| f.logical_id == "grok.native.AGENTS.md")
            .expect("grok user-level AGENTS.md slot");
        assert!(
            !grok_agents.exists,
            "repo AGENTS.md must not count as grok user native file"
        );

        let cursor = dto
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Cursor)
            .expect("cursor agent");
        assert!(
            !cursor
                .native_files
                .iter()
                .any(|f| f.logical_id.contains("AGENTS.md")),
            "cursor must not invent ~/.cursor/AGENTS.md or pick up repo AGENTS.md"
        );
        assert!(cursor
            .native_files
            .iter()
            .any(|f| f.logical_id == "cursor.slot.adapted"));
        assert!(cursor
            .native_files
            .iter()
            .any(|f| f.logical_id == "cursor.slot.exclusive"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Codex 独有槽物化到 `AGENTS.override.md`；镜像必须按 logical_id 收录，不能只扫 AGENTS.md。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 `~/.codex/AGENTS.override.md`，断言 Codex native_files 含 `codex.slot.exclusive` 且 exists。
    #[tokio::test]
    async fn codex_override_md_is_listed_as_slot_exclusive() {
        let env = seed_user_mirror_homes().await;
        write(
            env.home.join(".codex/AGENTS.override.md").as_path(),
            "# exclusive override\n",
        );

        let dto = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();
        let codex = dto
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Codex)
            .expect("codex agent");
        assert!(
            codex
                .native_files
                .iter()
                .any(|f| { f.logical_id == "codex.slot.exclusive" && f.exists && f.size > 0 }),
            "Codex AGENTS.override.md must appear as codex.slot.exclusive"
        );
    }
}
