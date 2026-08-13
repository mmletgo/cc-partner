/**
 * 提示词三栏 pure 状态机。
 *
 * Business Logic（为什么需要这个函数模块）:
 *   打开提示词展示用户已有原始文件 ③，并从后端 canonical hydrate 持久化块 ①（含 per-agent
 *   variants）；预览 ② 由块按当前 agent 合成。块固定为公共/适配/独有三槽（每 mode 最多一个）。
 *   块/原始可独立脏；同步前在双脏分歧时强制选基线。
 *
 * Code Logic（做什么）:
 *   无 React、无 api 的不可变 state 变换：初始加载/hydrate、整篇 shared 解析、三槽 normalize、
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
 * Business Logic: ① 三槽块 ② 当前 agent 合成预览 ③ 原始文件 + 双脏 / 外部漂移标记。
 * Code Logic: 纯数据；dirty 与内容由 helpers 维护。
 */
export interface InstructionThreePaneState {
  originalPath: string | null;
  originalText: string;
  blocks: InstructionBlockDraft[];
  previewText: string;
  blocksDirty: boolean;
  originalDirty: boolean;
  /** Canonical head / inventory snapshot 在当前草稿 lease 期间变化；会阻止 Hub Save。 */
  externalDrift: boolean;
  /** 本机原始来源在当前草稿期间变化；只标 stale，不替换草稿或抬高 CAS base。 */
  sourceDrift: boolean;
}

/** 同步写入选用的内容基线。 */
export type SyncBaseline = 'blocks' | 'original';

/**
 * 三栏动作 busy 种类。
 *
 * Business Logic: 不同按钮不得共用同一 spinner——分析拆解时转圈必须在「分析拆解」上，
 *   不能误挂到「保存三槽」。
 * Code Logic: null = 空闲；actionBusy 由 busyAction!=null 派生，供互斥禁用。
 */
export type InstructionBusyAction = 'save' | 'analyze' | 'adapt' | 'sync' | 'revise';

const MODE_ORDER: InstructionBlockDraft['mode'][] = ['shared', 'adapted', 'targetOnly'];
const AGENT_TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];

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

/**
 * 块列表按 target 合成纯文本（预览 / 同步基线）。
 *
 * Business Logic: 固定顺序 公共 → 适配 → 独有，避免旧多块无序拼接。
 */
export function joinBlocksForTarget(
  blocks: InstructionBlockDraft[],
  target: AgentTarget,
): string {
  const ordered = sortBlocksByMode(blocks);
  return ordered
    .map((block) => resolveBlockText(block, target))
    .filter((text): text is string => typeof text === 'string' && text.length > 0)
    .join('\n\n');
}

/**
 * Business Logic: 旧 N 块 / 混杂 mode 归并为固定三槽（每 mode 至多 1）。
 * Code Logic: 按 mode 分组；common 与各 agent variant 用 \\n\\n 拼接；顺序 shared/adapted/targetOnly。
 */
export function normalizeInstructionBlocks(
  blocks: InstructionBlockDraft[],
): InstructionBlockDraft[] {
  if (blocks.length === 0) return [];

  const byMode: Record<InstructionBlockDraft['mode'], InstructionBlockDraft[]> = {
    shared: [],
    adapted: [],
    targetOnly: [],
  };
  for (const block of blocks) {
    byMode[block.mode].push(block);
  }

  const result: InstructionBlockDraft[] = [];
  for (const mode of MODE_ORDER) {
    const group = byMode[mode];
    if (group.length === 0) continue;
    result.push(mergeModeGroup(mode, group));
  }
  return result;
}

/**
 * Business Logic（为什么需要）:
 *   打开提示词时读盘展示原文 ③，并从后端 canonical hydrate 持久化块 ①；预览 ② 按 agent 合成。
 *   无持久化块时 ① 空，由用户显式「从原始重新解析」或 lane 懒创建。
 *
 * Code Logic（做什么）:
 *   写入 path/text；可选 blocks 浅拷贝后 normalize 为三槽；preview 按 agent 合成。
 */
export function initialThreePaneFromDisk(
  path: string | null,
  text: string,
  blocks?: InstructionBlockDraft[] | null,
  agent?: AgentTarget,
): InstructionThreePaneState {
  const hydrated =
    blocks && blocks.length > 0
      ? normalizeInstructionBlocks(blocks.map((block) => ({ ...block })))
      : [];
  return {
    originalPath: path,
    originalText: text,
    blocks: hydrated,
    previewText: agent ? joinBlocksForTarget(hydrated, agent) : '',
    blocksDirty: false,
    originalDirty: false,
    externalDrift: false,
    sourceDrift: false,
  };
}

/**
 * Business Logic（为什么需要）:
 *   无持久化块或用户显式「从原始重新解析」时，把 ③ 原文整篇降级为单个 shared 公共块。
 *
 * Code Logic（做什么）:
 *   非空原文 → 1 shared；空 → []；再 recomputePreview。
 */
export function parseBlocksFromOriginal(
  state: InstructionThreePaneState,
  agent: AgentTarget,
): InstructionThreePaneState {
  return replaceBlocksFromOriginal(state, agent, true);
}

/**
 * Business Logic（为什么需要）:
 *   成功持久化后重新 hydrate 原始来源只是建立已保存视图，不能伪造一次用户编辑。
 *
 * Code Logic（做什么）:
 *   与显式 parse 使用同一解析规则，但返回 blocksDirty=false。
 */
export function hydrateBlocksFromOriginal(
  state: InstructionThreePaneState,
  agent: AgentTarget,
): InstructionThreePaneState {
  return replaceBlocksFromOriginal(state, agent, false);
}

function replaceBlocksFromOriginal(
  state: InstructionThreePaneState,
  agent: AgentTarget,
  blocksDirty: boolean,
): InstructionThreePaneState {
  const blocks = parseWholeDocumentAsShared(state.originalText);
  return recomputePreview(
    {
      ...state,
      blocks,
      blocksDirty,
    },
    agent,
  );
}

/**
 * Business Logic（为什么需要）:
 *   「原始文件」是整篇正文，不能按 Markdown heading 拆成多个 canonical 块；
 *   选择 original 作为同步基线时，canonical 必须先收到唯一 shared 槽。
 *
 * Code Logic（做什么）:
 *   用给定正文创建单一 shared 草稿；空正文返回空列表，换行与尾部空白按原文解析规则归一。
 */
export function blocksFromOriginalContent(
  content: string,
): InstructionBlockDraft[] {
  return parseWholeDocumentAsShared(content);
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
      blocks: normalizeInstructionBlocks(nextBlocks),
      blocksDirty: true,
    },
    agent,
  );
}

/**
 * Business Logic（为什么需要）:
 *   三槽懒创建：公共/适配/独有 lane 首次编辑时确保对应 mode 块存在。
 *
 * Code Logic（做什么）:
 *   已有 mode 则原样；否则 append 空槽块并 normalize。
 */
export function ensureModeBlock(
  state: InstructionThreePaneState,
  mode: InstructionBlockDraft['mode'],
  agent: AgentTarget,
): InstructionThreePaneState {
  if (state.blocks.some((block) => block.mode === mode)) {
    return state;
  }
  const block = emptyModeBlock(mode, agent);
  return recomputePreview(
    {
      ...state,
      blocks: normalizeInstructionBlocks([...state.blocks, block]),
      blocksDirty: true,
    },
    agent,
  );
}

/**
 * Business Logic（为什么需要）:
 *   用户可手填块（兼容路径）；最终仍 normalize 为三槽。
 *
 * Code Logic（做什么）:
 *   追加 block 后 normalize，blocksDirty=true，recomputePreview。
 */
export function addBlock(
  state: InstructionThreePaneState,
  block: InstructionBlockDraft,
  agent: AgentTarget,
): InstructionThreePaneState {
  return recomputePreview(
    {
      ...state,
      blocks: normalizeInstructionBlocks([...state.blocks, block]),
      blocksDirty: true,
    },
    agent,
  );
}

/**
 * Business Logic: 取指定 mode 的唯一槽（三槽规范下 0 或 1）。
 */
export function findBlockByMode(
  blocks: InstructionBlockDraft[],
  mode: InstructionBlockDraft['mode'],
): InstructionBlockDraft | null {
  return blocks.find((block) => block.mode === mode) ?? null;
}

/**
 * 分析拆解产物：公共 / 当前 agent 适配 / 当前 agent 独有。
 *
 * Business Logic: Claude 把原始文件拆成三部分后，直接覆盖对应三槽（非追加）。
 */
export interface InstructionAnalyzeParts {
  common: string;
  adapted: string;
  exclusive: string;
}

/** 表面层关键词：命中则整段视为混写，只能进 adapted。 */
const SURFACE_MARKERS = [
  'claude.md',
  'agents.md',
  'agents.override.md',
  '~/.claude',
  '~/.codex',
  '~/.config/opencode',
  'claude_config_dir',
  'codex_home',
  'opencode_config',
  'claude code',
  'anthropic',
  'sonnet',
  'opus',
  'haiku',
  'gpt-5',
  'o3-',
  'o4-',
  'cc-partner',
  '.claude/',
  '.codex/',
  'opencode',
  'codex cli',
  'claude cli',
] as const;

/**
 * Business Logic: 产品层强制「语义+表面混写只进适配」；模型偶发把同一段同时放进
 *   common/adapted 时，前端写入前再去重（与后端 normalize_instruction_analyze_parts 对齐）。
 * Code Logic: 按空行分段；表面词 → 并入 adapted；被 adapted 覆盖/高重叠 → 从 common 删除。
 */
export function normalizeAnalyzeParts(parts: InstructionAnalyzeParts): InstructionAnalyzeParts {
  const adaptedBlocks = splitInstructionBlocks(parts.adapted);
  const exclusiveBlocks = splitInstructionBlocks(parts.exclusive);
  const commonBlocks = splitInstructionBlocks(parts.common);
  const adaptedFull = parts.adapted;

  const keptCommon: string[] = [];
  for (const block of commonBlocks) {
    if (blockHasSurfaceMarkers(block)) {
      pushUniqueInstructionBlock(adaptedBlocks, block);
      continue;
    }
    if (blockCoveredByAdapted(block, adaptedBlocks, adaptedFull)) {
      continue;
    }
    keptCommon.push(block);
  }

  const keptExclusive: string[] = [];
  const joinedAdapted = joinInstructionBlocks(adaptedBlocks);
  for (const block of exclusiveBlocks) {
    if (
      blockCoveredByAdapted(block, adaptedBlocks, adaptedFull) ||
      blockCoveredByAdapted(block, adaptedBlocks, joinedAdapted)
    ) {
      continue;
    }
    keptExclusive.push(block);
  }

  return {
    common: joinInstructionBlocks(keptCommon),
    adapted: joinInstructionBlocks(adaptedBlocks),
    exclusive: joinInstructionBlocks(keptExclusive),
  };
}

function splitInstructionBlocks(text: string): string[] {
  return text
    .replace(/\r\n/g, '\n')
    .split(/\n\n+/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
}

function joinInstructionBlocks(blocks: string[]): string {
  return blocks
    .map((block) => block.trimEnd())
    .filter((block) => block.length > 0)
    .join('\n\n');
}

function normalizeInstructionWs(text: string): string {
  return text.split(/\s+/).filter(Boolean).join(' ').toLowerCase();
}

function blockHasSurfaceMarkers(block: string): boolean {
  const lower = block.toLowerCase();
  return SURFACE_MARKERS.some((marker) => lower.includes(marker));
}

function instructionWordSet(text: string): Set<string> {
  const words = text
    .split(/\s+/)
    .map((word) => word.replace(/[^a-zA-Z0-9_-]/g, '').toLowerCase())
    .filter((word) => word.length > 0);
  return new Set(words);
}

function jaccardSimilarity(a: Set<string>, b: Set<string>): number {
  if (a.size === 0 && b.size === 0) return 1;
  let inter = 0;
  for (const word of a) {
    if (b.has(word)) inter += 1;
  }
  const union = a.size + b.size - inter;
  return union === 0 ? 0 : inter / union;
}

function blockCoveredByAdapted(
  commonBlock: string,
  adaptedBlocks: string[],
  adaptedFull: string,
): boolean {
  const cn = normalizeInstructionWs(commonBlock);
  if (!cn) return true;
  const an = normalizeInstructionWs(adaptedFull);
  if (an && an.includes(cn)) return true;
  const cset = instructionWordSet(commonBlock);
  for (const ab of adaptedBlocks) {
    const abn = normalizeInstructionWs(ab);
    if (abn === cn) return true;
    const aset = instructionWordSet(ab);
    const j = jaccardSimilarity(cset, aset);
    if (j >= 0.82) {
      const cl = cn.length;
      const al = abn.length;
      if (cl <= Math.floor((al * 12) / 10) + 48) return true;
    }
    if (cset.size > 0) {
      let covered = 0;
      for (const word of cset) {
        if (aset.has(word)) covered += 1;
      }
      if (covered / cset.size >= 0.9 && cset.size >= 4) return true;
    }
  }
  const asetAll = instructionWordSet(adaptedFull);
  if (cset.size > 0 && asetAll.size > 0) {
    let covered = 0;
    for (const word of cset) {
      if (asetAll.has(word)) covered += 1;
    }
    if (covered / cset.size >= 0.92 && cset.size >= 6) return true;
  }
  return false;
}

function pushUniqueInstructionBlock(blocks: string[], block: string): void {
  const bn = normalizeInstructionWs(block);
  if (!bn) return;
  if (blocks.some((existing) => normalizeInstructionWs(existing) === bn)) return;
  blocks.push(block);
}

/**
 * Business Logic: 把分析拆解结果直接覆盖现有三槽正文（公共/适配/独有），不追加。
 * Code Logic: 先 normalize 混写/重叠 → ensure 三 mode → 各槽写新值；适配/独有写 variants[agent]。
 *   拆解结果全空时保留现状（避免 LLM 空响应误清空）。
 */
export function replaceAnalyzedParts(
  state: InstructionThreePaneState,
  parts: InstructionAnalyzeParts,
  agent: AgentTarget,
): InstructionThreePaneState {
  const normalized = normalizeAnalyzeParts(parts);
  let next = ensureModeBlock(state, 'shared', agent);
  next = ensureModeBlock(next, 'adapted', agent);
  next = ensureModeBlock(next, 'targetOnly', agent);

  const shared = findBlockByMode(next.blocks, 'shared');
  const adapted = findBlockByMode(next.blocks, 'adapted');
  const exclusive = findBlockByMode(next.blocks, 'targetOnly');
  if (!shared || !adapted || !exclusive) {
    return next;
  }

  const commonNext = normalized.common.trim();
  const adaptedNext = normalized.adapted.trim();
  const exclusiveNext = normalized.exclusive.trim();
  if (!commonNext && !adaptedNext && !exclusiveNext) {
    return next;
  }

  let working = next;
  working = updateBlock(
    working,
    shared.id,
    { commonMarkdown: commonNext },
    agent,
  );

  const adaptedNow = findBlockByMode(working.blocks, 'adapted') ?? adapted;
  working = updateBlock(
    working,
    adaptedNow.id,
    {
      variants: {
        ...adaptedNow.variants,
        [agent]: adaptedNext,
      },
    },
    agent,
  );

  const exclusiveNow = findBlockByMode(working.blocks, 'targetOnly') ?? exclusive;
  working = updateBlock(
    working,
    exclusiveNow.id,
    {
      variants: {
        ...exclusiveNow.variants,
        [agent]: exclusiveNext,
      },
      sourceTarget: exclusiveNext
        ? (exclusiveNow.sourceTarget ?? agent)
        : exclusiveNow.sourceTarget,
    },
    agent,
  );
  return working;
}

/**
 * Business Logic: 把当前 agent 的适配正文跨 agent 改写后，追加到其它 agent 的适配变体尾。
 * Code Logic: ensure adapted → 对每个 destination 用 joinNonEmpty 追加 variants[dest]。
 */
export function appendAdaptedVariants(
  state: InstructionThreePaneState,
  variants: Partial<Record<AgentTarget, string>>,
  previewAgent: AgentTarget,
): InstructionThreePaneState {
  let next = ensureModeBlock(state, 'adapted', previewAgent);
  const adapted = findBlockByMode(next.blocks, 'adapted');
  if (!adapted) return next;

  let nextVariants = { ...adapted.variants };
  let changed = false;
  for (const target of AGENT_TARGETS) {
    const append = (variants[target] ?? '').trim();
    if (!append) continue;
    const existing =
      typeof nextVariants[target] === 'string'
        ? nextVariants[target]!
        : target === previewAgent
          ? adapted.commonMarkdown
          : '';
    nextVariants = {
      ...nextVariants,
      [target]: joinNonEmpty([existing, append]),
    };
    changed = true;
  }
  if (!changed) return next;
  return updateBlock(next, adapted.id, { variants: nextVariants }, previewAgent);
}

/**
 * Business Logic: AI 改写适配槽时一次覆盖全部 Agent 变体（含空串清空），不追加。
 * Code Logic: ensure adapted → 对传入的每个目标写 trim 后正文；清空 commonMarkdown，
 *   避免空变体被 normalize 丢掉后回落到旧底稿。
 */
export function replaceAdaptedVariants(
  state: InstructionThreePaneState,
  variants: Partial<Record<AgentTarget, string>>,
  previewAgent: AgentTarget,
): InstructionThreePaneState {
  let next = ensureModeBlock(state, 'adapted', previewAgent);
  const adapted = findBlockByMode(next.blocks, 'adapted');
  if (!adapted) return next;

  const nextVariants: Partial<Record<AgentTarget, string>> = { ...adapted.variants };
  for (const target of AGENT_TARGETS) {
    if (!Object.prototype.hasOwnProperty.call(variants, target)) {
      continue;
    }
    nextVariants[target] = (variants[target] ?? '').trim();
  }
  return updateBlock(
    next,
    adapted.id,
    { variants: nextVariants, commonMarkdown: '' },
    previewAgent,
  );
}

/**
 * Business Logic: 把 Claude 改写结果落到对应 lane 槽，供保存前展示。
 * Code Logic: 公共写 shared.common；独有写当前 agent variants；适配覆盖三端。
 */
export function applyInstructionReviseResult(
  state: InstructionThreePaneState,
  lane: 'common' | 'adapted' | 'exclusive',
  agent: AgentTarget,
  result: {
    common?: string | null;
    exclusive?: string | null;
    variants?: Partial<Record<AgentTarget, string>> | null;
  },
): InstructionThreePaneState {
  if (lane === 'common') {
    const next = ensureModeBlock(state, 'shared', agent);
    const block = findBlockByMode(next.blocks, 'shared');
    if (!block) return next;
    return updateBlock(next, block.id, { commonMarkdown: result.common ?? '' }, agent);
  }
  if (lane === 'exclusive') {
    const next = ensureModeBlock(state, 'targetOnly', agent);
    const block = findBlockByMode(next.blocks, 'targetOnly');
    if (!block) return next;
    return updateBlock(
      next,
      block.id,
      {
        variants: { ...block.variants, [agent]: result.exclusive ?? '' },
        sourceTarget: block.sourceTarget ?? agent,
      },
      agent,
    );
  }
  return replaceAdaptedVariants(
    state,
    {
      claude: result.variants?.claude ?? '',
      codex: result.variants?.codex ?? '',
      opencode: result.variants?.opencode ?? '',
    },
    agent,
  );
}

/**
 * Business Logic: 适配槽按当前 agent 读取可编辑正文（变体优先，回落 commonMarkdown 兼容旧 Claude 底稿）。
 */
export function resolveAdaptedSlotText(
  block: InstructionBlockDraft | null | undefined,
  agent: AgentTarget,
): string {
  if (!block) return '';
  const variant = block.variants[agent];
  if (typeof variant === 'string') return variant;
  return block.commonMarkdown;
}

// ── internal helpers ──────────────────────────────────────────────

function sortBlocksByMode(blocks: InstructionBlockDraft[]): InstructionBlockDraft[] {
  return MODE_ORDER.flatMap((mode) => blocks.filter((block) => block.mode === mode));
}

function joinNonEmpty(parts: string[]): string {
  return parts
    .map((part) => part.replace(/\r\n/g, '\n').trimEnd())
    .filter((part) => part.length > 0)
    .join('\n\n');
}

function mergeModeGroup(
  mode: InstructionBlockDraft['mode'],
  group: InstructionBlockDraft[],
): InstructionBlockDraft {
  const first = group[0]!;
  const commons = group.map((block) => block.commonMarkdown);
  const variants: Partial<Record<AgentTarget, string>> = {};
  for (const agent of AGENT_TARGETS) {
    const parts = group
      .map((block) => block.variants[agent])
      .filter((text): text is string => typeof text === 'string');
    const joined = joinNonEmpty(parts);
    if (joined.length > 0) {
      variants[agent] = joined;
    }
  }

  const sourceTarget =
    group.map((block) => block.sourceTarget).find((target) => target != null) ?? null;
  const needsAdaptation = group.some((block) => block.needsAdaptation);
  const headingPath = first.headingPath.length > 0 ? [...first.headingPath] : [];

  if (mode === 'shared') {
    return {
      id: first.id,
      mode: 'shared',
      commonMarkdown: joinNonEmpty(commons),
      variants: {},
      headingPath,
      sourceTarget: null,
      needsAdaptation: false,
    };
  }

  if (mode === 'adapted') {
    return {
      id: first.id,
      mode: 'adapted',
      commonMarkdown: joinNonEmpty(commons),
      variants,
      headingPath,
      sourceTarget: null,
      needsAdaptation: false,
    };
  }

  return {
    id: first.id,
    mode: 'targetOnly',
    commonMarkdown: '',
    variants,
    headingPath,
    sourceTarget,
    needsAdaptation,
  };
}

function emptyModeBlock(
  mode: InstructionBlockDraft['mode'],
  agent: AgentTarget,
): InstructionBlockDraft {
  const id = `slot-${mode}`;
  if (mode === 'shared') {
    return {
      id,
      mode: 'shared',
      commonMarkdown: '',
      variants: {},
      headingPath: [],
      sourceTarget: null,
      needsAdaptation: false,
    };
  }
  if (mode === 'adapted') {
    return {
      id,
      mode: 'adapted',
      commonMarkdown: '',
      variants: {},
      headingPath: [],
      sourceTarget: null,
      needsAdaptation: false,
    };
  }
  return {
    id,
    mode: 'targetOnly',
    commonMarkdown: '',
    variants: {},
    headingPath: [],
    sourceTarget: agent,
    needsAdaptation: false,
  };
}

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
 * Business Logic: 无持久化块时把原文整篇降级为单个 shared 公共块。
 * Code Logic: 非空 trim 后 commonMarkdown；空原文 → []。
 */
function parseWholeDocumentAsShared(text: string): InstructionBlockDraft[] {
  if (!hasMeaningfulContent(text)) {
    return [];
  }
  return [
    {
      id: 'slot-shared',
      mode: 'shared',
      commonMarkdown: text.replace(/\r\n/g, '\n').trimEnd(),
      variants: {},
      headingPath: [],
      sourceTarget: null,
      needsAdaptation: false,
    },
  ];
}
