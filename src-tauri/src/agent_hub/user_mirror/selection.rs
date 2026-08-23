//! user_mirror/selection — 把用户级 inventory 冻结为 SnapshotEnvelope + CAS
//!
//! Business Logic（为什么需要这个模块）:
//!     源端必须在传输前冻结三槽、原生提示词文件和 Skill/Command/Plugin/MCP 字节；
//!     冻结不得 adopt/卸载本机资产，凭据只进 CAS 对象、不得进 envelope 元数据。
//!
//! Code Logic（这个模块做什么）:
//!     按 inventory 身份读源文件 → `ObjectStore::put_blob` / tree → 组装 envelope 与
//!     item_bindings；累计超 512 MiB fail-closed；legacyLossy MCP 标 blocked 且不把占位当凭据。

use super::models::{
    UserMirrorAgentInventoryDto, UserMirrorInventoryDto, USER_MIRROR_DEST_MAX_TOTAL_BYTES,
    USER_MIRROR_PLAN_TTL_MINUTES, USER_MIRROR_TRANSFER_LIMIT,
};
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_with_env_query, PortableAssetKind, PortableInventoryItemDto,
    PortableInventoryQuery,
};
use crate::agent_hub::service::instruction_document_from_block_dtos;
use crate::agent_hub::snapshot::envelope::{
    compute_snapshot_hash, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
    SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
    FORMAT_NAME, FORMAT_VERSION,
};
use crate::agent_hub::snapshot::portable_builder::{
    bytes_are_legacy_lossy, pack_inventory_item, pack_plugin_item,
};
use crate::agent_hub::targets::{TargetEnvironment, TargetPathResolver};
use crate::agent_hub::user_instructions::{
    extract_slot_text, inspect_user_instruction_workspace_with_env, user_level_mirror_native_paths,
    InstructionSlotKey, MAX_NATIVE_FILE_BYTES,
};
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// 源端冻结后的 envelope、CAS 字节与 identity→hash 绑定。
///
/// Business Logic: 后续 chunk 读取与 dest apply 都依赖这次冻结的对象集。
/// Code Logic: `object_bytes` 供进程内 staging；`item_bindings` 对号入座。
#[derive(Debug, Clone)]
pub struct BuiltUserMirrorSelection {
    /// SnapshotEnvelope v1（无 secret 正文）
    pub envelope: SnapshotEnvelopeV1,
    /// object_hash → 完整字节（含 MCP 凭据原文）
    pub object_bytes: BTreeMap<String, Vec<u8>>,
    /// 进程内 staging 键，供后续 chunk 读取
    pub transfer_id: String,
    /// logical_id 或 (target,kind,native_id) → object hash
    pub item_bindings: Vec<UserMirrorObjectBinding>,
}

/// 冻结条目与 CAS 对象的对应关系。
///
/// Business Logic: dest apply 按逻辑 id / portable 身份取对象，不得依赖绝对路径。
/// Code Logic: 原生文件填 `logical_id`；portable 填 `kind`+`native_id`；blocked 表示不得当凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorObjectBinding {
    /// 所属 Agent
    pub target: AgentTarget,
    /// 原生文件 / Hub 槽逻辑 id
    pub logical_id: Option<String>,
    /// portable 资产种类
    pub kind: Option<PortableAssetKind>,
    /// portable 原生 id
    pub native_id: Option<String>,
    /// CAS object SHA-256；blocked 且跳过写入时为空
    pub object_hash: String,
    /// legacyLossy 或不可用凭据，apply 不得覆盖真凭据
    pub blocked: bool,
}

/// 进程内冻结对象 staging（transfer_id → bytes），TTL 与 plan 对齐。
struct StagedUserMirrorSelection {
    object_bytes: BTreeMap<String, Vec<u8>>,
    created_at: Instant,
}

fn staging_map() -> &'static Mutex<BTreeMap<String, StagedUserMirrorSelection>> {
    static MAP: OnceLock<Mutex<BTreeMap<String, StagedUserMirrorSelection>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// 登记冻结对象，供后续 chunk 读取。
///
/// Business Logic: 源端 objects 路由按 transfer 续传，不得把整包长期留在内存。
/// Code Logic: TTL 淘汰后插入；失败不阻断已构建的返回值。
fn staging_insert(transfer_id: String, object_bytes: BTreeMap<String, Vec<u8>>) {
    let ttl = Duration::from_secs(USER_MIRROR_PLAN_TTL_MINUTES.saturating_mul(60) as u64);
    if let Ok(mut guard) = staging_map().lock() {
        let now = Instant::now();
        guard.retain(|_, staged| now.duration_since(staged.created_at) < ttl);
        guard.insert(
            transfer_id,
            StagedUserMirrorSelection {
                object_bytes,
                created_at: now,
            },
        );
    }
}

/// 源端按 offset 读取已冻结 object chunk（≤ 8 MiB）。
///
/// Business Logic: LAN objects 路由续传同一 transfer，不把整包再载入。
/// Code Logic: staging 查找 hash；越界返回空；未登记则 not_found。
pub fn source_read_user_mirror_object_chunk(
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    const MAX_CHUNK: usize = 8 * 1024 * 1024;
    let mut guard = staging_map()
        .lock()
        .map_err(|_| AppError::generic("user_mirror staging lock"))?;
    let now = Instant::now();
    let ttl = Duration::from_secs(USER_MIRROR_PLAN_TTL_MINUTES.saturating_mul(60) as u64);
    guard.retain(|_, staged| now.duration_since(staged.created_at) < ttl);
    let staged = guard
        .get(transfer_id)
        .ok_or_else(|| AppError::not_found("USER_MIRROR_TRANSFER_NOT_FOUND".to_string()))?;
    let bytes = staged
        .object_bytes
        .get(object_hash)
        .ok_or_else(|| AppError::not_found("USER_MIRROR_OBJECT_NOT_FOUND".to_string()))?;
    if offset as usize >= bytes.len() {
        return Ok(Vec::new());
    }
    let end = (offset as usize).saturating_add(MAX_CHUNK).min(bytes.len());
    Ok(bytes[offset as usize..end].to_vec())
}

/// 冻结过程累加器。
struct FreezeAcc {
    store: ObjectStore,
    replica_id: String,
    created_at: String,
    projected: u64,
    object_bytes: BTreeMap<String, Vec<u8>>,
    seen: BTreeSet<String>,
    objects: Vec<SnapshotObjectDescriptor>,
    assets: Vec<SnapshotAsset>,
    lineages: Vec<SnapshotLineage>,
    revisions: Vec<SnapshotRevision>,
    asset_heads: BTreeMap<String, Vec<String>>,
    bindings: Vec<UserMirrorObjectBinding>,
}

/// 把本机用户级 inventory 冻结进 CAS 与 SnapshotEnvelope。
///
/// Business Logic（为什么需要这个函数）:
///     Pull/Push 传输前源端必须冻结当前用户级事实；冻结本身不得改变源磁盘纳管状态。
///
/// Code Logic（这个函数做什么）:
///     读白名单原生文件、Hub 槽正文与 portable 源路径 → put CAS → 组装 envelope；
///     超 512 MiB 返回 `USER_MIRROR_TRANSFER_LIMIT`。
pub async fn freeze_user_mirror_selection(
    state: &AppState,
    inventory: &UserMirrorInventoryDto,
) -> Result<BuiltUserMirrorSelection, AppError> {
    let env = TargetEnvironment::from_process();
    freeze_user_mirror_selection_with_env(state, inventory, &env).await
}

/// 注入环境下的冻结（测试与生产共用规则）。
///
/// Business Logic: 隔离 HOME 必须与生产走同一白名单，且不得触发 adopt。
/// Code Logic: 扫原生路径 + user-scope portable → 写入 CAS → 组装 envelope / staging。
pub(crate) async fn freeze_user_mirror_selection_with_env(
    state: &AppState,
    inventory: &UserMirrorInventoryDto,
    env: &TargetEnvironment,
) -> Result<BuiltUserMirrorSelection, AppError> {
    let data_dir = crate::config::data_dir()?;
    let mut acc = FreezeAcc {
        store: ObjectStore::open(&data_dir)?,
        replica_id: replica_id(state, inventory),
        created_at: Utc::now().to_rfc3339(),
        projected: 0,
        object_bytes: BTreeMap::new(),
        seen: BTreeSet::new(),
        objects: Vec::new(),
        assets: Vec::new(),
        lineages: Vec::new(),
        revisions: Vec::new(),
        asset_heads: BTreeMap::new(),
        bindings: Vec::new(),
    };
    freeze_native_and_hub_slots(&mut acc, state, inventory, env).await?;
    freeze_portable_items(&mut acc, state, inventory, env).await?;
    let envelope = finish_envelope(&mut acc, inventory)?;
    let transfer_id = format!("umirror-src-{}", Uuid::now_v7());
    staging_insert(transfer_id.clone(), acc.object_bytes.clone());
    Ok(BuiltUserMirrorSelection {
        envelope,
        object_bytes: acc.object_bytes,
        transfer_id,
        item_bindings: acc.bindings,
    })
}

/// 累计 object 字节前检查 512 MiB 上限。
///
/// Business Logic: 超限不得部分冻结后静默截断。
/// Code Logic: `current+next > USER_MIRROR_DEST_MAX_TOTAL_BYTES` → validation。
pub(crate) fn ensure_user_mirror_bytes_budget(current: u64, next: u64) -> Result<(), AppError> {
    if current.saturating_add(next) > USER_MIRROR_DEST_MAX_TOTAL_BYTES {
        return Err(AppError::validation(USER_MIRROR_TRANSFER_LIMIT.to_string()));
    }
    Ok(())
}

/// 选择 envelope 的 replica id：优先合法 UUID。
fn replica_id(state: &AppState, inventory: &UserMirrorInventoryDto) -> String {
    if Uuid::parse_str(&inventory.source_device_id).is_ok() {
        inventory.source_device_id.clone()
    } else if Uuid::parse_str(state.device_id.as_str()).is_ok() {
        state.device_id.as_str().to_string()
    } else {
        Uuid::now_v7().to_string()
    }
}

/// 冻结白名单原生文件与 Hub 三槽正文。
///
/// Business Logic: dest 必须拿到源端实际文件字节，而不是用槽重新编译近似。
/// Code Logic: 按 logical_id 对白名单路径有界读；槽文本来自 workspace canonical。
async fn freeze_native_and_hub_slots(
    acc: &mut FreezeAcc,
    state: &AppState,
    inventory: &UserMirrorInventoryDto,
    env: &TargetEnvironment,
) -> Result<(), AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let path_by_id: HashMap<(AgentTarget, String), PathBuf> =
        user_level_mirror_native_paths(&homes)
            .into_iter()
            .map(|(target, logical_id, path)| ((target, logical_id), path))
            .collect();
    let workspace = inspect_user_instruction_workspace_with_env(state, env).await?;
    let document = workspace
        .canonical
        .as_ref()
        .map(|canonical| instruction_document_from_block_dtos(&canonical.blocks))
        .transpose()?;
    let cap = MAX_NATIVE_FILE_BYTES as u64;
    for agent in &inventory.agents {
        for file in &agent.native_files {
            if !file.exists || file.size == 0 || file.size > cap {
                continue;
            }
            let Some(path) = path_by_id.get(&(agent.target, file.logical_id.clone())) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let bytes = match fs::read(path) {
                Ok(bytes) if (bytes.len() as u64) <= cap && !bytes.is_empty() => bytes,
                _ => continue,
            };
            let hash = record_bytes(acc, bytes, false).await?;
            push_asset(
                acc,
                agent.target,
                Some(file.logical_id.as_str()),
                None,
                None,
                AssetKind::Instruction,
                "native-file",
                hash,
                None,
                false,
            );
        }
        if let Some(document) = document.as_ref() {
            freeze_hub_slot_text(
                acc,
                agent,
                agent.slots.common.is_some(),
                &extract_slot_text(document, InstructionSlotKey::Shared),
                "common",
            )
            .await?;
            freeze_hub_slot_text(
                acc,
                agent,
                agent.slots.adapted.is_some(),
                &extract_slot_text(
                    document,
                    InstructionSlotKey::Adapted {
                        agent: agent.target,
                    },
                ),
                "adapted",
            )
            .await?;
            freeze_hub_slot_text(
                acc,
                agent,
                agent.slots.exclusive.is_some(),
                &extract_slot_text(
                    document,
                    InstructionSlotKey::TargetOnly {
                        agent: agent.target,
                    },
                ),
                "exclusive",
            )
            .await?;
        }
    }
    Ok(())
}

/// 把单个 Hub 槽正文写入 CAS（空槽跳过）。
async fn freeze_hub_slot_text(
    acc: &mut FreezeAcc,
    agent: &UserMirrorAgentInventoryDto,
    present: bool,
    text: &str,
    slot: &str,
) -> Result<(), AppError> {
    if !present || text.is_empty() {
        return Ok(());
    }
    let logical_id = format!("{}.hub.{slot}", agent.target.as_str());
    let hash = record_bytes(acc, text.as_bytes().to_vec(), false).await?;
    push_asset(
        acc,
        agent.target,
        Some(&logical_id),
        None,
        None,
        AssetKind::Instruction,
        "hub-slot",
        hash,
        None,
        false,
    );
    Ok(())
}

/// 按 inventory portable 身份冻结 Skill/Command/Plugin/MCP。
///
/// Business Logic: 源端只冻结事实，不 adopt；MCP 凭据进 CAS；legacyLossy 标 blocked。
/// Code Logic: live scan 用 (target,kind,native_id) 对齐；skill/plugin 先写 tree。
async fn freeze_portable_items(
    acc: &mut FreezeAcc,
    state: &AppState,
    inventory: &UserMirrorInventoryDto,
    env: &TargetEnvironment,
) -> Result<(), AppError> {
    let snapshot = inspect_portable_inventory_with_env_query(
        state,
        env,
        PortableInventoryQuery {
            target: None,
            kind: None,
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        },
    )
    .await?;
    let mut live: HashMap<(AgentTarget, PortableAssetKind, String), PortableInventoryItemDto> =
        HashMap::new();
    for item in snapshot.items {
        if item.scope_kind != ScopeKind::User {
            continue;
        }
        live.insert((item.target, item.kind, item.native_id.clone()), item);
    }
    for agent in &inventory.agents {
        for listed in &agent.items {
            let Some(item) = live
                .get(&(agent.target, listed.kind, listed.native_id.clone()))
                .cloned()
            else {
                continue;
            };
            freeze_one_portable(acc, agent.target, &item).await?;
        }
    }
    Ok(())
}

/// 冻结单条 portable 资产。
///
/// Business Logic: Skill/Plugin 目录树与 MCP 凭据必须进 CAS；legacyLossy 不得当真凭据。
/// Code Logic: 先 put_tree；再 pack payload；占位凭据只记 blocked binding。
async fn freeze_one_portable(
    acc: &mut FreezeAcc,
    target: AgentTarget,
    item: &PortableInventoryItemDto,
) -> Result<(), AppError> {
    let mut tree_hash: Option<String> = None;
    if matches!(
        item.kind,
        PortableAssetKind::Skill | PortableAssetKind::Plugin
    ) {
        if let Some(dir) = portable_tree_dir(item) {
            match record_tree_from_directory(acc, &dir).await {
                Ok(hash) => tree_hash = Some(hash),
                Err(err) if item.kind == PortableAssetKind::Plugin => return Err(err),
                Err(_) => {}
            }
        } else if item.kind == PortableAssetKind::Plugin {
            return Err(AppError::validation(format!(
                "USER_MIRROR_PLUGIN_TREE_UNAVAILABLE:{}",
                item.native_id
            )));
        }
    }

    let packed = if item.kind == PortableAssetKind::Plugin {
        pack_plugin_item(item, tree_hash.as_deref())?
    } else {
        pack_inventory_item(item)?
    };
    let legacy_lossy = packed.legacy_lossy || bytes_are_legacy_lossy(&packed.bytes);
    if legacy_lossy {
        acc.bindings.push(UserMirrorObjectBinding {
            target,
            logical_id: None,
            kind: Some(item.kind),
            native_id: Some(item.native_id.clone()),
            object_hash: String::new(),
            blocked: true,
        });
        return Ok(());
    }

    let hash = record_bytes(acc, packed.bytes, false).await?;
    push_asset(
        acc,
        target,
        None,
        Some(item.kind),
        Some(item.native_id.as_str()),
        item.kind.to_asset_kind(),
        item.source_origin.default_origin_namespace(),
        hash,
        tree_hash.or_else(|| item.tree_hash.clone()),
        false,
    );
    Ok(())
}

/// Skill/Plugin 目录根：文件则取其父目录。
fn portable_tree_dir(item: &PortableInventoryItemDto) -> Option<PathBuf> {
    let path = item
        .source_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())?;
    if path.is_dir() {
        Some(path)
    } else if path.is_file() {
        path.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// 把目录树写入 CAS 并把 manifest/blob 记入 object_bytes。
async fn record_tree_from_directory(acc: &mut FreezeAcc, dir: &Path) -> Result<String, AppError> {
    let put = acc.store.put_tree_from_directory(dir).await?;
    let tree_hash = put.object.hash.clone();
    if let Ok(manifest_bytes) = acc.store.get_blob(&tree_hash).await {
        record_bytes(acc, manifest_bytes, true).await?;
    }
    if let Ok(manifest) = acc.store.get_tree(&tree_hash).await {
        for entry in manifest.entries {
            if acc.seen.contains(&entry.blob_hash) {
                continue;
            }
            if let Ok(bytes) = acc.store.get_blob(&entry.blob_hash).await {
                record_bytes(acc, bytes, true).await?;
            }
        }
    }
    Ok(tree_hash)
}

/// 写入（或登记已入库）blob，并计入 512 MiB 预算。
async fn record_bytes(
    acc: &mut FreezeAcc,
    bytes: Vec<u8>,
    already_stored: bool,
) -> Result<String, AppError> {
    let size = bytes.len() as u64;
    let hash = sha256_hex(&bytes);
    if acc.seen.contains(&hash) {
        acc.object_bytes.entry(hash.clone()).or_insert(bytes);
        return Ok(hash);
    }
    ensure_user_mirror_bytes_budget(acc.projected, size)?;
    if !already_stored && !bytes.is_empty() {
        acc.store.put_blob(&bytes).await?;
    }
    acc.projected = acc.projected.saturating_add(size);
    acc.seen.insert(hash.clone());
    acc.object_bytes.insert(hash.clone(), bytes);
    acc.objects.push(SnapshotObjectDescriptor {
        hash: hash.clone(),
        size: size.to_string(),
    });
    Ok(hash)
}

/// 登记 envelope asset/revision 与 item binding。
#[allow(clippy::too_many_arguments)]
fn push_asset(
    acc: &mut FreezeAcc,
    target: AgentTarget,
    logical_id: Option<&str>,
    kind: Option<PortableAssetKind>,
    native_id: Option<&str>,
    asset_kind: AssetKind,
    origin_namespace: &str,
    payload_hash: String,
    tree_manifest_hash: Option<String>,
    blocked: bool,
) {
    let logical_key = logical_id
        .or(native_id)
        .unwrap_or(target.as_str())
        .to_string();
    let material = format!(
        "user-mirror|{}|{}|{}|{}",
        target.as_str(),
        origin_namespace,
        asset_kind.as_str(),
        logical_key
    );
    let asset_id = stable_asset_id(&material);
    let rev_id = Uuid::now_v7().to_string();
    acc.assets.push(SnapshotAsset {
        id: asset_id.clone(),
        scope_id: "user".into(),
        kind: asset_kind,
        origin_namespace: origin_namespace.to_string(),
        logical_key: logical_key.clone(),
        display_name: logical_key,
        policy: AssetPolicy::Shared,
        deleted_at: None,
    });
    acc.lineages.push(SnapshotLineage {
        id: asset_id.clone(),
        root_asset_id: asset_id.clone(),
    });
    acc.revisions.push(SnapshotRevision {
        id: rev_id.clone(),
        asset_lineage_id: asset_id.clone(),
        parents: vec![],
        generation: "0".into(),
        operation: RevisionOperation::Upsert,
        origin_kind: RevisionOriginKind::Ui,
        origin_target: Some(target),
        origin_replica_id: acc.replica_id.clone(),
        payload_hash: Some(payload_hash.clone()),
        tree_manifest_hash,
        created_at: acc.created_at.clone(),
    });
    acc.asset_heads.insert(asset_id, vec![rev_id]);
    acc.bindings.push(UserMirrorObjectBinding {
        target,
        logical_id: logical_id.map(str::to_string),
        kind,
        native_id: native_id.map(str::to_string),
        object_hash: payload_hash,
        blocked,
    });
}

/// 由身份材料派生稳定 asset id（UUID 形态 hex）。
fn stable_asset_id(material: &str) -> String {
    let hash = sha256_hex(material.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &hash[0..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..32]
    )
}

/// 组装 SnapshotEnvelope：objects/revisions 排序后填 snapshot_hash。
fn finish_envelope(
    acc: &mut FreezeAcc,
    inventory: &UserMirrorInventoryDto,
) -> Result<SnapshotEnvelopeV1, AppError> {
    acc.objects.sort_by(|a, b| a.hash.cmp(&b.hash));
    acc.objects.dedup_by(|a, b| a.hash == b.hash);
    acc.revisions.sort_by(|a, b| a.id.cmp(&b.id));
    let asset_ids: Vec<String> = acc.assets.iter().map(|asset| asset.id.clone()).collect();
    let mut scope_ids: BTreeSet<String> = BTreeSet::new();
    scope_ids.insert("user".into());
    for agent in &inventory.agents {
        scope_ids.insert(format!("user:{}", agent.target.as_str()));
    }
    let mut envelope = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: Uuid::now_v7().to_string(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: acc.replica_id.clone(),
        created_at: acc.created_at.clone(),
        selection: SnapshotSelection {
            scope_ids: scope_ids.into_iter().collect(),
            asset_ids,
            include_history: false,
        },
        asset_heads: acc.asset_heads.clone(),
        assets: acc.assets.clone(),
        lineages: acc.lineages.clone(),
        revisions: acc.revisions.clone(),
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects: acc.objects.clone(),
    };
    envelope.snapshot_hash = compute_snapshot_hash(&envelope)
        .map_err(|err| AppError::generic(format!("user_mirror snapshot hash: {err}")))?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_user_mirror_bytes_budget, freeze_user_mirror_selection, USER_MIRROR_TRANSFER_LIMIT,
    };
    use crate::agent_hub::models::AgentTarget;
    use crate::agent_hub::portable_inventory::{
        inspect_portable_inventory_force_with_env_query, PortableAssetKind, PortableInventoryQuery,
    };
    use crate::agent_hub::snapshot::portable_builder::LEGACY_LOSSY_PLACEHOLDER;
    use crate::agent_hub::targets::TargetEnvironment;
    use crate::agent_hub::user_mirror::build_local_user_mirror_inventory;
    use crate::agent_hub::user_mirror::models::USER_MIRROR_DEST_MAX_TOTAL_BYTES;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::error::AppError;
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
    ///     冻结测试必须隔离 HOME 与 data_dir，避免扫到开发者真实配置或凭据。
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

    fn seed_claude_native_skill_and_mcp(env: &UserMirrorHomes) {
        write(env.claude_home.join("CLAUDE.md").as_path(), "# src claude");
        write(
            env.claude_home.join("skills/hello/SKILL.md").as_path(),
            "---\nname: hello\ndescription: d\n---\n",
        );
        write(
            env.home.join(".claude.json").as_path(),
            r#"{"mcpServers":{"s":{"command":"uvx","args":["srv"],"env":{"TOKEN":"plain-secret-xyz"},"enabled":true}}}"#,
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源端冻结必须把原生 CLAUDE.md 与 skill 树写入 CAS，MCP 凭据只进对象字节。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同源 inventory fixture；冻结后断言 CAS 含原生/技能字节与 MCP secret，envelope JSON 无 secret。
    #[tokio::test]
    async fn freeze_user_mirror_selection_puts_native_bytes_and_skill_tree_in_cas() {
        let env = seed_user_mirror_homes().await;
        seed_claude_native_skill_and_mcp(&env);
        let inventory = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();

        let built = freeze_user_mirror_selection(&env.app_state, &inventory)
            .await
            .unwrap();
        assert!(!built.envelope.objects.is_empty());
        let total: u64 = built.object_bytes.values().map(|b| b.len() as u64).sum();
        assert!(total > 0);
        assert!(total <= USER_MIRROR_DEST_MAX_TOTAL_BYTES);
        let json = serde_json::to_string(&built.envelope).unwrap();
        assert!(
            !json.contains("plain-secret-xyz"),
            "envelope metadata must not contain MCP secret plaintext"
        );
        const SECRET: &[u8] = b"plain-secret-xyz";
        let secret_obj = built
            .object_bytes
            .values()
            .any(|b| b.windows(SECRET.len()).any(|window| window == SECRET));
        assert!(secret_obj, "MCP credential bytes belong in CAS objects");
        assert!(
            built
                .object_bytes
                .values()
                .any(|b| b.as_slice() == b"# src claude"),
            "native CLAUDE.md bytes must be in CAS"
        );
        assert!(
            built.object_bytes.values().any(|b| {
                let text = String::from_utf8_lossy(b);
                text.contains("name: hello") || text.contains("\"path\":\"SKILL.md\"")
            }),
            "skill tree / SKILL.md must be in CAS"
        );
        assert!(built
            .item_bindings
            .iter()
            .any(|b| b.logical_id.as_deref() == Some("claude.native.CLAUDE.md") && !b.blocked));
        assert!(built.item_bindings.iter().any(|b| {
            b.target == AgentTarget::Claude
                && b.kind == Some(PortableAssetKind::Skill)
                && b.native_id.as_deref() == Some("hello")
                && !b.blocked
        }));
        assert!(!built.transfer_id.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     源端冻结只读 CAS，不得把磁盘 portable 资产 adopt / uninstall。
    ///
    /// Code Logic（这个测试做什么）:
    ///     冻结后再扫；hello skill 仍在 `~/.claude/skills`，store 未挂上。
    #[tokio::test]
    async fn freeze_user_mirror_selection_does_not_adopt_source_assets() {
        let env = seed_user_mirror_homes().await;
        seed_claude_native_skill_and_mcp(&env);
        let skill_md = env.claude_home.join("skills/hello/SKILL.md");
        let before = fs::read(&skill_md).expect("skill before");
        let inventory = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();
        freeze_user_mirror_selection(&env.app_state, &inventory)
            .await
            .unwrap();

        assert_eq!(fs::read(&skill_md).expect("skill after"), before);
        assert!(env.claude_home.join("CLAUDE.md").is_file());
        let env_scan = TargetEnvironment::from_process();
        let snap = inspect_portable_inventory_force_with_env_query(
            &env.app_state,
            &env_scan,
            PortableInventoryQuery {
                target: Some(AgentTarget::Claude),
                kind: Some(PortableAssetKind::Skill),
                scope_kind: Some(crate::agent_hub::models::ScopeKind::User),
                local_project_id: None,
            },
        )
        .await
        .unwrap();
        let hello = snap
            .items
            .iter()
            .find(|item| item.kind == PortableAssetKind::Skill && item.native_id == "hello")
            .expect("hello skill still discovered");
        assert!(
            !hello.store.store_attached,
            "freeze must not adopt into portable-store"
        );
        if let Some(path) = hello.source_path.as_deref() {
            assert!(
                path.contains("skills/hello"),
                "skill must remain on native disk path, got {path}"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超过 512 MiB 必须 fail-closed，禁止靠分配整包来测上限。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用伪造累计大小调用 budget helper，不断言真实 512MiB 分配。
    #[test]
    fn ensure_user_mirror_bytes_budget_rejects_over_512_mib() {
        let err = ensure_user_mirror_bytes_budget(USER_MIRROR_DEST_MAX_TOTAL_BYTES, 1).unwrap_err();
        assert!(
            err.to_string().contains(USER_MIRROR_TRANSFER_LIMIT),
            "expected {USER_MIRROR_TRANSFER_LIMIT}, got {err}"
        );
        ensure_user_mirror_bytes_budget(0, USER_MIRROR_DEST_MAX_TOTAL_BYTES).unwrap();
        ensure_user_mirror_bytes_budget(USER_MIRROR_DEST_MAX_TOTAL_BYTES - 1, 1).unwrap();
        let eq = ensure_user_mirror_bytes_budget(0, USER_MIRROR_DEST_MAX_TOTAL_BYTES + 1);
        assert!(
            matches!(eq, Err(AppError::Validation(code)) if code == USER_MIRROR_TRANSFER_LIMIT)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 peer 脱敏占位不得当真实凭据进入 CAS 供 dest 覆盖。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入含 placeholder 的 MCP；断言 binding blocked 且 object_bytes 不含占位凭据对象。
    #[tokio::test]
    async fn freeze_legacy_lossy_mcp_marks_binding_blocked_without_placeholder_credential() {
        let env = seed_user_mirror_homes().await;
        write(env.claude_home.join("CLAUDE.md").as_path(), "# src claude");
        write(
            env.home.join(".claude.json").as_path(),
            &format!(
                r#"{{"mcpServers":{{"lossy":{{"command":"uvx","args":["srv"],"env":{{"TOKEN":"{LEGACY_LOSSY_PLACEHOLDER}"}},"enabled":true}}}}}}"#
            ),
        );
        let inventory = build_local_user_mirror_inventory(&env.app_state, "dev-a")
            .await
            .unwrap();
        let built = freeze_user_mirror_selection(&env.app_state, &inventory)
            .await
            .unwrap();
        let binding = built
            .item_bindings
            .iter()
            .find(|b| {
                b.kind == Some(PortableAssetKind::Mcp) && b.native_id.as_deref() == Some("lossy")
            })
            .expect("lossy mcp binding");
        assert!(binding.blocked, "legacyLossy MCP must be marked blocked");
        let placeholder = LEGACY_LOSSY_PLACEHOLDER.as_bytes();
        assert!(
            !built
                .object_bytes
                .values()
                .any(|b| b.windows(placeholder.len()).any(|w| w == placeholder)),
            "placeholder must not be put as a real credential object"
        );
        let json = serde_json::to_string(&built.envelope).unwrap();
        assert!(!json.contains(LEGACY_LOSSY_PLACEHOLDER));
    }
}
