/**
 * 提示词三栏 pure 状态机。
 *
 * Business Logic（为什么需要）:
 *   打开提示词必须展示用户已有原始文件，但禁止自动 parse 成块；
 *   用户显式「从原始重新解析」或手填块后，块 / 合成预览 / 原始三源可独立脏；
 *   同步前需在双脏分歧时强制选基线，避免静默覆盖。
 *
 * Code Logic（做什么）:
 *   无 React、无 api 的不可变 state 变换：初始加载、markdown 分节解析、
 *   预览合成、同步内容 resolve，以及原文/块编辑 dirty 辅助。
 */

/** 块草稿：公共 / 当前 Agent 专属 / 需跨 Agent 适配。 */
export interface InstructionBlockDraft {
  id: string;
  mode: 'shared' | 'targetOnly' | 'needsAdaptation';
  title: string;
  body: string;
}

/**
 * 三栏编辑器完整状态。
 *
 * Business Logic: ① 块 ② 合成预览 ③ 原始文件 + 双脏 / 外部漂移标记。
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

/**
 * Business Logic（为什么需要）:
 *   打开提示词时必须读盘展示原文，且块与预览为空（Spec 硬规则：禁止自动 parse）。
 *
 * Code Logic（做什么）:
 *   写入 path/text；blocks=[]、preview=''、脏标记与 externalDrift 全 false。
 */
export function initialThreePaneFromDisk(
  path: string | null,
  text: string,
): InstructionThreePaneState {
  return {
    originalPath: path,
    originalText: text,
    blocks: [],
    previewText: '',
    blocksDirty: false,
    originalDirty: false,
    externalDrift: false,
  };
}

/**
 * Business Logic（为什么需要）:
 *   用户在原始栏显式点「从原始重新解析块」时才填充 ①②；打开 Tab 永不调用。
 *
 * Code Logic（做什么）:
 *   按 `## ` 标题拆 markdown 为 blocks；空原文 → 空块；再 recomputePreview；
 *   块来自原文故 blocksDirty=false（不改变 originalDirty）。
 */
export function parseBlocksFromOriginal(
  state: InstructionThreePaneState,
): InstructionThreePaneState {
  const blocks = parseMarkdownSections(state.originalText);
  return recomputePreview({
    ...state,
    blocks,
    blocksDirty: false,
  });
}

/**
 * Business Logic（为什么需要）:
 *   预览栏只读，始终由块合成；块编辑后即时跟随。
 *
 * Code Logic（做什么）:
 *   将 blocks 拼为 markdown 写入 previewText；不改 dirty 标记。
 */
export function recomputePreview(
  state: InstructionThreePaneState,
): InstructionThreePaneState {
  return {
    ...state,
    previewText: joinBlocksToMarkdown(state.blocks),
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
):
  | { ok: true; baseline: SyncBaseline; content: string }
  | { ok: false; reason: 'dual_dirty_conflict' | 'empty' } {
  const blocksContent = joinBlocksToMarkdown(state.blocks);
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
 *   块栏单块编辑使 ① 脏，② 必须跟随重算。
 *
 * Code Logic（做什么）:
 *   按 id 浅合并 patch；未命中则原样返回；命中则 blocksDirty + recomputePreview。
 */
export function updateBlock(
  state: InstructionThreePaneState,
  id: string,
  patch: Partial<Omit<InstructionBlockDraft, 'id'>>,
): InstructionThreePaneState {
  const index = state.blocks.findIndex((b) => b.id === id);
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
  return recomputePreview({
    ...state,
    blocks: nextBlocks,
    blocksDirty: true,
  });
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
): InstructionThreePaneState {
  return recomputePreview({
    ...state,
    blocks: [...state.blocks, block],
    blocksDirty: true,
  });
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
 * Business Logic: 简单 markdown 分节，便于「从原始重新解析」得到可编辑块。
 * Code Logic: 按行匹配 `^##\s+`；无标题时整篇为一块；空正文 → []。
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
    title: section.title.length > 0 ? section.title : `Section ${index + 1}`,
    body: trimSectionBody(section.bodyLines),
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

/**
 * Business Logic: 预览与块基线同步内容的统一合成格式。
 * Code Logic: 每块 `## title` + 空行 + body，块间双换行。
 */
function joinBlocksToMarkdown(blocks: InstructionBlockDraft[]): string {
  if (blocks.length === 0) {
    return '';
  }
  return blocks
    .map((block) => {
      const title = block.title.trim().length > 0 ? block.title.trim() : 'Untitled';
      const body = block.body.replace(/^\n+|\n+$/g, '');
      return body.length > 0 ? `## ${title}\n\n${body}` : `## ${title}`;
    })
    .join('\n\n');
}
