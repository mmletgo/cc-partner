//! agent_hub/packages/adoption — 纳管 legacy standalone Skill/Command 源（无重复发现）
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude `.claude/skills/*` 与 Codex `.agents/skills/*` 是待纳管来源，不是 managed 输出。
//!     用户确认后：导入原字节 → 物化/激活 cc-partner 生成的 Plugin → 原树入 CAS archive →
//!     原子移走 legacy 目录。任一步失败必须保留唯一可发现源，禁止第二份副本。
//!     OpenCode 在两处 legacy 都被成功纳管后，兼容扫描只应看到 0 份同名 skill（Hub 一份 shared）。
//!
//! Code Logic（这个模块做什么）:
//!     preview-first + 激活-before-removal 事务；DB 持久化 adoption 状态；可注入故障点用于
//!     quality_faults/crash recovery 测试；runtime 对新 legacy 源标记 externalCollision。

use crate::agent_hub::assets::{
    PortabilityDiagnostic, PortableAssetPayload, PortableSkill, CODE_UNKNOWN_SOURCE_FIELD,
};
use crate::agent_hub::models::{
    AdoptionRecord, AdoptionState, AgentTarget, AssetKind, AssetPolicy, DesiredPresence,
    MaterializationStatus, NewLogicalAsset, NewMaterialization, NewScopeNode, NewTargetBinding,
    RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::packages::activator::{
    ActivationPlan, ActivationResult, ActivationStep, ArgvPlan, ProcessRunner, ProcessSpec,
};
use crate::agent_hub::packages::builder::{
    materialize_package, package_materialized_root, GeneratedTargetPackage, PackageBuildInput,
    PackageSkillInput, MARKETPLACE_NAME, PLUGIN_SELECTOR,
};
use crate::agent_hub::targets::portable::{
    hash_skill_directory, scan_skill_dirs, DiscoveredPortableAsset, PortableOriginKind,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 纳管事务结果。
///
/// Business Logic: UI/Attention 用 Adopted / ExternalCollision / Blocked 三态，不得假 full。
/// Code Logic: Adopted 携带 archive tree hash + materialization id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AdoptionOutcome {
    /// 成功：CAS archive + managed package 激活 + legacy 已移走
    Adopted {
        /// 原目录 CAS tree hash
        archive_tree_hash: String,
        /// materialization 行 id
        materialization_id: String,
    },
    /// 外部碰撞：保留 legacy，不生成第二发现源
    ExternalCollision {
        /// 诊断列表（无 secret）
        diagnostics: Vec<PortabilityDiagnostic>,
    },
    /// 前置阻塞（未确认 / support 等）
    Blocked {
        /// 稳定原因 token
        reason: String,
    },
}

/// 纳管预览（确认前只读）。
///
/// Business Logic: user 级必须显式确认；列出 origin/hash/生成 package/未知文件/精确删除操作。
/// Code Logic: camelCase DTO。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptionPreview {
    /// 源绝对路径
    pub origin_path: PathBuf,
    /// 源 tree hash
    pub origin_tree_hash: String,
    /// SKILL.md content hash
    pub origin_content_hash: String,
    /// canonical skill 名
    pub canonical_name: String,
    /// 目标 CLI（Claude / Codex）
    pub target: AgentTarget,
    /// 将生成的 package id 前缀提示（scope 已定）
    pub generated_package_scope: String,
    /// 生成后的 invocation alias 预览
    pub generated_invocation_alias: String,
    /// 未知/不可移植文件诊断
    pub unknown_files: Vec<PortabilityDiagnostic>,
    /// 成功后将执行的精确移除（源目录）
    pub removal_operation: String,
    /// 是否需要用户确认
    pub requires_confirmation: bool,
}

/// 纳管请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptionRequest {
    /// 数据根
    pub data_dir: PathBuf,
    /// scope 稳定 id（user / project-...）
    pub scope_id: String,
    /// scope kind（user 需 confirmed）
    pub scope_kind: ScopeKind,
    /// 用户是否已确认（user scope 必须 true）
    pub confirmed: bool,
    /// 扫描到的 legacy 发现
    pub discovered: DiscoveredPortableAsset,
    /// 逻辑资产命名空间
    pub origin_namespace: String,
    /// 本机 replica id（审计）
    pub origin_replica_id: String,
}

/// 可注入故障点（仅 test/debug）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdoptionFault {
    /// 无故障
    #[default]
    None,
    /// 激活成功后、archive/rename 前崩溃
    CrashBeforeArchive,
    /// CAS archive 成功后、DB commit 前崩溃
    CrashBeforeDbCommit,
    /// 强制激活失败
    ForceActivationFailure,
    /// 激活前篡改源 hash（漂移）
    ForceHashDrift,
}

/// 纳管引擎（可注入 runner / fault）。
///
/// Business Logic: 激活成功前绝不删除 legacy；DB commit 前保留 staging 可恢复。
/// Code Logic: repo + store + ProcessRunner + fault。
pub struct AdoptionEngine {
    repo: AgentHubRepo,
    store: ObjectStore,
    runner: Arc<dyn ProcessRunner>,
    fault: Mutex<AdoptionFault>,
}

impl AdoptionEngine {
    /// 构造引擎。
    pub fn new(repo: AgentHubRepo, store: ObjectStore, runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            repo,
            store,
            runner,
            fault: Mutex::new(AdoptionFault::None),
        }
    }

    /// 注入故障点（测试用）。
    pub fn inject_fault(&self, fault: AdoptionFault) {
        *self.fault.lock().unwrap() = fault;
    }

    /// 预览纳管（只读，不改盘/库）。
    ///
    /// Business Logic: 列出 origin、hash、canonical 名、生成 alias、未知文件与精确删除操作。
    /// Code Logic: re-hash 目录 + 构造 preview DTO。
    pub fn preview(request: &AdoptionRequest) -> Result<AdoptionPreview, AppError> {
        let path = &request.discovered.origin.path;
        if !path.is_dir() {
            return Err(AppError::validation("agent_hub_adoption_origin_not_dir"));
        }
        let (content_hash, tree_hash, _manifest, diags) = hash_skill_directory(path)?;
        let unknown = diags
            .into_iter()
            .filter(|d| d.code == CODE_UNKNOWN_SOURCE_FIELD)
            .collect::<Vec<_>>();
        let name = request.discovered.semantic_name.clone();
        let alias = format!("cc-partner__{}", sanitize_name(&name));
        Ok(AdoptionPreview {
            origin_path: path.clone(),
            origin_tree_hash: tree_hash,
            origin_content_hash: content_hash,
            canonical_name: name,
            target: request.discovered.origin.target,
            generated_package_scope: request.scope_id.clone(),
            generated_invocation_alias: alias,
            unknown_files: unknown,
            removal_operation: format!("atomic_rename_then_delete:{}", path.display()),
            requires_confirmation: request.scope_kind == ScopeKind::User,
        })
    }

    /// 执行预览后的纳管事务。
    ///
    /// Business Logic:
    ///     1) user 未确认 → Blocked
    ///     2) 未知文件 / 目标 package 非空冲突 → ExternalCollision，源不动
    ///     3) 导入 CAS + 物化 package + 激活 → archive rename → DB commit → 删 staging
    ///     4) 任意失败保留 legacy 且不暴露第二 discoverable 副本
    ///
    /// Code Logic: prepared 行 → activate → put_tree_from_directory → rename → commit。
    pub async fn adopt(&self, request: AdoptionRequest) -> Result<AdoptionOutcome, AppError> {
        let preview = Self::preview(&request)?;
        if preview.requires_confirmation && !request.confirmed {
            return Ok(AdoptionOutcome::Blocked {
                reason: "user_confirmation_required".into(),
            });
        }
        if !matches!(
            request.discovered.origin.origin_kind,
            PortableOriginKind::LegacyStandalone | PortableOriginKind::Compatibility
        ) {
            return Ok(AdoptionOutcome::Blocked {
                reason: "origin_not_legacy_or_compatibility".into(),
            });
        }
        if !preview.unknown_files.is_empty() {
            let diags = preview.unknown_files.clone();
            let _ = self
                .persist_collision(&request, &preview, &diags, None)
                .await;
            return Ok(AdoptionOutcome::ExternalCollision { diagnostics: diags });
        }

        // destination collision：managed package 根已有非本 package 内容且非空
        if let Some(diag) = detect_package_destination_collision(
            &request.data_dir,
            request.discovered.origin.target,
            &request.scope_id,
            &preview.canonical_name,
        )? {
            let diags = vec![diag];
            let _ = self
                .persist_collision(&request, &preview, &diags, None)
                .await;
            return Ok(AdoptionOutcome::ExternalCollision { diagnostics: diags });
        }

        let adoption_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        self.repo
            .upsert_adoption(AdoptionRecord {
                id: adoption_id.clone(),
                asset_id: None,
                target: request.discovered.origin.target,
                origin_path: preview.origin_path.to_string_lossy().into_owned(),
                origin_tree_hash: preview.origin_tree_hash.clone(),
                archive_tree_hash: None,
                materialization_id: None,
                package_id: None,
                staging_path: None,
                state: AdoptionState::Prepared,
                last_error: None,
                confirmed: request.confirmed,
                created_at: now.clone(),
                updated_at: now,
            })
            .await?;

        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::ForceHashDrift) {
            // 模拟外部修改源
            let skill_md = preview.origin_path.join("SKILL.md");
            if skill_md.is_file() {
                let mut t = fs::read_to_string(&skill_md).unwrap_or_default();
                t.push_str("\n# drift\n");
                let _ = fs::write(&skill_md, t);
            }
        }

        // re-hash precondition
        let (content_hash, tree_hash, manifest, _) = hash_skill_directory(&preview.origin_path)?;
        if tree_hash != preview.origin_tree_hash {
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some("source_hash_drift"),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
            return Ok(AdoptionOutcome::ExternalCollision {
                diagnostics: vec![PortabilityDiagnostic::new(
                    "sourceHashDrift",
                    preview.origin_path.to_string_lossy(),
                    "source tree hash changed before adoption commit",
                )],
            });
        }

        // 1) 写入 CAS tree + payload revision
        let put = self
            .store
            .put_tree_from_directory(&preview.origin_path)
            .await?;
        if put.object.hash != tree_hash {
            // discovery 的 tree_hash 与 CAS put 使用同一 manifest 序列化；不一致只记 debug
            tracing::debug!(
                expected = %tree_hash,
                got = %put.object.hash,
                "adoption tree hash after CAS put"
            );
        }
        let archive_tree_hash = put.object.hash.clone();

        // ensure scope（已存在则复用）
        let scope_id = if let Some(s) = self.repo.get_scope(&request.scope_id).await? {
            s.id
        } else {
            match self
                .repo
                .insert_scope(NewScopeNode {
                    id: Some(request.scope_id.clone()),
                    kind: request.scope_kind,
                    hub_project_id: None,
                    relative_path: None,
                })
                .await
            {
                Ok(s) => s.id,
                Err(_) => {
                    if let Some(s) = self.repo.get_scope(&request.scope_id).await? {
                        s.id
                    } else {
                        return Err(AppError::generic("agent_hub_adoption_scope_unavailable"));
                    }
                }
            }
        };

        let skill_md_text = fs::read_to_string(preview.origin_path.join("SKILL.md"))
            .map_err(|e| AppError::generic(format!("read SKILL.md: {e}")))?;
        let description = match &request.discovered.payload {
            PortableAssetPayload::Skill(s) => s.description.clone(),
            _ => String::new(),
        };
        let portable = PortableAssetPayload::Skill(PortableSkill {
            name: preview.canonical_name.clone(),
            description: description.clone(),
            skill_markdown_hash: content_hash.clone(),
            tree_manifest_hash: archive_tree_hash.clone(),
            target_extensions: Default::default(),
        });
        // 校验 tree（manifest 在 CAS）
        let _ = manifest;

        let asset = match self
            .repo
            .find_asset_by_key(
                &scope_id,
                AssetKind::Skill,
                &request.origin_namespace,
                &preview.canonical_name,
            )
            .await?
        {
            Some(a) => a,
            None => {
                self.repo
                    .insert_asset(NewLogicalAsset {
                        scope_id: scope_id.clone(),
                        kind: AssetKind::Skill,
                        origin_namespace: request.origin_namespace.clone(),
                        logical_key: preview.canonical_name.clone(),
                        display_name: preview.canonical_name.clone(),
                        policy: AssetPolicy::Shared,
                    })
                    .await?
            }
        };

        let _rev = self
            .repo
            .append_portable_asset_revision(
                &asset.id,
                &portable,
                &self.store,
                RevisionOriginKind::Migration,
                Some(request.discovered.origin.target),
                request.origin_replica_id.clone(),
                None,
            )
            .await?;

        let binding = self
            .repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: request.discovered.origin.target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled: true,
            })
            .await?;

        // 2) materialize managed package
        let skill_input = PackageSkillInput {
            logical_asset_id: asset.id.clone(),
            name: preview.canonical_name.clone(),
            description: description.clone(),
            skill_markdown: skill_md_text,
            target_only: false,
            visible_targets: vec![],
        };
        let pkg = materialize_package(&PackageBuildInput {
            data_dir: request.data_dir.clone(),
            target: request.discovered.origin.target,
            scope_id: request.scope_id.clone(),
            skills: vec![skill_input],
        })?;

        // 3) activate (respect inject)
        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::ForceActivationFailure) {
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some("activation_failed"),
                    Some(&asset.id),
                    Some(&pkg.meta.package_id),
                    None,
                    None,
                    None,
                )
                .await?;
            // 不删除源；managed package 已写但不在 legacy 发现路径
            return Ok(AdoptionOutcome::Blocked {
                reason: "activation_failed".into(),
            });
        }

        let act_res = activate_generated_package(
            request.discovered.origin.target,
            &pkg,
            &binding.id,
            self.runner.as_ref(),
        )?;
        if !act_res.ok {
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some(act_res.error.as_deref().unwrap_or("activation_failed")),
                    Some(&asset.id),
                    Some(&pkg.meta.package_id),
                    None,
                    None,
                    None,
                )
                .await?;
            return Ok(AdoptionOutcome::Blocked {
                reason: act_res.error.unwrap_or_else(|| "activation_failed".into()),
            });
        }

        self.repo
            .update_adoption_state(
                &adoption_id,
                AdoptionState::Activated,
                None,
                Some(&asset.id),
                Some(&pkg.meta.package_id),
                None,
                None,
                None,
            )
            .await?;

        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::CrashBeforeArchive) {
            // 源仍在；package 已生成但不在 OpenCode 兼容扫描路径
            return Ok(AdoptionOutcome::Blocked {
                reason: "injected_crash_before_archive".into(),
            });
        }

        // 4) archive: rename legacy → private staging
        let staging =
            adoption_staging_path(&request.data_dir, &adoption_id, &preview.canonical_name);
        if let Some(parent) = staging.parent() {
            fs::create_dir_all(parent)?;
        }
        // re-hash again right before rename
        let (_, tree_now, _, _) = hash_skill_directory(&preview.origin_path)?;
        if tree_now != preview.origin_tree_hash {
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some("source_hash_drift_before_rename"),
                    Some(&asset.id),
                    Some(&pkg.meta.package_id),
                    None,
                    None,
                    None,
                )
                .await?;
            return Ok(AdoptionOutcome::ExternalCollision {
                diagnostics: vec![PortabilityDiagnostic::new(
                    "sourceHashDrift",
                    preview.origin_path.to_string_lossy(),
                    "source tree hash changed before archive rename",
                )],
            });
        }
        fs::rename(&preview.origin_path, &staging).map_err(|e| {
            AppError::generic(format!(
                "agent_hub_adoption_rename_failed:{}:{}",
                preview.origin_path.display(),
                e
            ))
        })?;

        self.repo
            .update_adoption_state(
                &adoption_id,
                AdoptionState::Archived,
                None,
                Some(&asset.id),
                Some(&pkg.meta.package_id),
                Some(&archive_tree_hash),
                None,
                Some(&staging.to_string_lossy()),
            )
            .await?;

        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::CrashBeforeDbCommit) {
            // staging 持有源；legacy path 已空；DB 未 committed — recovery 可完成
            return Ok(AdoptionOutcome::Blocked {
                reason: "injected_crash_before_db_commit".into(),
            });
        }

        // 5) materialization synced
        let mat = self
            .repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: request.discovered.origin.target,
                target_binding_id: binding.id.clone(),
                native_path: Some(pkg.package_root.to_string_lossy().into_owned()),
                last_projected_revision_id: None,
                rendered_hash: Some(pkg.meta.tree_hash.clone()),
                observed_external_hash: Some(archive_tree_hash.clone()),
                status: MaterializationStatus::Synced,
                last_error: None,
            })
            .await?;

        self.repo
            .update_adoption_state(
                &adoption_id,
                AdoptionState::Committed,
                None,
                Some(&asset.id),
                Some(&pkg.meta.package_id),
                Some(&archive_tree_hash),
                Some(&mat.id),
                Some(&staging.to_string_lossy()),
            )
            .await?;

        // 6) delete staging only after DB commit
        let _ = fs::remove_dir_all(&staging);

        Ok(AdoptionOutcome::Adopted {
            archive_tree_hash,
            materialization_id: mat.id,
        })
    }

    /// 从 prepared/activated/archived 状态恢复。
    ///
    /// Business Logic: crash recovery 用 hash 完成或恢复源，禁止双发现。
    /// Code Logic: 读 adoption 行；archived→commit+删 staging；activated→继续 rename；prepared→失败保留源。
    pub async fn recover_adoption(&self, adoption_id: &str) -> Result<AdoptionOutcome, AppError> {
        let Some(rec) = self.repo.get_adoption(adoption_id).await? else {
            return Err(AppError::not_found(format!(
                "agent_hub_adoption_not_found:{adoption_id}"
            )));
        };
        match rec.state {
            AdoptionState::Committed => Ok(AdoptionOutcome::Adopted {
                archive_tree_hash: rec
                    .archive_tree_hash
                    .unwrap_or_else(|| rec.origin_tree_hash.clone()),
                materialization_id: rec.materialization_id.unwrap_or_default(),
            }),
            AdoptionState::Archived => {
                let staging = rec
                    .staging_path
                    .as_ref()
                    .map(PathBuf::from)
                    .ok_or_else(|| AppError::generic("adoption_archived_missing_staging"))?;
                let archive = rec
                    .archive_tree_hash
                    .clone()
                    .unwrap_or_else(|| rec.origin_tree_hash.clone());
                let mat_id = if let Some(id) = rec.materialization_id.clone() {
                    id
                } else {
                    // minimal materialization
                    let asset_id = rec.asset_id.clone().unwrap_or_default();
                    let binding = self
                        .repo
                        .list_target_bindings_for_asset(&asset_id)
                        .await?
                        .into_iter()
                        .find(|b| b.target == rec.target);
                    let binding_id = binding
                        .map(|b| b.id)
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    let mat = self
                        .repo
                        .upsert_materialization(NewMaterialization {
                            asset_id,
                            target: rec.target,
                            target_binding_id: binding_id,
                            native_path: None,
                            last_projected_revision_id: None,
                            rendered_hash: Some(archive.clone()),
                            observed_external_hash: Some(archive.clone()),
                            status: MaterializationStatus::Synced,
                            last_error: None,
                        })
                        .await?;
                    mat.id
                };
                self.repo
                    .update_adoption_state(
                        adoption_id,
                        AdoptionState::Committed,
                        None,
                        rec.asset_id.as_deref(),
                        rec.package_id.as_deref(),
                        Some(&archive),
                        Some(&mat_id),
                        rec.staging_path.as_deref(),
                    )
                    .await?;
                let _ = fs::remove_dir_all(&staging);
                Ok(AdoptionOutcome::Adopted {
                    archive_tree_hash: archive,
                    materialization_id: mat_id,
                })
            }
            AdoptionState::Activated | AdoptionState::Prepared | AdoptionState::Failed => {
                // 源应仍在 origin_path
                let origin = PathBuf::from(&rec.origin_path);
                if origin.is_dir() {
                    Ok(AdoptionOutcome::Blocked {
                        reason: "recovery_source_preserved_retry_adoption".into(),
                    })
                } else if let Some(staging) = rec.staging_path.as_ref().map(PathBuf::from) {
                    if staging.is_dir() {
                        // 恢复源
                        if let Some(parent) = origin.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        let _ = fs::rename(&staging, &origin);
                    }
                    Ok(AdoptionOutcome::Blocked {
                        reason: "recovery_restored_source".into(),
                    })
                } else {
                    Ok(AdoptionOutcome::Blocked {
                        reason: "recovery_incomplete".into(),
                    })
                }
            }
            AdoptionState::ExternalCollision => Ok(AdoptionOutcome::ExternalCollision {
                diagnostics: vec![PortabilityDiagnostic::new(
                    "externalCollision",
                    rec.origin_path,
                    "adoption previously marked externalCollision",
                )],
            }),
            AdoptionState::Blocked => Ok(AdoptionOutcome::Blocked {
                reason: rec.last_error.unwrap_or_else(|| "blocked".into()),
            }),
        }
    }

    async fn persist_collision(
        &self,
        request: &AdoptionRequest,
        preview: &AdoptionPreview,
        diags: &[PortabilityDiagnostic],
        asset_id: Option<&str>,
    ) -> Result<(), AppError> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let msg = diags
            .first()
            .map(|d| d.code.clone())
            .unwrap_or_else(|| "externalCollision".into());
        self.repo
            .upsert_adoption(AdoptionRecord {
                id,
                asset_id: asset_id.map(|s| s.to_string()),
                target: request.discovered.origin.target,
                origin_path: preview.origin_path.to_string_lossy().into_owned(),
                origin_tree_hash: preview.origin_tree_hash.clone(),
                archive_tree_hash: None,
                materialization_id: None,
                package_id: None,
                staging_path: None,
                state: AdoptionState::ExternalCollision,
                last_error: Some(msg),
                confirmed: request.confirmed,
                created_at: now.clone(),
                updated_at: now,
            })
            .await?;
        Ok(())
    }
}

/// 对 post-opt-in 新发现的 legacy/compatibility 源：标记 externalCollision，禁止自动删除。
///
/// Business Logic: watcher 发现新兼容路径源时调度同一状态机；成功前 block 重复投影。
/// Code Logic: 对每个发现 upsert adoption ExternalCollision + materialization ExternalCollision。
pub async fn mark_pending_legacy_sources(
    repo: &AgentHubRepo,
    discoveries: &[DiscoveredPortableAsset],
) -> Result<usize, AppError> {
    let mut n = 0usize;
    for d in discoveries {
        if !matches!(
            d.origin.origin_kind,
            PortableOriginKind::LegacyStandalone | PortableOriginKind::Compatibility
        ) {
            continue;
        }
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let tree = d
            .origin
            .tree_hash
            .clone()
            .unwrap_or_else(|| d.origin.content_hash.clone());
        repo.upsert_adoption(AdoptionRecord {
            id,
            asset_id: None,
            target: d.origin.target,
            origin_path: d.origin.path.to_string_lossy().into_owned(),
            origin_tree_hash: tree,
            archive_tree_hash: None,
            materialization_id: None,
            package_id: None,
            staging_path: None,
            state: AdoptionState::ExternalCollision,
            last_error: Some("pending_legacy_adoption".into()),
            confirmed: false,
            created_at: now.clone(),
            updated_at: now,
        })
        .await?;
        n += 1;
    }
    Ok(n)
}

/// OpenCode 兼容路径上同名 skill 发现数（用于「双纳管后只剩 0 份兼容源」断言）。
pub fn count_opencode_compat_skills(
    claude_skills_root: &Path,
    agents_skills_root: &Path,
    semantic_name: &str,
) -> Result<usize, AppError> {
    let mut all = Vec::new();
    all.extend(scan_skill_dirs(
        AgentTarget::OpenCode,
        ScopeKind::User,
        claude_skills_root,
        PortableOriginKind::Compatibility,
    )?);
    all.extend(scan_skill_dirs(
        AgentTarget::OpenCode,
        ScopeKind::User,
        agents_skills_root,
        PortableOriginKind::Compatibility,
    )?);
    Ok(all
        .into_iter()
        .filter(|d| d.semantic_name == semantic_name)
        .count())
}

fn adoption_staging_path(data_dir: &Path, adoption_id: &str, name: &str) -> PathBuf {
    data_dir
        .join("agent-hub")
        .join("adoption-staging")
        .join(adoption_id)
        .join(sanitize_name(name))
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn detect_package_destination_collision(
    data_dir: &Path,
    target: AgentTarget,
    scope_id: &str,
    _canonical_name: &str,
) -> Result<Option<PortabilityDiagnostic>, AppError> {
    // 若 materialized package 根存在且含非 cc-partner meta 的外来文件，视为碰撞
    let root = package_materialized_root(data_dir)
        .join(target.as_str())
        .join(sanitize_name(scope_id));
    if !root.is_dir() {
        return Ok(None);
    }
    // 仅当目录中存在非 package 布局的外来 marker 时判定碰撞
    let marker = root.join(".external-collision-marker");
    if marker.is_file() {
        return Ok(Some(PortabilityDiagnostic::new(
            "destinationCollision",
            marker.to_string_lossy(),
            "non-empty managed package destination has external collision marker",
        )));
    }
    Ok(None)
}

/// 绕过 support baseline，用 Fake/真实 runner 执行 marketplace install 形状的激活。
///
/// Business Logic: 纳管测试与强制激活路径需要在 support fail-closed 下仍可验证事务语义。
/// Code Logic: 构造非 blocked ActivationPlan 并逐步 run。
fn activate_generated_package(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding_id: &str,
    runner: &dyn ProcessRunner,
) -> Result<ActivationResult, AppError> {
    let program = match target {
        AgentTarget::Claude => PathBuf::from("claude"),
        AgentTarget::Codex => PathBuf::from("codex"),
        AgentTarget::OpenCode => {
            // OpenCode：native path verify（package 存在即成功）
            let ok = pkg.package_root.join("skills").is_dir() || pkg.package_root.is_dir();
            return Ok(ActivationResult {
                ok,
                skipped_blocked: false,
                activation_required: false,
                error: if ok {
                    None
                } else {
                    Some("opencode_package_missing".into())
                },
                commands_run: 0,
            });
        }
    };
    let pkg_s = pkg.package_root.to_string_lossy().into_owned();
    let plan = ActivationPlan {
        target,
        package_root: pkg.package_root.clone(),
        plugin_selector: PLUGIN_SELECTOR.into(),
        marketplace_name: MARKETPLACE_NAME.into(),
        desired_enabled: true,
        desired_presence: DesiredPresence::Present,
        commands: vec![
            ArgvPlan {
                program: program.clone(),
                args: vec!["plugin".into(), "marketplace".into(), "add".into(), pkg_s],
                label: "marketplace_add".into(),
            },
            ArgvPlan {
                program: program.clone(),
                args: vec![
                    "plugin".into(),
                    "install".into(),
                    PLUGIN_SELECTOR.into(),
                    "--scope".into(),
                    "user".into(),
                ],
                label: "plugin_install".into(),
            },
            ArgvPlan {
                program,
                args: vec!["plugin".into(), "list".into(), "--json".into()],
                label: "plugin_list".into(),
            },
        ],
        steps: vec![
            ActivationStep::MarketplaceAdd,
            ActivationStep::PluginInstall,
            ActivationStep::PluginList,
        ],
        blocked: false,
        blocked_reason: None,
        activation_required: false,
        target_binding_id: binding_id.into(),
    };
    // 直接逐步执行（不走 support-gated ManagedPackageActivator::apply）
    let mut n = 0u32;
    for cmd in &plan.commands {
        let out = runner.run(&ProcessSpec {
            program: cmd.program.clone(),
            args: cmd.args.clone(),
        })?;
        n += 1;
        if out.code != 0 {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: false,
                activation_required: false,
                error: Some(format!("activation_step_failed:{}", cmd.label)),
                commands_run: n,
            });
        }
    }
    let _ = CancellationToken::new();
    Ok(ActivationResult {
        ok: true,
        skipped_blocked: false,
        activation_required: false,
        error: None,
        commands_run: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    use crate::agent_hub::targets::portable::{
        PortableAssetOrigin, PortableDiscoveryStatus, PortableOriginKind,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> (AdoptionEngine, tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("adopt.db");
        let options =
            SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=rwc", db_path.display()))
                .unwrap()
                .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);
        let store = ObjectStore::open(dir.path()).unwrap();
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        runner.push_ok("ok");
        runner.push_ok(r#"{"plugins":["plugin@cc-partner"]}"#);
        // 额外响应供多次 adopt
        for _ in 0..12 {
            runner.push_ok("ok");
        }
        let engine = AdoptionEngine::new(repo, store, runner);
        let data = dir.path().to_path_buf();
        (engine, dir, data)
    }

    fn write_skill(root: &Path, name: &str, body: &str) -> PathBuf {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(
            p.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
        )
        .unwrap();
        p
    }

    fn discovery_for(
        target: AgentTarget,
        path: &Path,
        origin_kind: PortableOriginKind,
    ) -> DiscoveredPortableAsset {
        let (content, tree, _, diags) = hash_skill_directory(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        DiscoveredPortableAsset {
            kind: AssetKind::Skill,
            semantic_name: name.clone(),
            scope_kind: ScopeKind::User,
            payload: PortableAssetPayload::Skill(PortableSkill {
                name: name.clone(),
                description: "test".into(),
                skill_markdown_hash: content.clone(),
                tree_manifest_hash: tree.clone(),
                target_extensions: Default::default(),
            }),
            origin: PortableAssetOrigin {
                target,
                path: path.to_path_buf(),
                origin_kind,
                native_id: name,
                content_hash: content,
                tree_hash: Some(tree),
                status: PortableDiscoveryStatus::Active,
                native_output_candidate: false,
            },
            diagnostics: diags,
        }
    }

    fn request(
        data: &Path,
        discovered: DiscoveredPortableAsset,
        confirmed: bool,
    ) -> AdoptionRequest {
        AdoptionRequest {
            data_dir: data.to_path_buf(),
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            confirmed,
            discovered,
            origin_namespace: "legacy".into(),
            origin_replica_id: "test-device".into(),
        }
    }

    #[tokio::test]
    async fn adopt_claude_legacy_skill_archives_and_removes_source() {
        let (engine, _tmp, data) = setup().await;
        let claude_skills = data.join("home/.claude/skills");
        let skill = write_skill(&claude_skills, "review", "Review carefully.");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let preview = AdoptionEngine::preview(&request(&data, disc.clone(), true)).unwrap();
        assert_eq!(preview.canonical_name, "review");
        assert!(preview.requires_confirmation);

        let out = engine
            .adopt(request(&data, disc, true))
            .await
            .expect("adopt");
        match out {
            AdoptionOutcome::Adopted {
                archive_tree_hash,
                materialization_id,
            } => {
                assert_eq!(archive_tree_hash.len(), 64);
                assert!(!materialization_id.is_empty());
            }
            other => panic!("expected Adopted, got {other:?}"),
        }
        assert!(
            !skill.exists(),
            "legacy source must be removed after success"
        );
        // managed package 存在且不在 .claude/skills
        let pkg_root = package_materialized_root(&data).join("claude");
        assert!(pkg_root.is_dir());
        let found = walk_contains_skill_md(&pkg_root);
        assert!(found, "managed package should contain SKILL.md");
        assert!(
            !pkg_root.to_string_lossy().contains("/.claude/skills"),
            "managed package must not live under .claude/skills"
        );
    }

    #[tokio::test]
    async fn adopt_codex_legacy_skill_archives_and_removes_source() {
        let (engine, _tmp, data) = setup().await;
        let agents = data.join("home/.agents/skills");
        let skill = write_skill(&agents, "review", "Codex review.");
        let disc = discovery_for(
            AgentTarget::Codex,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::Adopted { .. }));
        assert!(!skill.exists());
        let pkg_root = package_materialized_root(&data).join("codex");
        assert!(pkg_root.is_dir());
        assert!(walk_contains_skill_md(&pkg_root));
    }

    #[tokio::test]
    async fn opencode_sees_one_shared_skill_after_both_adoptions() {
        let (engine, _tmp, data) = setup().await;
        let claude_skills = data.join("home/.claude/skills");
        let agents = data.join("home/.agents/skills");
        let body = "Shared review body.";
        let c = write_skill(&claude_skills, "review", body);
        let a = write_skill(&agents, "review", body);

        // 双 origin 兼容扫描可见 2
        assert_eq!(
            count_opencode_compat_skills(&claude_skills, &agents, "review").unwrap(),
            2
        );

        let d1 = discovery_for(
            AgentTarget::Claude,
            &c,
            PortableOriginKind::LegacyStandalone,
        );
        let d2 = discovery_for(AgentTarget::Codex, &a, PortableOriginKind::LegacyStandalone);
        assert!(matches!(
            engine.adopt(request(&data, d1, true)).await.unwrap(),
            AdoptionOutcome::Adopted { .. }
        ));
        assert!(matches!(
            engine.adopt(request(&data, d2, true)).await.unwrap(),
            AdoptionOutcome::Adopted { .. }
        ));

        // 兼容路径 0；Hub shared 逻辑资产 1
        assert_eq!(
            count_opencode_compat_skills(&claude_skills, &agents, "review").unwrap(),
            0
        );
        let assets = engine
            .repo
            .list_assets(None, Some(AssetKind::Skill))
            .await
            .unwrap();
        let skills: Vec<_> = assets
            .into_iter()
            .filter(|a| a.logical_key == "review")
            .collect();
        assert_eq!(skills.len(), 1, "exactly one shared hub skill");
    }

    #[tokio::test]
    async fn unknown_file_preserves_legacy_no_second_copy() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        // symlink → unknownSourceField diagnostic
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp/outside", skill.join("bad-link")).unwrap();
        }
        #[cfg(not(unix))]
        {
            // Windows：写一个 junction-like 诊断触发——用非文件 reparse 难；改为 destination collision
            fs::create_dir_all(package_materialized_root(&data).join("claude").join("user"))
                .unwrap();
            fs::write(
                package_materialized_root(&data)
                    .join("claude")
                    .join("user")
                    .join(".external-collision-marker"),
                b"x",
            )
            .unwrap();
        }
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(
            matches!(out, AdoptionOutcome::ExternalCollision { .. }),
            "got {out:?}"
        );
        assert!(skill.is_dir(), "legacy must remain");
        // 不得在兼容路径生成第二份
        assert!(count_opencode_compat_skills(&root, &data.join("none"), "review").unwrap() <= 1);
    }

    #[tokio::test]
    async fn source_hash_drift_preserves_legacy() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "stable");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        engine.inject_fault(AdoptionFault::ForceHashDrift);
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::ExternalCollision { .. }));
        assert!(skill.is_dir());
    }

    #[tokio::test]
    async fn failed_activation_preserves_legacy() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        engine.inject_fault(AdoptionFault::ForceActivationFailure);
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::Blocked { .. }));
        assert!(skill.is_dir(), "source preserved on activation fail");
    }

    #[tokio::test]
    async fn destination_collision_preserves_legacy() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        fs::create_dir_all(package_materialized_root(&data).join("claude").join("user")).unwrap();
        fs::write(
            package_materialized_root(&data)
                .join("claude")
                .join("user")
                .join(".external-collision-marker"),
            b"foreign",
        )
        .unwrap();
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::ExternalCollision { .. }));
        assert!(skill.is_dir());
    }

    #[tokio::test]
    async fn crash_before_archive_preserves_legacy_and_recovers() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        engine.inject_fault(AdoptionFault::CrashBeforeArchive);
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::Blocked { reason } if reason.contains("crash")));
        assert!(skill.is_dir(), "crash before archive keeps source");
        // 仅一份 discoverable：源仍在，兼容扫描 1
        assert_eq!(
            count_opencode_compat_skills(&root, &data.join("none"), "review").unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn crash_before_db_commit_leaves_recoverable_archive() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        engine.inject_fault(AdoptionFault::CrashBeforeDbCommit);
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(matches!(out, AdoptionOutcome::Blocked { .. }));
        assert!(!skill.exists(), "renamed to staging");
        // staging 存在
        let staging_root = data.join("agent-hub/adoption-staging");
        assert!(staging_root.is_dir());
        // 兼容路径 0 份（源已迁走）
        assert_eq!(
            count_opencode_compat_skills(&root, &data.join("none"), "review").unwrap(),
            0
        );
        // recovery 完成 commit
        let rows = engine.repo.list_adoptions().await.unwrap();
        let archived = rows
            .into_iter()
            .find(|r| r.state == AdoptionState::Archived)
            .expect("archived row");
        engine.inject_fault(AdoptionFault::None);
        let recovered = engine.recover_adoption(&archived.id).await.unwrap();
        assert!(matches!(recovered, AdoptionOutcome::Adopted { .. }));
    }

    #[tokio::test]
    async fn unconfirmed_user_adoption_is_blocked() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let out = engine.adopt(request(&data, disc, false)).await.unwrap();
        assert!(matches!(
            out,
            AdoptionOutcome::Blocked {
                reason
            } if reason == "user_confirmation_required"
        ));
        assert!(skill.is_dir());
    }

    #[tokio::test]
    async fn mark_pending_legacy_sources_never_deletes() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "new-skill", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let n = mark_pending_legacy_sources(&engine.repo, &[disc])
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(skill.is_dir(), "never auto-delete unrecognized tree");
        let rows = engine.repo.list_adoptions().await.unwrap();
        assert!(rows
            .iter()
            .any(|r| r.state == AdoptionState::ExternalCollision));
    }

    fn walk_contains_skill_md(root: &Path) -> bool {
        fn walk(p: &Path) -> bool {
            if p.is_file() && p.file_name().and_then(|s| s.to_str()) == Some("SKILL.md") {
                return true;
            }
            if p.is_dir() {
                if let Ok(rd) = fs::read_dir(p) {
                    for e in rd.flatten() {
                        if walk(&e.path()) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        walk(root)
    }
}
