//! agent_hub/user_instructions/slot_history — 三槽（公共 / 适配 / 独有）历史快照
//!
//! Business Logic（为什么需要这个模块）:
//!     AgentHub 提示词固定三槽（公共 `shared` / 适配 `adapted` / 独有 `targetOnly`），
//!     共享同一 canonical head；但用户希望每个槽独立保留历史版本，
//!     便于「公共回退到旧版本 / 适配只切 Claude 变体」等操作。
//!     本模块把每个逻辑槽映射到 `content_versions` 表的稳定 item_id，
//!     并提供纯函数 `extract_slot_text` / `replace_slot_text`，
//!     与 `commit_user_instruction_document` CAS 链路集成后即可复用 Prompts 历史的
//!     20 条 history / 30 天保留策略。
//!
//! Code Logic（这个模块做什么）:
//!     - `InstructionSlotKey` 三槽 + 三 agent = 7 闭集；
//!     - `to_item_id(asset_id, slot) -> String` 产出 `content_versions.item_id`；
//!     - `extract_slot_text(document, slot) -> String` 取出该槽当前正文；
//!     - `replace_slot_text(document, slot, new_text) -> InstructionDocument` 替换
//!       并重新 `normalize_to_three_slots`，不修改其它槽；
//!     - `snapshot_dirty_slot_versions(before, after, asset_id, source_device, repo)`
//!       把两次 document 之间被改动的槽写入 content_versions history
//!       （与 `snapshot_prompt_history` 同模式），空内容视为 no-op；
//!     - `replace_slot_text` 缺块时懒创建，确保替换后 `normalize_to_three_slots` 不丢槽。

use crate::agent_hub::instructions::{InstructionBlock, InstructionBlockMode, InstructionDocument};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use crate::storage::content_version_repo::{ContentVersion, ContentVersionRepo, KIND_HISTORY};
use crate::storage::sync_request_ledger_repo::DOMAIN_AGENT_HUB_USER_INSTRUCTION;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 三槽历史快照的逻辑槽位标识（闭集）。
///
/// Business Logic: 公共槽不分 agent（跨 agent 共享）；
///   适配/独有槽按 lane × agent 各 1 个 item_id，与 `InstructionBlockMode`
///   + `AgentTarget` 组合的 7 个稳定标识一一对应。
///
/// Code Logic: serde camelCase；wire 字符串与前端 `InstructionSlotKey`
///   字段直接对齐（`shared` / `adapted:claude` 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InstructionSlotKey {
    /// 公共槽（跨 agent 共享）。
    Shared,
    /// 适配槽（指定 agent 的 variant）。
    Adapted { agent: AgentTarget },
    /// 独有槽（指定 agent 的 variant）。
    TargetOnly { agent: AgentTarget },
}

impl InstructionSlotKey {
    /// 槽位稳定字符串（用于 content_versions.item_id 第三段）。
    fn segment(self) -> String {
        match self {
            Self::Shared => "shared".to_string(),
            Self::Adapted { agent } => format!("adapted:{}", agent.as_str()),
            Self::TargetOnly { agent } => format!("targetOnly:{}", agent.as_str()),
        }
    }

    /// 构造 content_versions.item_id：`{asset_id}:{segment}`。
    pub fn to_item_id(self, asset_id: &str) -> String {
        format!("{asset_id}:{}", self.segment())
    }
}

/// 取出指定槽位的当前正文（无内容视为空串）。
///
/// Business Logic: 三槽正文必须可空；空文本与「未设置」UI 一致。
/// Code Logic: shared 取 `block.common_markdown`；adapted/targetOnly 取
///   `block.variants[agent]`；adapted 在 variant 缺失时回落到 common（与
///   `resolveAdaptedSlotText` 同语义）。
pub fn extract_slot_text(document: &InstructionDocument, slot: InstructionSlotKey) -> String {
    match slot {
        InstructionSlotKey::Shared => document
            .blocks
            .iter()
            .find(|block| block.mode == InstructionBlockMode::Shared)
            .and_then(|block| block.common_markdown.clone())
            .unwrap_or_default(),
        InstructionSlotKey::Adapted { agent } => document
            .blocks
            .iter()
            .find(|block| block.mode == InstructionBlockMode::Adapted)
            .map(|block| {
                if let Some(text) = block.variants.get(&agent) {
                    text.clone()
                } else {
                    block.common_markdown.clone().unwrap_or_default()
                }
            })
            .unwrap_or_default(),
        InstructionSlotKey::TargetOnly { agent } => document
            .blocks
            .iter()
            .find(|block| block.mode == InstructionBlockMode::TargetOnly)
            .and_then(|block| block.variants.get(&agent).cloned())
            .unwrap_or_default(),
    }
}

/// 替换指定槽位的正文，返回新的 `InstructionDocument`。
///
/// Business Logic: 替换不应影响其它槽；缺块时懒创建该 mode 的空块再写入，
///   以保证 `normalize_to_three_slots` 不丢槽；adapted 写入 `variants[agent]`
///   而非 `common_markdown`（避免 shared 串味）。
///
/// Code Logic: 不可变复制；只在确有必要时新建/覆盖块；末尾 `normalize_to_three_slots`
///   保证下游投影与预览一致。
pub fn replace_slot_text(
    document: &InstructionDocument,
    slot: InstructionSlotKey,
    new_text: &str,
) -> InstructionDocument {
    let mut next = document.clone();
    match slot {
        InstructionSlotKey::Shared => {
            let mut found = false;
            for block in next.blocks.iter_mut() {
                if block.mode != InstructionBlockMode::Shared {
                    continue;
                }
                found = true;
                block.common_markdown = if new_text.is_empty() {
                    None
                } else {
                    Some(new_text.to_string())
                };
                block.variants.clear();
                break;
            }
            if !found {
                next.blocks.push(InstructionBlock::shared(
                    "slot-shared",
                    new_text.to_string(),
                    Vec::new(),
                ));
            }
        }
        InstructionSlotKey::Adapted { agent } => {
            let mut found = false;
            for block in next.blocks.iter_mut() {
                if block.mode != InstructionBlockMode::Adapted {
                    continue;
                }
                found = true;
                block.variants.insert(agent, new_text.to_string());
                // adapted 在 variant 命中时不需要 common（避免 shared 串味）。
                break;
            }
            if !found {
                let mut variants = BTreeMap::new();
                variants.insert(agent, new_text.to_string());
                next.blocks.push(InstructionBlock::adapted(
                    "slot-adapted",
                    None,
                    None,
                    variants,
                    Vec::new(),
                ));
            }
        }
        InstructionSlotKey::TargetOnly { agent } => {
            let mut found = false;
            for block in next.blocks.iter_mut() {
                if block.mode != InstructionBlockMode::TargetOnly {
                    continue;
                }
                found = true;
                block.variants.insert(agent, new_text.to_string());
                block.source_target.get_or_insert(agent);
                break;
            }
            if !found {
                let mut variants = BTreeMap::new();
                variants.insert(agent, new_text.to_string());
                next.blocks.push(InstructionBlock::target_only(
                    "slot-targetOnly",
                    agent,
                    new_text.to_string(),
                    Vec::new(),
                    false,
                ));
            }
        }
    }
    next.normalize_to_three_slots();
    next
}

/// 把 `(before, after)` 之间每个被改动的槽写入 `content_versions` history。
///
/// Business Logic: 与 `snapshot_prompt_history` / `snapshot_scratchpad_history`
///   同模式；同槽前后相等视为 no-op，避免 0 字节快照污染保留窗口；
///   同 `(domain, item_id, source_device, content_hash)` 由
///   `ContentVersionRepo::insert_idempotent` 去重，重放 no-op。
///
/// Code Logic: 纯同步函数，调用方负责 repo + lease；返回写入条数。
pub async fn snapshot_dirty_slot_versions(
    before: &InstructionDocument,
    after: &InstructionDocument,
    asset_id: &str,
    source_device: &str,
    repo: &ContentVersionRepo,
) -> Result<usize, AppError> {
    let mut written = 0usize;
    for slot in all_slot_keys() {
        let before_text = extract_slot_text(before, slot);
        let after_text = extract_slot_text(after, slot);
        if before_text == after_text {
            continue;
        }
        let item_id = slot.to_item_id(asset_id);
        if before_text.is_empty() {
            continue;
        }
        let content_hash = sha256_hex(before_text.as_bytes());
        let snapshot_json = serde_json::to_string(&SlotSnapshot {
            content: before_text,
        })
        .map_err(|e| AppError::generic(format!("slot_history_serialize_failed:{e}")))?;
        let now = Utc::now().to_rfc3339();
        let version = ContentVersion {
            id: ContentVersionRepo::deterministic_id(
                DOMAIN_AGENT_HUB_USER_INSTRUCTION,
                &item_id,
                source_device,
                &content_hash,
            ),
            domain: DOMAIN_AGENT_HUB_USER_INSTRUCTION.to_string(),
            item_id,
            source_device: source_device.to_string(),
            content_hash,
            created_at: now,
            kind: KIND_HISTORY.to_string(),
            snapshot_json,
        };
        let inserted = repo.insert_idempotent(&version).await?;
        if inserted {
            written += 1;
        }
    }
    Ok(written)
}

/// 所有逻辑槽位的稳定迭代器（公共 1 + 适配 3 + 独有 3 = 7）。
pub fn all_slot_keys() -> [InstructionSlotKey; 7] {
    use AgentTarget::*;
    [
        InstructionSlotKey::Shared,
        InstructionSlotKey::Adapted { agent: Claude },
        InstructionSlotKey::Adapted { agent: Codex },
        InstructionSlotKey::Adapted { agent: OpenCode },
        InstructionSlotKey::TargetOnly { agent: Claude },
        InstructionSlotKey::TargetOnly { agent: Codex },
        InstructionSlotKey::TargetOnly { agent: OpenCode },
    ]
}

/// history 行的 `snapshot_json` 形态。
///
/// Business Logic: 与 `PromptSnapshotFields` / `ScratchpadSnapshotFields`
///   一致语义：只存槽位正文即可恢复，无需完整 row 反序列化。
///
/// Code Logic: JSON serialize 为 `{"content":"<text>"}`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotSnapshot {
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::instructions::{InstructionBlock, InstructionDocument};

    fn doc_with_three_slots() -> InstructionDocument {
        let mut variants_a = BTreeMap::new();
        variants_a.insert(AgentTarget::Claude, "a-claude".to_string());
        let mut variants_t = BTreeMap::new();
        variants_t.insert(AgentTarget::OpenCode, "o-only".to_string());
        InstructionDocument {
            relative_key: String::new(),
            blocks: vec![
                InstructionBlock::shared("slot-shared", "shared-body", Vec::new()),
                InstructionBlock::adapted(
                    "slot-adapted",
                    Some("common-adapted".into()),
                    None,
                    variants_a,
                    Vec::new(),
                ),
                InstructionBlock::target_only(
                    "slot-targetOnly",
                    AgentTarget::OpenCode,
                    "o-only",
                    Vec::new(),
                    false,
                ),
            ],
        }
    }

    #[test]
    fn slot_key_to_item_id_matches_segment_contract() {
        assert_eq!(
            InstructionSlotKey::Shared.to_item_id("asset-1"),
            "asset-1:shared"
        );
        assert_eq!(
            InstructionSlotKey::Adapted {
                agent: AgentTarget::Claude
            }
            .to_item_id("asset-1"),
            "asset-1:adapted:claude"
        );
        assert_eq!(
            InstructionSlotKey::TargetOnly {
                agent: AgentTarget::OpenCode
            }
            .to_item_id("asset-1"),
            "asset-1:targetOnly:opencode"
        );
    }

    #[test]
    fn extract_slot_text_returns_canonical_per_slot() {
        let doc = doc_with_three_slots();
        assert_eq!(
            extract_slot_text(&doc, InstructionSlotKey::Shared),
            "shared-body"
        );
        assert_eq!(
            extract_slot_text(
                &doc,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Claude
                }
            ),
            "a-claude"
        );
        assert_eq!(
            extract_slot_text(
                &doc,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Codex
                }
            ),
            "common-adapted",
        );
        assert_eq!(
            extract_slot_text(
                &doc,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::OpenCode
                }
            ),
            "o-only"
        );
        assert_eq!(
            extract_slot_text(
                &doc,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::Claude
                }
            ),
            ""
        );
    }

    #[test]
    fn replace_slot_text_shared_round_trip() {
        let doc = doc_with_three_slots();
        let next = replace_slot_text(&doc, InstructionSlotKey::Shared, "new-shared");
        assert_eq!(
            extract_slot_text(&next, InstructionSlotKey::Shared),
            "new-shared"
        );
        // 其它槽不被动
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Claude
                }
            ),
            "a-claude"
        );
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::OpenCode
                }
            ),
            "o-only"
        );
    }

    #[test]
    fn replace_slot_text_adapted_overrides_variant_not_common() {
        let doc = doc_with_three_slots();
        let next = replace_slot_text(
            &doc,
            InstructionSlotKey::Adapted {
                agent: AgentTarget::Codex,
            },
            "codex-only",
        );
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Codex
                }
            ),
            "codex-only"
        );
        // common 槽不变
        assert_eq!(
            extract_slot_text(&next, InstructionSlotKey::Shared),
            "shared-body"
        );
        // 已有 variant 不被覆盖（按 AgentTarget 精确写入）
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Claude
                }
            ),
            "a-claude"
        );
    }

    #[test]
    fn replace_slot_text_target_only_round_trip() {
        let doc = doc_with_three_slots();
        let next = replace_slot_text(
            &doc,
            InstructionSlotKey::TargetOnly {
                agent: AgentTarget::Claude,
            },
            "claude-only",
        );
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::Claude
                }
            ),
            "claude-only"
        );
        // opencode 仍为 o-only
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::OpenCode
                }
            ),
            "o-only"
        );
    }

    #[test]
    fn replace_slot_text_lazily_creates_missing_block() {
        let doc = InstructionDocument::default();
        let next = replace_slot_text(&doc, InstructionSlotKey::Shared, "first");
        assert_eq!(
            extract_slot_text(&next, InstructionSlotKey::Shared),
            "first"
        );
        // adapted 懒创建
        let next = replace_slot_text(
            &next,
            InstructionSlotKey::Adapted {
                agent: AgentTarget::Claude,
            },
            "a",
        );
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::Adapted {
                    agent: AgentTarget::Claude
                }
            ),
            "a"
        );
        // targetOnly 懒创建
        let next = replace_slot_text(
            &next,
            InstructionSlotKey::TargetOnly {
                agent: AgentTarget::OpenCode,
            },
            "o",
        );
        assert_eq!(
            extract_slot_text(
                &next,
                InstructionSlotKey::TargetOnly {
                    agent: AgentTarget::OpenCode
                }
            ),
            "o"
        );
    }

    #[test]
    fn replace_slot_text_empty_text_clears_shared() {
        let doc = doc_with_three_slots();
        let next = replace_slot_text(&doc, InstructionSlotKey::Shared, "");
        assert_eq!(extract_slot_text(&next, InstructionSlotKey::Shared), "");
        // 块仍存在，但 common_markdown 为 None
        let shared_block = next
            .blocks
            .iter()
            .find(|block| block.mode == InstructionBlockMode::Shared)
            .expect("shared block must exist");
        assert!(shared_block.common_markdown.is_none());
    }

    #[test]
    fn replace_slot_text_normalize_keeps_three_slots_order() {
        let doc = doc_with_three_slots();
        let next = replace_slot_text(&doc, InstructionSlotKey::Shared, "new");
        let modes: Vec<InstructionBlockMode> = next.blocks.iter().map(|block| block.mode).collect();
        assert_eq!(
            modes,
            vec![
                InstructionBlockMode::Shared,
                InstructionBlockMode::Adapted,
                InstructionBlockMode::TargetOnly,
            ]
        );
    }

    #[test]
    fn all_slot_keys_covers_7_distinct_keys() {
        let keys = all_slot_keys();
        assert_eq!(keys.len(), 7);
        let item_ids: Vec<String> = keys
            .iter()
            .map(|key| key.to_item_id("a"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(item_ids.len(), 7);
    }
}
