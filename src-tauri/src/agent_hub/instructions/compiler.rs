//! agent_hub/instructions/compiler — 确定性块解析与目标渲染
//!
//! Business Logic（为什么需要这个模块）:
//!     首次纳管与后续投影必须按确定性规则拆块/分类/渲染，禁止把自由文本送给模型猜意图。
//!
//! Code Logic（这个模块做什么）:
//!     Markdown 边界切块、导入分类、CLI 专属术语扫描、OpenCode prelude、
//!     DiscoveryBeforeEdit 三目标 renderer、CompiledRenderedInstruction 输出。

use super::document::{
    new_block_id, InstructionBlock, InstructionBlockMode, InstructionDocument,
    PortabilityDiagnostic, RenderedBlockRange, StructuredInstructionIntent,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::targets::InstructionRenderContext;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 解析出的原始 Markdown 块（尚未分类）。
///
/// Business Logic（为什么需要这个结构体）:
///     切块阶段只保留字节切片与 heading，分类阶段再赋 mode/id。
///
/// Code Logic（这个结构体做什么）:
///     text + heading_path + 是否 heading 起头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMarkdownBlock {
    /// 块原文（保留边界内精确字节）
    pub text: String,
    /// 父 heading 路径（含本块 heading 标题文本，若有）
    pub heading_path: Vec<String>,
    /// 本块是否以 AT 行开头
    pub starts_with_heading: bool,
}

/// 编译后的渲染结果（含 block_map 与 managed_prefix）。
///
/// Business Logic（为什么需要这个结构体）:
///     projection/reconcile 需要字节、块区间与 managed 前缀长度；旧 stub 只有 content 字符串不够。
///
/// Code Logic（这个结构体做什么）:
///     bytes + block_map + managed_prefix_len + diagnostics + file_name/target。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledRenderedInstruction {
    /// 目标 CLI
    pub target: AgentTarget,
    /// 输出文件名
    pub file_name: String,
    /// 完整文件字节（UTF-8）
    pub bytes: Vec<u8>,
    /// 块区间映射
    pub block_map: Vec<RenderedBlockRange>,
    /// managed prelude 占用的前缀字节数（OpenCode）
    pub managed_prefix_len: usize,
    /// 诊断
    pub diagnostics: Vec<PortabilityDiagnostic>,
}

impl CompiledRenderedInstruction {
    /// 以 UTF-8 字符串借用完整内容。
    ///
    /// Business Logic: 兼容旧 RenderedInstruction.content。
    /// Code Logic: lossy 不应发生；bytes 保证 UTF-8。
    pub fn content_str(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    /// managed prelude 文本（若有）。
    ///
    /// Business Logic: reverse reconcile 将 prelude 视为 OpenCode target-only。
    /// Code Logic: bytes[..managed_prefix_len]。
    pub fn managed_prelude(&self) -> Option<&str> {
        if self.managed_prefix_len == 0 {
            None
        } else {
            std::str::from_utf8(&self.bytes[..self.managed_prefix_len]).ok()
        }
    }

    /// 用户正文（去掉 managed 前缀）。
    ///
    /// Business Logic: 三 target 用户 body 应 byte-identical。
    /// Code Logic: bytes[managed_prefix_len..]。
    pub fn user_body(&self) -> &str {
        std::str::from_utf8(&self.bytes[self.managed_prefix_len..]).unwrap_or("")
    }
}

/// 按 heading / 段落 / fenced-code 边界切 Markdown。
///
/// Business Logic（为什么需要这个函数）:
///     块是共享/独有分类的最小单位；必须保留精确字节切片与顺序，不插入 marker。
///
/// Code Logic（这个函数做什么）:
///     扫描行：fenced code 合并至闭合；ATX heading 开新块；空行分隔段落；
///     连续非空非 heading 行并入当前段落块。
pub fn parse_markdown_blocks(markdown: &str) -> Vec<ParsedMarkdownBlock> {
    if markdown.is_empty() {
        return vec![];
    }

    let lines: Vec<&str> = split_lines_keep_ends(markdown);
    let mut blocks: Vec<ParsedMarkdownBlock> = Vec::new();
    let mut heading_stack: Vec<(usize, String)> = Vec::new(); // (level, title)
    let mut current: Vec<String> = Vec::new();
    let mut current_starts_heading = false;
    let mut in_fence = false;
    let mut fence_marker: Option<String> = None;

    let flush = |current: &mut Vec<String>,
                 current_starts_heading: &mut bool,
                 heading_stack: &[(usize, String)],
                 blocks: &mut Vec<ParsedMarkdownBlock>| {
        if current.is_empty() {
            return;
        }
        let text = current.join("");
        // 去掉块尾多余的仅换行？保留原字节：直接 join 行（含各行结尾）
        let path: Vec<String> = heading_stack.iter().map(|(_, t)| t.clone()).collect();
        blocks.push(ParsedMarkdownBlock {
            text,
            heading_path: path,
            starts_with_heading: *current_starts_heading,
        });
        current.clear();
        *current_starts_heading = false;
    };

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let trimmed_start = trimmed.trim_start();

        if in_fence {
            current.push(line.to_string());
            if let Some(marker) = &fence_marker {
                if trimmed_start.starts_with(marker)
                    && trimmed_start
                        .chars()
                        .all(|c| c == '`' || c == '~' || c.is_whitespace())
                {
                    in_fence = false;
                    fence_marker = None;
                }
            }
            i += 1;
            continue;
        }

        // fenced code open
        if let Some(marker) = detect_fence_open(trimmed_start) {
            // 若当前已有非 fence 内容，先 flush 成独立块
            if !current.is_empty() {
                flush(
                    &mut current,
                    &mut current_starts_heading,
                    &heading_stack,
                    &mut blocks,
                );
            }
            in_fence = true;
            fence_marker = Some(marker);
            current.push(line.to_string());
            i += 1;
            continue;
        }

        // ATX heading
        if let Some((level, title)) = parse_atx_heading(trimmed) {
            flush(
                &mut current,
                &mut current_starts_heading,
                &heading_stack,
                &mut blocks,
            );
            while heading_stack
                .last()
                .map(|(l, _)| *l >= level)
                .unwrap_or(false)
            {
                heading_stack.pop();
            }
            heading_stack.push((level, title));
            current_starts_heading = true;
            current.push(line.to_string());
            i += 1;
            // 后续非空行并入 heading 块，直到空行或下一 heading/fence
            while i < lines.len() {
                let next = lines[i];
                let nt = next.trim_end_matches(['\r', '\n']);
                let nts = nt.trim_start();
                if nt.trim().is_empty() {
                    // 保留一个尾随空行在块内，然后结束该 heading 块
                    current.push(next.to_string());
                    i += 1;
                    break;
                }
                if detect_fence_open(nts).is_some() || parse_atx_heading(nt).is_some() {
                    break;
                }
                current.push(next.to_string());
                i += 1;
            }
            flush(
                &mut current,
                &mut current_starts_heading,
                &heading_stack,
                &mut blocks,
            );
            continue;
        }

        // 空行：若当前有内容则结束段落
        if trimmed.trim().is_empty() {
            if !current.is_empty() {
                current.push(line.to_string());
                flush(
                    &mut current,
                    &mut current_starts_heading,
                    &heading_stack,
                    &mut blocks,
                );
            } else {
                // 文档前导空行：作为独立空白块保留（极少见）
                // 跳过连续前导空行不单独成块，避免噪声
            }
            i += 1;
            continue;
        }

        // 普通段落行
        current.push(line.to_string());
        i += 1;
        while i < lines.len() {
            let next = lines[i];
            let nt = next.trim_end_matches(['\r', '\n']);
            let nts = nt.trim_start();
            if nt.trim().is_empty() {
                current.push(next.to_string());
                i += 1;
                break;
            }
            if detect_fence_open(nts).is_some() || parse_atx_heading(nt).is_some() {
                break;
            }
            current.push(next.to_string());
            i += 1;
        }
        flush(
            &mut current,
            &mut current_starts_heading,
            &heading_stack,
            &mut blocks,
        );
    }

    flush(
        &mut current,
        &mut current_starts_heading,
        &heading_stack,
        &mut blocks,
    );

    // 若整篇无法切出块（极端），整篇一块
    if blocks.is_empty() && !markdown.is_empty() {
        blocks.push(ParsedMarkdownBlock {
            text: markdown.to_string(),
            heading_path: vec![],
            starts_with_heading: false,
        });
    }

    // 去掉块文本末尾多余：保留原样（join 已含行尾）
    // 但 trim 掉块之间因空行产生的纯空白块
    blocks.retain(|b| !b.text.trim().is_empty());
    blocks
}

/// 扫描块是否含 CLI 专属术语（确定性，不调用模型）。
///
/// Business Logic（为什么需要这个函数）:
///     含 CLAUDE.md / Read / hook / 配置路径 的自由文本不能默认 shared 到其他 CLI。
///
/// Code Logic（这个函数做什么）:
///     子串匹配固定模式列表；命中返回 true。
pub fn block_needs_target_isolation(text: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "CLAUDE.md",
        "AGENTS.override.md",
        "AGENTS.md",
        "opencode.json",
        ".claude/",
        ".codex/",
        ".opencode/",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "OPENCODE_CONFIG",
        "OPENCODE_CONFIG_DIR",
        // Claude 工具 / 事件
        "\nRead ",
        "\nRead\n",
        " Read ",
        "`Read`",
        "PreToolUse",
        "PostToolUse",
        "Notification",
        "Stop",
        "SubagentStart",
        "UserPromptSubmit",
        "hook event",
        "hooks.json",
        "settings.json",
        // Codex 优先序列
        "project_doc_fallback",
        // 裸工具调用常见写法
        "mcp__",
    ];
    // 额外：行首工具名
    if text.lines().any(|l| {
        let t = l.trim();
        t == "Read" || t.starts_with("Read ") || t.starts_with("Read(")
    }) {
        return true;
    }
    PATTERNS.iter().any(|p| text.contains(p))
}

/// 导入作用域上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     项目根 vs 非根子目录决定单来源默认 shared 还是 targetOnly。
///
/// Code Logic（这个结构体做什么）:
///     is_project_root / is_user_scope。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportScopeContext {
    /// 是否用户级 scope
    pub is_user_scope: bool,
    /// 是否项目根（relative_key 空）
    pub is_project_root: bool,
}

impl ImportScopeContext {
    /// 项目非根子目录。
    pub fn project_subdirectory() -> Self {
        Self {
            is_user_scope: false,
            is_project_root: false,
        }
    }

    /// 项目根。
    pub fn project_root() -> Self {
        Self {
            is_user_scope: false,
            is_project_root: true,
        }
    }

    /// 用户级。
    pub fn user() -> Self {
        Self {
            is_user_scope: true,
            is_project_root: false,
        }
    }
}

/// 单 target 导入源（路径角色 + 正文）。
///
/// Business Logic（为什么需要这个结构体）:
///     首次纳管可能只有部分 target 有文件。
///
/// Code Logic（这个结构体做什么）:
///     target + markdown 正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetMarkdownSource {
    /// 来源 target
    pub target: AgentTarget,
    /// 文件 UTF-8 正文
    pub markdown: String,
}

/// 导入分类结果。
///
/// Business Logic（为什么需要这个结构体）:
///     preview 需要文档与 needsAdaptation 诊断列表。
///
/// Code Logic（这个结构体做什么）:
///     document + diagnostics。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportClassification {
    /// 分类后的文档
    pub document: InstructionDocument,
    /// 诊断
    pub diagnostics: Vec<PortabilityDiagnostic>,
}

/// 从多 target 源文件导入并分类块。
///
/// Business Logic（为什么需要这个函数）:
///     1) 非根单来源 ordinary→shared；2) 根/用户单来源→targetOnly；
///     3) 全源文件 identical 且无 CLI 术语→shared；含 CLI 术语→每 source target 各一份 targetOnly + needsAdaptation；
///     4) 部分源 identical（present 真子集）→不得 Shared 注入缺失源，改为每个 present targetOnly；
///     5) CLI 术语块一律 source targetOnly + needsAdaptation。
///
/// Code Logic（这个函数做什么）:
///     切块 → 按规范化文本对齐 → 赋 mode/id → 扫描隔离；Shared 仅在 all targets 均持有且无术语时创建。
pub fn classify_import(
    relative_key: impl Into<String>,
    scope: ImportScopeContext,
    sources: &[TargetMarkdownSource],
) -> ImportClassification {
    let relative_key = relative_key.into();
    let mut diagnostics = Vec::new();

    if sources.is_empty() {
        return ImportClassification {
            document: InstructionDocument {
                relative_key,
                blocks: vec![],
            },
            diagnostics,
        };
    }

    // 单来源
    if sources.len() == 1 {
        let src = &sources[0];
        let parsed = parse_markdown_blocks(&src.markdown);
        let force_target_only = scope.is_user_scope || scope.is_project_root;
        let mut blocks = Vec::with_capacity(parsed.len());
        for p in parsed {
            let needs = block_needs_target_isolation(&p.text);
            let id = new_block_id();
            if force_target_only || needs {
                if needs {
                    diagnostics.push(PortabilityDiagnostic::needs_adaptation(
                        &id,
                        "块含 CLI 专属术语，保留来源 targetOnly",
                    ));
                }
                blocks.push(InstructionBlock::target_only(
                    id,
                    src.target,
                    p.text,
                    p.heading_path,
                    needs,
                ));
            } else {
                blocks.push(InstructionBlock::shared(id, p.text, p.heading_path));
            }
        }
        return ImportClassification {
            document: InstructionDocument {
                relative_key,
                blocks,
            },
            diagnostics,
        };
    }

    // 多来源：按规范化文本分组
    let per_target: BTreeMap<AgentTarget, Vec<ParsedMarkdownBlock>> = sources
        .iter()
        .map(|s| (s.target, parse_markdown_blocks(&s.markdown)))
        .collect();

    // 若所有源文件全文完全相同
    if sources
        .windows(2)
        .all(|w| normalize_for_compare(&w[0].markdown) == normalize_for_compare(&w[1].markdown))
    {
        let first = sources.first().unwrap();
        let parsed = parse_markdown_blocks(&first.markdown);
        let mut blocks = Vec::new();
        for p in parsed {
            let needs = block_needs_target_isolation(&p.text);
            if needs {
                // H6：全文 identical 但含 CLI 术语 → 每个 source target 各建 targetOnly（同文）+ 诊断
                // 禁止 Shared（否则 compile_render 会把术语块注入全部 target，掩盖隔离意图）
                for src in sources {
                    let bid = new_block_id();
                    diagnostics.push(PortabilityDiagnostic::needs_adaptation(
                        &bid,
                        "块含 CLI 专属术语，保留来源 targetOnly",
                    ));
                    blocks.push(InstructionBlock::target_only(
                        bid,
                        src.target,
                        p.text.clone(),
                        p.heading_path.clone(),
                        true,
                    ));
                }
            } else {
                blocks.push(InstructionBlock::shared(
                    new_block_id(),
                    p.text,
                    p.heading_path,
                ));
            }
        }
        return ImportClassification {
            document: InstructionDocument {
                relative_key,
                blocks,
            },
            diagnostics,
        };
    }

    // 不同文件：对齐相同块
    // 策略：收集所有 (norm_text -> (targets, sample text, heading))
    let mut norm_order: Vec<String> = Vec::new();
    let mut norm_info: BTreeMap<String, (BTreeMap<AgentTarget, String>, Vec<String>)> =
        BTreeMap::new();

    // 保持首次出现顺序（按 sources 顺序 + 块序）
    for src in sources {
        let parsed = per_target.get(&src.target).cloned().unwrap_or_default();
        for p in parsed {
            let key = normalize_for_compare(&p.text);
            if !norm_info.contains_key(&key) {
                norm_order.push(key.clone());
                norm_info.insert(key.clone(), (BTreeMap::new(), p.heading_path.clone()));
            }
            if let Some((map, _)) = norm_info.get_mut(&key) {
                map.insert(src.target, p.text.clone());
            }
        }
    }

    let all_targets: Vec<AgentTarget> = sources.iter().map(|s| s.target).collect();
    let mut blocks = Vec::new();
    for key in norm_order {
        let Some((variants_map, heading_path)) = norm_info.remove(&key) else {
            continue;
        };
        let present: Vec<AgentTarget> = variants_map.keys().copied().collect();
        let sample = variants_map.values().next().cloned().unwrap_or_default();
        let needs = block_needs_target_isolation(&sample);
        let id = new_block_id();

        if needs {
            // 每个出现的 target 各自 targetOnly
            for (t, text) in variants_map {
                let bid = new_block_id();
                diagnostics.push(PortabilityDiagnostic::needs_adaptation(
                    &bid,
                    "块含 CLI 专属术语，保留来源 targetOnly",
                ));
                blocks.push(InstructionBlock::target_only(
                    bid,
                    t,
                    text,
                    heading_path.clone(),
                    true,
                ));
            }
            continue;
        }

        let all_present = all_targets.iter().all(|t| present.contains(t));
        if all_present
            && variants_map
                .values()
                .all(|v| normalize_for_compare(v) == key)
        {
            // 全部 source 均持有 identical 块且无术语 → Shared
            blocks.push(InstructionBlock::shared(id, sample, heading_path));
        } else if present.len() == 1 {
            let t = present[0];
            let text = variants_map.get(&t).cloned().unwrap_or_default();
            blocks.push(InstructionBlock::target_only(
                id,
                t,
                text,
                heading_path,
                false,
            ));
        } else {
            // H7：部分 source identical（present 真子集）→ 禁止 Shared 注入缺失源
            // Adapted 在 resolve_block_text 会对无 variant 的 target 回退 common，仍会注入；
            // 因此对每个 present target 建独立 targetOnly（文本可相同）。
            for (t, text) in variants_map {
                blocks.push(InstructionBlock::target_only(
                    new_block_id(),
                    t,
                    text,
                    heading_path.clone(),
                    false,
                ));
            }
        }
    }

    ImportClassification {
        document: InstructionDocument {
            relative_key,
            blocks,
        },
        diagnostics,
    }
}

/// 渲染 DiscoveryBeforeEdit 到指定 target（版本化）。
///
/// Business Logic（为什么需要这个函数）:
///     structured intent 必须确定性生成 Claude/Codex/OpenCode 措辞，禁止模型重写。
///
/// Code Logic（这个函数做什么）:
///     match version；未知 version 回落 v1。
pub fn render_discovery_before_edit(
    target: AgentTarget,
    version: u32,
    relative_key: &str,
) -> String {
    let _ = version; // 未来 v2 分支
    match target {
        AgentTarget::Claude => {
            format!(
                "## 分层发现（Claude）\n\n\
在 Read/Edit 之前：\n\
1. 从项目根到本目录（`{relative_key}`）依次阅读各层 `CLAUDE.md`；\n\
2. 更深层规则覆盖更上层；\n\
3. 先用分层代码地图与定向读取，再扩大搜索。\n"
            )
        }
        AgentTarget::Codex => {
            format!(
                "## 分层发现（Codex）\n\n\
在修改本目录（`{relative_key}`）前，按优先级读取：\n\
1. 本层 `AGENTS.override.md`（Hub 受管投影，最高优先）；\n\
2. 本层 `AGENTS.md`；\n\
3. 配置的 project_doc fallback 文件名；\n\
4. 再沿祖先目录同样顺序查找。\n\
先定向读取，再扩大搜索。\n"
            )
        }
        AgentTarget::OpenCode => {
            format!(
                "## 分层发现（OpenCode）\n\n\
在处理本目录（`{relative_key}`）前：\n\
1. 按 managed prelude 列出的相对路径依次读取祖先 `AGENTS.md`；\n\
2. 再应用当前 `AGENTS.md`；\n\
3. 若 `opencode.json` 的 instructions 字段另有路径，一并遵守；\n\
4. 更深层规则覆盖更上层。\n"
            )
        }
    }
}

/// 渲染 OpenCode managed prelude（祖先相对路径，root→parent）。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 不原生拼接祖先链；Hub 写入显式路径列表且不复制祖先正文。
///
/// Code Logic（这个函数做什么）:
///     固定中文 contract + 编号列表；末尾保证换行。
pub fn render_opencode_prelude(ancestor_paths: &[String]) -> String {
    if ancestor_paths.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<!-- 该段由 cc-partner OpenCode adapter 生成，用户正文从下一标题开始 -->\n\
在处理本目录前，依次读取并遵守：\n",
    );
    for (i, p) in ancestor_paths.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, p));
    }
    out.push_str("然后应用当前 AGENTS.md；更深层规则覆盖更上层规则。\n");
    out
}

/// 从 relative 目录计算到祖先的 `../` 相对 AGENTS.md 路径（root→parent）。
///
/// Business Logic（为什么需要这个函数）:
///     nested `src-tauri/src/net` 需要 `../../../AGENTS.md` … `../AGENTS.md`。
///
/// Code Logic（这个函数做什么）:
///     按 depth 生成；existing_ancestors 过滤（相对项目根的目录键，空串=根）。
pub fn ancestor_agent_paths_for_directory(
    directory_relative: &str,
    existing_ancestor_dirs: &[String],
) -> Vec<String> {
    let dir = directory_relative.trim_matches('/');
    if dir.is_empty() {
        return vec![];
    }
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    let depth = parts.len();
    let mut out = Vec::new();
    // root-to-parent：i=0 为根，i=depth-1 为父
    for i in 0..depth {
        let ancestor_key = if i == 0 {
            String::new()
        } else {
            parts[..i].join("/")
        };
        if !existing_ancestor_dirs
            .iter()
            .any(|d| normalize_rel(d) == normalize_rel(&ancestor_key))
        {
            continue;
        }
        let ups = depth - i;
        let mut rel = String::new();
        for _ in 0..ups {
            rel.push_str("../");
        }
        rel.push_str("AGENTS.md");
        out.push(rel);
    }
    out
}

/// 编译文档到目标文件字节。
///
/// Business Logic（为什么需要这个函数）:
///     Claude/Codex/OpenCode 文件名与 OpenCode prelude 不同；用户 body 在 shared 时应对齐。
///
/// Code Logic（这个函数做什么）:
///     拼 target 可见块（structured intent 走 renderer）；OpenCode 前置 prelude；填 block_map。
pub fn compile_render(
    document: &InstructionDocument,
    target: AgentTarget,
    context: &InstructionRenderContext,
) -> CompiledRenderedInstruction {
    let file_name = match target {
        AgentTarget::Claude => "CLAUDE.md",
        AgentTarget::Codex => "AGENTS.override.md",
        AgentTarget::OpenCode => "AGENTS.md",
    }
    .to_string();

    let mut diagnostics = Vec::new();
    let mut managed_prefix = String::new();
    if target == AgentTarget::OpenCode && !context.ancestor_agent_paths.is_empty() {
        managed_prefix = render_opencode_prelude(&context.ancestor_agent_paths);
        if !managed_prefix.is_empty() && !managed_prefix.ends_with('\n') {
            managed_prefix.push('\n');
        }
        // prelude 与用户正文之间保留一个空行分隔（若 prelude 已以 \n 结束则再加 \n）
        if !managed_prefix.ends_with("\n\n") {
            managed_prefix.push('\n');
        }
    }
    let managed_prefix_len = managed_prefix.len();

    let mut body = String::new();
    let mut block_map: Vec<RenderedBlockRange> = Vec::new();

    if managed_prefix_len > 0 {
        block_map.push(RenderedBlockRange {
            block_id: None,
            start: 0,
            end: managed_prefix_len,
            managed: true,
            content_hash: sha256_hex(managed_prefix.as_bytes()),
        });
    }

    let mut first_body = true;
    for block in &document.blocks {
        if block.needs_adaptation {
            diagnostics.push(PortabilityDiagnostic::needs_adaptation(
                &block.id,
                "块标记 needsAdaptation",
            ));
        }

        let text = resolve_block_text(block, target, &document.relative_key);
        let Some(text) = text else {
            continue;
        };
        if text.is_empty() {
            continue;
        }

        if !first_body {
            body.push_str("\n\n");
        }
        first_body = false;

        let start = managed_prefix_len + body.len();
        body.push_str(&text);
        let end = managed_prefix_len + body.len();
        block_map.push(RenderedBlockRange {
            block_id: Some(block.id.clone()),
            start,
            end,
            managed: false,
            content_hash: sha256_hex(text.as_bytes()),
        });
    }

    let mut bytes = Vec::with_capacity(managed_prefix_len + body.len());
    bytes.extend_from_slice(managed_prefix.as_bytes());
    bytes.extend_from_slice(body.as_bytes());

    CompiledRenderedInstruction {
        target,
        file_name,
        bytes,
        block_map,
        managed_prefix_len,
        diagnostics,
    }
}

/// 解析块在目标上的最终文本。
///
/// Business Logic: structured intent 走版本化 renderer；否则 variant/common。
/// Code Logic: intent 优先于 variant 文本（若无 variant）；variant 覆盖 common。
fn resolve_block_text(
    block: &InstructionBlock,
    target: AgentTarget,
    relative_key: &str,
) -> Option<String> {
    if let Some(v) = block.variants.get(&target) {
        return Some(v.clone());
    }
    if let Some(StructuredInstructionIntent::DiscoveryBeforeEdit { version }) =
        &block.structured_intent
    {
        // targetOnly intent 仅当 source 匹配或 mode 非 targetOnly
        if block.mode == InstructionBlockMode::TargetOnly && block.source_target != Some(target) {
            return None;
        }
        return Some(render_discovery_before_edit(target, *version, relative_key));
    }
    match block.mode {
        InstructionBlockMode::Shared | InstructionBlockMode::Adapted => {
            block.common_markdown.clone()
        }
        InstructionBlockMode::TargetOnly => None,
    }
}

/// 比较用规范化：统一换行、去掉末尾空白行差异但保留内部。
fn normalize_for_compare(s: &str) -> String {
    let mut t = s.replace("\r\n", "\n").replace('\r', "\n");
    while t.ends_with('\n') {
        t.pop();
    }
    t
}

fn normalize_rel(s: &str) -> String {
    s.trim().trim_matches('/').to_string()
}

/// 保留行尾切分行（最后一行可无换行）。
fn split_lines_keep_ends(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn detect_fence_open(trimmed_start: &str) -> Option<String> {
    let t = trimmed_start;
    if t.starts_with("```") {
        let ticks = t.chars().take_while(|c| *c == '`').count();
        if ticks >= 3 {
            return Some("`".repeat(ticks));
        }
    }
    if t.starts_with("~~~") {
        let ticks = t.chars().take_while(|c| *c == '~').count();
        if ticks >= 3 {
            return Some("~".repeat(ticks));
        }
    }
    None
}

fn parse_atx_heading(line: &str) -> Option<(usize, String)> {
    let t = line.trim_end();
    if !t.starts_with('#') {
        return None;
    }
    let mut level = 0usize;
    for c in t.chars() {
        if c == '#' {
            level += 1;
        } else {
            break;
        }
    }
    if level == 0 || level > 6 {
        return None;
    }
    let rest = t[level..].to_string();
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        // ##not a heading
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim().to_string();
    Some((level, title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::InstructionRenderContext;
    use std::path::PathBuf;

    #[test]
    fn import_non_root_only_claude_shares_ordinary_blocks() {
        let src = TargetMarkdownSource {
            target: AgentTarget::Claude,
            markdown: "本目录负责网络层\n".into(),
        };
        let result = classify_import(
            "src-tauri/src/net",
            ImportScopeContext::project_subdirectory(),
            &[src],
        );
        assert_eq!(result.document.blocks.len(), 1);
        assert_eq!(result.document.blocks[0].mode, InstructionBlockMode::Shared);
        assert_eq!(
            result.document.blocks[0]
                .common_markdown
                .as_deref()
                .map(str::trim),
            Some("本目录负责网络层")
        );
    }

    #[test]
    fn import_project_root_only_claude_is_target_only() {
        let src = TargetMarkdownSource {
            target: AgentTarget::Claude,
            markdown: "# Root rules\n\nDo not commit secrets.\n".into(),
        };
        let result = classify_import("", ImportScopeContext::project_root(), &[src]);
        assert!(!result.document.blocks.is_empty());
        assert!(result
            .document
            .blocks
            .iter()
            .all(|b| b.mode == InstructionBlockMode::TargetOnly));
        assert!(result
            .document
            .blocks
            .iter()
            .all(|b| b.source_target == Some(AgentTarget::Claude)));
    }

    #[test]
    fn import_identical_three_files_are_shared() {
        let body = "## Shared\n\nhello from all\n";
        let sources = [
            TargetMarkdownSource {
                target: AgentTarget::Claude,
                markdown: body.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::Codex,
                markdown: body.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::OpenCode,
                markdown: body.into(),
            },
        ];
        let result = classify_import("src", ImportScopeContext::project_subdirectory(), &sources);
        assert!(result
            .document
            .blocks
            .iter()
            .all(|b| b.mode == InstructionBlockMode::Shared));
    }

    #[test]
    fn import_different_files_share_identical_blocks_only() {
        let claude = "# A\n\ncommon para\n\n# ClaudeOnly\n\nclaude secret\n";
        let codex = "# A\n\ncommon para\n\n# CodexOnly\n\ncodex secret\n";
        let opencode = "# A\n\ncommon para\n";
        let sources = [
            TargetMarkdownSource {
                target: AgentTarget::Claude,
                markdown: claude.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::Codex,
                markdown: codex.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::OpenCode,
                markdown: opencode.into(),
            },
        ];
        let result = classify_import("pkg", ImportScopeContext::project_subdirectory(), &sources);
        let shared: Vec<_> = result
            .document
            .blocks
            .iter()
            .filter(|b| b.mode == InstructionBlockMode::Shared)
            .collect();
        let target_only: Vec<_> = result
            .document
            .blocks
            .iter()
            .filter(|b| b.mode == InstructionBlockMode::TargetOnly)
            .collect();
        assert!(
            shared.iter().any(|b| b
                .common_markdown
                .as_deref()
                .map(|t| t.contains("common para"))
                .unwrap_or(false)),
            "shared={shared:?}"
        );
        assert!(target_only.iter().any(|b| {
            b.source_target == Some(AgentTarget::Claude)
                && b.variants
                    .get(&AgentTarget::Claude)
                    .map(|t| t.contains("claude secret"))
                    .unwrap_or(false)
        }));
        assert!(target_only.iter().any(|b| {
            b.source_target == Some(AgentTarget::Codex)
                && b.variants
                    .get(&AgentTarget::Codex)
                    .map(|t| t.contains("codex secret"))
                    .unwrap_or(false)
        }));
    }

    #[test]
    fn import_cli_terms_become_target_only_with_needs_adaptation() {
        let markdown = "Before Edit, call Read on CLAUDE.md and handle PreToolUse hook event.\n";
        let src = TargetMarkdownSource {
            target: AgentTarget::Claude,
            markdown: markdown.into(),
        };
        let result = classify_import(
            "src/util",
            ImportScopeContext::project_subdirectory(),
            &[src],
        );
        assert_eq!(result.document.blocks.len(), 1);
        let b = &result.document.blocks[0];
        assert_eq!(b.mode, InstructionBlockMode::TargetOnly);
        assert!(b.needs_adaptation);
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == "needsAdaptation"));
    }

    /// H6：三方 identical 全文含 CLAUDE.md 术语 → 每 target 一份 targetOnly，禁止 Shared。
    #[test]
    fn import_identical_three_files_with_cli_terms_are_target_only_per_source() {
        let body = "Always read CLAUDE.md before Edit and respect PreToolUse hooks.\n";
        let sources = [
            TargetMarkdownSource {
                target: AgentTarget::Claude,
                markdown: body.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::Codex,
                markdown: body.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::OpenCode,
                markdown: body.into(),
            },
        ];
        let result = classify_import("src", ImportScopeContext::project_subdirectory(), &sources);
        assert!(
            result
                .document
                .blocks
                .iter()
                .all(|b| b.mode == InstructionBlockMode::TargetOnly),
            "blocks={:?}",
            result.document.blocks
        );
        assert!(
            result.document.blocks.iter().all(|b| b.needs_adaptation),
            "all CLI-term blocks need adaptation"
        );
        for t in [
            AgentTarget::Claude,
            AgentTarget::Codex,
            AgentTarget::OpenCode,
        ] {
            assert!(
                result
                    .document
                    .blocks
                    .iter()
                    .any(|b| b.source_target == Some(t)
                        && b.variants
                            .get(&t)
                            .map(|s| s.contains("CLAUDE.md"))
                            .unwrap_or(false)),
                "missing targetOnly for {t:?}"
            );
        }
        assert!(
            !result
                .document
                .blocks
                .iter()
                .any(|b| b.mode == InstructionBlockMode::Shared),
            "must not keep Shared for multi-source CLI terms"
        );
        assert!(result
            .diagnostics
            .iter()
            .any(|d| d.code == "needsAdaptation"));
    }

    /// H7：Claude+Codex 共有段落、OpenCode 缺失 → 不得 Shared 注入 OpenCode 渲染。
    #[test]
    fn import_partial_shared_paragraph_does_not_inject_into_missing_target() {
        let claude = "# A\n\nshared by two only\n\n# ClaudeOnly\n\nclaude secret\n";
        let codex = "# A\n\nshared by two only\n\n# CodexOnly\n\ncodex secret\n";
        let opencode = "# A\n\nopencode different para\n";
        let sources = [
            TargetMarkdownSource {
                target: AgentTarget::Claude,
                markdown: claude.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::Codex,
                markdown: codex.into(),
            },
            TargetMarkdownSource {
                target: AgentTarget::OpenCode,
                markdown: opencode.into(),
            },
        ];
        let result = classify_import("pkg", ImportScopeContext::project_subdirectory(), &sources);

        // 不得出现会把 "shared by two only" 投影给全部 target 的 Shared
        assert!(
            !result.document.blocks.iter().any(|b| {
                b.mode == InstructionBlockMode::Shared
                    && b.common_markdown
                        .as_deref()
                        .map(|t| t.contains("shared by two only"))
                        .unwrap_or(false)
            }),
            "partial identical must not become Shared; blocks={:?}",
            result.document.blocks
        );

        let ctx = InstructionRenderContext {
            project_root: None,
            directory_relative: Some("pkg".into()),
            ancestor_agent_paths: vec![],
        };
        let oc = compile_render(&result.document, AgentTarget::OpenCode, &ctx);
        assert!(
            !oc.content_str().contains("shared by two only"),
            "OpenCode render must not contain paragraph missing from its source: {}",
            oc.content_str()
        );
        let claude_out = compile_render(&result.document, AgentTarget::Claude, &ctx);
        let codex_out = compile_render(&result.document, AgentTarget::Codex, &ctx);
        assert!(claude_out.content_str().contains("shared by two only"));
        assert!(codex_out.content_str().contains("shared by two only"));
    }

    #[test]
    fn nested_directory_render_user_bodies_match_and_opencode_has_prelude() {
        let body = "本目录负责 Rust 网络层";
        let doc = InstructionDocument {
            relative_key: "src-tauri/src/net".into(),
            blocks: vec![InstructionBlock::shared(new_block_id(), body, vec![])],
        };
        let ancestors = ancestor_agent_paths_for_directory(
            "src-tauri/src/net",
            &["", "src-tauri", "src-tauri/src"].map(String::from),
        );
        assert_eq!(
            ancestors,
            vec![
                "../../../AGENTS.md".to_string(),
                "../../AGENTS.md".to_string(),
                "../AGENTS.md".to_string(),
            ]
        );
        let ctx = InstructionRenderContext {
            project_root: Some(PathBuf::from("/repo")),
            directory_relative: Some("src-tauri/src/net".into()),
            ancestor_agent_paths: ancestors,
        };

        let claude = compile_render(&doc, AgentTarget::Claude, &ctx);
        let codex = compile_render(&doc, AgentTarget::Codex, &ctx);
        let oc = compile_render(&doc, AgentTarget::OpenCode, &ctx);

        assert_eq!(claude.file_name, "CLAUDE.md");
        assert_eq!(codex.file_name, "AGENTS.override.md");
        assert_eq!(oc.file_name, "AGENTS.md");

        assert_eq!(claude.user_body().trim(), body);
        assert_eq!(codex.user_body().trim(), body);
        assert_eq!(oc.user_body().trim(), body);
        // byte-identical user bodies
        assert_eq!(claude.user_body(), codex.user_body());
        assert_eq!(codex.user_body(), oc.user_body());

        assert!(oc.managed_prefix_len > 0);
        assert_eq!(claude.managed_prefix_len, 0);
        assert_eq!(codex.managed_prefix_len, 0);

        let prelude = oc.managed_prelude().expect("prelude");
        assert!(prelude.contains("../../../AGENTS.md"));
        assert!(prelude.contains("../../AGENTS.md"));
        assert!(prelude.contains("../AGENTS.md"));
        // 不复制父正文
        assert!(!prelude.contains("父目录"));
        assert!(!oc.content_str().contains("父目录负责"));
        // 祖先路径顺序 root-to-parent
        let p1 = prelude.find("../../../AGENTS.md").unwrap();
        let p2 = prelude.find("../../AGENTS.md").unwrap();
        let p3 = prelude.find("../AGENTS.md").unwrap();
        assert!(p1 < p2 && p2 < p3);
    }

    #[test]
    fn discovery_intent_renders_three_targets() {
        let text_c = render_discovery_before_edit(AgentTarget::Claude, 1, "src");
        let text_x = render_discovery_before_edit(AgentTarget::Codex, 1, "src");
        let text_o = render_discovery_before_edit(AgentTarget::OpenCode, 1, "src");
        assert!(text_c.contains("CLAUDE.md"));
        assert!(text_x.contains("AGENTS.override.md"));
        assert!(text_o.contains("opencode.json") || text_o.contains("AGENTS.md"));
        assert_ne!(text_c, text_x);
    }

    #[test]
    fn parse_preserves_fenced_code_and_headings() {
        let md = "# H1\n\npara\n\n```rust\nfn main() {}\n```\n\n## H2\n\nmore\n";
        let blocks = parse_markdown_blocks(md);
        assert!(blocks.len() >= 3, "blocks={blocks:?}");
        assert!(blocks.iter().any(|b| b.text.contains("```rust")));
        assert!(blocks.iter().any(|b| b.starts_with_heading));
    }
}
