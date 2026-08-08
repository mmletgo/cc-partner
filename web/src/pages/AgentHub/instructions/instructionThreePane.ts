/**
 * 提示词三栏 pure 状态机。
 *
 * Business Logic（为什么需要这个函数模块）:
 *   打开提示词展示用户已有原始文件 ③，并从后端 canonical hydrate 持久化块 ①（含 per-agent
 *   variants）；预览 ② 由块按当前 agent 合成。块/原始可独立脏；同步前在双脏分歧时强制选基线。
 *   块草稿与 InstructionBlockDto 同构，apply 投影时保留 mode/variants，服务三 agent 适配。
 *
 * Code Logic（做什么）:
 *   无 React、无 api 的不可变 state 变换：初始加载/hydrate、markdown fallback 解析、
 *   per-agent 预览合成、同步内容 resolve，以及原文/块编辑 dirty 辅助。
 */

import type { AgentTarget, InstructionBlockDto } from '@/lib/types/agentHub';

/** 块草稿：与 InstructionBlockDto 同构（mode/variants/headingPath/sourceTarget/needsAdaptation）。 */
export interface InstructionBlockDraft {
  id: string;
  mode: 'shared' | 'adapted' | 'targetOnly';
  commonMarkdown: string;
  variants: Partial<Record<AgentTarget, string>>;
  headingPath: string[];
  sourceTarget: AgentTarget | null;
  needsAdaptation: boolean;
}

/**
 * 三栏编辑器完整状态。
 *
 * Business Logic: ① 块 ② 当前 agent 合成预览 ③ 原始文件 + 双脏 / 外部漂移标记。
 * Code Logic: 纯数据；dirty 与内容由 helpers 维护。
 */
export interface InstructionThreePaneState {
  originalPath: string | null;
  originalText: string;
  blocks: InstructionBlockDraft[];
  previewText: string;
  blocksDirty: boolean;
  originalDirty: boolean;
  externalDrift: boolean;
}

/** 同步写入选用的内容基线。 */
export type SyncBaseline = 'blocks' | 'original';

/** InstructionBlockDto → InstructionBlockDraft。 */
export function dtoToDraft(dto: InstructionBlockDto): InstructionBlockDraft {
  return {
    id: dto.id,
    mode: dto.mode,
    commonMarkdown: dto.commonMarkdown,
    variants: { ...(dto.variants ?? {}) },
    headingPath: dto.headingPath ? [...dto.headingPath] : [],
    sourceTarget: dto.sourceTarget ?? null,
    needsAdaptation: dto.needsAdaptation ?? false,
  };
}

/** InstructionBlockDraft → InstructionBlockDto（空 variants/headingPath 归一为 null）。 */
export function draftToDto(draft: InstructionBlockDraft): InstructionBlockDto {
  return {
    id: draft.id,
    mode: draft.mode,
    commonMarkdown: draft.commonMarkdown,
    variants: Object.keys(draft.variants).length > 0 ? { ...draft.variants } : null,
    headingPath: draft.headingPath.length > 0 ? [...draft.headingPath] : null,
    sourceTarget: draft.sourceTarget,
    needsAdaptation: draft.needsAdaptation,
  };
}

/**
 * 单块按 target 取渲染正文。
 *
 * Business Logic: 投影时 variant 命中优先；shared/adapted 取 common；targetOnly 无 variant 则空。
 */
export function resolveBlockText(
  block: InstructionBlockDraft,
  target: AgentTarget,
): string | null {
  const variant = block.variants[target];
  if (typeof variant === 'string') return variant;
  switch (block.mode) {
    case 'shared':
    case 'adapted':
      return block.commonMarkdown;
    case 'targetOnly':
      return null;
  }
}

/** 块列表按 target 合成纯文本（预览 / 同步基线）。 */
export function joinBlocksForTarget(
  blocks: InstructionBlockDraft[],
  target: AgentTarget,
): string {
  return blocks
    .map((block) => resolveBlockText(block, target))
    .filter((text): text is string => typeof text === 'string' && text.length > 0)
    .join('\n\n');
}

/**
 * Business Logic（为什么需要）:
 *   打开提示词时读盘展示原文 ③，并从后端 canonical hydrate 持久化块 ①；预览 ② 按 agent 合成。
 *   无持久化块时 ① 空，由用户显式「从原始重新解析」或手动加块。
 *
 * Code Logic（做什么）:
 *   写入 path/text；可选 blocks 浅拷贝 hydrate；preview 按 agent 合成；脏标记与 drift 全 false。
 */
export function initialThreePaneFromDisk(
  path: string | null,
  text: string,
  blocks?: InstructionBlockDraft[] | null,
  agent?: AgentTarget,
): InstructionThreePaneState {
  const hydrated =
    blocks && blocks.length > 0 ? blocks.map((block) => ({ ...block })) : [];
  return {
    originalPath: path,
    originalText: text,
    blocks: hydrated,
    previewText: agent ? joinBlocksForTarget(hydrated, agent) : '',
    blocksDirty: false,
    originalDirty: false,
    externalDrift: false,
  };
}

/**
 * Business Logic（为什么需要）:
 *   无持久化块或用户显式「从原始重新解析」时，把 ③ 原文按 `## ` 分节降级为 shared 块。
 *
 * Code Logic（做什么）:
 *   parseMarkdownSections 产 shared 块；再 recomputePreview；块来自原文故 blocksDirty=false。
 */
export function parseBlocksFromOriginal(
  state: InstructionThreePaneState,
  agent: AgentTarget,
): InstructionThreePaneState {
  const blocks = parseMarkdownSections(state.originalText);
  return recomputePreview(
    {
      ...state,
      blocks,
      blocksDirty: false,
    },
    agent,
  );
}

/**
 * Business Logic（为什么需要）:
 *   预览栏只读，始终由块按当前 agent 合成；块编辑后即时跟随。
 *
 * Code Logic（做什么）:
 *   joinBlocksForTarget(blocks, agent) 写入 previewText；不改 dirty 标记。
 */
export function recomputePreview(
  state: InstructionThreePaneState,
  agent: AgentTarget,
): InstructionThreePaneState {
  return {
    ...state,
    previewText: joinBlocksForTarget(state.blocks, agent),
  };
}

/**
 * Business Logic（为什么需要）:
 *   「同步到原生文件」前决定写入内容与基线；双脏且内容分歧时强制用户选择。
 *
 * Code Logic（做什么）:
 *   - 双脏且 blocks 合成 ≠ original → dual_dirty_conflict
 *   - 仅 blocksDirty → baseline blocks + preview
 *   - 仅 originalDirty → baseline original
 *   - 皆未脏：有 original 优先 original，否则非空 blocks → blocks
 *   - 两侧皆空 → empty
 */
export function resolveSyncContent(
  state: InstructionThreePaneState,
  agent: AgentTarget,
):
  | { ok: true; baseline: SyncBaseline; content: string }
  | { ok: false; reason: 'dual_dirty_conflict' | 'empty' } {
  const blocksContent = joinBlocksForTarget(state.blocks, agent);
  const originalContent = state.originalText;
  const blocksEmpty = !hasMeaningfulContent(blocksContent) && state.blocks.length === 0;
  const originalEmpty = !hasMeaningfulContent(originalContent);

  if (state.blocksDirty && state.originalDirty) {
    if (normalizeContent(blocksContent) !== normalizeContent(originalContent)) {
      return { ok: false, reason: 'dual_dirty_conflict' };
    }
    if (originalEmpty && blocksEmpty) {
      return { ok: false, reason: 'empty' };
    }
    // 内容一致：优先原始基线
    return {
      ok: true,
      baseline: originalEmpty ? 'blocks' : 'original',
      content: originalEmpty ? blocksContent : originalContent,
    };
  }

  if (state.blocksDirty && !state.originalDirty) {
    const content = hasMeaningfulContent(state.previewText)
      ? state.previewText
      : blocksContent;
    if (!hasMeaningfulContent(content) && state.blocks.length === 0) {
      return { ok: false, reason: 'empty' };
    }
    return { ok: true, baseline: 'blocks', content };
  }

  if (state.originalDirty && !state.blocksDirty) {
    if (originalEmpty) {
      return { ok: false, reason: 'empty' };
    }
    return { ok: true, baseline: 'original', content: originalContent };
  }

  // neither dirty
  if (!originalEmpty) {
    return { ok: true, baseline: 'original', content: originalContent };
  }
  if (state.blocks.length > 0 || hasMeaningfulContent(blocksContent)) {
    return {
      ok: true,
      baseline: 'blocks',
      content: hasMeaningfulContent(state.previewText) ? state.previewText : blocksContent,
    };
  }
  return { ok: false, reason: 'empty' };
}

/**
 * Business Logic（为什么需要）:
 *   原始栏编辑使 ③ 脏，同步前可选用原文基线。
 *
 * Code Logic（做什么）:
 *   更新 originalText 并置 originalDirty=true。
 */
export function updateOriginalText(
  state: InstructionThreePaneState,
  text: string,
): InstructionThreePaneState {
  return {
    ...state,
    originalText: text,
    originalDirty: true,
  };
}

/**
 * Business Logic（为什么需要）:
 *   块栏单块编辑（commonMarkdown / variants / mode 等）使 ① 脏，② 必须跟随重算。
 *
 * Code Logic（做什么）:
 *   按 id 浅合并 patch；未命中则原样返回；命中则 blocksDirty + recomputePreview。
 */
export function updateBlock(
  state: InstructionThreePaneState,
  id: string,
  patch: Partial<Omit<InstructionBlockDraft, 'id'>>,
  agent: AgentTarget,
): InstructionThreePaneState {
  const index = state.blocks.findIndex((block) => block.id === id);
  if (index < 0) {
    return state;
  }
  const nextBlocks = state.blocks.slice();
  const current = nextBlocks[index]!;
  nextBlocks[index] = {
    ...current,
    ...patch,
    id: current.id,
  };
  return recomputePreview(
    {
      ...state,
      blocks: nextBlocks,
      blocksDirty: true,
    },
    agent,
  );
}

/**
 * Business Logic（为什么需要）:
 *   用户可手填块（不经原始解析）；② 跟随。
 *
 * Code Logic（做什么）:
 *   追加 block，blocksDirty=true，recomputePreview。
 */
export function addBlock(
  state: InstructionThreePaneState,
  block: InstructionBlockDraft,
  agent: AgentTarget,
): InstructionThreePaneState {
  return recomputePreview(
    {
      ...state,
      blocks: [...state.blocks, block],
      blocksDirty: true,
    },
    agent,
  );
}

// ── internal helpers ──────────────────────────────────────────────

/**
 * Business Logic: 空或仅空白视为无有效同步内容。
 * Code Logic: trim 后长度为 0。
 */
function hasMeaningfulContent(text: string): boolean {
  return text.trim().length > 0;
}

/**
 * Business Logic: 双脏比较时忽略尾随空白差异，减少伪冲突。
 * Code Logic: trimEnd + 统一换行。
 */
function normalizeContent(text: string): string {
  return text.replace(/\r\n/g, '\n').trimEnd();
}

/**
 * Business Logic: 无持久化块时把原文降级为 shared 块，便于「从原始重新解析」得到可编辑块。
 * Code Logic: 按行匹配 `^##\s+`；标题进 headingPath，正文进 commonMarkdown；空原文 → []。
 */
function parseMarkdownSections(text: string): InstructionBlockDraft[] {
  if (!hasMeaningfulContent(text)) {
    return [];
  }

  const lines = text.replace(/\r\n/g, '\n').split('\n');
  type Acc = { title: string; bodyLines: string[] };
  const sections: Acc[] = [];
  let current: Acc | null = null;

  for (const line of lines) {
    const heading = /^##\s+(.*)$/.exec(line);
    if (heading) {
      if (current) {
        sections.push(current);
      }
      current = { title: heading[1]!.trim(), bodyLines: [] };
      continue;
    }
    if (!current) {
      current = { title: '', bodyLines: [line] };
    } else {
      current.bodyLines.push(line);
    }
  }
  if (current) {
    sections.push(current);
  }

  return sections.map((section, index) => ({
    id: `block-${index + 1}`,
    mode: 'shared' as const,
    commonMarkdown: trimSectionBody(section.bodyLines),
    variants: {} as Partial<Record<AgentTarget, string>>,
    headingPath: section.title.length > 0 ? [section.title] : [],
    sourceTarget: null,
    needsAdaptation: false,
  }));
}

/** 去掉分节 body 首尾空行，保留中间结构。 */
function trimSectionBody(bodyLines: string[]): string {
  let start = 0;
  let end = bodyLines.length;
  while (start < end && bodyLines[start]!.trim() === '') {
    start += 1;
  }
  while (end > start && bodyLines[end - 1]!.trim() === '') {
    end -= 1;
  }
  return bodyLines.slice(start, end).join('\n');
}
