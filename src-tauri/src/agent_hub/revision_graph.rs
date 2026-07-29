//! agent_hub/revision_graph — Revision DAG 共同祖先与 merge-base
//!
//! Business Logic（为什么需要这个模块）:
//!     两设备对同一逻辑资产分叉后，接收方必须算出 maximal common ancestors 作为三方合并 base；
//!     无共同祖先且内容不等时必须 Conflict，禁止静默覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     基于 AgentHubRepo 批量读 revision parents，visited 集合上限 100_000；
//!     maximal_common_ancestors 剔除被另一共同祖先祖先化的候选；merge_base 在多 base
//!     时按 revision-ID 字典序递归调用内容合并器生成 ephemeral virtual base。

use crate::agent_hub::models::{Revision, RevisionId};
use crate::error::AppError;
use crate::storage::agent_hub_repo::AgentHubRepo;
use std::collections::{HashMap, HashSet, VecDeque};

/// 单次 DAG 遍历最多访问的 revision 数（与 snapshot 上限一致）。
pub const MAX_VISITED_REVISIONS: usize = 100_000;

/// merge-base 计算结果。
///
/// Business Logic（为什么需要这个枚举）:
///     调用方需要区分：单一 base、递归合并后的 virtual base、或无法合并的 Conflict。
///
/// Code Logic（这个枚举做什么）:
///     Base 携带 revision id 与是否 virtual；Conflict 不写库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBaseOutcome {
    /// 可用 base（真实 revision 或 ephemeral virtual）
    Base {
        /// base revision id（virtual 时为合成 id，不落库）
        revision_id: RevisionId,
        /// 是否为内存 virtual base
        virtual_base: bool,
        /// 合并后的内容摘要（供上层构造 merge revision）
        payload: MergePayload,
    },
    /// 无法确定唯一可合并 base
    Conflict {
        /// 相关 heads / 失败的 bases
        heads: Vec<RevisionId>,
        /// 人类可读原因 code
        reason: String,
    },
}

/// 合并内容摘要（payload / tree hash + 删除语义）。
///
/// Business Logic（为什么需要这个结构体）:
///     merge-base / content merger 只需比较内容身份，不需要完整 blob。
///
/// Code Logic（这个结构体做什么）:
///     保存 optional payload_hash / tree_manifest_hash 与 tombstone 标记。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergePayload {
    /// 单文件内容 hash
    pub payload_hash: Option<String>,
    /// 目录 manifest hash
    pub tree_manifest_hash: Option<String>,
    /// 是否为 delete tombstone
    pub is_delete: bool,
}

impl MergePayload {
    /// 从 Revision 提取内容摘要。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     无共同祖先时用内容相等折叠 merge。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射 payload/tree hash 与 operation==delete。
    pub fn from_revision(rev: &Revision) -> Self {
        Self {
            payload_hash: rev.payload_hash.clone(),
            tree_manifest_hash: rev.tree_manifest_hash.clone(),
            is_delete: rev.operation == crate::agent_hub::models::RevisionOperation::Delete,
        }
    }

    /// 内容是否完全相同。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     无祖先相等内容可折叠；不等则 Conflict。
    ///
    /// Code Logic（这个函数做什么）:
    ///     比较三个字段。
    pub fn content_eq(&self, other: &Self) -> bool {
        self.payload_hash == other.payload_hash
            && self.tree_manifest_hash == other.tree_manifest_hash
            && self.is_delete == other.is_delete
    }
}

/// 内容合并结果。
///
/// Business Logic（为什么需要这个枚举）:
///     多 base 递归合并时，merger 可能产出成功 payload 或冲突。
///
/// Code Logic（这个枚举做什么）:
///     Merged / Conflict。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentMergeResult {
    /// 合并成功
    Merged(MergePayload),
    /// 内容冲突
    Conflict,
}

/// 由调用方提供的内容三方/两方合并器。
///
/// Business Logic（为什么需要这个类型）:
///     Instruction 块级合并与目录 merge 策略不同，DAG 层只负责顺序调用。
///
/// Code Logic（这个类型做什么）:
///     `(left, right) -> ContentMergeResult` 异步闭包签名由 merge_base 接受。
pub type ContentMerger =
    Box<dyn Fn(MergePayload, MergePayload) -> ContentMergeResult + Send + Sync>;

/// Revision DAG 查询服务。
///
/// Business Logic（为什么需要这个结构体）:
///     同步/import 路径需要可测试的 DAG 算法入口。
///
/// Code Logic（这个结构体做什么）:
///     持有 AgentHubRepo 引用，提供 maximal_common_ancestors / merge_base。
#[derive(Clone)]
pub struct RevisionGraph {
    repo: AgentHubRepo,
}

impl RevisionGraph {
    /// 构造 DAG 查询器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     与生产 repo 共享同一 SQLite。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 repo clone。
    pub fn new(repo: AgentHubRepo) -> Self {
        Self { repo }
    }

    /// 计算 left/right 的 maximal common ancestors。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     criss-cross 图可能有多个共同祖先；必须剔除“是另一共同祖先之祖先”的候选。
    ///
    /// Code Logic（这个函数做什么）:
    ///     收集 left 祖先闭包与 right 祖先闭包的交集；再剔除作为其他候选祖先的 id；
    ///     结果按 revision-ID 字典序排序。visited 超限返回 Validation。
    pub async fn maximal_common_ancestors(
        &self,
        left: &RevisionId,
        right: &RevisionId,
    ) -> Result<Vec<RevisionId>, AppError> {
        if left == right {
            return Ok(vec![left.clone()]);
        }
        let left_anc = self.ancestor_closure(left).await?;
        let right_anc = self.ancestor_closure(right).await?;
        let common: Vec<RevisionId> = left_anc.intersection(&right_anc).cloned().collect();
        if common.is_empty() {
            return Ok(vec![]);
        }
        // 剔除被另一 common 祖先化的候选（非 maximal）
        let mut maximal = Vec::new();
        for cand in &common {
            let mut dominated = false;
            for other in &common {
                if cand == other {
                    continue;
                }
                if self.is_ancestor(cand, other).await? {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                maximal.push(cand.clone());
            }
        }
        maximal.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(maximal)
    }

    /// 计算 merge base：单祖先直接用；多祖先按 ID 序递归 content-merge 为 virtual base。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     三方合并需要唯一 base；virtual base 冲突不得写 revision。
    ///
    /// Code Logic（这个函数做什么）:
    ///     maximal_common_ancestors → 0 个：比较 heads 内容相等则 synthetic base，否则 Conflict；
    ///     1 个：Base{virtual=false}；多个：按 ID 排序 reduce content_merger，失败 Conflict。
    pub async fn merge_base(
        &self,
        left: &RevisionId,
        right: &RevisionId,
        content_merger: &dyn Fn(MergePayload, MergePayload) -> ContentMergeResult,
    ) -> Result<MergeBaseOutcome, AppError> {
        let left_rev = self.require_revision(left).await?;
        let right_rev = self.require_revision(right).await?;
        let bases = self.maximal_common_ancestors(left, right).await?;

        if bases.is_empty() {
            let lp = MergePayload::from_revision(&left_rev);
            let rp = MergePayload::from_revision(&right_rev);
            if lp.content_eq(&rp) {
                // 完全相同内容可折叠；使用字典序较小 head 作为 base id 标记
                let id = if left.as_str() <= right.as_str() {
                    left.clone()
                } else {
                    right.clone()
                };
                return Ok(MergeBaseOutcome::Base {
                    revision_id: id,
                    virtual_base: true,
                    payload: lp,
                });
            }
            return Ok(MergeBaseOutcome::Conflict {
                heads: vec![left.clone(), right.clone()],
                reason: "agent_hub_no_common_ancestor_unequal_payload".into(),
            });
        }

        if bases.len() == 1 {
            let base_id = bases.into_iter().next().unwrap();
            let base_rev = self.require_revision(&base_id).await?;
            return Ok(MergeBaseOutcome::Base {
                revision_id: base_id,
                virtual_base: false,
                payload: MergePayload::from_revision(&base_rev),
            });
        }

        // 多 base：字典序后从左到右递归 content merge
        let mut ordered = bases;
        ordered.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut acc_payload =
            MergePayload::from_revision(&self.require_revision(&ordered[0]).await?);
        let mut acc_id = ordered[0].clone();
        for next_id in ordered.iter().skip(1) {
            let next_payload = MergePayload::from_revision(&self.require_revision(next_id).await?);
            match content_merger(acc_payload.clone(), next_payload) {
                ContentMergeResult::Merged(p) => {
                    acc_payload = p;
                    // virtual id = "virtual:" + 参与 id 拼接（不落库，仅内存）
                    acc_id = RevisionId::from(format!(
                        "virtual:{}+{}",
                        acc_id.as_str(),
                        next_id.as_str()
                    ));
                }
                ContentMergeResult::Conflict => {
                    return Ok(MergeBaseOutcome::Conflict {
                        heads: ordered,
                        reason: "agent_hub_virtual_base_content_conflict".into(),
                    });
                }
            }
        }
        Ok(MergeBaseOutcome::Base {
            revision_id: acc_id,
            virtual_base: true,
            payload: acc_payload,
        })
    }

    /// 判断 `maybe_ancestor` 是否为 `node` 的祖先（含相等）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     maximal 过滤需要祖先判定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     BFS 向上，visited 封顶。
    pub async fn is_ancestor(
        &self,
        maybe_ancestor: &RevisionId,
        node: &RevisionId,
    ) -> Result<bool, AppError> {
        if maybe_ancestor == node {
            return Ok(true);
        }
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<RevisionId> = VecDeque::new();
        queue.push_back(node.clone());
        while let Some(cur) = queue.pop_front() {
            if !visited.insert(cur.0.clone()) {
                continue;
            }
            if visited.len() > MAX_VISITED_REVISIONS {
                return Err(AppError::validation(
                    "agent_hub_revision_graph_visited_limit",
                ));
            }
            if &cur == maybe_ancestor {
                return Ok(true);
            }
            let rev = match self.repo.get_revision(&cur).await? {
                Some(r) => r,
                None => continue,
            };
            for p in rev.parents {
                if !visited.contains(p.as_str()) {
                    queue.push_back(p);
                }
            }
        }
        Ok(false)
    }

    /// 收集 node 的祖先闭包（含自身）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     common ancestors = 两闭包交集。
    ///
    /// Code Logic（这个函数做什么）:
    ///     BFS parents；batched 通过连续 get_revision；visited 封顶。
    async fn ancestor_closure(&self, node: &RevisionId) -> Result<HashSet<RevisionId>, AppError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut out: HashSet<RevisionId> = HashSet::new();
        let mut queue: VecDeque<RevisionId> = VecDeque::new();
        queue.push_back(node.clone());
        // 小缓存减少重复 get
        let mut cache: HashMap<String, Revision> = HashMap::new();

        while let Some(cur) = queue.pop_front() {
            if !visited.insert(cur.0.clone()) {
                continue;
            }
            if visited.len() > MAX_VISITED_REVISIONS {
                return Err(AppError::validation(
                    "agent_hub_revision_graph_visited_limit",
                ));
            }
            out.insert(cur.clone());
            let rev = if let Some(r) = cache.remove(cur.as_str()) {
                r
            } else {
                match self.repo.get_revision(&cur).await? {
                    Some(r) => r,
                    None => continue,
                }
            };
            for p in rev.parents {
                if !visited.contains(p.as_str()) {
                    queue.push_back(p);
                }
            }
        }
        Ok(out)
    }

    /// 读取 revision，缺失则 NotFound。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     merge 路径不能对缺失 head 静默成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     get_revision 映射 None → not_found。
    async fn require_revision(&self, id: &RevisionId) -> Result<Revision, AppError> {
        self.repo
            .get_revision(id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("agent_hub_revision_not_found:{}", id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use sqlx::sqlite::SqlitePoolOptions;

    /// 内存库 + schema。
    ///
    /// Business Logic: 隔离真实磁盘。
    /// Code Logic: sqlite :memory: + ensure_schema。
    async fn test_repo() -> AgentHubRepo {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    /// 插入 scope+asset，返回 asset_id。
    ///
    /// Business Logic: revision 必须挂 lineage。
    /// Code Logic: insert_scope + insert_asset。
    async fn seed_asset(repo: &AgentHubRepo) -> String {
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-test".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "rules".into(),
                display_name: "rules".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        asset.id
    }

    /// 追加 revision helper。
    ///
    /// Business Logic: 测试图构造。
    /// Code Logic: NewRevision + append_revision。
    async fn add_rev(
        repo: &AgentHubRepo,
        lineage: &str,
        id: &str,
        parents: Vec<&str>,
        payload: Option<&str>,
    ) -> Revision {
        repo.append_revision(NewRevision {
            id: RevisionId::from(id),
            asset_lineage_id: lineage.to_string(),
            parents: parents.into_iter().map(RevisionId::from).collect(),
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: Some(AgentTarget::Claude),
            origin_replica_id: "dev-a".into(),
            payload_hash: payload.map(|s| s.to_string()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T00:00:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn maximal_common_ancestors_simple_diamond() {
        //      a1──a2──left
        //       └──b2──right
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "a1", vec![], Some("h1")).await;
        add_rev(&repo, &asset, "a2", vec!["a1"], Some("h2")).await;
        add_rev(&repo, &asset, "b2", vec!["a1"], Some("h3")).await;
        add_rev(&repo, &asset, "left", vec!["a2"], Some("hl")).await;
        add_rev(&repo, &asset, "right", vec!["b2"], Some("hr")).await;

        let graph = RevisionGraph::new(repo);
        let mca = graph
            .maximal_common_ancestors(&RevisionId::from("left"), &RevisionId::from("right"))
            .await
            .unwrap();
        assert_eq!(mca, vec![RevisionId::from("a1")]);
    }

    #[tokio::test]
    async fn maximal_common_ancestors_criss_cross_two_maximals() {
        // Criss-cross:
        //   a──b──d──left
        //   │  └──e──right
        //   └──c──e
        //        └──d
        // 共同祖先含 a,b,c；maximal 为 b 与 c（a 是二者祖先）
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "a", vec![], Some("ha")).await;
        add_rev(&repo, &asset, "b", vec!["a"], Some("hb")).await;
        add_rev(&repo, &asset, "c", vec!["a"], Some("hc")).await;
        add_rev(&repo, &asset, "d", vec!["b", "c"], Some("hd")).await;
        add_rev(&repo, &asset, "e", vec!["b", "c"], Some("he")).await;
        add_rev(&repo, &asset, "left", vec!["d"], Some("hl")).await;
        add_rev(&repo, &asset, "right", vec!["e"], Some("hr")).await;

        let graph = RevisionGraph::new(repo);
        let mca = graph
            .maximal_common_ancestors(&RevisionId::from("left"), &RevisionId::from("right"))
            .await
            .unwrap();
        assert_eq!(
            mca,
            vec![RevisionId::from("b"), RevisionId::from("c")],
            "expected maximal {{b,c}}, got {mca:?}"
        );
    }

    #[tokio::test]
    async fn merge_base_single_ancestor() {
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "a1", vec![], Some("h1")).await;
        add_rev(&repo, &asset, "left", vec!["a1"], Some("hl")).await;
        add_rev(&repo, &asset, "right", vec!["a1"], Some("hr")).await;
        let graph = RevisionGraph::new(repo);
        let merger = |_l: MergePayload, _r: MergePayload| ContentMergeResult::Conflict;
        let outcome = graph
            .merge_base(
                &RevisionId::from("left"),
                &RevisionId::from("right"),
                &merger,
            )
            .await
            .unwrap();
        match outcome {
            MergeBaseOutcome::Base {
                revision_id,
                virtual_base,
                payload,
            } => {
                assert_eq!(revision_id, RevisionId::from("a1"));
                assert!(!virtual_base);
                assert_eq!(payload.payload_hash.as_deref(), Some("h1"));
            }
            other => panic!("expected Base, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn merge_base_criss_cross_invokes_merger_in_revision_id_order() {
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "a", vec![], Some("ha")).await;
        add_rev(&repo, &asset, "b", vec!["a"], Some("hb")).await;
        add_rev(&repo, &asset, "c", vec!["a"], Some("hc")).await;
        add_rev(&repo, &asset, "d", vec!["b", "c"], Some("hd")).await;
        add_rev(&repo, &asset, "e", vec!["b", "c"], Some("he")).await;
        add_rev(&repo, &asset, "left", vec!["d"], Some("hl")).await;
        add_rev(&repo, &asset, "right", vec!["e"], Some("hr")).await;

        let graph = RevisionGraph::new(repo);
        let call_order = std::sync::Mutex::new(Vec::<(String, String)>::new());
        let outcome = {
            let call_order = &call_order;
            let merger = move |l: MergePayload, r: MergePayload| {
                call_order.lock().unwrap().push((
                    l.payload_hash.clone().unwrap_or_default(),
                    r.payload_hash.clone().unwrap_or_default(),
                ));
                ContentMergeResult::Merged(MergePayload {
                    payload_hash: Some(format!(
                        "merged:{}+{}",
                        l.payload_hash.unwrap_or_default(),
                        r.payload_hash.unwrap_or_default()
                    )),
                    tree_manifest_hash: None,
                    is_delete: false,
                })
            };
            graph
                .merge_base(
                    &RevisionId::from("left"),
                    &RevisionId::from("right"),
                    &merger,
                )
                .await
                .unwrap()
        };

        let order = call_order.into_inner().unwrap();
        // b before c lexicographically → merge(hb, hc)
        assert_eq!(order, vec![("hb".into(), "hc".into())]);
        match outcome {
            MergeBaseOutcome::Base {
                virtual_base,
                payload,
                revision_id,
            } => {
                assert!(virtual_base);
                assert_eq!(payload.payload_hash.as_deref(), Some("merged:hb+hc"));
                assert!(revision_id.as_str().starts_with("virtual:"));
            }
            other => panic!("expected virtual Base, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn merge_base_virtual_conflict_returns_conflict_without_writing() {
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "a", vec![], Some("ha")).await;
        add_rev(&repo, &asset, "b", vec!["a"], Some("hb")).await;
        add_rev(&repo, &asset, "c", vec!["a"], Some("hc")).await;
        add_rev(&repo, &asset, "d", vec!["b", "c"], Some("hd")).await;
        add_rev(&repo, &asset, "e", vec!["b", "c"], Some("he")).await;
        add_rev(&repo, &asset, "left", vec!["d"], Some("hl")).await;
        add_rev(&repo, &asset, "right", vec!["e"], Some("hr")).await;
        let graph = RevisionGraph::new(repo);
        let merger = |_l: MergePayload, _r: MergePayload| ContentMergeResult::Conflict;
        let outcome = graph
            .merge_base(
                &RevisionId::from("left"),
                &RevisionId::from("right"),
                &merger,
            )
            .await
            .unwrap();
        match outcome {
            MergeBaseOutcome::Conflict { reason, heads } => {
                assert!(reason.contains("virtual_base"));
                assert!(heads.iter().any(|h| h.as_str() == "b"));
                assert!(heads.iter().any(|h| h.as_str() == "c"));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // 不应写入 virtual revision
        assert!(graph
            .repo
            .get_revision(&RevisionId::from("virtual:b+c"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn merge_base_no_common_ancestor_unequal_payload_is_conflict() {
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        // 两个无 parent 的独立 root
        add_rev(&repo, &asset, "root-a", vec![], Some("hash-a")).await;
        add_rev(&repo, &asset, "root-b", vec![], Some("hash-b")).await;
        let graph = RevisionGraph::new(repo);
        let merger = |_l: MergePayload, _r: MergePayload| ContentMergeResult::Conflict;
        let outcome = graph
            .merge_base(
                &RevisionId::from("root-a"),
                &RevisionId::from("root-b"),
                &merger,
            )
            .await
            .unwrap();
        match outcome {
            MergeBaseOutcome::Conflict { reason, .. } => {
                assert!(reason.contains("no_common_ancestor"));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn merge_base_no_common_ancestor_equal_payload_collapses() {
        let repo = test_repo().await;
        let asset = seed_asset(&repo).await;
        add_rev(&repo, &asset, "root-a", vec![], Some("same-hash")).await;
        add_rev(&repo, &asset, "root-b", vec![], Some("same-hash")).await;
        let graph = RevisionGraph::new(repo);
        let merger = |_l: MergePayload, _r: MergePayload| ContentMergeResult::Conflict;
        let outcome = graph
            .merge_base(
                &RevisionId::from("root-a"),
                &RevisionId::from("root-b"),
                &merger,
            )
            .await
            .unwrap();
        match outcome {
            MergeBaseOutcome::Base {
                virtual_base,
                payload,
                ..
            } => {
                assert!(virtual_base);
                assert_eq!(payload.payload_hash.as_deref(), Some("same-hash"));
            }
            other => panic!("expected collapse Base, got {other:?}"),
        }
    }
}
