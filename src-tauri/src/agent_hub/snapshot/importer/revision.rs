//! agent_hub/snapshot/importer/revision — revision 闭包校验与拓扑/MCA 内存图辅助
//!
//! Business Logic（为什么需要这个模块）:
//!     import 写库前必须证明 incoming envelope 的 revision DAG 完整（parents 都在闭包内、
//!     heads 不悬空），并在本地库与 envelope 合并后的内存图上做祖先/MCA 判定，
//!     才能安全收敛分叉 heads，禁止 last-write-wins。
//!
//! Code Logic（这个模块做什么）:
//!     提供 revision 闭包校验、引用对象 hash 收集、Kahn 拓扑排序、
//!     envelope+本地库 BFS 建 parent index、内存祖先判定与 maximal common ancestors
//!     计算；仅 importer 父模块（及其测试）可访问。

use crate::agent_hub::models::RevisionId;
use crate::agent_hub::snapshot::envelope::{SnapshotEnvelopeV1, SnapshotRevision};
use crate::error::AppError;
use crate::storage::agent_hub_repo::AgentHubRepo;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

pub(super) fn validate_revision_closure(env: &SnapshotEnvelopeV1) -> Result<(), AppError> {
    let ids: HashSet<&str> = env.revisions.iter().map(|r| r.id.as_str()).collect();
    for r in &env.revisions {
        for p in &r.parents {
            if !ids.contains(p.as_str()) {
                return Err(AppError::validation(format!(
                    "agent_hub_import_parent_missing:{p}"
                )));
            }
        }
    }
    for heads in env.asset_heads.values() {
        for h in heads {
            if !ids.contains(h.as_str()) {
                return Err(AppError::validation(format!(
                    "agent_hub_import_head_not_in_closure:{h}"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn collect_referenced_object_hashes(env: &SnapshotEnvelopeV1) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for r in &env.revisions {
        if let Some(h) = &r.payload_hash {
            set.insert(h.clone());
        }
        if let Some(h) = &r.tree_manifest_hash {
            set.insert(h.clone());
        }
    }
    for o in &env.objects {
        // objects 列表是协商集合；严格引用仍以 revision 为准
        let _ = o;
    }
    set
}

pub(super) fn topological_revisions(
    revs: &[SnapshotRevision],
) -> Result<Vec<&SnapshotRevision>, AppError> {
    let mut by_id: HashMap<&str, &SnapshotRevision> = HashMap::new();
    for r in revs {
        by_id.insert(r.id.as_str(), r);
    }
    let mut indeg: HashMap<&str, usize> = HashMap::new();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for r in revs {
        indeg.entry(r.id.as_str()).or_insert(0);
        for p in &r.parents {
            *indeg.entry(r.id.as_str()).or_insert(0) += 1;
            children.entry(p.as_str()).or_default().push(r.id.as_str());
        }
    }
    let mut q: VecDeque<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut out = Vec::new();
    while let Some(id) = q.pop_front() {
        if let Some(r) = by_id.get(id) {
            out.push(*r);
        }
        if let Some(chs) = children.get(id) {
            for c in chs {
                if let Some(d) = indeg.get_mut(c) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        q.push_back(c);
                    }
                }
            }
        }
    }
    if out.len() != revs.len() {
        return Err(AppError::validation(
            "agent_hub_import_revision_cycle".to_string(),
        ));
    }
    Ok(out)
}

pub(super) async fn build_parent_index(
    env: &SnapshotEnvelopeV1,
    repo: &AgentHubRepo,
    seed_ids: &[&str],
) -> Result<HashMap<String, Vec<String>>, AppError> {
    let mut idx: HashMap<String, Vec<String>> = HashMap::new();
    for r in &env.revisions {
        idx.insert(r.id.clone(), r.parents.clone());
    }
    // 从 envelope 节点 + 调用方 seed（local/remote heads）BFS 补本地 parents。
    // multi-replica 场景 local head 往往不在 *incoming* envelope，不 seed 则 MCA 为空。
    let mut frontier: VecDeque<String> = idx.keys().cloned().collect();
    for id in seed_ids {
        frontier.push_back((*id).to_string());
    }
    let mut visited = HashSet::new();
    while let Some(id) = frontier.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        if idx.contains_key(&id) {
            for p in idx.get(&id).cloned().unwrap_or_default() {
                frontier.push_back(p);
            }
            continue;
        }
        if let Some(r) = repo.get_revision(&RevisionId(id.clone())).await? {
            let parents: Vec<String> = r.parents.iter().map(|p| p.as_str().to_string()).collect();
            for p in &parents {
                frontier.push_back(p.clone());
            }
            idx.insert(id, parents);
        }
    }
    Ok(idx)
}

pub(super) fn is_ancestor_mem(
    parents: &HashMap<String, Vec<String>>,
    maybe_ancestor: &str,
    node: &str,
) -> bool {
    if maybe_ancestor == node {
        return true;
    }
    let mut q = VecDeque::new();
    let mut seen = HashSet::new();
    q.push_back(node.to_string());
    while let Some(cur) = q.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if cur == maybe_ancestor {
            return true;
        }
        if let Some(ps) = parents.get(&cur) {
            for p in ps {
                q.push_back(p.clone());
            }
        }
    }
    false
}

pub(super) fn maximal_common_ancestors_mem(
    parents: &HashMap<String, Vec<String>>,
    left: &str,
    right: &str,
) -> Vec<String> {
    let left_anc = ancestor_closure_mem(parents, left);
    let right_anc = ancestor_closure_mem(parents, right);
    let common: Vec<String> = left_anc.intersection(&right_anc).cloned().collect();
    let mut maximal = Vec::new();
    for cand in &common {
        let mut dominated = false;
        for other in &common {
            if cand == other {
                continue;
            }
            if is_ancestor_mem(parents, cand, other) {
                dominated = true;
                break;
            }
        }
        if !dominated {
            maximal.push(cand.clone());
        }
    }
    maximal.sort();
    maximal
}

fn ancestor_closure_mem(parents: &HashMap<String, Vec<String>>, node: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut q = VecDeque::new();
    q.push_back(node.to_string());
    while let Some(cur) = q.pop_front() {
        if !out.insert(cur.clone()) {
            continue;
        }
        if let Some(ps) = parents.get(&cur) {
            for p in ps {
                q.push_back(p.clone());
            }
        }
    }
    out
}
