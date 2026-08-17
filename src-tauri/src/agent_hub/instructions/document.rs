//! agent_hub/instructions/document — 指令文档块模型
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 需要把目录级指令拆成稳定块，并标记 shared / adapted / targetOnly，
//!     以便跨 Claude / Codex / OpenCode 投影且不插入用户文件私有 marker。
//!
//! Code Logic（这个模块做什么）:
//!     定义 InstructionBlock、InstructionDocument、StructuredInstructionIntent、
//!     RenderedBlockRange、PortabilityDiagnostic 与冲突/修订 DTO。

use crate::agent_hub::models::AgentTarget;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 指令块共享策略。
///
/// Business Logic（为什么需要这个枚举）:
///     shared/adapted/targetOnly 决定块如何在多 target 间分配与反向合并。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化；与 AssetPolicy 语义对齐但作用于块级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionBlockMode {
    /// 共同 Markdown，跨 target 共享
    Shared,
    /// 共同意图 + 各 target 变体
    Adapted,
    /// 仅单一 target 持有
    TargetOnly,
}

impl InstructionBlockMode {
    /// 稳定 wire 字符串。
    ///
    /// Business Logic: 持久化 base-map 与 UI 依赖稳定 token。
    /// Code Logic: `shared` / `adapted` / `targetOnly`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Adapted => "adapted",
            Self::TargetOnly => "targetOnly",
        }
    }
}

/// 版本化结构化指令意图。
///
/// Business Logic（为什么需要这个枚举）:
///     某些合同（如分层发现）必须由确定性 renderer 生成，而不是自由文本丢给模型。
///
/// Code Logic（这个枚举做什么）:
///     首版仅 `DiscoveryBeforeEdit`；带 version 字段供 renderer 分支。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StructuredInstructionIntent {
    /// 先发现分层指令再编辑
    #[serde(rename = "instruction.discovery.before-edit")]
    DiscoveryBeforeEdit {
        /// renderer 版本
        version: u32,
    },
}

impl StructuredInstructionIntent {
    /// 构造默认 v1 DiscoveryBeforeEdit。
    ///
    /// Business Logic: OpenCode 子目录 prelude 与导入适配共用同一 intent 标识。
    /// Code Logic: version=1。
    pub fn discovery_before_edit_v1() -> Self {
        Self::DiscoveryBeforeEdit { version: 1 }
    }
}

/// 有序指令块。
///
/// Business Logic（为什么需要这个结构体）:
///     块是编译/对账的最小单位；ID 稳定，mode 决定投影与反向合并。
///
/// Code Logic（这个结构体做什么）:
///     持有 id/mode/common 或 structured intent/variants/heading_path 与诊断标志。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionBlock {
    /// 稳定块 ID（base-map 复用或 UUIDv7）
    pub id: String,
    /// shared / adapted / targetOnly
    pub mode: InstructionBlockMode,
    /// 共同 Markdown（shared 或 adapted 的 common）
    pub common_markdown: Option<String>,
    /// 结构化意图（与 common 互斥优先）
    pub structured_intent: Option<StructuredInstructionIntent>,
    /// 各 target 显式变体（adapted / targetOnly）
    pub variants: BTreeMap<AgentTarget, String>,
    /// 父 heading 路径
    pub heading_path: Vec<String>,
    /// 来源 target（targetOnly 导入时）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_target: Option<AgentTarget>,
    /// 含 CLI 专属术语，需人工确认适配
    #[serde(default)]
    pub needs_adaptation: bool,
}

impl InstructionBlock {
    /// 新建 shared 文本块。
    ///
    /// Business Logic: 子目录默认导入与 identical 块合并走 shared。
    /// Code Logic: mode=Shared，填 common_markdown。
    pub fn shared(
        id: impl Into<String>,
        markdown: impl Into<String>,
        heading_path: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            mode: InstructionBlockMode::Shared,
            common_markdown: Some(markdown.into()),
            structured_intent: None,
            variants: BTreeMap::new(),
            heading_path,
            source_target: None,
            needs_adaptation: false,
        }
    }

    /// 新建 targetOnly 文本块。
    ///
    /// Business Logic: 根/用户级单来源或 unique 块保留来源 target。
    /// Code Logic: mode=TargetOnly，variants 只含来源。
    pub fn target_only(
        id: impl Into<String>,
        target: AgentTarget,
        markdown: impl Into<String>,
        heading_path: Vec<String>,
        needs_adaptation: bool,
    ) -> Self {
        let mut variants = BTreeMap::new();
        variants.insert(target, markdown.into());
        Self {
            id: id.into(),
            mode: InstructionBlockMode::TargetOnly,
            common_markdown: None,
            structured_intent: None,
            variants,
            heading_path,
            source_target: Some(target),
            needs_adaptation,
        }
    }

    /// 新建 adapted 块（common + 可选 variants）。
    ///
    /// Business Logic: 用户配对多 target 变体或 structured intent 渲染结果。
    /// Code Logic: mode=Adapted。
    pub fn adapted(
        id: impl Into<String>,
        common: Option<String>,
        intent: Option<StructuredInstructionIntent>,
        variants: BTreeMap<AgentTarget, String>,
        heading_path: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            mode: InstructionBlockMode::Adapted,
            common_markdown: common,
            structured_intent: intent,
            variants,
            heading_path,
            source_target: None,
            needs_adaptation: false,
        }
    }

    /// 取指定 target 的渲染正文。
    ///
    /// Business Logic: 投影时 shared 用 common，adapted/targetOnly 优先 variant。
    /// Code Logic: variants 命中 → structured renderer 外层处理 → common → 空。
    pub fn text_for_target(&self, target: AgentTarget) -> Option<&str> {
        if let Some(v) = self.variants.get(&target) {
            return Some(v.as_str());
        }
        match self.mode {
            InstructionBlockMode::Shared | InstructionBlockMode::Adapted => {
                self.common_markdown.as_deref()
            }
            InstructionBlockMode::TargetOnly => None,
        }
    }

    /// 块内容指纹（用于三方比较）。
    ///
    /// Business Logic: reconcile 按块 hash 比较 base/hub/external。
    /// Code Logic: 拼接 mode + common + intent + sorted variants 再 sha256。
    pub fn content_fingerprint(&self) -> String {
        use crate::agent_hub::object_store::sha256_hex;
        let mut buf = String::new();
        buf.push_str(self.mode.as_str());
        buf.push('\n');
        if let Some(c) = &self.common_markdown {
            buf.push_str("common:");
            buf.push_str(c);
            buf.push('\n');
        }
        if let Some(intent) = &self.structured_intent {
            buf.push_str("intent:");
            buf.push_str(&serde_json::to_string(intent).unwrap_or_default());
            buf.push('\n');
        }
        for (t, v) in &self.variants {
            buf.push_str(t.as_str());
            buf.push(':');
            buf.push_str(v);
            buf.push('\n');
        }
        if self.needs_adaptation {
            buf.push_str("needsAdaptation\n");
        }
        sha256_hex(buf.as_bytes())
    }
}

/// 目录级指令文档（有序块）。
///
/// Business Logic（为什么需要这个结构体）:
///     一个目录对应一个文档；render/reconcile 都以块序列为权威。
///
/// Code Logic（这个结构体做什么）:
///     保存 relative_key 与有序 blocks。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDocument {
    /// 逻辑相对键（"" = 项目根 / "src-tauri/src/net"）
    pub relative_key: String,
    /// 有序指令块
    pub blocks: Vec<InstructionBlock>,
}

impl InstructionDocument {
    /// 从单段 common Markdown 构造（整篇 shared）。
    ///
    /// Business Logic: 子目录单来源导入与简单 render stub 兼容。
    /// Code Logic: 解析为块后全部标 Shared（调用方再 reclassify）。
    pub fn from_shared_markdown(
        relative_key: impl Into<String>,
        markdown: impl Into<String>,
    ) -> Self {
        let markdown = markdown.into();
        let relative_key = relative_key.into();
        if markdown.is_empty() {
            return Self {
                relative_key,
                blocks: vec![],
            };
        }
        Self {
            relative_key,
            blocks: vec![InstructionBlock::shared(new_block_id(), markdown, vec![])],
        }
    }

    /// 拼接 shared/adapted common 与全部 variants 的用户可见摘要（调试用）。
    ///
    /// Business Logic: 旧 adapter stub 需要一份 common 正文。
    /// Code Logic: 按块顺序拼接 shared/adapted common；跳过纯 targetOnly。
    pub fn joined_shared_body(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for block in &self.blocks {
            match block.mode {
                InstructionBlockMode::Shared | InstructionBlockMode::Adapted => {
                    if let Some(c) = block.common_markdown.as_deref() {
                        parts.push(c);
                    }
                }
                InstructionBlockMode::TargetOnly => {}
            }
        }
        join_block_texts(&parts)
    }

    /// 取指定 target 的用户正文（不含 OpenCode managed prelude）。
    ///
    /// Business Logic: 三 target 用户 body 应 byte-identical（shared 情形）。
    /// Code Logic: 顺序拼接该 target 可见块文本。
    pub fn body_for_target(&self, target: AgentTarget) -> String {
        let mut parts: Vec<String> = Vec::new();
        for block in &self.blocks {
            if let Some(text) = block.text_for_target(target) {
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
        join_owned_block_texts(&parts)
    }

    /// 归并为固定三槽：每 mode 至多 1 块，顺序 shared → adapted → targetOnly。
    ///
    /// Business Logic: 用户级提示词编辑面只认公共/适配/独有；旧 N 块打开或保存时直接迁移。
    /// Code Logic: 按 mode 分组；common 与各 target variant 用 `\n\n` 拼接；保留首个 id。
    pub fn normalize_to_three_slots(&mut self) {
        if self.blocks.is_empty() {
            return;
        }
        let mut shared: Vec<InstructionBlock> = Vec::new();
        let mut adapted: Vec<InstructionBlock> = Vec::new();
        let mut target_only: Vec<InstructionBlock> = Vec::new();
        for block in self.blocks.drain(..) {
            match block.mode {
                InstructionBlockMode::Shared => shared.push(block),
                InstructionBlockMode::Adapted => adapted.push(block),
                InstructionBlockMode::TargetOnly => target_only.push(block),
            }
        }
        let mut next = Vec::with_capacity(3);
        if let Some(block) = merge_mode_group(InstructionBlockMode::Shared, shared) {
            next.push(block);
        }
        if let Some(block) = merge_mode_group(InstructionBlockMode::Adapted, adapted) {
            next.push(block);
        }
        if let Some(block) = merge_mode_group(InstructionBlockMode::TargetOnly, target_only) {
            next.push(block);
        }
        self.blocks = next;
    }
}

/// 合并同 mode 多块为单个块。
fn merge_mode_group(
    mode: InstructionBlockMode,
    group: Vec<InstructionBlock>,
) -> Option<InstructionBlock> {
    if group.is_empty() {
        return None;
    }
    let first = &group[0];
    let id = first.id.clone();
    let heading_path = first.heading_path.clone();
    let needs_adaptation = group.iter().any(|b| b.needs_adaptation);
    let source_target = group.iter().find_map(|b| b.source_target);

    let commons: Vec<&str> = group
        .iter()
        .filter_map(|b| b.common_markdown.as_deref())
        .filter(|s| !s.is_empty())
        .collect();
    let common_joined = join_block_texts(&commons);

    let mut variants: BTreeMap<AgentTarget, String> = BTreeMap::new();
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        let parts: Vec<&str> = group
            .iter()
            .filter_map(|b| b.variants.get(&target).map(String::as_str))
            .filter(|s| !s.is_empty())
            .collect();
        let joined = join_block_texts(&parts);
        if !joined.is_empty() {
            variants.insert(target, joined);
        }
    }

    match mode {
        InstructionBlockMode::Shared => Some(InstructionBlock {
            id,
            mode: InstructionBlockMode::Shared,
            common_markdown: if common_joined.is_empty() {
                None
            } else {
                Some(common_joined)
            },
            structured_intent: None,
            variants: BTreeMap::new(),
            heading_path,
            source_target: None,
            needs_adaptation: false,
        }),
        InstructionBlockMode::Adapted => Some(InstructionBlock {
            id,
            mode: InstructionBlockMode::Adapted,
            common_markdown: if common_joined.is_empty() {
                None
            } else {
                Some(common_joined)
            },
            structured_intent: None,
            variants,
            heading_path,
            source_target: None,
            needs_adaptation: false,
        }),
        InstructionBlockMode::TargetOnly => Some(InstructionBlock {
            id,
            mode: InstructionBlockMode::TargetOnly,
            common_markdown: None,
            structured_intent: None,
            variants,
            heading_path,
            source_target,
            needs_adaptation,
        }),
    }
}

/// 渲染结果中的块字节区间。
///
/// Business Logic（为什么需要这个结构体）:
///     materialization base-map 记录上次投影的块 ID 与区间，供反向 reconcile 对齐。
///
/// Code Logic（这个结构体做什么）:
///     start/end 为字节偏移（半开区间）；managed 段可无 block_id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedBlockRange {
    /// 块 ID（managed prelude 为 None）
    pub block_id: Option<String>,
    /// 起始字节偏移
    pub start: usize,
    /// 结束字节偏移（不含）
    pub end: usize,
    /// 是否为 managed（非用户正文）
    pub managed: bool,
    /// 内容 hash
    pub content_hash: String,
}

/// 可移植/适配诊断。
///
/// Business Logic（为什么需要这个结构体）:
///     needsAdaptation 等信号必须稳定 code，且不泄露凭据原文。
///
/// Code Logic（这个结构体做什么）:
///     code + path + 可选 message。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityDiagnostic {
    /// 稳定诊断码（如 needsAdaptation）
    pub code: String,
    /// 块 ID 或 JSON pointer 路径
    pub path: String,
    /// 人类可读摘要（无敏感原文）
    pub message: String,
}

impl PortabilityDiagnostic {
    /// 构造 needsAdaptation 诊断。
    ///
    /// Business Logic: CLI 专属术语块必须提示用户确认适配。
    /// Code Logic: code=`needsAdaptation`。
    pub fn needs_adaptation(block_id: &str, message: impl Into<String>) -> Self {
        Self {
            code: "needsAdaptation".into(),
            path: format!("blocks/{block_id}"),
            message: message.into(),
        }
    }

    /// 构造 foreignPluginReference 诊断。
    ///
    /// Business Logic: 非 Claude CLI 预览到 Claude 插件/斜杠调用时只警告，不自动安装。
    /// Code Logic: code=`foreignPluginReference`。
    pub fn foreign_plugin_reference(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: "foreignPluginReference".into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

/// 冲突作用域。
///
/// Business Logic（为什么需要这个枚举）:
///     common payload 冲突冻结合规资产；target-only 冲突只冻该 target 投影。
///
/// Code Logic（这个枚举做什么）:
///     CanonicalAsset / Target。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentHubConflictScope {
    /// 共同/canonical 载荷冲突
    CanonicalAsset,
    /// 单 target 变体冲突
    Target,
}

/// 新建冲突输入（尚未落库）。
///
/// Business Logic（为什么需要这个结构体）:
///     reconcile 产出 conflict 时尚未有 DB id；service 层负责持久化。
///
/// Code Logic（这个结构体做什么）:
///     保存 scope/target/block/base/hub/external hash 与 detail。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAgentHubConflict {
    /// 冲突作用域
    pub scope: AgentHubConflictScope,
    /// 受影响 target（Canonical 时为 None）
    pub target: Option<AgentTarget>,
    /// 冲突块 ID（整文件冲突时为 None）
    pub block_id: Option<String>,
    /// base 块 hash
    pub base_hash: Option<String>,
    /// Hub current hash
    pub hub_hash: Option<String>,
    /// external hash
    pub external_hash: Option<String>,
    /// 详情摘要
    pub detail: String,
}

/// 新建指令修订（尚未落库）。
///
/// Business Logic（为什么需要这个结构体）:
///     auto-merge 或单 target 编辑成功后产出新文档 revision 载荷。
///
/// Code Logic（这个结构体做什么）:
///     持有合并后文档、来源 target 与 payload hash。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewInstructionRevision {
    /// 合并后的文档
    pub document: InstructionDocument,
    /// 触发来源 target（Hub UI 编辑时为 None）
    pub origin_target: Option<AgentTarget>,
    /// 文档序列化 hash
    pub payload_hash: String,
}

/// 生成新块 UUIDv7。
///
/// Business Logic: 新块需要跨设备唯一且时间有序的 ID。
/// Code Logic: Uuid::now_v7().to_string()。
pub fn new_block_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// 用双换行拼接块文本（保留末尾单换行习惯）。
///
/// Business Logic: 多块投影时块间分隔稳定。
/// Code Logic: 非空片段 `\n\n` 连接。
fn join_block_texts(parts: &[&str]) -> String {
    let filtered: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    filtered.join("\n\n")
}

/// owned 版本拼接。
fn join_owned_block_texts(parts: &[String]) -> String {
    let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
    join_block_texts(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_to_three_slots_merges_same_mode_blocks() {
        let mut doc = InstructionDocument {
            relative_key: String::new(),
            blocks: vec![
                InstructionBlock::shared("s1", "A", vec![]),
                InstructionBlock::shared("s2", "B", vec![]),
                InstructionBlock::adapted(
                    "a1",
                    Some("base".into()),
                    None,
                    BTreeMap::from([(AgentTarget::Claude, "c1".into())]),
                    vec![],
                ),
                InstructionBlock::target_only("t1", AgentTarget::Claude, "only-c", vec![], false),
                InstructionBlock::target_only("t2", AgentTarget::Codex, "only-x", vec![], false),
            ],
        };
        doc.normalize_to_three_slots();
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(doc.blocks[0].mode, InstructionBlockMode::Shared);
        assert_eq!(doc.blocks[0].common_markdown.as_deref(), Some("A\n\nB"));
        assert_eq!(doc.blocks[1].mode, InstructionBlockMode::Adapted);
        assert_eq!(
            doc.blocks[1]
                .variants
                .get(&AgentTarget::Claude)
                .map(String::as_str),
            Some("c1")
        );
        assert_eq!(doc.blocks[2].mode, InstructionBlockMode::TargetOnly);
        assert_eq!(
            doc.blocks[2]
                .variants
                .get(&AgentTarget::Claude)
                .map(String::as_str),
            Some("only-c")
        );
        assert_eq!(
            doc.blocks[2]
                .variants
                .get(&AgentTarget::Codex)
                .map(String::as_str),
            Some("only-x")
        );
    }
}
