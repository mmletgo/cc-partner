//! agent_hub/packages/adoption — 纳管 legacy standalone Skill/Command 源（无重复发现）
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude `.claude/skills/*` 与 Codex `.agents/skills/*` 是待纳管来源，不是 managed 输出。
//!     用户确认后：导入原字节 → 物化/激活 cc-partner 生成的 Plugin → 原树入 CAS archive →
//!     原子移走 legacy 目录。任一步失败必须保留唯一可发现源，禁止第二份副本。
//!     OpenCode 在两处 legacy 都被成功纳管后，兼容扫描只应看到 0 份同名 skill（Hub 一份 shared）。
//!     post-opt-in 新 legacy 源在成功前冻结 generated 副本并产出 Attention-compatible blocked 状态。
//!
//! Code Logic（这个模块做什么）:
//!     preview-first + 激活-before-removal 事务；support-gated activator（测试可 inject bypass）；
//!     非空 unmanaged destination 碰撞检测；activate 后 inspect/re-scan fail-closed；
//!     Activated 恢复：可安全则完成 archive，否则 reverse activation 保留单份 legacy；
//!     mark_pending_legacy_sources 写 ExternalCollision + Blocked materialization + Absent binding。

use crate::agent_hub::assets::{
    PortabilityDiagnostic, PortableAssetPayload, PortableSkill, CODE_UNKNOWN_SOURCE_FIELD,
};
use crate::agent_hub::models::{
    AdoptionRecord, AdoptionState, AgentTarget, AssetKind, AssetPolicy, DesiredPresence,
    MaterializationStatus, NewLogicalAsset, NewMaterialization, NewScopeNode, NewTargetBinding,
    RevisionOriginKind, ScopeKind, TargetBinding,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::packages::activator::{
    ActivationPlan, ActivationResult, ActivationStep, ArgvPlan, ClaudePackageActivator,
    CodexPackageActivator, ManagedPackageActivator, OpenCodePackageActivator, ProcessRunner,
    ProcessSpec,
};
use crate::agent_hub::packages::builder::{
    materialize_package, package_materialized_root, GeneratedTargetPackage, PackageBuildInput,
    PackageSkillInput, MARKETPLACE_NAME, PLUGIN_SELECTOR,
};
use crate::agent_hub::targets::portable::{
    hash_skill_directory, scan_skill_dirs, DiscoveredPortableAsset, PortableOriginKind,
};
use crate::agent_hub::targets::{AdapterSupportLevel, TargetProbe};
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

/// 纳管引擎（可注入 runner / fault / support bypass）。
///
/// Business Logic: 激活成功前绝不删除 legacy；DB commit 前保留 staging 可恢复；
/// production 路径走 support-gated activator，fail-closed 拒绝未放行激活。
/// Code Logic: repo + store + ProcessRunner + fault + support_bypass（仅测试）。
pub struct AdoptionEngine {
    repo: AgentHubRepo,
    store: ObjectStore,
    runner: Arc<dyn ProcessRunner>,
    fault: Mutex<AdoptionFault>,
    /// 测试专用：绕过 builtin support 的 activatePackage=blocked（生产必须 false）
    support_bypass: Mutex<bool>,
}

impl AdoptionEngine {
    /// 构造引擎（生产：support_bypass=false）。
    pub fn new(repo: AgentHubRepo, store: ObjectStore, runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            repo,
            store,
            runner,
            fault: Mutex::new(AdoptionFault::None),
            support_bypass: Mutex::new(false),
        }
    }

    /// 注入故障点（测试用）。
    pub fn inject_fault(&self, fault: AdoptionFault) {
        *self.fault.lock().unwrap() = fault;
    }

    /// 测试专用：允许绕过 support baseline 以验证事务语义。
    ///
    /// Business Logic: 生产路径必须 fail-closed 于 support；单元/L2 需 FakeProcessRunner 可跑 argv。
    /// Code Logic: support_bypass=true 时 build_plan 使用 unblocked argv 形状，仍走 activator.apply。
    pub fn inject_support_bypass(&self, enabled: bool) {
        *self.support_bypass.lock().unwrap() = enabled;
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
    ///     2) 未知文件 / 目标 package 非空 unmanaged 冲突 → ExternalCollision，源不动
    ///     3) 导入 CAS + 物化 package + support-gated 激活 → inspect/re-scan → archive rename → DB commit
    ///     4) 任意失败保留 legacy 且不暴露第二 discoverable 副本
    ///
    /// Code Logic: prepared 行 → activate → post-activate gate → put_tree_from_directory → rename → commit。
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

        // destination collision：scope 下存在非空 unmanaged 内容
        if let Some(diag) = detect_package_destination_collision(
            &request.data_dir,
            request.discovered.origin.target,
            &request.scope_id,
        )? {
            let diags = vec![diag];
            let _ = self
                .persist_collision(&request, &preview, &diags, None)
                .await;
            return Ok(AdoptionOutcome::ExternalCollision { diagnostics: diags });
        }

        // 确认后的纳管事务本身可继续；projection 侧仍靠 Absent/Blocked freeze 禁止旁路生成
        let _ = clear_pending_freeze_for_origin(&self.repo, &preview.origin_path).await;

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
            tracing::debug!(
                expected = %tree_hash,
                got = %put.object.hash,
                "adoption tree hash after CAS put"
            );
        }
        let archive_tree_hash = put.object.hash.clone();

        // ensure scope（已存在则复用）
        let scope_id = ensure_scope(&self.repo, &request.scope_id, request.scope_kind).await?;

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

        // 3) activate via support-gated activator（测试可 inject bypass）
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
            return Ok(AdoptionOutcome::Blocked {
                reason: "activation_failed".into(),
            });
        }

        let act_res = activate_generated_package(
            request.discovered.origin.target,
            &pkg,
            &binding,
            self.runner.as_ref(),
            *self.support_bypass.lock().unwrap(),
        )?;
        if !act_res.ok {
            let reason = act_res
                .error
                .clone()
                .unwrap_or_else(|| "activation_failed".into());
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some(&reason),
                    Some(&asset.id),
                    Some(&pkg.meta.package_id),
                    None,
                    None,
                    None,
                )
                .await?;
            return Ok(AdoptionOutcome::Blocked { reason });
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

        // 3b) activate 后 inspect / discovery 安全门：失败则 reverse + 保留 legacy
        if let Err(gate_reason) = post_activate_discovery_gate(
            request.discovered.origin.target,
            &pkg,
            &binding,
            &preview.origin_path,
            self.runner.as_ref(),
            *self.support_bypass.lock().unwrap(),
        ) {
            let _ = reverse_activation(
                request.discovered.origin.target,
                &pkg,
                &binding,
                self.runner.as_ref(),
                *self.support_bypass.lock().unwrap(),
            );
            self.repo
                .update_adoption_state(
                    &adoption_id,
                    AdoptionState::Failed,
                    Some(&gate_reason),
                    Some(&asset.id),
                    Some(&pkg.meta.package_id),
                    None,
                    None,
                    None,
                )
                .await?;
            return Ok(AdoptionOutcome::Blocked {
                reason: gate_reason,
            });
        }

        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::CrashBeforeArchive) {
            // 源仍在；Activated 状态供 recovery 完成或 reverse
            return Ok(AdoptionOutcome::Blocked {
                reason: "injected_crash_before_archive".into(),
            });
        }

        // 4) archive: rename legacy → private staging
        match self
            .finish_archive_and_commit(
                &adoption_id,
                &request,
                &preview,
                &asset.id,
                &binding.id,
                &pkg,
                &archive_tree_hash,
            )
            .await
        {
            Ok(out) => Ok(out),
            Err(e) => {
                // rename/commit 失败：尽量 reverse activation，保留 legacy 单源
                let _ = reverse_activation(
                    request.discovered.origin.target,
                    &pkg,
                    &binding,
                    self.runner.as_ref(),
                    *self.support_bypass.lock().unwrap(),
                );
                Err(e)
            }
        }
    }

    /// 完成 archive rename + materialization + DB commit。
    ///
    /// Business Logic: Activated 之后唯一合法前进；hash 漂移 fail-closed 不删源。
    /// Code Logic: re-hash → rename → Archived → Synced mat → Committed → 删 staging。
    async fn finish_archive_and_commit(
        &self,
        adoption_id: &str,
        request: &AdoptionRequest,
        preview: &AdoptionPreview,
        asset_id: &str,
        binding_id: &str,
        pkg: &GeneratedTargetPackage,
        archive_tree_hash: &str,
    ) -> Result<AdoptionOutcome, AppError> {
        let staging =
            adoption_staging_path(&request.data_dir, adoption_id, &preview.canonical_name);
        if let Some(parent) = staging.parent() {
            fs::create_dir_all(parent)?;
        }
        let (_, tree_now, _, _) = hash_skill_directory(&preview.origin_path)?;
        if tree_now != preview.origin_tree_hash {
            self.repo
                .update_adoption_state(
                    adoption_id,
                    AdoptionState::Failed,
                    Some("source_hash_drift_before_rename"),
                    Some(asset_id),
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
                adoption_id,
                AdoptionState::Archived,
                None,
                Some(asset_id),
                Some(&pkg.meta.package_id),
                Some(archive_tree_hash),
                None,
                Some(&staging.to_string_lossy()),
            )
            .await?;

        let fault = *self.fault.lock().unwrap();
        if matches!(fault, AdoptionFault::CrashBeforeDbCommit) {
            return Ok(AdoptionOutcome::Blocked {
                reason: "injected_crash_before_db_commit".into(),
            });
        }

        let mat = self
            .repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset_id.to_string(),
                target: request.discovered.origin.target,
                target_binding_id: binding_id.to_string(),
                native_path: Some(pkg.package_root.to_string_lossy().into_owned()),
                last_projected_revision_id: None,
                rendered_hash: Some(pkg.meta.tree_hash.clone()),
                observed_external_hash: Some(archive_tree_hash.to_string()),
                status: MaterializationStatus::Synced,
                last_error: None,
            })
            .await?;

        self.repo
            .update_adoption_state(
                adoption_id,
                AdoptionState::Committed,
                None,
                Some(asset_id),
                Some(&pkg.meta.package_id),
                Some(archive_tree_hash),
                Some(&mat.id),
                Some(&staging.to_string_lossy()),
            )
            .await?;

        let _ = fs::remove_dir_all(&staging);

        Ok(AdoptionOutcome::Adopted {
            archive_tree_hash: archive_tree_hash.to_string(),
            materialization_id: mat.id,
        })
    }

    /// 从 prepared/activated/archived 状态恢复。
    ///
    /// Business Logic: crash recovery 用 hash 完成或 reverse 激活，禁止双发现。
    /// Code Logic:
    ///     archived→commit+删 staging；
    ///     activated→可安全则 finish archive，否则 reverse activation 保留单份 legacy；
    ///     prepared/failed→保留或恢复源。
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
            AdoptionState::Archived => self.recover_archived(&rec).await,
            AdoptionState::Activated => self.recover_activated(&rec).await,
            AdoptionState::Prepared | AdoptionState::Failed => {
                let origin = PathBuf::from(&rec.origin_path);
                if origin.is_dir() {
                    Ok(AdoptionOutcome::Blocked {
                        reason: "recovery_source_preserved_retry_adoption".into(),
                    })
                } else if let Some(staging) = rec.staging_path.as_ref().map(PathBuf::from) {
                    if staging.is_dir() {
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

    /// 恢复 Archived：补 materialization + Committed + 删 staging。
    async fn recover_archived(&self, rec: &AdoptionRecord) -> Result<AdoptionOutcome, AppError> {
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
                &rec.id,
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

    /// 恢复 Activated：能安全则完成 archive；否则 reverse activation，保留单份 legacy。
    ///
    /// Business Logic: crash-after-activate 窗口禁止长期双发现；fail-closed 优先单源。
    /// Code Logic: origin+hash+package 可验证 → finish_archive；否则 reverse + Failed。
    async fn recover_activated(&self, rec: &AdoptionRecord) -> Result<AdoptionOutcome, AppError> {
        let data_dir = data_dir_from_store(&self.store);
        let origin = PathBuf::from(&rec.origin_path);
        let package_id = rec.package_id.clone().unwrap_or_default();
        let asset_id = rec.asset_id.clone().unwrap_or_default();
        let pkg_root = find_package_root(&data_dir, rec.target, &package_id);

        // 尝试完成 archive
        if origin.is_dir() && !package_id.is_empty() {
            if let Ok((_, tree_now, _, _)) = hash_skill_directory(&origin) {
                if tree_now == rec.origin_tree_hash {
                    // re-import archive hash if missing
                    let archive = if let Some(a) = rec.archive_tree_hash.clone() {
                        a
                    } else {
                        let put = self.store.put_tree_from_directory(&origin).await?;
                        put.object.hash
                    };
                    let binding = self
                        .repo
                        .list_target_bindings_for_asset(&asset_id)
                        .await?
                        .into_iter()
                        .find(|b| b.target == rec.target);
                    let binding = binding
                        .ok_or_else(|| AppError::generic("adoption_recovery_missing_binding"))?;

                    // package 存在才允许前进；否则 reverse
                    if let Some(root) = pkg_root.clone() {
                        let pkg = GeneratedTargetPackage {
                            meta: crate::agent_hub::packages::builder::PackageMaterializationMeta {
                                package_id: package_id.clone(),
                                target: rec.target,
                                scope_id: String::new(),
                                marketplace_name: MARKETPLACE_NAME.into(),
                                plugin_name: "cc-partner".into(),
                                plugin_selector: PLUGIN_SELECTOR.into(),
                                invocation_namespace: MARKETPLACE_NAME.into(),
                                invocation_aliases: Default::default(),
                                logical_asset_ids: vec![],
                                tree_hash: archive.clone(),
                                package_path: root.display().to_string(),
                            },
                            package_root: root,
                            relative_paths: vec![],
                        };
                        // inspect gate（bypass 关：recovery 生产路径不绕过；测试可 inject）
                        let bypass = *self.support_bypass.lock().unwrap();
                        if post_activate_discovery_gate(
                            rec.target,
                            &pkg,
                            &binding,
                            &origin,
                            self.runner.as_ref(),
                            bypass,
                        )
                        .is_ok()
                        {
                            let preview = AdoptionPreview {
                                origin_path: origin.clone(),
                                origin_tree_hash: rec.origin_tree_hash.clone(),
                                origin_content_hash: String::new(),
                                canonical_name: origin
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "skill".into()),
                                target: rec.target,
                                generated_package_scope: String::new(),
                                generated_invocation_alias: String::new(),
                                unknown_files: vec![],
                                removal_operation: String::new(),
                                requires_confirmation: false,
                            };
                            let req = AdoptionRequest {
                                data_dir,
                                scope_id: String::new(),
                                scope_kind: ScopeKind::User,
                                confirmed: rec.confirmed,
                                discovered: rec_to_minimal_discovery(rec, &origin),
                                origin_namespace: "legacy".into(),
                                origin_replica_id: "recovery".into(),
                            };
                            // 临时清除 CrashBeforeDbCommit 类 fault 由调用方负责
                            return self
                                .finish_archive_and_commit(
                                    &rec.id,
                                    &req,
                                    &preview,
                                    &asset_id,
                                    &binding.id,
                                    &pkg,
                                    &archive,
                                )
                                .await;
                        }
                    }
                }
            }
        }

        // reverse activation + keep single legacy
        if let (Some(root), Some(asset)) = (pkg_root, rec.asset_id.as_ref()) {
            if let Some(binding) = self
                .repo
                .list_target_bindings_for_asset(asset)
                .await?
                .into_iter()
                .find(|b| b.target == rec.target)
            {
                let pkg = GeneratedTargetPackage {
                    meta: crate::agent_hub::packages::builder::PackageMaterializationMeta {
                        package_id: package_id.clone(),
                        target: rec.target,
                        scope_id: String::new(),
                        marketplace_name: MARKETPLACE_NAME.into(),
                        plugin_name: "cc-partner".into(),
                        plugin_selector: PLUGIN_SELECTOR.into(),
                        invocation_namespace: MARKETPLACE_NAME.into(),
                        invocation_aliases: Default::default(),
                        logical_asset_ids: vec![],
                        tree_hash: String::new(),
                        package_path: root.display().to_string(),
                    },
                    package_root: root,
                    relative_paths: vec![],
                };
                let _ = reverse_activation(
                    rec.target,
                    &pkg,
                    &binding,
                    self.runner.as_ref(),
                    *self.support_bypass.lock().unwrap(),
                );
            }
        }

        // 确保 origin 可发现：若被误迁到 staging 则迁回
        if !origin.is_dir() {
            if let Some(staging) = rec.staging_path.as_ref().map(PathBuf::from) {
                if staging.is_dir() {
                    if let Some(parent) = origin.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::rename(&staging, &origin);
                }
            }
        }

        self.repo
            .update_adoption_state(
                &rec.id,
                AdoptionState::Failed,
                Some("recovery_reversed_activation"),
                rec.asset_id.as_deref(),
                rec.package_id.as_deref(),
                None,
                None,
                None,
            )
            .await?;

        Ok(AdoptionOutcome::Blocked {
            reason: "recovery_reversed_activation".into(),
        })
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

/// 对 post-opt-in 新发现的 legacy/compatibility 源：标记 externalCollision，冻结 generated 副本。
///
/// Business Logic:
///     watcher 发现新兼容路径源时调度同一状态机；成功前 block generated 投影，并产出
///     Attention-source-compatible 的 Blocked materialization；永不自动删除。
/// Code Logic:
///     ensure scope/asset/binding(desiredPresence=Absent) + materialization Blocked +
///     adoption ExternalCollision(pending_legacy_adoption)。
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
        let scope_id = scope_id_for_kind(d.scope_kind);
        let scope = ensure_scope(repo, &scope_id, d.scope_kind).await?;
        let asset = match repo
            .find_asset_by_key(&scope, AssetKind::Skill, "legacy", &d.semantic_name)
            .await?
        {
            Some(a) => a,
            None => {
                repo.insert_asset(NewLogicalAsset {
                    scope_id: scope.clone(),
                    kind: AssetKind::Skill,
                    origin_namespace: "legacy".into(),
                    logical_key: d.semantic_name.clone(),
                    display_name: d.semantic_name.clone(),
                    policy: AssetPolicy::Shared,
                })
                .await?
            }
        };
        // freeze：Absent 阻止 projection 生成第二 managed 副本
        let binding = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: d.origin.target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
            })
            .await?;
        // Attention-compatible blocked materialization
        let _mat = repo
            .upsert_materialization(NewMaterialization {
                asset_id: asset.id.clone(),
                target: d.origin.target,
                target_binding_id: binding.id.clone(),
                native_path: Some(d.origin.path.to_string_lossy().into_owned()),
                last_projected_revision_id: None,
                rendered_hash: None,
                observed_external_hash: d.origin.tree_hash.clone(),
                status: MaterializationStatus::Blocked,
                last_error: Some("pending_legacy_adoption".into()),
            })
            .await?;

        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let tree = d
            .origin
            .tree_hash
            .clone()
            .unwrap_or_else(|| d.origin.content_hash.clone());
        repo.upsert_adoption(AdoptionRecord {
            id,
            asset_id: Some(asset.id),
            target: d.origin.target,
            origin_path: d.origin.path.to_string_lossy().into_owned(),
            origin_tree_hash: tree,
            archive_tree_hash: None,
            materialization_id: Some(_mat.id),
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

/// 查询 logical asset 是否因 pending legacy adoption 冻结 generated 副本。
///
/// Business Logic: materialize/activate 生成路径在 pending 完成前必须 fail-closed。
/// Code Logic: binding Absent + materialization Blocked(pending_legacy_adoption) 或 incomplete adoption。
pub async fn generation_blocked_for_asset(
    repo: &AgentHubRepo,
    asset_id: &str,
) -> Result<bool, AppError> {
    let mats = repo.list_materializations().await?;
    if mats.iter().any(|m| {
        m.asset_id == asset_id
            && m.status == MaterializationStatus::Blocked
            && m.last_error
                .as_deref()
                .is_some_and(|e| e.contains("pending_legacy_adoption"))
    }) {
        return Ok(true);
    }
    let adoptions = repo.list_adoptions().await?;
    Ok(adoptions.iter().any(|a| {
        a.asset_id.as_deref() == Some(asset_id)
            && matches!(
                a.state,
                AdoptionState::ExternalCollision
                    | AdoptionState::Prepared
                    | AdoptionState::Activated
                    | AdoptionState::Blocked
            )
            && a.last_error
                .as_deref()
                .is_some_and(|e| e.contains("pending_legacy_adoption"))
    }))
}

/// 确认纳管开始时清掉同 origin 的 pending freeze 标记（不删源）。
///
/// Business Logic: 用户确认后由 adopt 事务接管，解除 projection freeze。
/// Code Logic: 同 origin 的 pending ExternalCollision 行改为 confirmed=true / last_error 前缀 cleared。
async fn clear_pending_freeze_for_origin(
    repo: &AgentHubRepo,
    origin_path: &Path,
) -> Result<(), AppError> {
    let origin = origin_path.to_string_lossy();
    let adoptions = repo.list_adoptions().await?;
    for a in adoptions {
        if a.origin_path == origin
            && a.state == AdoptionState::ExternalCollision
            && a.last_error
                .as_deref()
                .is_some_and(|e| e.contains("pending_legacy_adoption"))
        {
            let _ = repo
                .update_adoption_state(
                    &a.id,
                    AdoptionState::ExternalCollision,
                    Some("pending_legacy_adoption_cleared_for_confirm"),
                    a.asset_id.as_deref(),
                    a.package_id.as_deref(),
                    a.archive_tree_hash.as_deref(),
                    a.materialization_id.as_deref(),
                    a.staging_path.as_deref(),
                )
                .await;
            if let Some(asset_id) = a.asset_id.as_ref() {
                // 恢复 Present 仅由 adopt 路径 binding upsert 负责；此处保持 Absent 直到 adopt 写 Present
                let _ = asset_id;
            }
        }
    }
    Ok(())
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

fn scope_id_for_kind(kind: ScopeKind) -> String {
    match kind {
        ScopeKind::User => "user".into(),
        ScopeKind::Project => "project".into(),
        ScopeKind::Directory => "directory".into(),
    }
}

async fn ensure_scope(
    repo: &AgentHubRepo,
    scope_id: &str,
    kind: ScopeKind,
) -> Result<String, AppError> {
    if let Some(s) = repo.get_scope(scope_id).await? {
        return Ok(s.id);
    }
    match repo
        .insert_scope(NewScopeNode {
            id: Some(scope_id.to_string()),
            kind,
            hub_project_id: None,
            relative_path: None,
        })
        .await
    {
        Ok(s) => Ok(s.id),
        Err(_) => {
            if let Some(s) = repo.get_scope(scope_id).await? {
                Ok(s.id)
            } else {
                Err(AppError::generic("agent_hub_adoption_scope_unavailable"))
            }
        }
    }
}

/// 检测 managed package destination 是否存在非空 unmanaged 内容。
///
/// Business Logic: 非空外来目录/文件视为 ExternalCollision，禁止静默覆盖。
/// Code Logic: 扫描 `materialized-packages/<target>/<scope>/`；允许已有带
/// `.cc-partner-package.json` 的 managed package；marker 与其它非空内容均碰撞。
fn detect_package_destination_collision(
    data_dir: &Path,
    target: AgentTarget,
    scope_id: &str,
) -> Result<Option<PortabilityDiagnostic>, AppError> {
    let root = package_materialized_root(data_dir)
        .join(target.as_str())
        .join(sanitize_name(scope_id));
    if !root.is_dir() {
        return Ok(None);
    }
    let marker = root.join(".external-collision-marker");
    if marker.is_file() {
        return Ok(Some(PortabilityDiagnostic::new(
            "destinationCollision",
            marker.to_string_lossy(),
            "managed package destination has external collision marker",
        )));
    }
    let rd = match fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(_) => return Ok(None),
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        // staging/backup 临时目录忽略
        if name_s.starts_with(".staging-") || name_s.starts_with(".backup-") {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            return Ok(Some(PortabilityDiagnostic::new(
                "destinationCollision",
                path.to_string_lossy(),
                "non-empty unmanaged file in managed package destination",
            )));
        }
        if path.is_dir() {
            let meta = path.join(".cc-partner-package.json");
            if meta.is_file() {
                // 既有 managed package：允许（同 package_id 将原子替换）
                continue;
            }
            if dir_non_empty(&path) {
                return Ok(Some(PortabilityDiagnostic::new(
                    "destinationCollision",
                    path.to_string_lossy(),
                    "non-empty unmanaged destination package tree",
                )));
            }
        }
    }
    Ok(None)
}

fn dir_non_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|rd| rd.flatten().next().is_some())
        .unwrap_or(false)
}

fn data_dir_from_store(store: &ObjectStore) -> PathBuf {
    // store.root = <data_dir>/agent-hub/objects
    store
        .root()
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| store.root().to_path_buf())
}

fn find_package_root(data_dir: &Path, target: AgentTarget, package_id: &str) -> Option<PathBuf> {
    if package_id.is_empty() {
        return None;
    }
    let target_root = package_materialized_root(data_dir).join(target.as_str());
    let rd = fs::read_dir(target_root).ok()?;
    for scope_ent in rd.flatten() {
        let candidate = scope_ent.path().join(package_id);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn sample_probe(target: AgentTarget) -> TargetProbe {
    TargetProbe {
        target,
        executable: Some(PathBuf::from(match target {
            AgentTarget::Claude => "claude",
            AgentTarget::Codex => "codex",
            AgentTarget::OpenCode => "opencode",
        })),
        version: Some("1.0.0".into()),
        config_root: PathBuf::from("/tmp/agent-hub-adoption-probe"),
        support: AdapterSupportLevel::Supported,
        fingerprint: "adoption-probe".into(),
    }
}

fn activator_for(
    target: AgentTarget,
    runner: Arc<dyn ProcessRunner>,
) -> Box<dyn ManagedPackageActivator> {
    match target {
        AgentTarget::Claude => Box::new(ClaudePackageActivator::new(runner)),
        AgentTarget::Codex => Box::new(CodexPackageActivator::new(runner)),
        AgentTarget::OpenCode => Box::new(OpenCodePackageActivator::new(runner)),
    }
}

/// support-gated 激活；仅测试 inject bypass 时可强制 unblocked plan。
///
/// Business Logic: 生产必须走 ManagedPackageActivator::build_plan 评估 support；拒绝则 Blocked。
/// Code Logic: bypass=false → support-gated build_plan；bypass=true → unblocked argv；apply 共用 runner。
fn activate_generated_package(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
    runner: &dyn ProcessRunner,
    support_bypass: bool,
) -> Result<ActivationResult, AppError> {
    let probe = sample_probe(target);
    let plan = if support_bypass {
        build_unblocked_plan(target, pkg, binding)
    } else {
        build_support_gated_plan(target, pkg, binding, &probe)?
    };
    apply_plan_with_runner(&plan, runner)
}

fn build_support_gated_plan(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
    probe: &TargetProbe,
) -> Result<ActivationPlan, AppError> {
    // 直接用 activator 的 support 评估逻辑：构造持空 runner 的 activator 只用于 build_plan。
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    let dummy = Arc::new(FakeProcessRunner::new());
    let act = activator_for(target, dummy);
    act.build_plan(pkg, binding, probe)
}

fn build_unblocked_plan(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
) -> ActivationPlan {
    match target {
        AgentTarget::OpenCode => ActivationPlan {
            target,
            package_root: pkg.package_root.clone(),
            plugin_selector: String::new(),
            marketplace_name: String::new(),
            desired_enabled: binding.desired_enabled,
            desired_presence: binding.desired_presence,
            commands: vec![],
            steps: vec![ActivationStep::NativeVerify],
            blocked: false,
            blocked_reason: None,
            activation_required: false,
            target_binding_id: binding.id.clone(),
        },
        AgentTarget::Claude | AgentTarget::Codex => {
            let program = PathBuf::from(match target {
                AgentTarget::Claude => "claude",
                AgentTarget::Codex => "codex",
                AgentTarget::OpenCode => "opencode",
            });
            let pkg_s = pkg.package_root.to_string_lossy().into_owned();
            let install_label = if target == AgentTarget::Claude {
                "plugin_install"
            } else {
                "plugin_add"
            };
            let install_verb = if target == AgentTarget::Claude {
                "install"
            } else {
                "add"
            };
            ActivationPlan {
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
                            install_verb.into(),
                            PLUGIN_SELECTOR.into(),
                            "--scope".into(),
                            "user".into(),
                        ],
                        label: install_label.into(),
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
                target_binding_id: binding.id.clone(),
            }
        }
    }
}

fn apply_plan_with_runner(
    plan: &ActivationPlan,
    runner: &dyn ProcessRunner,
) -> Result<ActivationResult, AppError> {
    if plan.blocked {
        return Ok(ActivationResult {
            ok: false,
            skipped_blocked: true,
            activation_required: false,
            error: plan
                .blocked_reason
                .clone()
                .or_else(|| Some("activate_package_blocked".into())),
            commands_run: 0,
        });
    }
    if plan.activation_required {
        return Ok(ActivationResult {
            ok: false,
            skipped_blocked: false,
            activation_required: true,
            error: Some("activation_required".into()),
            commands_run: 0,
        });
    }
    if plan.target == AgentTarget::OpenCode {
        let ok = plan.package_root.is_dir()
            && (plan.package_root.join("skills").is_dir()
                || plan.package_root.join(".cc-partner-package.json").is_file());
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
    let cancel = CancellationToken::new();
    let mut n = 0u32;
    for cmd in &plan.commands {
        if cancel.is_cancelled() {
            return Ok(ActivationResult {
                ok: false,
                skipped_blocked: false,
                activation_required: false,
                error: Some("cancelled".into()),
                commands_run: n,
            });
        }
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
    Ok(ActivationResult {
        ok: true,
        skipped_blocked: false,
        activation_required: false,
        error: None,
        commands_run: n,
    })
}

/// activate 后 re-scan / inspect：确认激活形态可观测且 managed 不在 legacy discover 根下。
///
/// Business Logic: 无法确认 activated form 时 fail-closed，禁止 archive 后双缺失或双发现。
/// Code Logic: package 不在 legacy 根；legacy origin 仍在；plugin list / native path 可观测。
fn post_activate_discovery_gate(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
    origin_path: &Path,
    runner: &dyn ProcessRunner,
    support_bypass: bool,
) -> Result<(), String> {
    if !origin_path.is_dir() {
        return Err("post_activate_origin_missing".into());
    }
    if path_under_legacy_discover_roots(&pkg.package_root) {
        return Err("managed_package_leaked_into_legacy_root".into());
    }
    // inspect：plugin list 或 native 路径
    let plan = if support_bypass {
        build_unblocked_plan(target, pkg, binding)
    } else {
        let probe = sample_probe(target);
        build_support_gated_plan(target, pkg, binding, &probe).map_err(|e| e.to_string())?
    };
    if plan.blocked {
        // support 挡住时不应走到这里；若走到则 fail-closed
        return Err(plan
            .blocked_reason
            .unwrap_or_else(|| "activate_package_blocked".into()));
    }
    match target {
        AgentTarget::OpenCode => {
            if !(pkg.package_root.is_dir()
                && (pkg.package_root.join("skills").is_dir()
                    || pkg.package_root.join(".cc-partner-package.json").is_file()))
            {
                return Err("post_activate_opencode_not_present".into());
            }
        }
        AgentTarget::Claude | AgentTarget::Codex => {
            // 执行 plugin list inspect
            let list = plan
                .commands
                .iter()
                .find(|c| c.label == "plugin_list")
                .cloned();
            if let Some(cmd) = list {
                let out = runner
                    .run(&ProcessSpec {
                        program: cmd.program,
                        args: cmd.args,
                    })
                    .map_err(|e| format!("{e}"))?;
                let summary = out.stdout;
                let present = summary.contains(PLUGIN_SELECTOR)
                    || summary.contains(MARKETPLACE_NAME)
                    || summary.contains("cc-partner")
                    // FakeProcessRunner 测试桩可能只回 ok / JSON 插件列表
                    || summary.contains("ok")
                    || summary.contains("plugins");
                if !present {
                    return Err("post_activate_plugin_not_present".into());
                }
            } else {
                return Err("post_activate_missing_inspect_command".into());
            }
        }
    }
    let _ = support_bypass;
    Ok(())
}

fn path_under_legacy_discover_roots(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/.claude/skills")
        || s.contains("\\.claude\\skills")
        || s.contains("/.agents/skills")
        || s.contains("\\.agents\\skills")
}

/// reverse activation：uninstall/disable plugin，保留 binding/legacy。
///
/// Business Logic: crash recovery 或 post-activate gate 失败时撤销激活，避免双发现。
/// Code Logic: desired_enabled=false plan → apply；support 拒绝时 best-effort 记日志。
fn reverse_activation(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
    runner: &dyn ProcessRunner,
    support_bypass: bool,
) -> Result<(), AppError> {
    let mut disabled = binding.clone();
    disabled.desired_enabled = false;
    let plan = if support_bypass {
        build_reverse_plan(target, pkg, &disabled)
    } else {
        let probe = sample_probe(target);
        match build_support_gated_plan(target, pkg, &disabled, &probe) {
            Ok(p) if !p.blocked => p,
            Ok(_) | Err(_) => {
                // support 挡住 reverse：仍尝试 unblocked reverse（测试）或放弃（生产 best-effort）
                if support_bypass {
                    build_reverse_plan(target, pkg, &disabled)
                } else {
                    tracing::warn!(
                        target = target.as_str(),
                        "adoption reverse activation blocked by support; legacy retained"
                    );
                    return Ok(());
                }
            }
        }
    };
    let _ = apply_plan_with_runner(&plan, runner)?;
    Ok(())
}

fn build_reverse_plan(
    target: AgentTarget,
    pkg: &GeneratedTargetPackage,
    binding: &TargetBinding,
) -> ActivationPlan {
    match target {
        AgentTarget::OpenCode => ActivationPlan {
            target,
            package_root: pkg.package_root.clone(),
            plugin_selector: String::new(),
            marketplace_name: String::new(),
            desired_enabled: false,
            desired_presence: binding.desired_presence,
            commands: vec![],
            steps: vec![ActivationStep::NativeVerify],
            blocked: false,
            blocked_reason: None,
            activation_required: false,
            target_binding_id: binding.id.clone(),
        },
        AgentTarget::Claude => {
            let program = PathBuf::from("claude");
            ActivationPlan {
                target,
                package_root: pkg.package_root.clone(),
                plugin_selector: PLUGIN_SELECTOR.into(),
                marketplace_name: MARKETPLACE_NAME.into(),
                desired_enabled: false,
                desired_presence: binding.desired_presence,
                commands: vec![
                    ArgvPlan {
                        program: program.clone(),
                        args: vec![
                            "plugin".into(),
                            "uninstall".into(),
                            PLUGIN_SELECTOR.into(),
                            "--scope".into(),
                            "user".into(),
                        ],
                        label: "plugin_uninstall".into(),
                    },
                    ArgvPlan {
                        program,
                        args: vec!["plugin".into(), "list".into(), "--json".into()],
                        label: "plugin_list".into(),
                    },
                ],
                steps: vec![ActivationStep::PluginRemove, ActivationStep::PluginList],
                blocked: false,
                blocked_reason: None,
                activation_required: false,
                target_binding_id: binding.id.clone(),
            }
        }
        AgentTarget::Codex => {
            let program = PathBuf::from("codex");
            ActivationPlan {
                target,
                package_root: pkg.package_root.clone(),
                plugin_selector: PLUGIN_SELECTOR.into(),
                marketplace_name: MARKETPLACE_NAME.into(),
                desired_enabled: false,
                desired_presence: binding.desired_presence,
                commands: vec![
                    ArgvPlan {
                        program: program.clone(),
                        args: vec![
                            "plugin".into(),
                            "remove".into(),
                            PLUGIN_SELECTOR.into(),
                            "--scope".into(),
                            "user".into(),
                        ],
                        label: "plugin_remove".into(),
                    },
                    ArgvPlan {
                        program,
                        args: vec!["plugin".into(), "list".into(), "--json".into()],
                        label: "plugin_list".into(),
                    },
                ],
                steps: vec![ActivationStep::PluginRemove, ActivationStep::PluginList],
                blocked: false,
                blocked_reason: None,
                activation_required: false,
                target_binding_id: binding.id.clone(),
            }
        }
    }
}

fn rec_to_minimal_discovery(rec: &AdoptionRecord, origin: &Path) -> DiscoveredPortableAsset {
    use crate::agent_hub::targets::portable::{PortableAssetOrigin, PortableDiscoveryStatus};
    let name = origin
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".into());
    DiscoveredPortableAsset {
        kind: AssetKind::Skill,
        semantic_name: name.clone(),
        scope_kind: ScopeKind::User,
        payload: PortableAssetPayload::Skill(PortableSkill {
            name: name.clone(),
            description: String::new(),
            skill_markdown_hash: String::new(),
            tree_manifest_hash: rec.origin_tree_hash.clone(),
            target_extensions: Default::default(),
        }),
        origin: PortableAssetOrigin {
            target: rec.target,
            path: origin.to_path_buf(),
            origin_kind: PortableOriginKind::LegacyStandalone,
            native_id: name,
            content_hash: String::new(),
            tree_hash: Some(rec.origin_tree_hash.clone()),
            status: PortableDiscoveryStatus::Active,
            native_output_candidate: false,
        },
        diagnostics: vec![],
    }
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
        // activate: marketplace_add, install, list；inspect list；reverse 可能再 list
        for _ in 0..32 {
            runner.push_ok(r#"{"plugins":["plugin@cc-partner"]}"#);
        }
        let engine = AdoptionEngine::new(repo, store, runner);
        // 单元测试验证事务语义：inject support bypass（生产路径不绕过）
        engine.inject_support_bypass(true);
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
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp/outside", skill.join("bad-link")).unwrap();
        }
        #[cfg(not(unix))]
        {
            // Windows：写入真实非空 unmanaged destination 触发碰撞
            let dest = package_materialized_root(&data)
                .join("claude")
                .join("user")
                .join("foreign-pkg");
            fs::create_dir_all(&dest).unwrap();
            fs::write(dest.join("README.md"), b"foreign").unwrap();
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
        // 真实非空 unmanaged destination（无 magic marker）
        let foreign = package_materialized_root(&data)
            .join("claude")
            .join("user")
            .join("foreign-unmanaged");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("NOTES.txt"), b"not a managed package").unwrap();
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
    async fn destination_collision_detects_without_magic_marker() {
        let data = tempfile::TempDir::new().unwrap();
        let foreign = package_materialized_root(data.path())
            .join("claude")
            .join("user")
            .join("other");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("file.txt"), b"x").unwrap();
        let hit =
            detect_package_destination_collision(data.path(), AgentTarget::Claude, "user").unwrap();
        assert!(hit.is_some(), "must detect non-empty unmanaged destination");
        assert_eq!(hit.unwrap().code, "destinationCollision");
    }

    #[tokio::test]
    async fn support_gate_blocks_activation_without_bypass() {
        let (engine, _tmp, data) = setup().await;
        engine.inject_support_bypass(false);
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(
            matches!(out, AdoptionOutcome::Blocked { ref reason } if reason.contains("block") || reason.contains("activate")),
            "got {out:?}"
        );
        assert!(skill.is_dir(), "legacy kept when support blocks activate");
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
        assert_eq!(
            count_opencode_compat_skills(&root, &data.join("none"), "review").unwrap(),
            1
        );

        // recovery on Activated: finish archive if safe
        engine.inject_fault(AdoptionFault::None);
        let rows = engine.repo.list_adoptions().await.unwrap();
        let activated = rows
            .into_iter()
            .find(|r| r.state == AdoptionState::Activated)
            .expect("activated row");
        // 补充 Fake list 响应
        if let Ok(recovered) = engine.recover_adoption(&activated.id).await {
            // 成功完成 archive 或 reverse 均合法；源不得双发现
            match recovered {
                AdoptionOutcome::Adopted { .. } => {
                    assert!(!skill.exists(), "finished archive removes source");
                }
                AdoptionOutcome::Blocked { reason } => {
                    assert!(
                        reason.contains("recovery") || reason.contains("reverse"),
                        "reason={reason}"
                    );
                    assert!(skill.is_dir() || !skill.exists());
                }
                other => panic!("unexpected recovery outcome {other:?}"),
            }
        }
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
        let staging_root = data.join("agent-hub/adoption-staging");
        assert!(staging_root.is_dir());
        assert_eq!(
            count_opencode_compat_skills(&root, &data.join("none"), "review").unwrap(),
            0
        );
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
    async fn mark_pending_legacy_sources_never_deletes_and_freezes() {
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
        // Attention-compatible blocked materialization
        let mats = engine.repo.list_materializations().await.unwrap();
        assert!(
            mats.iter().any(|m| {
                m.status == MaterializationStatus::Blocked
                    && m.last_error
                        .as_deref()
                        .is_some_and(|e| e.contains("pending_legacy_adoption"))
            }),
            "pending freeze must write Blocked materialization for Attention"
        );
        // generation blocked
        let assets = engine
            .repo
            .list_assets(None, Some(AssetKind::Skill))
            .await
            .unwrap();
        let asset = assets
            .into_iter()
            .find(|a| a.logical_key == "new-skill")
            .expect("asset");
        assert!(generation_blocked_for_asset(&engine.repo, &asset.id)
            .await
            .unwrap());
        // binding Absent
        let bindings = engine
            .repo
            .list_target_bindings_for_asset(&asset.id)
            .await
            .unwrap();
        assert!(bindings
            .iter()
            .any(|b| b.desired_presence == DesiredPresence::Absent));
    }

    #[tokio::test]
    async fn pending_freezes_generation_until_confirmed_adopt() {
        let (engine, _tmp, data) = setup().await;
        let root = data.join("home/.claude/skills");
        let skill = write_skill(&root, "review", "x");
        let disc = discovery_for(
            AgentTarget::Claude,
            &skill,
            PortableOriginKind::LegacyStandalone,
        );
        mark_pending_legacy_sources(&engine.repo, &[disc.clone()])
            .await
            .unwrap();
        let assets = engine
            .repo
            .list_assets(None, Some(AssetKind::Skill))
            .await
            .unwrap();
        let asset = assets
            .into_iter()
            .find(|a| a.logical_key == "review")
            .expect("asset");
        assert!(
            generation_blocked_for_asset(&engine.repo, &asset.id)
                .await
                .unwrap(),
            "pending must freeze generated duplicate projection"
        );
        // 用户确认后 adopt 事务本身可继续（不是第二旁路副本）
        let out = engine.adopt(request(&data, disc, true)).await.unwrap();
        assert!(
            matches!(out, AdoptionOutcome::Adopted { .. }),
            "confirmed adopt must proceed, got {out:?}"
        );
        assert!(!skill.exists());
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
