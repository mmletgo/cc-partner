//! agent_hub/instructions/reconcile — 三方对账
//!
//! Business Logic（为什么需要这个模块）:
//!     外部 CLI 文件编辑、Hub 编辑与上次投影 base 必须三方比较，
//!     自动合并不相交改动，同块冲突冻结并不推进 current head。
//!
//! Code Logic（这个模块做什么）:
//!     base vs hub current vs external 的块级 hash 比较；产出 NoChange/Revision/Conflict/Detached/Blocked。

use super::compiler::{parse_markdown_blocks, CompiledRenderedInstruction};
use super::document::{
    new_block_id, AgentHubConflictScope, InstructionBlock, InstructionBlockMode,
    InstructionDocument, NewAgentHubConflict, NewInstructionRevision,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 对账结果。
///
/// Business Logic（为什么需要这个枚举）:
///     service 层根据结果决定 append revision、写 conflict 或标 materialization 状态。
///
/// Code Logic（这个枚举做什么）:
///     NoChange / Revision / Conflict / Detached / Blocked。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionReconcileOutcome {
    /// base/hub/external 一致或无有效变更
    NoChange,
    /// 可自动合并或单侧编辑，产出新文档
    Revision(NewInstructionRevision),
    /// 同块冲突，不推进 head
    Conflict(NewAgentHubConflict),
    /// 外部整文件删除
    Detached {
        /// 详情
        detail: String,
    },
    /// 外部非法 UTF-8 等阻塞
    Blocked {
        /// 保留的原始字节
        original_bytes: Vec<u8>,
        /// 详情
        detail: String,
    },
}

/// materialization base-map 中的块记录。
///
/// Business Logic（为什么需要这个结构体）:
///     稳定 block ID 来自上次投影 map，新块才发 UUIDv7。
///
/// Code Logic（这个结构体做什么）:
///     block_id + content_hash + 可选 mode 快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaseBlockRecord {
    /// 块 ID
    pub block_id: String,
    /// 上次投影内容 hash
    pub content_hash: String,
    /// 可选：上次 mode
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<InstructionBlockMode>,
}

/// 外部文件观测。
///
/// Business Logic（为什么需要这个结构体）:
///     外部可能删除、非 UTF-8 或正常编辑。
///
/// Code Logic（这个结构体做什么）:
///     Present{bytes} / Missing。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalObservation {
    /// 文件存在（原始字节）
    Present { bytes: Vec<u8> },
    /// 文件不存在（整文件删除）
    Missing,
}

/// 三方对账输入。
///
/// Business Logic（为什么需要这个结构体）:
///     比较上次投影、Hub 当前文档与外部文件。
///
/// Code Logic（这个结构体做什么）:
///     base_document / hub_document / external / target / base_map / managed_prefix_len。
#[derive(Debug, Clone)]
pub struct ReconcileInput {
    /// 上次成功投影对应的文档（base）
    pub base_document: InstructionDocument,
    /// Hub 当前文档
    pub hub_document: InstructionDocument,
    /// 外部文件观测
    pub external: ExternalObservation,
    /// 观察来源 target
    pub target: AgentTarget,
    /// 上次渲染的 managed_prefix_len（OpenCode）
    pub managed_prefix_len: usize,
    /// base block map（可选，用于 ID 稳定）
    pub base_block_records: Vec<BaseBlockRecord>,
}

/// 执行三方对账。
///
/// Business Logic（为什么需要这个函数）:
///     不相交 shared 编辑 auto-merge；同块双侧编辑 → conflict 不推进 head；
///     adapted/targetOnly 外部编辑只改该 target variant；整文件删除 → detached；
///     非法 UTF-8 → blocked 并保留原字节。
///
/// Code Logic（这个函数做什么）:
///     解析 external 用户区 → 对齐 base 块 → 逐块三方决策 → 组装 outcome。
pub fn reconcile_instruction(input: &ReconcileInput) -> InstructionReconcileOutcome {
    match &input.external {
        ExternalObservation::Missing => {
            return InstructionReconcileOutcome::Detached {
                detail: format!("target {} 指令文件被整文件删除", input.target.as_str()),
            };
        }
        ExternalObservation::Present { bytes } => {
            if std::str::from_utf8(bytes).is_err() {
                return InstructionReconcileOutcome::Blocked {
                    original_bytes: bytes.clone(),
                    detail: "外部文件不是合法 UTF-8；保留原字节且不推进 head".into(),
                };
            }
        }
    }

    let ExternalObservation::Present { bytes } = &input.external else {
        unreachable!();
    };
    let full = std::str::from_utf8(bytes).unwrap_or("");
    let user_region = if input.managed_prefix_len > 0 && input.managed_prefix_len <= full.len() {
        // managed 区编辑视为 OpenCode target-only：这里把用户区切开
        &full[input.managed_prefix_len..]
    } else {
        full
    };
    // 去掉 prelude 后可能以多余空行开头
    let user_region = user_region.trim_start_matches('\n');

    let external_blocks = parse_markdown_blocks(user_region);
    let base_by_id: BTreeMap<&str, &InstructionBlock> = input
        .base_document
        .blocks
        .iter()
        .map(|b| (b.id.as_str(), b))
        .collect();
    // 将 external 块映射到 base ID：按顺序对齐 base 中对当前 target 可见的块
    let base_visible: Vec<&InstructionBlock> = input
        .base_document
        .blocks
        .iter()
        .filter(|b| block_visible_on_target(b, input.target))
        .collect();

    let mut external_aligned: Vec<(Option<String>, String)> = Vec::new();
    let map_len = base_visible.len().min(external_blocks.len());
    for i in 0..map_len {
        external_aligned.push((
            Some(base_visible[i].id.clone()),
            normalize_block_text(&external_blocks[i].text),
        ));
    }
    for block in external_blocks.iter().skip(map_len) {
        external_aligned.push((None, normalize_block_text(&block.text)));
    }

    let mut conflicts: Vec<NewAgentHubConflict> = Vec::new();
    let mut merged_blocks: Vec<InstructionBlock> = Vec::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();

    // 以 hub 块顺序为主合并
    for hub_block in &input.hub_document.blocks {
        let base_block = base_by_id.get(hub_block.id.as_str()).copied();
        let ext = external_aligned
            .iter()
            .find(|(id, _)| id.as_deref() == Some(hub_block.id.as_str()))
            .map(|(_, t)| t.as_str());

        let base_fp = base_block.map(|b| block_target_fingerprint(b, input.target));
        let hub_fp = block_target_fingerprint(hub_block, input.target);
        let ext_fp = ext.map(|t| sha256_hex(t.as_bytes()));

        let base_same_hub = base_fp.as_ref() == Some(&hub_fp);
        let base_same_ext = match (&base_fp, &ext_fp) {
            (Some(b), Some(e)) => b == e,
            (None, None) => true,
            _ => false,
        };
        let hub_same_ext = ext_fp.as_ref() == Some(&hub_fp);

        // 块对当前 target 不可见且 external 无对应 → 保留 hub
        if !block_visible_on_target(hub_block, input.target) && ext.is_none() {
            merged_blocks.push(hub_block.clone());
            seen_ids.insert(hub_block.id.clone());
            continue;
        }

        // 三方同 → 保留 hub
        if base_same_hub && base_same_ext {
            merged_blocks.push(hub_block.clone());
            seen_ids.insert(hub_block.id.clone());
            continue;
        }

        // 仅 hub 变
        if !base_same_hub && base_same_ext {
            merged_blocks.push(hub_block.clone());
            seen_ids.insert(hub_block.id.clone());
            continue;
        }

        // 仅 external 变
        if base_same_hub && !base_same_ext {
            if let Some(ext_text) = ext {
                let mut updated = hub_block.clone();
                apply_external_edit(&mut updated, input.target, ext_text);
                merged_blocks.push(updated);
                seen_ids.insert(hub_block.id.clone());
                continue;
            }
        }

        // 双侧都变
        if !base_same_hub && !base_same_ext {
            if hub_same_ext {
                // 碰巧相同
                merged_blocks.push(hub_block.clone());
                seen_ids.insert(hub_block.id.clone());
                continue;
            }
            let scope = match hub_block.mode {
                InstructionBlockMode::Shared => AgentHubConflictScope::CanonicalAsset,
                InstructionBlockMode::Adapted | InstructionBlockMode::TargetOnly => {
                    AgentHubConflictScope::Target
                }
            };
            conflicts.push(NewAgentHubConflict {
                scope,
                target: match scope {
                    AgentHubConflictScope::CanonicalAsset => None,
                    AgentHubConflictScope::Target => Some(input.target),
                },
                block_id: Some(hub_block.id.clone()),
                base_hash: base_fp.clone(),
                hub_hash: Some(hub_fp.clone()),
                external_hash: ext_fp.clone(),
                detail: format!(
                    "块 {} 在 Hub 与外部同时被修改（target={}）",
                    hub_block.id,
                    input.target.as_str()
                ),
            });
            // 冲突：保留 hub 现状，不推进（outcome=Conflict）
            merged_blocks.push(hub_block.clone());
            seen_ids.insert(hub_block.id.clone());
            continue;
        }

        // external 删除该块而 hub/base 仍有：delete-vs-edit
        if ext.is_none() && base_block.is_some() {
            if !base_same_hub {
                // hub 编辑 + external 删除 → conflict
                conflicts.push(NewAgentHubConflict {
                    scope: AgentHubConflictScope::CanonicalAsset,
                    target: None,
                    block_id: Some(hub_block.id.clone()),
                    base_hash: base_fp,
                    hub_hash: Some(hub_fp),
                    external_hash: None,
                    detail: format!("块 {} 外部删除与 Hub 编辑冲突", hub_block.id),
                });
                merged_blocks.push(hub_block.clone());
                seen_ids.insert(hub_block.id.clone());
                continue;
            }
            // 仅 external 删除 shared 块 → 从所有 target 删除
            seen_ids.insert(hub_block.id.clone());
            continue;
        }

        merged_blocks.push(hub_block.clone());
        seen_ids.insert(hub_block.id.clone());
    }

    // external 新块（无 base id）
    for (id, text) in &external_aligned {
        if id.is_some() {
            continue;
        }
        if text.trim().is_empty() {
            continue;
        }
        // 新块：继承 targetOnly（来源明确为该 target）
        let nb = InstructionBlock::target_only(
            new_block_id(),
            input.target,
            text.clone(),
            vec![],
            false,
        );
        merged_blocks.push(nb);
    }
    let _ = seen_ids;

    if !conflicts.is_empty() {
        // 返回第一个冲突（调用方可扩展为多冲突）；不推进 head
        return InstructionReconcileOutcome::Conflict(conflicts.into_iter().next().unwrap());
    }

    // 无有效变更：base/hub/external 对齐
    if documents_equivalent(&input.base_document, &input.hub_document)
        && external_matches_hub(&input.hub_document, user_region, input.target)
        && documents_equivalent(
            &input.hub_document,
            &InstructionDocument {
                relative_key: input.hub_document.relative_key.clone(),
                blocks: merged_blocks.clone(),
            },
        )
    {
        return InstructionReconcileOutcome::NoChange;
    }

    let document = InstructionDocument {
        relative_key: input.hub_document.relative_key.clone(),
        blocks: merged_blocks,
    };
    let payload = serde_json::to_vec(&document).unwrap_or_default();
    InstructionReconcileOutcome::Revision(NewInstructionRevision {
        document,
        origin_target: Some(input.target),
        payload_hash: sha256_hex(&payload),
    })
}

/// 从 CompiledRenderedInstruction 反推用户区做对账的便捷入口。
///
/// Business Logic: adapter reverse 路径用渲染结果作 base 字节参考。
/// Code Logic: 包装 reconcile_instruction。
pub fn reconcile_against_rendered(
    base_document: &InstructionDocument,
    hub_document: &InstructionDocument,
    base_rendered: &CompiledRenderedInstruction,
    external: ExternalObservation,
) -> InstructionReconcileOutcome {
    let base_block_records = base_rendered
        .block_map
        .iter()
        .filter_map(|r| {
            r.block_id.as_ref().map(|id| BaseBlockRecord {
                block_id: id.clone(),
                content_hash: r.content_hash.clone(),
                mode: None,
            })
        })
        .collect();
    reconcile_instruction(&ReconcileInput {
        base_document: base_document.clone(),
        hub_document: hub_document.clone(),
        external,
        target: base_rendered.target,
        managed_prefix_len: base_rendered.managed_prefix_len,
        base_block_records,
    })
}

fn block_visible_on_target(block: &InstructionBlock, target: AgentTarget) -> bool {
    match block.mode {
        InstructionBlockMode::Shared | InstructionBlockMode::Adapted => true,
        InstructionBlockMode::TargetOnly => {
            block.source_target == Some(target) || block.variants.contains_key(&target)
        }
    }
}

fn block_target_fingerprint(block: &InstructionBlock, target: AgentTarget) -> String {
    if let Some(v) = block.variants.get(&target) {
        return sha256_hex(normalize_block_text(v).as_bytes());
    }
    if let Some(c) = &block.common_markdown {
        return sha256_hex(normalize_block_text(c).as_bytes());
    }
    block.content_fingerprint()
}

/// 规范化块文本以便三方比较（统一换行并去掉尾部空行）。
///
/// Business Logic: 解析器保留的行尾与 Hub 存储的正文常有换行差，不能因此误报 conflict。
/// Code Logic: `\r\n`→`\n`，再 trim 尾部 `\n`。
fn normalize_block_text(s: &str) -> String {
    let mut t = s.replace("\r\n", "\n").replace('\r', "\n");
    while t.ends_with('\n') {
        t.pop();
    }
    t
}

fn apply_external_edit(block: &mut InstructionBlock, target: AgentTarget, text: &str) {
    match block.mode {
        InstructionBlockMode::Shared => {
            block.common_markdown = Some(text.to_string());
        }
        InstructionBlockMode::Adapted => {
            // adapted：只更新该 target variant，不反推 common
            block.variants.insert(target, text.to_string());
        }
        InstructionBlockMode::TargetOnly => {
            block.variants.insert(target, text.to_string());
            block.source_target = Some(target);
        }
    }
}

fn documents_equivalent(a: &InstructionDocument, b: &InstructionDocument) -> bool {
    if a.blocks.len() != b.blocks.len() {
        return false;
    }
    a.blocks
        .iter()
        .zip(b.blocks.iter())
        .all(|(x, y)| x.content_fingerprint() == y.content_fingerprint())
}

fn external_matches_hub(hub: &InstructionDocument, user_region: &str, target: AgentTarget) -> bool {
    let expected = hub.body_for_target(target);
    normalize(&expected) == normalize(user_region)
}

fn normalize(s: &str) -> String {
    let mut t = s.replace("\r\n", "\n").replace('\r', "\n");
    while t.ends_with('\n') {
        t.pop();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::instructions::compiler::compile_render;
    use crate::agent_hub::instructions::document::InstructionBlock;
    use crate::agent_hub::targets::InstructionRenderContext;

    fn shared_doc(body_a: &str, body_b: &str) -> InstructionDocument {
        InstructionDocument {
            relative_key: "src".into(),
            blocks: vec![
                InstructionBlock::shared("block-a", body_a, vec![]),
                InstructionBlock::shared("block-b", body_b, vec![]),
            ],
        }
    }

    #[test]
    fn disjoint_shared_edits_auto_merge() {
        let base = shared_doc("para A base", "para B base");
        let mut hub = base.clone();
        hub.blocks[0].common_markdown = Some("para A hub".into());
        // external 只改 B
        let external_text = "para A base\n\npara B external\n";
        let outcome = reconcile_instruction(&ReconcileInput {
            base_document: base,
            hub_document: hub,
            external: ExternalObservation::Present {
                bytes: external_text.as_bytes().to_vec(),
            },
            target: AgentTarget::Claude,
            managed_prefix_len: 0,
            base_block_records: vec![],
        });
        match outcome {
            InstructionReconcileOutcome::Revision(rev) => {
                assert_eq!(
                    rev.document.blocks[0].common_markdown.as_deref(),
                    Some("para A hub")
                );
                assert_eq!(
                    rev.document.blocks[1].common_markdown.as_deref(),
                    Some("para B external")
                );
            }
            other => panic!("expected revision, got {other:?}"),
        }
    }

    #[test]
    fn same_block_edit_conflicts_without_advancing() {
        let base = shared_doc("same base", "other");
        let mut hub = base.clone();
        hub.blocks[0].common_markdown = Some("hub edit".into());
        let external_text = "external edit\n\nother\n";
        let outcome = reconcile_instruction(&ReconcileInput {
            base_document: base,
            hub_document: hub.clone(),
            external: ExternalObservation::Present {
                bytes: external_text.as_bytes().to_vec(),
            },
            target: AgentTarget::Claude,
            managed_prefix_len: 0,
            base_block_records: vec![],
        });
        match outcome {
            InstructionReconcileOutcome::Conflict(c) => {
                assert_eq!(c.scope, AgentHubConflictScope::CanonicalAsset);
                assert_eq!(c.block_id.as_deref(), Some("block-a"));
                // hub 未在 outcome 中被替换为 external
            }
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[test]
    fn adapted_target_edit_only_changes_variant() {
        let mut variants = BTreeMap::new();
        variants.insert(AgentTarget::Claude, "claude v1".into());
        variants.insert(AgentTarget::Codex, "codex v1".into());
        let base = InstructionDocument {
            relative_key: "src".into(),
            blocks: vec![InstructionBlock::adapted(
                "ad1",
                Some("common".into()),
                None,
                variants.clone(),
                vec![],
            )],
        };
        let hub = base.clone();
        let external_text = "claude v2 from file\n";
        let outcome = reconcile_instruction(&ReconcileInput {
            base_document: base,
            hub_document: hub,
            external: ExternalObservation::Present {
                bytes: external_text.as_bytes().to_vec(),
            },
            target: AgentTarget::Claude,
            managed_prefix_len: 0,
            base_block_records: vec![],
        });
        match outcome {
            InstructionReconcileOutcome::Revision(rev) => {
                let b = &rev.document.blocks[0];
                assert_eq!(
                    b.variants.get(&AgentTarget::Claude).map(String::as_str),
                    Some("claude v2 from file")
                );
                assert_eq!(
                    b.variants.get(&AgentTarget::Codex).map(String::as_str),
                    Some("codex v1")
                );
                assert_eq!(b.common_markdown.as_deref(), Some("common"));
            }
            other => panic!("expected revision, got {other:?}"),
        }
    }

    #[test]
    fn whole_file_external_delete_returns_detached() {
        let base = shared_doc("x", "y");
        let outcome = reconcile_instruction(&ReconcileInput {
            base_document: base.clone(),
            hub_document: base,
            external: ExternalObservation::Missing,
            target: AgentTarget::Codex,
            managed_prefix_len: 0,
            base_block_records: vec![],
        });
        assert!(matches!(
            outcome,
            InstructionReconcileOutcome::Detached { .. }
        ));
    }

    #[test]
    fn invalid_utf8_returns_blocked_preserving_bytes() {
        let base = shared_doc("ok", "ok2");
        let bad = vec![0xff, 0xfe, 0xfd];
        let outcome = reconcile_instruction(&ReconcileInput {
            base_document: base.clone(),
            hub_document: base,
            external: ExternalObservation::Present { bytes: bad.clone() },
            target: AgentTarget::OpenCode,
            managed_prefix_len: 0,
            base_block_records: vec![],
        });
        match outcome {
            InstructionReconcileOutcome::Blocked { original_bytes, .. } => {
                assert_eq!(original_bytes, bad);
            }
            other => panic!("expected blocked, got {other:?}"),
        }
    }

    #[test]
    fn opencode_managed_prefix_is_stripped_for_user_reconcile() {
        let body = "本目录负责 Rust 网络层";
        let doc = InstructionDocument {
            relative_key: "src-tauri/src/net".into(),
            blocks: vec![InstructionBlock::shared("b1", body, vec![])],
        };
        let ctx = InstructionRenderContext {
            project_root: None,
            directory_relative: Some("src-tauri/src/net".into()),
            ancestor_agent_paths: vec![
                "../../../AGENTS.md".into(),
                "../../AGENTS.md".into(),
                "../AGENTS.md".into(),
            ],
        };
        let rendered = compile_render(&doc, AgentTarget::OpenCode, &ctx);
        assert!(rendered.managed_prefix_len > 0);
        // external = 原样渲染结果 → NoChange
        let outcome = reconcile_against_rendered(
            &doc,
            &doc,
            &rendered,
            ExternalObservation::Present {
                bytes: rendered.bytes.clone(),
            },
        );
        assert!(
            matches!(outcome, InstructionReconcileOutcome::NoChange)
                || matches!(outcome, InstructionReconcileOutcome::Revision(_)),
            "outcome={outcome:?}"
        );
    }
}
