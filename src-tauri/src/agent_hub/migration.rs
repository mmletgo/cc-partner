//! agent_hub/migration — 用户级 CLAUDE.md → Agent Hub 迁移与 N/N+1 dual-write
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate A Task 10：把既有 `~/.claude/CLAUDE.md` 与 `claude_md` 表权威正文迁入 Hub
//!     的 user-scope instruction asset，并在 Hub 编辑路径上 dual-write 回 legacy 摘要行。
//!     迁移后三 target 绑定先 `desiredPresence=Absent`，等待用户确认再投影。
//!
//! Code Logic（这个模块做什么）:
//!     - 解析文件/DB 内容源（文件优先非空，其次 DB，否则空）
//!     - 幂等 seed：user scope + TargetOnly instruction asset + Migration revision
//!     - 为 Claude/Codex/OpenCode upsert Absent 绑定
//!     - 预览 Codex/OpenCode compile_render 全文
//!     - dual-write 仅更新 legacy `claude_md` 摘要（content/updated_at/device_id/vc），
//!       **永不**用 legacy vector_clock 裁决 Hub 冲突

use crate::agent_hub::instructions::{
    classify_import, compile_render, ImportScopeContext, InstructionBlockMode, InstructionDocument,
    TargetMarkdownSource,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset, NewLogicalAsset,
    NewRevision, NewScopeNode, NewTargetBinding, RevisionId, RevisionOperation, RevisionOriginKind,
    ScopeKind,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::targets::InstructionRenderContext;
use crate::error::AppError;
use crate::models::claude_md::{ClaudeMdRow, CLAUDE_MD_ID};
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use crate::storage::claude_md_repo::ClaudeMdRepo;
use crate::sync::vector_clock;
use chrono::Utc;
use std::fs;
use std::path::Path;

/// 用户级 scope 的稳定主键（跨设备/重启不变）。
pub const USER_SCOPE_STABLE_ID: &str = "agent-hub-scope-user";
/// 用户指令资产逻辑键（对齐 Claude 文件名）。
pub const USER_INSTRUCTION_LOGICAL_KEY: &str = "CLAUDE.md";
/// 独立（非 plugin）命名空间。
pub const USER_INSTRUCTION_NAMESPACE: &str = "standalone";
/// UI 展示名。
pub const USER_INSTRUCTION_DISPLAY_NAME: &str = "User CLAUDE.md";

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
///     且第二遍同 hash 不重复 revision；投影绑定先 Absent，避免未确认就写 Codex/OpenCode。
///
/// Code Logic（这个函数做什么）:
///     resolve content → ensure user scope/asset → classify_import(Claude) → CAS put →
///     同 payload_hash 跳过 append → 否则 Migration revision → 三 target Absent 绑定 →
///     compile_render Codex/OpenCode 全文预览。
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

    // 三 target 绑定：未确认前 Absent + disabled
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        deps.agent_hub
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
            })
            .await?;
    }

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

/// Dual-write legacy `claude_md` 摘要行（仅 content + 元数据）。
///
/// Business Logic（为什么需要这个函数）:
///     N/N+1 兼容旧 CLAUDE.md 页面/P2P：Hub 写成功后同步摘要到旧表，
///     但 **legacy vector_clock 永不裁决 Hub 冲突**（Hub 以 revision DAG 为权威）。
///
/// Code Logic（这个函数做什么）:
///     content 与现有行相等 → no-op；否则 upsert content/updated_at/device_id +
///     `increment(device_id)`（仅服务旧表 peer 兼容，不读 VC 做 merge 决策）。
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
///     单测与生产共用摘要 upsert 语义。
///
/// Code Logic（这个函数做什么）:
///     见 `dual_write_legacy_claude_md_summary`。
pub async fn dual_write_legacy_claude_md_summary_with(
    claude_md: &ClaudeMdRepo,
    device_id: &str,
    content: &str,
) -> Result<(), AppError> {
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

/// 若 head payload_hash 相同则跳过，否则 append Migration revision。
///
/// Business Logic（为什么需要这个函数）:
///     迁移幂等：同一正文二次运行不得产生新 revision。
///
/// Code Logic（这个函数做什么）:
///     读 current revision；hash 相同 → (false, Some(head_id))；否则 append_revision。
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

#[cfg(test)]
mod tests {
    //! Gate A Task 10 迁移 / dual-write 单测。

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

    /// 迁移应 seed TargetOnly 资产、1 revision、Absent 绑定，且 diffs 精确等于 compile_render。
    #[tokio::test]
    async fn migration_seeds_user_instruction_target_only_and_absent_bindings() {
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
        assert_eq!(bindings.len(), 3);
        for b in &bindings {
            assert_eq!(b.desired_presence, DesiredPresence::Absent);
            assert!(!b.desired_enabled);
        }

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
}
