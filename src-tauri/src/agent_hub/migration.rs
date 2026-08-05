//! agent_hub/migration — 用户级 CLAUDE.md / Plugin 迁移与 N/N+1 兼容门闩
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate A Task 10：把既有 `~/.claude/CLAUDE.md` 与 `claude_md` 表权威正文迁入 Hub
//!     的 user-scope instruction asset，并在 Hub 编辑路径上 dual-write 回 legacy 摘要行。
//!     迁移后三 target 绑定先 `desiredPresence=Absent`，等待用户确认再投影。
//!     Gate D Task 7：对 Claude/Codex/OpenCode Plugin 做幂等分解预览与确认 import；
//!     暴露 `LegacyAgentAssetCompatibilityStatus`（gaVersion / stableMigrationEvidence /
//!     earliestRemovalVersion）与 N+2 删除门闩；Hub 关闭时旧 façade 仅读最后成功 target
//!     文件、忽略未知 Hub 表、永不清理 CAS。
//!
//! Code Logic（这个模块做什么）:
//!     - 解析文件/DB 内容源（文件优先非空，其次 DB，否则空）
//!     - 幂等 seed：user scope + TargetOnly instruction asset + Migration revision
//!     - 为 Claude/Codex/OpenCode upsert Absent 绑定
//!     - 预览 Codex/OpenCode compile_render 全文
//!     - dual-write 仅更新 legacy `claude_md` 摘要（content/updated_at/device_id/vc），
//!       **永不**用 legacy vector_clock 裁决 Hub 冲突
//!     - Plugin：inspect-only preview → confirm import 单 package/child graph；二次无新 revision
//!     - N+2：running_version >= earliestRemovalVersion **且** 有 checked-in evidence 才允许删除

use crate::agent_hub::instructions::{
    classify_import, compile_render, ImportScopeContext, InstructionBlockMode, InstructionDocument,
    TargetMarkdownSource,
};
#[cfg(test)]
use crate::agent_hub::models::NewTargetBinding;
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset, NewLogicalAsset,
    NewRevision, NewScopeNode, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::plugins::{
    canonical_plugin_package_bytes, ensure_preview_skills_in_cas, import_confirmed,
    inspect_plugin_source, ConfirmedPluginDecomposition, DiscoveredPluginSource,
    PluginDecompositionPreview, PluginPackageRevision,
};
use crate::agent_hub::targets::InstructionRenderContext;
use crate::error::AppError;
use crate::models::claude_md::{ClaudeMdRow, CLAUDE_MD_ID};
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use crate::storage::claude_md_repo::ClaudeMdRepo;
use crate::sync::vector_clock;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 用户级 scope 的稳定主键（跨设备/重启不变）。
pub const USER_SCOPE_STABLE_ID: &str = "agent-hub-scope-user";
/// 用户指令资产逻辑键（对齐 Claude 文件名）。
pub const USER_INSTRUCTION_LOGICAL_KEY: &str = "CLAUDE.md";
/// 独立（非 plugin）命名空间。
pub const USER_INSTRUCTION_NAMESPACE: &str = "standalone";
/// UI 展示名。
pub const USER_INSTRUCTION_DISPLAY_NAME: &str = "User CLAUDE.md";

/// Agent Hub GA 版本号 N（与 `CARGO_PKG_VERSION` 同步；N+1 仍兼容，N+2 起可删）。
pub const AGENT_HUB_GA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// N+2 最早允许删除旧表/路由的版本（= GA + 2 个主次稳定位策略的**下限字符串**）。
///
/// Business Logic: GA=N，N 与 N+1 必须保留 dual-write/旧路由；最早 N+2 才可删。
/// Code Logic: 当前锁定为 `"0.10.0"`（0.8.x = N，0.9.x = N+1，0.10.0 = earliest N+2）。
pub const EARLIEST_LEGACY_REMOVAL_VERSION: &str = "0.10.0";

/// 已签入的稳定迁移 evidence ID（删除旧入口前必须存在）。
///
/// Business Logic: N+2 删除门闩要求 checked-in evidence，禁止“版本够就删”。
/// Code Logic: 本任务只登记 checklist/status，**不**执行删除；evidence 在 Gate D 认证后补齐。
pub const STABLE_MIGRATION_EVIDENCE_ID: Option<&str> = None;

/// 用户级 CLAUDE.md 迁移预览结果。
///
/// Business Logic（为什么需要这个结构体）:
///     迁移/预览 UI 需要知道是否新建 revision、策略、absent 绑定，以及 Codex/OpenCode 生成全文。
///
/// Code Logic（这个结构体做什么）:
///     聚合 asset/scope/revision 元数据与 compile_render 全文 diff。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeMdMigrationPreview {
    /// 用户指令资产 id
    pub asset_id: String,
    /// 用户 scope id
    pub scope_id: String,
    /// 本次 run 新建的 revision id；幂等跳过时为 None
    pub revision_id: Option<String>,
    /// 是否本轮 append 了新 revision
    pub created_revision: bool,
    /// 文档 payload SHA-256 hex
    pub payload_hash: String,
    /// 资产策略 wire token（恒为 targetOnly）
    pub policy: String,
    /// 绑定 desired presence wire token（恒为 absent）
    pub desired_presence: String,
    /// 所有块是否 targetOnly（Claude 单来源导入）
    pub blocks_target_only: bool,
    /// Codex 目标完整生成正文（UTF-8）
    pub codex_diff: String,
    /// OpenCode 目标完整生成正文（UTF-8）
    pub opencode_diff: String,
    /// 内容来源：`file` | `db` | `empty`
    pub content_source: String,
}

/// 迁移依赖注入（便于单测绕开完整 AppState）。
///
/// Business Logic（为什么需要这个结构体）:
///     单元测试只需 AgentHub/ClaudeMd 仓储 + device_id + object store 根。
///
/// Code Logic（这个结构体做什么）:
///     持有 repo 引用与路径/设备元数据（不 derive Debug：repo 非 Debug）。
#[derive(Clone, Copy)]
pub struct MigrationDeps<'a> {
    /// Agent Hub 仓储
    pub agent_hub: &'a AgentHubRepo,
    /// legacy CLAUDE.md 仓储
    pub claude_md: &'a ClaudeMdRepo,
    /// 本机 device_id
    pub device_id: &'a str,
    /// ObjectStore 根（通常 data_dir）
    pub object_store_root: &'a Path,
}

/// 解析当前用户 CLAUDE.md 正文。
///
/// Business Logic（为什么需要这个函数）:
///     迁移必须把“用户可见真相”写入 Hub：优先非空磁盘文件；文件缺失/空且 DB 有正文时用 DB；
///     否则空串。避免 DB 过期覆盖应用外编辑，也避免文件空时丢掉 DB 历史。
///
/// Code Logic（这个函数做什么）:
///     读文件（NotFound→空）→ 读 DB → 分支返回 (content, source=`file|db|empty`)。
pub async fn resolve_user_claude_md_content(
    state: &AppState,
    claude_md_file_path: &Path,
) -> Result<(String, &'static str), AppError> {
    resolve_user_claude_md_content_with(state.claude_md_repo.as_ref(), claude_md_file_path).await
}

/// 解析用户 CLAUDE.md 正文（可注入 ClaudeMdRepo）。
///
/// Business Logic（为什么需要这个函数）:
///     与 AppState 包装解耦，便于单测。
///
/// Code Logic（这个函数做什么）:
///     文件非空 → (`file`)；否则 DB 非空 → (`db`)；否则空 → (`empty`)。
pub async fn resolve_user_claude_md_content_with(
    claude_md: &ClaudeMdRepo,
    claude_md_file_path: &Path,
) -> Result<(String, &'static str), AppError> {
    let file_content = match fs::read_to_string(claude_md_file_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !file_content.trim().is_empty() {
        return Ok((file_content, "file"));
    }
    // 文件缺失或空白：允许回落 DB 非空内容
    if let Some(row) = claude_md.get().await? {
        if !row.content.trim().is_empty() {
            return Ok((row.content, "db"));
        }
    }
    // 文件空串时仍优先返回文件内容（可能含纯空白），source=empty
    if claude_md_file_path.exists() {
        return Ok((file_content, "empty"));
    }
    Ok((String::new(), "empty"))
}

/// 幂等迁移用户级 CLAUDE.md 到 Agent Hub。
///
/// Business Logic（为什么需要这个函数）:
///     旧 CLAUDE.md 编辑/同步路径仍在；Hub 启用后需把同一正文 seed 为 user instruction，
///     且第二遍同 hash 不重复 revision；V2 发现不创建 target binding，等待用户显式选择。
///
/// Code Logic（这个函数做什么）:
///     resolve content → ensure user scope/asset → classify_import(Claude) → CAS put →
///     同 payload_hash 跳过 append → 否则 Migration revision → 清理无物化/无 ownership 的
///     legacy Absent 伪绑定 → compile_render Codex/OpenCode 全文预览。
pub async fn migrate_user_claude_md_state(
    state: &AppState,
    claude_md_file_path: &Path,
    objects_root_data_dir: &Path,
) -> Result<ClaudeMdMigrationPreview, AppError> {
    let deps = MigrationDeps {
        agent_hub: state.agent_hub_repo.as_ref(),
        claude_md: state.claude_md_repo.as_ref(),
        device_id: state.device_id.as_str(),
        object_store_root: objects_root_data_dir,
    };
    migrate_user_claude_md_state_with(&deps, claude_md_file_path).await
}

/// 幂等迁移核心（可注入 deps）。
///
/// Business Logic（为什么需要这个函数）:
///     测试与生产共用同一迁移语义。
///
/// Code Logic（这个函数做什么）:
///     见 `migrate_user_claude_md_state`；使用 `MigrationDeps`。
pub async fn migrate_user_claude_md_state_with(
    deps: &MigrationDeps<'_>,
    claude_md_file_path: &Path,
) -> Result<ClaudeMdMigrationPreview, AppError> {
    let (content, content_source) =
        resolve_user_claude_md_content_with(deps.claude_md, claude_md_file_path).await?;

    let scope = ensure_user_scope(deps.agent_hub).await?;
    let asset = ensure_user_instruction_asset(deps.agent_hub, &scope.id).await?;

    let classification = classify_import(
        USER_INSTRUCTION_LOGICAL_KEY,
        ImportScopeContext {
            is_user_scope: true,
            is_project_root: false,
        },
        &[TargetMarkdownSource {
            target: AgentTarget::Claude,
            markdown: content.clone(),
        }],
    );
    // 稳定 block id：classify 默认随机 id，迁移必须内容→确定性 id 才能幂等。
    let mut document = classification.document;
    stabilize_migration_block_ids(&mut document);
    let blocks_target_only = document
        .blocks
        .iter()
        .all(|b| b.mode == InstructionBlockMode::TargetOnly);

    let bytes = serde_json::to_vec(&document)
        .map_err(|e| AppError::generic(format!("agent_hub_migration_serialize_failed:{e}")))?;
    let store = ObjectStore::open(deps.object_store_root)?;
    let stored = store.put_blob(&bytes).await?;
    let payload_hash = stored.hash;

    // 重新读取 asset head，避免同进程内其它写路径后的陈旧内存视图
    let asset =
        deps.agent_hub.get_asset(&asset.id).await?.ok_or_else(|| {
            AppError::not_found(format!("agent_hub_asset_not_found:{}", asset.id))
        })?;

    let (created_revision, revision_id) =
        ensure_migration_revision(deps, &asset, &payload_hash).await?;

    // V2 中“未选择”是中性 unmanaged，不用 Absent+disabled 伪 binding 表示。
    // 只清理无 mapping/checkout/materialization/ownership 的旧迁移草稿；真实用户选择不动。
    deps.agent_hub
        .delete_unmaterialized_absent_user_bindings(&asset.id)
        .await?;

    // 预览：对空 current bytes 的 compile_render = 完整生成正文
    let empty_ctx = InstructionRenderContext::default();
    let codex_rendered = compile_render(&document, AgentTarget::Codex, &empty_ctx);
    let opencode_rendered = compile_render(&document, AgentTarget::OpenCode, &empty_ctx);
    let codex_diff = String::from_utf8_lossy(&codex_rendered.bytes).into_owned();
    let opencode_diff = String::from_utf8_lossy(&opencode_rendered.bytes).into_owned();

    Ok(ClaudeMdMigrationPreview {
        asset_id: asset.id,
        scope_id: scope.id,
        revision_id,
        created_revision,
        payload_hash,
        policy: AssetPolicy::TargetOnly.as_str().to_string(),
        desired_presence: DesiredPresence::Absent.as_str().to_string(),
        blocks_target_only,
        codex_diff,
        opencode_diff,
        content_source: content_source.to_string(),
    })
}

/// Dual-write legacy `claude_md` 摘要行 + 用户 `CLAUDE.md` 文件。
///
/// Business Logic（为什么需要这个函数）:
///     N/N+1 兼容旧 CLAUDE.md 页面/P2P：Hub 写成功后同步摘要到旧表与磁盘文件，
///     避免 file-wins reconcile 用旧磁盘正文回滚 Hub revision。
///     **legacy vector_clock 永不裁决 Hub 冲突**（Hub 以 revision DAG 为权威）。
///
/// Code Logic（这个函数做什么）:
///     仅 upsert legacy `claude_md` 摘要行（content/updated_at/device_id/vc）；
///     **不**写磁盘目标文件（projector-only）。内容未变则 no-op。
pub async fn dual_write_legacy_claude_md_summary(
    state: &AppState,
    content: &str,
) -> Result<(), AppError> {
    dual_write_legacy_claude_md_summary_with(
        state.claude_md_repo.as_ref(),
        state.device_id.as_str(),
        content,
    )
    .await
}

/// Dual-write legacy 摘要（可注入 repo）。
///
/// Business Logic（为什么需要这个函数）:
///     单测与生产共用摘要 upsert 语义；Hub-on 仅更新 legacy 表。
///
/// Code Logic（这个函数做什么）:
///     见 `dual_write_legacy_claude_md_summary`（不写目标文件）。
pub async fn dual_write_legacy_claude_md_summary_with(
    claude_md: &ClaudeMdRepo,
    device_id: &str,
    content: &str,
) -> Result<(), AppError> {
    // Hub-on 合同：legacy dual-write **仅**更新摘要表；目标文件只允许经 binding/support 检查的 projector 写入。
    // 禁止在此路径 write ~/.claude/CLAUDE.md（会绕过 Absent/blocked render 门闸）。
    let existing = claude_md.get().await?;
    if let Some(row) = existing.as_ref() {
        if row.content == content {
            // 内容未变：明确 no-op，不触碰 vector_clock
            return Ok(());
        }
    }
    let old_vc = existing
        .as_ref()
        .map(|r| r.vector_clock.clone())
        .unwrap_or_default();
    let row = ClaudeMdRow {
        id: CLAUDE_MD_ID.to_string(),
        content: content.to_string(),
        updated_at: Utc::now().to_rfc3339(),
        device_id: device_id.to_string(),
        vector_clock: vector_clock::increment(&old_vc, device_id),
    };
    claude_md.upsert(&row).await?;
    Ok(())
}

/// 解析用户级 CLAUDE.md 磁盘路径。
///
/// Business Logic（为什么需要这个函数）:
///     dual-write 必须与 Claude adapter 同一路径空间（CLAUDE_CONFIG_DIR 或 ~/.claude）。
///
/// Code Logic（这个函数做什么）:
///     优先 CLAUDE_CONFIG_DIR 环境变量；否则 home/.claude/CLAUDE.md。
pub fn user_claude_md_file_path() -> Result<std::path::PathBuf, AppError> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return Ok(std::path::PathBuf::from(trimmed).join("CLAUDE.md"));
        }
    }
    let home = dirs::home_dir()
        .ok_or_else(|| AppError::validation("agent_hub_dual_write_home_missing".to_string()))?;
    Ok(home.join(".claude").join("CLAUDE.md"))
}

/// 将 Hub 用户指令摘要写入磁盘 CLAUDE.md。
///
/// Business Logic（为什么需要这个函数）:
///     仅写 legacy DB 会在 reconcile_from_file 时被旧文件冲掉；文件必须同步。
///
/// Code Logic（这个函数做什么）:
///     ensure parent dir → 写 sibling temp → rename 覆盖（失败回退 fs::write）。
#[allow(dead_code)] // projector / hub-off dual-write paths may re-enable explicit file write
#[allow(dead_code)] // projector may re-enable explicit file write
fn write_user_claude_md_file(content: &str) -> Result<(), AppError> {
    let path = user_claude_md_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::generic(format!("agent_hub_dual_write_mkdir_failed:{e}")))?;
    }
    let tmp = path.with_extension("md.tmp");
    match fs::write(&tmp, content.as_bytes()) {
        Ok(()) => {
            if let Err(e) = fs::rename(&tmp, &path) {
                let _ = fs::remove_file(&tmp);
                fs::write(&path, content.as_bytes()).map_err(|e2| {
                    AppError::generic(format!(
                        "agent_hub_dual_write_file_failed:rename={e};write={e2}"
                    ))
                })?;
            }
        }
        Err(_) => {
            fs::write(&path, content.as_bytes())
                .map_err(|e| AppError::generic(format!("agent_hub_dual_write_file_failed:{e}")))?;
        }
    }
    Ok(())
}

/// 从 instruction 文档提取 Claude 用户可见摘要正文。
///
/// Business Logic（为什么需要这个函数）:
///     Hub dual-write 到 legacy 时，应优先取 Claude targetOnly 变体；若无则回落 shared body。
///
/// Code Logic（这个函数做什么）:
///     顺序拼接 targetOnly 块的 Claude variant；空则 `joined_shared_body`。
pub fn claude_summary_markdown_from_document(document: &InstructionDocument) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for block in &document.blocks {
        if block.mode != InstructionBlockMode::TargetOnly {
            continue;
        }
        if let Some(text) = block.variants.get(&AgentTarget::Claude) {
            if !text.is_empty() {
                parts.push(text.as_str());
            }
        }
    }
    if parts.is_empty() {
        return document.joined_shared_body();
    }
    join_markdown_parts(&parts)
}

/// 确保用户 scope 存在。
///
/// Business Logic（为什么需要这个函数）:
///     用户指令必须挂在稳定 user scope 上，跨设备映射不依赖本机绝对路径。
///
/// Code Logic（这个函数做什么）:
///     get_scope(stable_id) 或 insert_scope(User, id=stable_id)。
async fn ensure_user_scope(
    agent_hub: &AgentHubRepo,
) -> Result<crate::agent_hub::models::ScopeNode, AppError> {
    if let Some(scope) = agent_hub.get_scope(USER_SCOPE_STABLE_ID).await? {
        return Ok(scope);
    }
    agent_hub
        .insert_scope(NewScopeNode {
            id: Some(USER_SCOPE_STABLE_ID.to_string()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
}

/// 确保用户 instruction 资产（TargetOnly 策略）。
///
/// Business Logic（为什么需要这个函数）:
///     用户级 CLAUDE.md 是单来源 Claude 文档，策略必须为 targetOnly。
///
/// Code Logic（这个函数做什么）:
///     get_asset_by_unique_key 或 insert_asset(Instruction, standalone, CLAUDE.md, TargetOnly)。
async fn ensure_user_instruction_asset(
    agent_hub: &AgentHubRepo,
    scope_id: &str,
) -> Result<LogicalAsset, AppError> {
    if let Some(asset) = agent_hub
        .get_asset_by_unique_key(
            scope_id,
            AssetKind::Instruction,
            USER_INSTRUCTION_NAMESPACE,
            USER_INSTRUCTION_LOGICAL_KEY,
        )
        .await?
    {
        return Ok(asset);
    }
    agent_hub
        .insert_asset(NewLogicalAsset {
            scope_id: scope_id.to_string(),
            kind: AssetKind::Instruction,
            origin_namespace: USER_INSTRUCTION_NAMESPACE.to_string(),
            logical_key: USER_INSTRUCTION_LOGICAL_KEY.to_string(),
            display_name: USER_INSTRUCTION_DISPLAY_NAME.to_string(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
}

/// 单事务 NULL-head CAS seed 结果。
///
/// Business Logic: CAS miss 表示并发 import 已建 head，调用方只能 targetOnly mutate。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedUserInstructionOutcome {
    /// 本次成功 seed 出 head（或同 payload 幂等）
    Seeded,
    /// head 已存在（含 CAS miss）；不得 reclassify Shared/Adapted
    HeadAlreadyPresent,
    /// scope/asset 已建但 head 仍空且 seed 跳过（罕见）
    NoOp,
}

/// 仅当 user instruction head 为 NULL 时 seed migration revision（NULL-head CAS）。
///
/// Business Logic（为什么需要这个函数）:
///     legacy push 的 need_seed 若在事务外判断，会与 LAN/Git import 竞态：
///     import 先建 Shared/Adapted head 后，migration 仍以 expected_parent=None 覆盖为 reclassify head。
///     必须单事务 CAS：仅 NULL head 才 seed；miss 时回滚 seed 意图并让调用方 reload head。
///
/// Code Logic（这个函数做什么）:
///     ensure scope/asset → 若 head 非空 → HeadAlreadyPresent；
///     否则 classify+append_revision(expected_parent_id=Some 表示要求当前 head 仍为 None 的 CAS 语义：
///     对首 revision 使用 expected_parent_id=None 但 append 路径在 head 已变时会 conflict——
///     此处再读 head，仅 NULL 才 append，append 后再次校验)。
pub async fn seed_user_instruction_if_head_null(
    state: &AppState,
    claude_md_file_path: &Path,
    objects_root_data_dir: &Path,
) -> Result<SeedUserInstructionOutcome, AppError> {
    let deps = MigrationDeps {
        agent_hub: state.agent_hub_repo.as_ref(),
        claude_md: state.claude_md_repo.as_ref(),
        device_id: state.device_id.as_str(),
        object_store_root: objects_root_data_dir,
    };
    seed_user_instruction_with_deps(&deps, claude_md_file_path).await
}

/// Testable core of [`seed_user_instruction_if_head_null`].
///
/// Business Logic: same NULL-head CAS as the public shim.
/// Code Logic: identical control flow, but takes `MigrationDeps` directly so unit
/// tests can exercise Seeded/HeadAlreadyPresent without spinning up a full `AppState`.
pub(crate) async fn seed_user_instruction_with_deps(
    deps: &MigrationDeps<'_>,
    claude_md_file_path: &Path,
) -> Result<SeedUserInstructionOutcome, AppError> {
    let scope = ensure_user_scope(deps.agent_hub).await?;
    let asset = ensure_user_instruction_asset(deps.agent_hub, &scope.id).await?;
    // 进入 seed 前重新读 head（CAS 观察点）
    let asset =
        deps.agent_hub.get_asset(&asset.id).await?.ok_or_else(|| {
            AppError::not_found(format!("agent_hub_asset_not_found:{}", asset.id))
        })?;
    if asset.current_revision_id.is_some() {
        return Ok(SeedUserInstructionOutcome::HeadAlreadyPresent);
    }

    let (content, _) =
        resolve_user_claude_md_content_with(deps.claude_md, claude_md_file_path).await?;
    let classification = classify_import(
        USER_INSTRUCTION_LOGICAL_KEY,
        ImportScopeContext {
            is_user_scope: true,
            is_project_root: false,
        },
        &[TargetMarkdownSource {
            target: AgentTarget::Claude,
            markdown: content.clone(),
        }],
    );
    let mut document = classification.document;
    stabilize_migration_block_ids(&mut document);
    let bytes = serde_json::to_vec(&document)
        .map_err(|e| AppError::generic(format!("agent_hub_migration_serialize_failed:{e}")))?;
    let store = ObjectStore::open(deps.object_store_root)?;
    let stored = store.put_blob(&bytes).await?;
    let payload_hash = stored.hash;

    // 再次确认 head 仍为 NULL（缩小 TOCTOU 窗口）；append 使用 expected_parent_id=None
    // 但 migration 路径对首 revision 在 head 被并发推进时靠二次 get 检测。
    let asset =
        deps.agent_hub.get_asset(&asset.id).await?.ok_or_else(|| {
            AppError::not_found(format!("agent_hub_asset_not_found:{}", asset.id))
        })?;
    if asset.current_revision_id.is_some() {
        return Ok(SeedUserInstructionOutcome::HeadAlreadyPresent);
    }

    // 首 revision：用 expected_parent_id=None；append_revision 在 concurrent head 时仍可能写入。
    // 为 fail-closed，append 后立即 re-read：若 head 不是我们刚写的 revision，说明并发赢了——
    // 不继续 seed bindings reclassify；返回 HeadAlreadyPresent（调用方只做 targetOnly）。
    let created = deps
        .agent_hub
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Migration,
            origin_target: Some(AgentTarget::Claude),
            origin_replica_id: deps.device_id.to_string(),
            payload_hash: Some(payload_hash.clone()),
            tree_manifest_hash: None,
            created_at: Utc::now().to_rfc3339(),
            // NULL-head CAS：要求当前 head 仍为 NULL
            expected_parent_id: None,
        })
        .await;

    match created {
        Ok(rev) => {
            let after = deps.agent_hub.get_asset(&asset.id).await?;
            if after
                .as_ref()
                .and_then(|a| a.current_revision_id.as_ref())
                .map(|h| h.as_str())
                != Some(rev.id.as_str())
            {
                // 并发 head 赢：不 reclassify，让调用方 reload
                return Ok(SeedUserInstructionOutcome::HeadAlreadyPresent);
            }
            // V2 seed 只建立 canonical 草稿；target 保持 unmanaged，等待显式 preview/apply。
            deps.agent_hub
                .delete_unmaterialized_absent_user_bindings(&asset.id)
                .await?;
            Ok(SeedUserInstructionOutcome::Seeded)
        }
        Err(e) => {
            // conflict 等：视为 head 已存在
            if e.ipc_category_code() == "conflict" {
                return Ok(SeedUserInstructionOutcome::HeadAlreadyPresent);
            }
            Err(e)
        }
    }
}

/// 若 head payload_hash 相同则跳过，否则 append Migration revision。
///
/// Business Logic（为什么需要这个函数）:
///     迁移幂等：同一正文二次运行不得产生新 revision。
///     已有 head 时必须带 expected_parent，禁止 None 覆盖并发 head。
///
/// Code Logic（这个函数做什么）:
///     读 current revision；hash 相同 → (false, Some(head_id))；
///     否则 append_revision(expected_parent_id=当前 head)。
async fn ensure_migration_revision(
    deps: &MigrationDeps<'_>,
    asset: &LogicalAsset,
    payload_hash: &str,
) -> Result<(bool, Option<String>), AppError> {
    if let Some(rev_id) = asset.current_revision_id.as_ref() {
        if let Some(rev) = deps.agent_hub.get_revision(rev_id).await? {
            if rev.payload_hash.as_deref() == Some(payload_hash) {
                return Ok((false, Some(rev_id.as_str().to_string())));
            }
        }
    }

    let parents = asset
        .current_revision_id
        .clone()
        .into_iter()
        .collect::<Vec<_>>();
    // NULL-head 才允许 expected_parent_id=None；已有 head 必须 CAS 到该 head
    let expected_parent_id = asset.current_revision_id.clone();
    let revision = deps
        .agent_hub
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Migration,
            origin_target: Some(AgentTarget::Claude),
            origin_replica_id: deps.device_id.to_string(),
            payload_hash: Some(payload_hash.to_string()),
            tree_manifest_hash: None,
            created_at: Utc::now().to_rfc3339(),
            expected_parent_id,
        })
        .await?;
    Ok((true, Some(revision.id.as_str().to_string())))
}

/// 拼接 Markdown 块（块间双换行）。
///
/// Business Logic（为什么需要这个函数）:
///     dual-write 摘要需要稳定、可读的多块正文。
///
/// Code Logic（这个函数做什么）:
///     非空 parts 用 `\n\n` 连接。
fn join_markdown_parts(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|s| s.trim_end_matches('\n'))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 将迁移文档块 id 改为内容指纹派生的稳定值。
///
/// Business Logic（为什么需要这个函数）:
///     迁移幂等依赖 payload hash；若块 id 随机，同正文每次 hash 不同会反复 append revision。
///
/// Code Logic（这个函数做什么）:
///     对每个块用 `content_fingerprint`（不含 id）生成 `mig-<fingerprint>`。
fn stabilize_migration_block_ids(document: &mut InstructionDocument) {
    for block in &mut document.blocks {
        let fp = block.content_fingerprint();
        block.id = format!("mig-{fp}");
    }
}

// ─── Gate D Task 7: Plugin migration + N+2 compatibility ───────────────────

/// 单条 Plugin 分解预览行（inspect-only，不写 revision）。
///
/// Business Logic: 首次迁移入口只能展示 preview；碰撞/未验证激活保持 sourceOnly/externalCollision。
/// Code Logic: camelCase；嵌套完整 `PluginDecompositionPreview`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMigrationPreviewItem {
    /// 发现源身份
    pub source: DiscoveredPluginSource,
    /// inspect 结果（component/residual 矩阵）
    pub preview: PluginDecompositionPreview,
    /// 是否已在 Hub 中存在同 hash package head（二次运行幂等）
    pub already_imported: bool,
    /// 聚合状态提示：`preview` | `sourceOnly` | `externalCollision` | `imported`
    pub status: String,
}

/// 多 target Plugin 迁移预览聚合。
///
/// Business Logic: 扫描 Claude/Codex/OpenCode Plugin 根，一次性给出预览，不 append revision。
/// Code Logic: items + 计数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMigrationPreview {
    /// 各源预览行
    pub items: Vec<PluginMigrationPreviewItem>,
    /// 本轮新建 revision 数（preview 恒为 0）
    pub created_revisions: u32,
}

/// 确认 import 一个 Plugin package + child graph 的结果。
///
/// Business Logic: 用户确认后才 import；二次同 hash 不新建 revision。
/// Code Logic: package revision 元数据 + created_revision 标志。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginMigrationConfirmResult {
    /// package 资产 id
    pub package_asset_id: String,
    /// head revision id
    pub revision_id: String,
    /// 本轮是否 append 了新 package revision
    pub created_revision: bool,
    /// package payload hash
    pub payload_hash: String,
    /// 状态：`imported` | `idempotent` | `sourceOnly` | `externalCollision`
    pub status: String,
    /// 若 status 为 externalCollision/sourceOnly 的原因 token
    pub reason: Option<String>,
}

/// N/N+1 旧入口兼容状态（删除门闩）。
///
/// Business Logic: UI/checklist/测试用它判断何时可删旧表与路由。
/// Code Logic: gaVersion + evidence + earliestRemovalVersion + 运行时比较。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyAgentAssetCompatibilityStatus {
    /// Agent Hub GA 版本 N（与编译版本一致）
    pub ga_version: String,
    /// 当前运行版本
    pub running_version: String,
    /// 已签入的稳定迁移 evidence ID（未就绪时 None）
    pub stable_migration_evidence: Option<String>,
    /// 最早允许删除旧入口的版本
    pub earliest_removal_version: String,
    /// 是否允许实际删除（running >= earliest **且** evidence 存在）
    pub removal_allowed: bool,
    /// 旧路由是否仍须注册（N/N+1 窗口内恒 true）
    pub legacy_routes_registered: bool,
    /// 旧 UI 入口是否隐藏（新 UI 用 `/agent-hub`）
    pub legacy_ui_hidden: bool,
    /// 删除 checklist（本任务只登记，不执行）
    pub removal_checklist: Vec<String>,
}

/// 兼容 façade 运行时策略（Hub 开/关时旧路由行为）。
///
/// Business Logic: Hub 启用时旧 DTO 只从 Hub 投影翻译，不得二次直接 mutation；
/// Hub 关闭时最后成功 target 文件可用，旧表/路由忽略未知 Hub 表、永不清理 CAS。
/// Code Logic: 纯策略结构，供 commands/routes 查询。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyFacadePolicy {
    /// Hub 是否启用
    pub hub_enabled: bool,
    /// 是否允许旧入口直接 mutation target 文件（仅 Hub 关闭时）
    pub allow_direct_target_mutation: bool,
    /// 是否允许清理 CAS（永远 false）
    pub allow_cas_gc: bool,
    /// 是否忽略未知 Hub 表（降级时 true）
    pub ignore_unknown_hub_tables: bool,
    /// 是否应从 Hub 翻译 DTO（Hub 启用时 true）
    pub translate_from_hub: bool,
}

/// 扫描 Plugin 根并生成幂等分解预览（**不** append revision）。
///
/// Business Logic: 首次迁移入口只展示 preview；确认后才 import 单 package/child graph。
/// Code Logic: 对每个 `DiscoveredPluginSource` 调用 `inspect_plugin_source` + 检查已导入 hash。
pub async fn preview_plugin_migration(
    agent_hub: &AgentHubRepo,
    object_store_root: &Path,
    sources: &[DiscoveredPluginSource],
) -> Result<PluginMigrationPreview, AppError> {
    let store = ObjectStore::open(object_store_root)?;
    let mut items = Vec::with_capacity(sources.len());
    for source in sources {
        let mut preview = inspect_plugin_source(source, &store).await?;
        ensure_preview_skills_in_cas(&mut preview, &store).await?;
        let already = package_already_imported(agent_hub, &store, &preview).await?;
        let status = if already {
            "imported".to_string()
        } else if preview.components.is_empty() && !preview.residuals.is_empty() {
            // 仅 residual / 无 portable child → sourceOnly 提示
            "sourceOnly".to_string()
        } else {
            "preview".to_string()
        };
        items.push(PluginMigrationPreviewItem {
            source: source.clone(),
            preview,
            already_imported: already,
            status,
        });
    }
    Ok(PluginMigrationPreview {
        items,
        created_revisions: 0,
    })
}

/// 确认 import 单个 Plugin（幂等：有序 component/residual 内容集合匹配 head 才短路）。
///
/// Business Logic: 用户确认后 import 一个 package/child graph；碰撞/未验证激活保持
/// sourceOnly/externalCollision 且不写新 head。仅同 plugin_id 不足以免陈旧 head 静默 no-op。
/// Code Logic: 可选 force_status → 否则 confirmed → component/residual 集合相同则跳过 append。
pub async fn confirm_plugin_migration_import(
    agent_hub: &AgentHubRepo,
    object_store_root: &Path,
    confirmed: ConfirmedPluginDecomposition,
    force_status: Option<&str>,
) -> Result<PluginMigrationConfirmResult, AppError> {
    if let Some(status) = force_status {
        if matches!(status, "sourceOnly" | "externalCollision") {
            return Ok(PluginMigrationConfirmResult {
                package_asset_id: String::new(),
                revision_id: String::new(),
                created_revision: false,
                payload_hash: String::new(),
                status: status.to_string(),
                reason: Some(if status == "externalCollision" {
                    "collision_or_unverified_activation".into()
                } else {
                    "source_only_or_unverified".into()
                }),
            });
        }
    }

    let store = ObjectStore::open(object_store_root)?;
    // 若已导入同 hash → 幂等返回
    if let Some((asset_id, rev_id, hash)) =
        existing_package_head(agent_hub, &store, &confirmed.preview).await?
    {
        return Ok(PluginMigrationConfirmResult {
            package_asset_id: asset_id,
            revision_id: rev_id,
            created_revision: false,
            payload_hash: hash,
            status: "idempotent".into(),
            reason: None,
        });
    }

    let result: PluginPackageRevision =
        import_confirmed(agent_hub, &store, confirmed, RevisionOriginKind::Migration).await?;
    let payload_hash = result.revision.payload_hash.clone().unwrap_or_else(|| {
        // 回退：从 payload 再算（理论不应缺）
        canonical_plugin_package_bytes(&result.payload)
            .map(|b| sha256_hex(&b))
            .unwrap_or_default()
    });

    Ok(PluginMigrationConfirmResult {
        package_asset_id: result.package_asset.id,
        revision_id: result.revision.id.as_str().to_string(),
        created_revision: true,
        payload_hash,
        status: "imported".into(),
        reason: None,
    })
}

/// 读取 N/N+1 兼容状态与 N+2 删除门闩。
///
/// Business Logic: 只有 running >= earliestRemovalVersion **且** 有 checked-in evidence
/// 才允许实际删除；本任务只暴露 guard/status/checklist，不删路由。
/// Code Logic: 比较 semver 核心三元组 + 常量 evidence。
pub fn legacy_agent_asset_compatibility_status(
    running_version: &str,
) -> LegacyAgentAssetCompatibilityStatus {
    let evidence = STABLE_MIGRATION_EVIDENCE_ID.map(|s| s.to_string());
    let version_ok = version_cmp(running_version, EARLIEST_LEGACY_REMOVAL_VERSION) >= 0;
    let removal_allowed = version_ok && evidence.is_some();
    LegacyAgentAssetCompatibilityStatus {
        ga_version: AGENT_HUB_GA_VERSION.to_string(),
        running_version: running_version.to_string(),
        stable_migration_evidence: evidence,
        earliest_removal_version: EARLIEST_LEGACY_REMOVAL_VERSION.to_string(),
        removal_allowed,
        // N/N+1 窗口：旧路由保持注册；新 UI 隐藏旧入口
        legacy_routes_registered: !removal_allowed,
        legacy_ui_hidden: true,
        removal_checklist: vec![
            "Confirm running_version >= earliestRemovalVersion".into(),
            "Check-in stable migration evidence ID (L2/L3 Gate D plugin migration)".into(),
            "Update docs/p2p-protocol.md route inventory and mixed-version harnesses".into(),
            "Unregister legacy /api/claude-code-assets/* and /api/sync/claude_md/* only after evidence".into(),
            "Never GC CAS or drop unknown Hub tables during downgrade".into(),
            "Keep /agent-hub as the only new UI entry; old frontend routes remain redirects".into(),
        ],
    }
}

/// 是否允许实际删除旧入口（纯门闩）。
///
/// Business Logic: 见 `legacy_agent_asset_compatibility_status`。
/// Code Logic: 委托 status.removal_allowed。
pub fn n_plus_two_removal_allowed(running_version: &str) -> bool {
    legacy_agent_asset_compatibility_status(running_version).removal_allowed
}

/// Hub 开关下的旧 façade 策略。
///
/// Business Logic: Hub on → 翻译 DTO、禁止二次直接 mutation；Hub off → 最后 target 文件可用、
/// 忽略未知 Hub 表、永不 CAS GC。
/// Code Logic: 由 hub_enabled 派生四布尔。
pub fn legacy_facade_policy(hub_enabled: bool) -> LegacyFacadePolicy {
    LegacyFacadePolicy {
        hub_enabled,
        allow_direct_target_mutation: !hub_enabled,
        allow_cas_gc: false,
        ignore_unknown_hub_tables: !hub_enabled,
        translate_from_hub: hub_enabled,
    }
}

/// Hub 关闭后旧 façade 可读最后成功 target 文件；重新启用时恢复 pending 投影调度。
///
/// Business Logic: 降级不清理 CAS/新表；re-enable 只 best-effort 重新调度 recoverable jobs。
/// Code Logic: hub_enabled=false → 返回可读文件列表；true → 列出 recoverable job 数（调用方调度）。
pub async fn downgrade_compatibility_facade_snapshot(
    agent_hub: &AgentHubRepo,
    hub_enabled: bool,
    last_target_files: &[PathBuf],
) -> Result<DowngradeFacadeSnapshot, AppError> {
    let policy = legacy_facade_policy(hub_enabled);
    let readable: Vec<PathBuf> = last_target_files
        .iter()
        .filter(|p| p.is_file())
        .cloned()
        .collect();
    let recoverable = if hub_enabled {
        agent_hub
            .list_recoverable_projection_jobs()
            .await
            .map(|jobs| jobs.len())
            .unwrap_or(0)
    } else {
        // 降级路径：不得依赖未知 Hub 表；失败视为 0 且不清理
        0
    };
    Ok(DowngradeFacadeSnapshot {
        policy,
        readable_target_files: readable,
        recoverable_projection_jobs: recoverable,
        cas_gc_attempted: false,
    })
}

/// 降级/恢复快照 DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DowngradeFacadeSnapshot {
    /// 当前 façade 策略
    pub policy: LegacyFacadePolicy,
    /// 仍可读的最后成功 target 文件
    pub readable_target_files: Vec<PathBuf>,
    /// re-enable 时 recoverable projection job 数
    pub recoverable_projection_jobs: usize,
    /// 是否尝试过 CAS GC（合同上恒 false）
    pub cas_gc_attempted: bool,
}

/// 比较两个 `x.y.z` 风格版本（忽略预发布后缀）；a<b → -1，a==b → 0，a>b → 1。
///
/// Business Logic: N+2 门闩需要确定性版本比较，不引入完整 semver 依赖。
/// Code Logic: 取前三段数字；非法段当 0。
pub fn version_cmp(a: &str, b: &str) -> i32 {
    let pa = parse_version_core(a);
    let pb = parse_version_core(b);
    for i in 0..3 {
        if pa[i] < pb[i] {
            return -1;
        }
        if pa[i] > pb[i] {
            return 1;
        }
    }
    0
}

fn parse_version_core(v: &str) -> [u64; 3] {
    let core = v.split(['-', '+']).next().unwrap_or(v);
    let mut out = [0u64; 3];
    for (i, part) in core.split('.').take(3).enumerate() {
        out[i] = part.parse().unwrap_or(0);
    }
    out
}

/// 若 package 已以相同 component/residual 内容集合作为 head，则返回 true。
async fn package_already_imported(
    agent_hub: &AgentHubRepo,
    store: &ObjectStore,
    preview: &PluginDecompositionPreview,
) -> Result<bool, AppError> {
    Ok(existing_package_head(agent_hub, store, preview)
        .await?
        .is_some())
}

/// preview 侧有序 component 内容集合：(kind, logical_key, content_fp, tree_hash)。
///
/// Business Logic: 源更新后 content/tree 变化必须触发 append；不依赖尚未生成的 revision id。
/// Code Logic: 从 typed ComponentPayloadPreview 派生稳定 content 指纹 + tree。
fn preview_component_content_set(
    preview: &PluginDecompositionPreview,
) -> Result<Vec<(String, String, String, String)>, AppError> {
    use crate::agent_hub::assets::{canonical_bytes, PortableAssetPayload};
    use crate::agent_hub::plugins::{canonical_portable_hook_bytes, ComponentPayloadPreview};

    let mut out: Vec<(String, String, String, String)> = Vec::new();
    for c in &preview.components {
        let (content, tree) = match &c.payload {
            ComponentPayloadPreview::Portable { payload } => match payload {
                PortableAssetPayload::Skill(s) => {
                    (s.skill_markdown_hash.clone(), s.tree_manifest_hash.clone())
                }
                other => {
                    let bytes = canonical_bytes(other)?;
                    (sha256_hex(&bytes), c.tree_hash.clone().unwrap_or_default())
                }
            },
            ComponentPayloadPreview::Hook { hook } => {
                let bytes = canonical_portable_hook_bytes(hook)?;
                (
                    sha256_hex(&bytes),
                    hook.command_tree_hash.clone().unwrap_or_default(),
                )
            }
        };
        out.push((
            c.kind.as_str().to_string(),
            c.logical_key.clone(),
            content,
            tree,
        ));
    }
    out.sort();
    Ok(out)
}

/// preview 侧有序 residual 集合：(target, residual_kind, tree_hash)。
fn preview_residual_set(preview: &PluginDecompositionPreview) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = preview
        .residuals
        .iter()
        .map(|r| {
            (
                r.target.as_str().to_string(),
                r.residual_kind.as_str().to_string(),
                r.tree_manifest_hash.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

/// 从 package head 的固定 component revision 还原与 preview 同构的内容集合。
///
/// Business Logic: component revision 的 tree/content 指纹才是“包语义”身份，不是 plugin_id。
/// Code Logic: load 每个 component revision payload；与 preview_component_content_set 同算法。
async fn head_component_content_set(
    agent_hub: &AgentHubRepo,
    store: &ObjectStore,
    payload: &crate::agent_hub::plugins::PluginPackagePayload,
) -> Result<Vec<(String, String, String, String)>, AppError> {
    use crate::agent_hub::assets::{from_canonical_bytes, PortableAssetPayload};
    use crate::agent_hub::plugins::{canonical_portable_hook_bytes, from_portable_hook_bytes};

    let mut out: Vec<(String, String, String, String)> = Vec::new();
    for cref in &payload.component_refs {
        let asset = agent_hub.get_asset(&cref.asset_id).await?;
        let logical_key = asset
            .map(|a| a.logical_key)
            .unwrap_or_else(|| cref.asset_id.clone());
        let Some(rev) = agent_hub.get_revision(&cref.revision_id).await? else {
            return Err(AppError::validation(format!(
                "agent_hub_plugin_component_revision_missing:{}",
                cref.revision_id.as_str()
            )));
        };
        let Some(ph) = rev.payload_hash.as_deref() else {
            return Err(AppError::validation(
                "agent_hub_plugin_component_missing_payload_hash".to_string(),
            ));
        };
        let bytes = store.get_blob(ph).await?;
        let (content, tree) = if let Ok(portable) = from_canonical_bytes(&bytes) {
            match portable {
                PortableAssetPayload::Skill(s) => (s.skill_markdown_hash, s.tree_manifest_hash),
                other => {
                    let _ = other;
                    (
                        ph.to_string(),
                        rev.tree_manifest_hash.clone().unwrap_or_default(),
                    )
                }
            }
        } else if let Ok(hook) = from_portable_hook_bytes(&bytes) {
            let hook_bytes = canonical_portable_hook_bytes(&hook)?;
            (
                sha256_hex(&hook_bytes),
                hook.command_tree_hash.unwrap_or_default(),
            )
        } else {
            (
                ph.to_string(),
                rev.tree_manifest_hash.clone().unwrap_or_default(),
            )
        };
        out.push((cref.kind.as_str().to_string(), logical_key, content, tree));
    }
    out.sort();
    Ok(out)
}

/// head residual 集合。
fn head_residual_set(
    payload: &crate::agent_hub::plugins::PluginPackagePayload,
) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = payload
        .residual_refs
        .iter()
        .map(|r| {
            (
                r.target.as_str().to_string(),
                r.residual_kind.as_str().to_string(),
                r.tree_manifest_hash.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

/// 查找同 plugin_id 的 package head，**仅当有序 component/residual 内容集合匹配** 才幂等返回。
///
/// Business Logic: 同 plugin_id 但源更新必须走 append/update，禁止静默 no-op 保留陈旧 head。
/// Code Logic: unique key 找 head → 解析 package payload → 比对 component content/tree 与 residual。
async fn existing_package_head(
    agent_hub: &AgentHubRepo,
    store: &ObjectStore,
    preview: &PluginDecompositionPreview,
) -> Result<Option<(String, String, String)>, AppError> {
    let Some(asset) = agent_hub
        .get_asset_by_unique_key(
            &preview.scope_id,
            AssetKind::Plugin,
            "standalone",
            &preview.plugin_id,
        )
        .await?
    else {
        return Ok(None);
    };
    let Some(rev_id) = asset.current_revision_id.clone() else {
        return Ok(None);
    };
    let Some(rev) = agent_hub.get_revision(&rev_id).await? else {
        return Ok(None);
    };
    let Some(hash) = rev.payload_hash.clone() else {
        return Ok(None);
    };
    let Ok(bytes) = store.get_blob(&hash).await else {
        return Ok(None);
    };
    let Ok(payload) = crate::agent_hub::plugins::from_plugin_package_bytes(&bytes) else {
        return Ok(None);
    };
    if payload.plugin_id != preview.plugin_id {
        return Ok(None);
    }
    let intended_components = preview_component_content_set(preview)?;
    let intended_residuals = preview_residual_set(preview);
    let head_components = head_component_content_set(agent_hub, store, &payload).await?;
    let head_residuals = head_residual_set(&payload);
    if intended_components == head_components && intended_residuals == head_residuals {
        return Ok(Some((asset.id, rev_id.as_str().to_string(), hash)));
    }
    // plugin_id 相同但 component/residual 内容集合已变 → 调用方走 append/update
    Ok(None)
}

#[cfg(test)]
mod tests {
    //! Gate A Task 10 + Gate D Task 7 迁移 / dual-write / plugin / N+2 单测。

    use super::*;
    use crate::agent_hub::instructions::compile_render;
    use crate::agent_hub::models::{AssetPolicy, DesiredPresence};
    use crate::agent_hub::targets::InstructionRenderContext;
    use crate::models::claude_md::{ClaudeMdRow, CLAUDE_MD_ID};
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use crate::storage::claude_md_repo::ClaudeMdRepo;
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// 内存 SQLite + agent_hub + claude_md schema。
    ///
    /// Business Logic: 迁移单测隔离，不触碰真实 home/db。
    /// Code Logic: memory pool + ensure schemas + ClaudeMdRepo/AgentHubRepo。
    async fn setup_repos() -> (AgentHubRepo, ClaudeMdRepo, SqlitePoolOptions) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS claude_md (
                id TEXT PRIMARY KEY NOT NULL,
                content TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                device_id TEXT NOT NULL,
                vector_clock TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let agent_hub = AgentHubRepo::with_gate(pool.clone(), gate.clone());
        let claude_md = ClaudeMdRepo::with_gate(pool, gate);
        // 第三个返回值仅占位保持签名稳定（测试不消费）
        (agent_hub, claude_md, SqlitePoolOptions::new())
    }

    /// 构造 MigrationDeps。
    fn deps<'a>(
        agent_hub: &'a AgentHubRepo,
        claude_md: &'a ClaudeMdRepo,
        objects_root: &'a Path,
    ) -> MigrationDeps<'a> {
        MigrationDeps {
            agent_hub,
            claude_md,
            device_id: "device-test-1",
            object_store_root: objects_root,
        }
    }

    /// 迁移应 seed TargetOnly 资产与 revision，但未选择目标时不创建伪 Absent binding。
    #[tokio::test]
    async fn migration_seeds_user_instruction_target_only_without_target_bindings() {
        let (agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects_root = tmp.path().join("data");
        fs::create_dir_all(&objects_root).unwrap();
        let claude_file = tmp.path().join("CLAUDE.md");
        let body = "# User rules\n\nAlways use Chinese for replies.\n";
        fs::write(&claude_file, body).unwrap();
        // 同时 seed DB 行（内容不同也不会覆盖非空文件）
        claude_md
            .upsert(&ClaudeMdRow {
                id: CLAUDE_MD_ID.into(),
                content: "db-only-should-not-win".into(),
                updated_at: Utc::now().to_rfc3339(),
                device_id: "device-test-1".into(),
                vector_clock: {
                    let mut m = HashMap::new();
                    m.insert("device-test-1".into(), 1);
                    m
                },
            })
            .await
            .unwrap();

        let preview = migrate_user_claude_md_state_with(
            &deps(&agent_hub, &claude_md, &objects_root),
            &claude_file,
        )
        .await
        .unwrap();

        assert_eq!(preview.content_source, "file");
        assert_eq!(preview.policy, "targetOnly");
        assert_eq!(preview.desired_presence, "absent");
        assert!(preview.created_revision);
        assert!(preview.revision_id.is_some());
        assert!(preview.blocks_target_only);

        let asset = agent_hub
            .get_asset(&preview.asset_id)
            .await
            .unwrap()
            .expect("asset");
        assert_eq!(asset.policy, AssetPolicy::TargetOnly);
        assert_eq!(asset.logical_key, USER_INSTRUCTION_LOGICAL_KEY);
        assert_eq!(asset.scope_id, USER_SCOPE_STABLE_ID);

        let bindings = agent_hub
            .list_target_bindings_for_asset(&preview.asset_id)
            .await
            .unwrap();
        assert!(bindings.is_empty(), "未选择 target 必须保持 unmanaged");

        // Claude targetOnly 不会投影到 Codex/OpenCode：diffs 可能为空，但必须精确等于
        // 从 CAS head 文档再次 compile_render 的结果（exact generated diffs）。
        let empty_ctx = InstructionRenderContext::default();
        let rev = agent_hub
            .get_revision(asset.current_revision_id.as_ref().unwrap())
            .await
            .unwrap()
            .unwrap();
        let store = ObjectStore::open(&objects_root).unwrap();
        let bytes = store
            .get_blob(rev.payload_hash.as_ref().unwrap())
            .await
            .unwrap();
        let doc: InstructionDocument = serde_json::from_slice(&bytes).unwrap();
        assert!(doc
            .blocks
            .iter()
            .all(|b| b.mode == InstructionBlockMode::TargetOnly));
        // Claude 侧正文必须保留
        let claude_text =
            String::from_utf8_lossy(&compile_render(&doc, AgentTarget::Claude, &empty_ctx).bytes)
                .into_owned();
        assert!(
            claude_text.contains("Always use Chinese") || claude_text.contains("User rules"),
            "claude projection missing user body: {claude_text:?}"
        );
        let codex2 =
            String::from_utf8_lossy(&compile_render(&doc, AgentTarget::Codex, &empty_ctx).bytes)
                .into_owned();
        let oc2 =
            String::from_utf8_lossy(&compile_render(&doc, AgentTarget::OpenCode, &empty_ctx).bytes)
                .into_owned();
        assert_eq!(
            preview.codex_diff, codex2,
            "codex_diff must be exact compile_render"
        );
        assert_eq!(
            preview.opencode_diff, oc2,
            "opencode_diff must be exact compile_render"
        );
    }

    /// 第二次迁移相同内容不得新建 revision。
    #[tokio::test]
    async fn migration_second_run_is_idempotent_no_new_revision() {
        let (agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects_root = tmp.path().join("data");
        fs::create_dir_all(&objects_root).unwrap();
        let claude_file = tmp.path().join("CLAUDE.md");
        fs::write(&claude_file, "stable migration body\n").unwrap();

        let d = deps(&agent_hub, &claude_md, &objects_root);
        let first = migrate_user_claude_md_state_with(&d, &claude_file)
            .await
            .unwrap();
        assert!(first.created_revision);
        let head1 = first.revision_id.clone().unwrap();

        let second = migrate_user_claude_md_state_with(&d, &claude_file)
            .await
            .unwrap();
        assert!(!second.created_revision);
        assert_eq!(second.revision_id.as_deref(), Some(head1.as_str()));
        assert_eq!(second.payload_hash, first.payload_hash);

        let asset = agent_hub.get_asset(&first.asset_id).await.unwrap().unwrap();
        assert_eq!(
            asset.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(head1.as_str())
        );
    }

    /// dual-write 只更新 legacy 摘要，不走 Hub merge。
    #[tokio::test]
    async fn dual_write_updates_legacy_summary_without_hub_merge() {
        let (_agent_hub, claude_md, _) = setup_repos().await;
        // 隔离到临时 CLAUDE_CONFIG_DIR，避免写真实 home 失败污染断言。
        let tmp = TempDir::new().unwrap();
        let claude_home = tmp.path().join(".claude");
        fs::create_dir_all(&claude_home).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", claude_home.to_string_lossy().as_ref());
        dual_write_legacy_claude_md_summary_with(&claude_md, "d1", "hello hub")
            .await
            .unwrap();
        let row = claude_md.get().await.unwrap().unwrap();
        assert_eq!(row.content, "hello hub");
        assert_eq!(row.device_id, "d1");
        assert_eq!(row.vector_clock.get("d1"), Some(&1));

        // 相同内容 no-op（clock 不变）
        dual_write_legacy_claude_md_summary_with(&claude_md, "d1", "hello hub")
            .await
            .unwrap();
        let row2 = claude_md.get().await.unwrap().unwrap();
        assert_eq!(row2.vector_clock.get("d1"), Some(&1));

        // 内容变化推进 clock
        dual_write_legacy_claude_md_summary_with(&claude_md, "d1", "hello hub v2")
            .await
            .unwrap();
        let row3 = claude_md.get().await.unwrap().unwrap();
        assert_eq!(row3.content, "hello hub v2");
        assert_eq!(row3.vector_clock.get("d1"), Some(&2));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }

    /// 文件空 + DB 有内容时 resolve 用 db。
    #[tokio::test]
    async fn resolve_prefers_db_when_file_empty() {
        let (_agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("CLAUDE.md");
        fs::write(&path, "   \n").unwrap();
        claude_md
            .upsert(&ClaudeMdRow {
                id: CLAUDE_MD_ID.into(),
                content: "from-db".into(),
                updated_at: Utc::now().to_rfc3339(),
                device_id: "d1".into(),
                vector_clock: HashMap::new(),
            })
            .await
            .unwrap();
        let (content, source) = resolve_user_claude_md_content_with(&claude_md, &path)
            .await
            .unwrap();
        assert_eq!(source, "db");
        assert_eq!(content, "from-db");
    }

    /// Business Logic: 迁移二次运行不得把用户已确认 Present binding 刷回 Absent。
    /// Code Logic: first migrate → set Claude Present → second migrate → still Present。
    #[tokio::test]
    async fn migration_second_run_preserves_present_binding() {
        let (agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects_root = tmp.path().join("data");
        fs::create_dir_all(&objects_root).unwrap();
        let claude_file = tmp.path().join("CLAUDE.md");
        fs::write(&claude_file, "body for present preserve\n").unwrap();
        let d = deps(&agent_hub, &claude_md, &objects_root);
        let first = migrate_user_claude_md_state_with(&d, &claude_file)
            .await
            .unwrap();
        agent_hub
            .upsert_target_binding(NewTargetBinding {
                asset_id: first.asset_id.clone(),
                target: AgentTarget::Claude,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await
            .unwrap();
        let second = migrate_user_claude_md_state_with(&d, &claude_file)
            .await
            .unwrap();
        assert_eq!(second.asset_id, first.asset_id);
        let bindings = agent_hub
            .list_target_bindings_for_asset(&first.asset_id)
            .await
            .unwrap();
        let claude = bindings
            .iter()
            .find(|b| b.target == AgentTarget::Claude)
            .expect("claude binding");
        assert_eq!(claude.desired_presence, DesiredPresence::Present);
        assert!(claude.desired_enabled);
    }

    /// Business Logic: hub-on dual-write 只更新 legacy 摘要表，不得绕过 projector 写目标文件。
    /// Code Logic: dual_write 后 DB=content，磁盘文件保持旧值。
    #[tokio::test]
    async fn dual_write_updates_summary_table_without_target_file() {
        let (_agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let claude_home = tmp.path().join(".claude");
        fs::create_dir_all(&claude_home).unwrap();
        let file = claude_home.join("CLAUDE.md");
        fs::write(&file, "old A\n").unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", claude_home.to_string_lossy().as_ref());
        dual_write_legacy_claude_md_summary_with(&claude_md, "device-test-1", "new B\n")
            .await
            .unwrap();
        let written = fs::read_to_string(user_claude_md_file_path().unwrap()).unwrap();
        assert_eq!(written, "old A\n", "dual-write must not write target file");
        let row = claude_md.get().await.unwrap().unwrap();
        assert_eq!(row.content, "new B\n");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }

    // ─── Gate D Task 7 ────────────────────────────────────────────────────

    use crate::agent_hub::models::ScopeKind;
    use crate::agent_hub::plugins::{
        ConfirmedPluginDecomposition, DiscoveredPluginSource, PluginDecompositionPreview,
    };
    use std::io::Write as _;

    fn write_plugin_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    fn claude_plugin_fixture(root: &Path) {
        write_plugin_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{
  "name": "claude-demo",
  "version": "0.1.0",
  "description": "mixed claude plugin",
  "skills": "./skills"
}"#,
        );
        write_plugin_file(
            &root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review carefully\n---\nDo review.\n",
        );
        write_plugin_file(
            &root.join("commands/ship.md"),
            "---\nname: ship\ndescription: Ship it\n---\nShip prompt\n",
        );
        write_plugin_file(
            &root.join("runtime/index.js"),
            "console.log('claude-runtime')\n",
        );
    }

    fn codex_plugin_fixture(root: &Path) {
        write_plugin_file(
            &root.join(".codex-plugin/plugin.json"),
            r#"{
  "name": "codex-demo",
  "version": "1.0.0",
  "description": "codex plugin"
}"#,
        );
        write_plugin_file(
            &root.join("skills/analyze/SKILL.md"),
            "---\nname: analyze\ndescription: Analyze\n---\nAnalyze body\n",
        );
        write_plugin_file(
            &root.join("config.toml"),
            "[agents.worker]\nconfig_file = \"agents/worker.md\"\n",
        );
    }

    fn opencode_plugin_fixture(root: &Path) {
        write_plugin_file(
            &root.join("package.json"),
            r#"{
  "name": "opencode-demo",
  "version": "2.0.0",
  "main": "index.ts",
  "dependencies": { "zod": "3.0.0" }
}"#,
        );
        write_plugin_file(
            &root.join("index.ts"),
            "export default function plugin() { return {}; }\n",
        );
        write_plugin_file(
            &root.join("skills/nearby/SKILL.md"),
            "---\nname: nearby\ndescription: Portable skill\n---\nSkill\n",
        );
    }

    fn discovered(
        plugin_id: &str,
        target: AgentTarget,
        root: &Path,
        scope_id: &str,
    ) -> DiscoveredPluginSource {
        DiscoveredPluginSource {
            plugin_id: plugin_id.into(),
            name: plugin_id.into(),
            version: None,
            description: None,
            source_target: target,
            root_path: root.to_path_buf(),
            scope_id: scope_id.into(),
            scope_kind: ScopeKind::User,
        }
    }

    /// Business Logic: 首次迁移只生成 preview，不 append revision。
    /// Code Logic: 三 target fixture → preview_plugin_migration → created_revisions=0。
    #[tokio::test]
    async fn plugin_migration_first_run_is_preview_only() {
        let (agent_hub, _claude_md, _) = setup_repos().await;
        agent_hub
            .insert_scope(NewScopeNode {
                id: Some(USER_SCOPE_STABLE_ID.into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let tmp = TempDir::new().unwrap();
        let objects = tmp.path().join("data");
        fs::create_dir_all(&objects).unwrap();
        let claude_root = tmp.path().join("claude-plugin");
        let codex_root = tmp.path().join("codex-plugin");
        let oc_root = tmp.path().join("opencode-plugin");
        claude_plugin_fixture(&claude_root);
        codex_plugin_fixture(&codex_root);
        opencode_plugin_fixture(&oc_root);

        let sources = vec![
            discovered(
                "claude-demo",
                AgentTarget::Claude,
                &claude_root,
                USER_SCOPE_STABLE_ID,
            ),
            discovered(
                "codex-demo",
                AgentTarget::Codex,
                &codex_root,
                USER_SCOPE_STABLE_ID,
            ),
            discovered(
                "opencode-demo",
                AgentTarget::OpenCode,
                &oc_root,
                USER_SCOPE_STABLE_ID,
            ),
        ];
        let preview = preview_plugin_migration(&agent_hub, &objects, &sources)
            .await
            .unwrap();
        assert_eq!(preview.created_revisions, 0);
        assert_eq!(preview.items.len(), 3);
        for item in &preview.items {
            assert!(!item.already_imported);
            assert!(
                item.status == "preview" || item.status == "sourceOnly",
                "unexpected status {}",
                item.status
            );
            assert!(
                !item.preview.components.is_empty() || !item.preview.residuals.is_empty(),
                "empty preview for {}",
                item.source.plugin_id
            );
        }
        // 无 package 资产写入
        let pkg = agent_hub
            .get_asset_by_unique_key(
                USER_SCOPE_STABLE_ID,
                AssetKind::Plugin,
                "standalone",
                "claude-demo",
            )
            .await
            .unwrap();
        assert!(pkg.is_none());
    }

    /// Business Logic: 确认 import 一个 package/child graph；二次运行无新 revision。
    /// Code Logic: confirm → created；再 confirm → idempotent。
    #[tokio::test]
    async fn plugin_migration_confirm_import_is_idempotent() {
        let (agent_hub, _claude_md, _) = setup_repos().await;
        agent_hub
            .insert_scope(NewScopeNode {
                id: Some(USER_SCOPE_STABLE_ID.into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let tmp = TempDir::new().unwrap();
        let objects = tmp.path().join("data");
        fs::create_dir_all(&objects).unwrap();
        let root = tmp.path().join("claude-plugin");
        claude_plugin_fixture(&root);
        let source = discovered(
            "claude-demo",
            AgentTarget::Claude,
            &root,
            USER_SCOPE_STABLE_ID,
        );
        let preview = preview_plugin_migration(&agent_hub, &objects, &[source])
            .await
            .unwrap();
        let item = &preview.items[0];
        let confirmed = ConfirmedPluginDecomposition {
            preview: item.preview.clone(),
            link_standalone: Default::default(),
            origin_replica_id: "device-test-1".into(),
        };
        let first = confirm_plugin_migration_import(&agent_hub, &objects, confirmed.clone(), None)
            .await
            .unwrap();
        assert!(first.created_revision);
        assert_eq!(first.status, "imported");
        assert!(!first.package_asset_id.is_empty());

        let second = confirm_plugin_migration_import(&agent_hub, &objects, confirmed, None)
            .await
            .unwrap();
        assert!(!second.created_revision);
        assert_eq!(second.status, "idempotent");
        assert_eq!(second.package_asset_id, first.package_asset_id);
        assert_eq!(second.revision_id, first.revision_id);
        assert_eq!(second.payload_hash, first.payload_hash);
    }

    /// Business Logic: 同 plugin_id 但 component 内容集合变化时不得 idempotent 短路。
    /// Code Logic: confirm → 改 skill 内容并 re-inspect → confirm 必须 created_revision。
    #[tokio::test]
    async fn plugin_migration_confirm_appends_when_component_content_changes() {
        let (agent_hub, _claude_md, _) = setup_repos().await;
        agent_hub
            .insert_scope(NewScopeNode {
                id: Some(USER_SCOPE_STABLE_ID.into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let tmp = TempDir::new().unwrap();
        let objects = tmp.path().join("data");
        fs::create_dir_all(&objects).unwrap();
        let root = tmp.path().join("claude-plugin");
        claude_plugin_fixture(&root);
        let source = discovered(
            "claude-demo",
            AgentTarget::Claude,
            &root,
            USER_SCOPE_STABLE_ID,
        );
        let preview = preview_plugin_migration(&agent_hub, &objects, std::slice::from_ref(&source))
            .await
            .unwrap();
        let first = confirm_plugin_migration_import(
            &agent_hub,
            &objects,
            ConfirmedPluginDecomposition {
                preview: preview.items[0].preview.clone(),
                link_standalone: Default::default(),
                origin_replica_id: "device-test-1".into(),
            },
            None,
        )
        .await
        .unwrap();
        assert!(first.created_revision);

        // mutate skill content so component content set diverges
        write_plugin_file(
            &root.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Portable skill UPDATED\n---\nSkill body changed\n",
        );
        let preview2 =
            preview_plugin_migration(&agent_hub, &objects, std::slice::from_ref(&source))
                .await
                .unwrap();
        let second = confirm_plugin_migration_import(
            &agent_hub,
            &objects,
            ConfirmedPluginDecomposition {
                preview: preview2.items[0].preview.clone(),
                link_standalone: Default::default(),
                origin_replica_id: "device-test-1".into(),
            },
            None,
        )
        .await
        .unwrap();
        assert!(
            second.created_revision,
            "source-updated re-confirm must append, got {:?}",
            second
        );
        assert_eq!(second.status, "imported");
        assert_ne!(second.revision_id, first.revision_id);
        assert_ne!(second.payload_hash, first.payload_hash);
    }

    /// Business Logic: 碰撞/未验证激活保持 sourceOnly/externalCollision，不写 revision。
    /// Code Logic: force_status 短路。
    #[tokio::test]
    async fn plugin_migration_collision_stays_source_only_or_external_collision() {
        let (agent_hub, _claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects = tmp.path().join("data");
        fs::create_dir_all(&objects).unwrap();
        // 空 confirmed（不会真正 import）
        let confirmed = ConfirmedPluginDecomposition {
            preview: PluginDecompositionPreview {
                plugin_id: "x".into(),
                name: "x".into(),
                version: None,
                description: None,
                source_target: AgentTarget::Claude,
                scope_id: USER_SCOPE_STABLE_ID.into(),
                scope_kind: ScopeKind::User,
                root_path: tmp.path().to_path_buf(),
                components: vec![],
                residuals: vec![],
                target_extensions: Default::default(),
            },
            link_standalone: Default::default(),
            origin_replica_id: "d1".into(),
        };
        let so = confirm_plugin_migration_import(
            &agent_hub,
            &objects,
            confirmed.clone(),
            Some("sourceOnly"),
        )
        .await
        .unwrap();
        assert!(!so.created_revision);
        assert_eq!(so.status, "sourceOnly");

        let ec = confirm_plugin_migration_import(
            &agent_hub,
            &objects,
            confirmed,
            Some("externalCollision"),
        )
        .await
        .unwrap();
        assert!(!ec.created_revision);
        assert_eq!(ec.status, "externalCollision");
    }

    /// Business Logic: Hub 关闭后最后 target 文件仍可读；不 GC CAS；忽略未知 Hub 表。
    /// Code Logic: downgrade snapshot + policy 断言。
    #[tokio::test]
    async fn downgrade_keeps_last_target_files_and_never_cleans_cas() {
        let (agent_hub, _claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let target_file = tmp.path().join("CLAUDE.md");
        fs::write(&target_file, "last successful projection\n").unwrap();
        // Hub off
        let off = downgrade_compatibility_facade_snapshot(
            &agent_hub,
            false,
            std::slice::from_ref(&target_file),
        )
        .await
        .unwrap();
        assert!(!off.policy.hub_enabled);
        assert!(off.policy.allow_direct_target_mutation);
        assert!(off.policy.ignore_unknown_hub_tables);
        assert!(!off.policy.allow_cas_gc);
        assert!(!off.policy.translate_from_hub);
        assert_eq!(off.readable_target_files, vec![target_file.clone()]);
        assert!(!off.cas_gc_attempted);
        assert_eq!(off.recoverable_projection_jobs, 0);

        // Hub on → translate + 可恢复 job 查询（空库 = 0）
        let on = downgrade_compatibility_facade_snapshot(&agent_hub, true, &[target_file])
            .await
            .unwrap();
        assert!(on.policy.hub_enabled);
        assert!(on.policy.translate_from_hub);
        assert!(!on.policy.allow_direct_target_mutation);
        assert!(!on.policy.allow_cas_gc);
        assert!(!on.cas_gc_attempted);
    }

    /// Business Logic: N+2 删除仅当 version>=earliest 且有 evidence。
    /// Code Logic: 当前 evidence=None → 任意版本 removal_allowed=false；模拟 evidence 后 0.10.0 通过。
    #[test]
    fn n_plus_two_guard_requires_version_and_evidence() {
        let status = legacy_agent_asset_compatibility_status(AGENT_HUB_GA_VERSION);
        assert_eq!(status.ga_version, AGENT_HUB_GA_VERSION);
        assert_eq!(
            status.earliest_removal_version,
            EARLIEST_LEGACY_REMOVAL_VERSION
        );
        assert!(status.stable_migration_evidence.is_none());
        assert!(!status.removal_allowed);
        assert!(status.legacy_routes_registered);
        assert!(status.legacy_ui_hidden);
        assert!(!status.removal_checklist.is_empty());
        assert!(!n_plus_two_removal_allowed("0.10.0"));
        assert!(!n_plus_two_removal_allowed("99.0.0"));
        assert!(version_cmp("0.10.0", EARLIEST_LEGACY_REMOVAL_VERSION) >= 0);
        assert!(version_cmp("0.8.2", EARLIEST_LEGACY_REMOVAL_VERSION) < 0);
        assert!(version_cmp("0.9.9", "0.10.0") < 0);
    }

    /// Business Logic: 旧 façade 在 Hub 启用时禁止二次直接 mutation。
    #[test]
    fn hub_enabled_facade_translates_without_direct_mutation() {
        let p = legacy_facade_policy(true);
        assert!(p.translate_from_hub);
        assert!(!p.allow_direct_target_mutation);
        assert!(!p.allow_cas_gc);
        let p2 = legacy_facade_policy(false);
        assert!(p2.allow_direct_target_mutation);
        assert!(p2.ignore_unknown_hub_tables);
    }

    /// R5 P2.3: green path — first call with empty Hub head returns `Seeded` and creates
    /// one migration revision with three Absent bindings.
    #[tokio::test]
    async fn seed_user_instruction_if_head_null_seeds_when_no_head() {
        let (agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects_root = tmp.path().join("data");
        fs::create_dir_all(&objects_root).unwrap();
        let claude_file = tmp.path().join("CLAUDE.md");
        fs::write(&claude_file, "always prefer Rust over Python\n").unwrap();

        let outcome = seed_user_instruction_with_deps(
            &deps(&agent_hub, &claude_md, &objects_root),
            &claude_file,
        )
        .await
        .expect("seed must succeed");
        assert_eq!(outcome, SeedUserInstructionOutcome::Seeded);

        // head must now be set and bindings seeded (3 Absent).
        let scope_id = agent_hub
            .resolve_user_scope_id()
            .await
            .unwrap()
            .expect("scope present");
        let asset = agent_hub
            .get_asset_by_unique_key(
                &scope_id,
                crate::agent_hub::models::AssetKind::Instruction,
                USER_INSTRUCTION_NAMESPACE,
                USER_INSTRUCTION_LOGICAL_KEY,
            )
            .await
            .unwrap()
            .expect("asset present");
        assert!(
            asset.current_revision_id.is_some(),
            "head must be populated"
        );
        let bindings = agent_hub
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        assert!(bindings.is_empty(), "seed 不得创建 legacy Absent 伪绑定");
    }

    /// R5 P2.3: simulated concurrent-import race — once an external head exists, the
    /// helper must report `HeadAlreadyPresent` and must **not** append a migration revision
    /// or re-seed bindings.
    #[tokio::test]
    async fn seed_user_instruction_if_head_null_returns_head_already_present_under_simulated_race()
    {
        let (agent_hub, claude_md, _) = setup_repos().await;
        let tmp = TempDir::new().unwrap();
        let objects_root = tmp.path().join("data");
        fs::create_dir_all(&objects_root).unwrap();
        let claude_file = tmp.path().join("CLAUDE.md");
        fs::write(&claude_file, "concurrent import body\n").unwrap();

        // Step 1: prime the head via a normal Seeded run.
        let first = seed_user_instruction_with_deps(
            &deps(&agent_hub, &claude_md, &objects_root),
            &claude_file,
        )
        .await
        .expect("first seed must succeed");
        assert_eq!(first, SeedUserInstructionOutcome::Seeded);

        // Step 2: simulate a concurrent import that pushes an unrelated revision on top.
        let scope_id = agent_hub
            .resolve_user_scope_id()
            .await
            .unwrap()
            .expect("scope present after first seed");
        let asset = agent_hub
            .get_asset_by_unique_key(
                &scope_id,
                crate::agent_hub::models::AssetKind::Instruction,
                USER_INSTRUCTION_NAMESPACE,
                USER_INSTRUCTION_LOGICAL_KEY,
            )
            .await
            .unwrap()
            .expect("asset present after first seed");

        let store = ObjectStore::open(&objects_root).unwrap();
        let pre_bytes = br#"{"blocks":[]}"#;
        let stored = store.put_blob(pre_bytes).await.unwrap();
        let concurrent_rev = agent_hub
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: Some(AgentTarget::Claude),
                origin_replica_id: "concurrent-device".to_string(),
                payload_hash: Some(stored.hash.clone()),
                tree_manifest_hash: None,
                created_at: Utc::now().to_rfc3339(),
                expected_parent_id: None,
            })
            .await
            .expect("concurrent head append");
        // Sanity: head is now the concurrent revision.
        let pre_asset = agent_hub.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            pre_asset.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(concurrent_rev.id.as_str())
        );

        // Step 3: now run the seed — must return HeadAlreadyPresent without touching the head.
        let outcome = seed_user_instruction_with_deps(
            &deps(&agent_hub, &claude_md, &objects_root),
            &claude_file,
        )
        .await
        .expect("seed must not error under race");
        assert_eq!(outcome, SeedUserInstructionOutcome::HeadAlreadyPresent);

        let after = agent_hub.get_asset(&asset.id).await.unwrap().unwrap();
        assert_eq!(
            after.current_revision_id.as_ref().map(|r| r.as_str()),
            Some(concurrent_rev.id.as_str()),
            "concurrent head must not be overwritten by the migration seed"
        );
    }
}
