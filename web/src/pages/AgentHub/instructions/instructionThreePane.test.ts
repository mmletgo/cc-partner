/**
 * 提示词三栏 pure 状态机测试。
 *
 * Business Logic: 锁定 hydrate/解析/per-agent 合成/双脏合流与同步基线选择。
 * Code Logic: 无 React / 无 api；仅调用 pure helpers 断言 state 与 resolve 结果。
 */

import { describe, expect, test } from 'vitest';
import type { AgentTarget } from '@/lib/types/agentHub';
import {
  addBlock,
  appendAdaptedVariants,
  applyInstructionReviseResult,
  extractSlotText,
  findInstructionTextChangeRange,
  replaceAdaptedVariants,
  replaceAnalyzedParts,
  replaceSlotText,
  dtoToDraft,
  draftToDto,
  ensureModeBlock,
  findBlockByMode,
  initialThreePaneFromDisk,
  joinBlocksForTarget,
  normalizeAnalyzeParts,
  normalizeInstructionBlocks,
  parseBlocksFromOriginal,
  recomputePreview,
  resolveAdaptedSlotText,
  resolveBlockText,
  resolveSyncContent,
  updateBlock,
  updateOriginalText,
  type InstructionBlockDraft,
} from './instructionThreePane';

const AGENT: AgentTarget = 'claude';

/** 带 ## 标题的样例原文；三槽模型下整篇应成为单个 shared。 */
const SAMPLE_ORIGINAL = `## Shared rules

Always use TypeScript.

## Target notes

CLI-specific flags only.
`;

/**
 * Business Logic: 构造一块草稿便于断言 dirty / preview。
 * Code Logic: 固定 id/mode，避免测试依赖随机 UUID。
 */
function makeBlock(
  overrides: Partial<InstructionBlockDraft> & Pick<InstructionBlockDraft, 'id'>,
): InstructionBlockDraft {
  return {
    mode: 'shared',
    commonMarkdown: '',
    variants: {},
    headingPath: [],
    sourceTarget: null,
    needsAdaptation: false,
    ...overrides,
  };
}

describe('dto<->draft adapters', () => {
  test('round-trip preserves mode/common/variants/headingPath/sourceTarget/needsAdaptation', () => {
    const draft = makeBlock({
      id: 'b1',
      mode: 'adapted',
      commonMarkdown: 'common',
      variants: { claude: 'c', codex: 'cd' },
      headingPath: ['Parent'],
      sourceTarget: null,
      needsAdaptation: true,
    });
    const dto = draftToDto(draft);
    expect(dto.variants).toEqual({ claude: 'c', codex: 'cd' });
    expect(dto.headingPath).toEqual(['Parent']);
    expect(dtoToDraft(dto)).toEqual(draft);
  });

  test('empty variants/headingPath normalize to null on dto', () => {
    const dto = draftToDto(makeBlock({ id: 'b', commonMarkdown: 'x' }));
    expect(dto.variants).toBeNull();
    expect(dto.headingPath).toBeNull();
  });
});

describe('resolveBlockText / joinBlocksForTarget', () => {
  test('shared uses commonMarkdown for every target', () => {
    const block = makeBlock({ id: 's', commonMarkdown: 'shared body' });
    expect(resolveBlockText(block, 'claude')).toBe('shared body');
    expect(resolveBlockText(block, 'codex')).toBe('shared body');
  });

  test('adapted prefers variant then falls back to common', () => {
    const block = makeBlock({
      id: 'a',
      mode: 'adapted',
      commonMarkdown: 'common',
      variants: { claude: 'claude-only' },
    });
    expect(resolveBlockText(block, 'claude')).toBe('claude-only');
    expect(resolveBlockText(block, 'codex')).toBe('common');
  });

  test('targetOnly yields null when variant absent', () => {
    const block = makeBlock({
      id: 't',
      mode: 'targetOnly',
      variants: { claude: 'only-claude' },
      sourceTarget: 'claude',
    });
    expect(resolveBlockText(block, 'claude')).toBe('only-claude');
    expect(resolveBlockText(block, 'codex')).toBeNull();
  });

  test('joinBlocksForTarget skips targetOnly gaps for non-matching target', () => {
    const blocks = [
      makeBlock({ id: '1', commonMarkdown: 'common' }),
      makeBlock({
        id: '2',
        mode: 'targetOnly',
        variants: { codex: 'codex-only' },
        sourceTarget: 'codex',
      }),
    ];
    expect(joinBlocksForTarget(blocks, 'claude')).toBe('common');
    expect(joinBlocksForTarget(blocks, 'codex')).toBe('common\n\ncodex-only');
  });
});

describe('initialThreePaneFromDisk', () => {
  test('without blocks leaves blocks + preview empty; preserves original', () => {
    const state = initialThreePaneFromDisk('/home/u/CLAUDE.md', SAMPLE_ORIGINAL, null, AGENT);
    expect(state.originalPath).toBe('/home/u/CLAUDE.md');
    expect(state.originalText).toBe(SAMPLE_ORIGINAL);
    expect(state.blocks).toEqual([]);
    expect(state.previewText).toBe('');
    expect(state.blocksDirty).toBe(false);
    expect(state.originalDirty).toBe(false);
    expect(state.externalDrift).toBe(false);
  });

  test('hydrates persisted blocks and composes preview for agent', () => {
    const blocks = [makeBlock({ id: 'h1', commonMarkdown: 'hydrated' })];
    const state = initialThreePaneFromDisk('/p.md', 'orig', blocks, AGENT);
    expect(state.blocks).toHaveLength(1);
    expect(state.blocks[0]?.commonMarkdown).toBe('hydrated');
    expect(state.previewText).toBe('hydrated');
  });
});

describe('replaceAnalyzedParts / appendAdaptedVariants', () => {
  test('normalizeAnalyzeParts drops common that overlaps adapted (mixed-content rule)', () => {
    const out = normalizeAnalyzeParts({
      common: 'Always use TypeScript for new modules.',
      adapted: 'Always use TypeScript for new modules.\nPrefer Claude Code with Sonnet.',
      exclusive: '',
    });
    expect(out.common).toBe('');
    expect(out.adapted).toContain('TypeScript');
    expect(out.adapted).toContain('Sonnet');
  });

  test('normalizeAnalyzeParts moves surface-bearing common into adapted', () => {
    const out = normalizeAnalyzeParts({
      common: 'Put project rules in CLAUDE.md and load ~/.claude settings.',
      adapted: 'Use Sonnet for implementation.',
      exclusive: '',
    });
    expect(out.common).toBe('');
    expect(out.adapted.toLowerCase()).toContain('claude.md');
    expect(out.adapted).toMatch(/Sonnet|implementation/i);
  });

  test('replaceAnalyzedParts applies normalize before replace (no dual common+adapted copy)', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'shared', AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    state = ensureModeBlock(state, 'targetOnly', AGENT);

    const next = replaceAnalyzedParts(
      state,
      {
        common: 'Always use TypeScript for new modules.',
        adapted: 'Always use TypeScript for new modules.\nPrefer Sonnet.',
        exclusive: '',
      },
      AGENT,
    );
    expect(findBlockByMode(next.blocks, 'shared')?.commonMarkdown ?? '').toBe('');
    expect(findBlockByMode(next.blocks, 'adapted')?.variants.claude ?? '').toContain('TypeScript');
    expect(findBlockByMode(next.blocks, 'adapted')?.variants.claude ?? '').toContain('Sonnet');
  });

  test('replaceAnalyzedParts overwrites existing slot text instead of appending', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'shared', AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    state = ensureModeBlock(state, 'targetOnly', AGENT);
    const shared = findBlockByMode(state.blocks, 'shared')!;
    const adapted = findBlockByMode(state.blocks, 'adapted')!;
    const exclusive = findBlockByMode(state.blocks, 'targetOnly')!;
    state = updateBlock(state, shared.id, { commonMarkdown: 'existing common' }, AGENT);
    state = updateBlock(
      state,
      adapted.id,
      { variants: { claude: 'existing adapted' } },
      AGENT,
    );
    state = updateBlock(
      state,
      exclusive.id,
      { variants: { claude: 'existing exclusive' }, sourceTarget: 'claude' },
      AGENT,
    );

    const next = replaceAnalyzedParts(
      state,
      {
        common: 'new common',
        adapted: 'new adapted',
        exclusive: 'new exclusive',
      },
      AGENT,
    );
    expect(findBlockByMode(next.blocks, 'shared')?.commonMarkdown).toBe('new common');
    expect(findBlockByMode(next.blocks, 'adapted')?.variants.claude).toBe('new adapted');
    expect(findBlockByMode(next.blocks, 'targetOnly')?.variants.claude).toBe('new exclusive');
    expect(next.blocksDirty).toBe(true);
  });

  test('replaceAnalyzedParts does not clear slots when analyze result is empty', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'shared', AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    state = ensureModeBlock(state, 'targetOnly', AGENT);
    const shared = findBlockByMode(state.blocks, 'shared')!;
    const adapted = findBlockByMode(state.blocks, 'adapted')!;
    state = updateBlock(state, shared.id, { commonMarkdown: 'keep me' }, AGENT);
    state = updateBlock(state, adapted.id, { variants: { claude: 'keep adapted' } }, AGENT);

    const next = replaceAnalyzedParts(
      state,
      { common: '   ', adapted: '', exclusive: '\n' },
      AGENT,
    );
    expect(findBlockByMode(next.blocks, 'shared')?.commonMarkdown).toBe('keep me');
    expect(findBlockByMode(next.blocks, 'adapted')?.variants.claude).toBe('keep adapted');
  });

  test('appendAdaptedVariants only appends destination agent variants', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    const adapted = findBlockByMode(state.blocks, 'adapted')!;
    state = updateBlock(
      state,
      adapted.id,
      { variants: { claude: 'src adapted', codex: 'old codex' } },
      AGENT,
    );
    const next = appendAdaptedVariants(
      state,
      { codex: 'new codex', opencode: 'new opencode' },
      AGENT,
    );
    const block = findBlockByMode(next.blocks, 'adapted')!;
    expect(block.variants.claude).toBe('src adapted');
    expect(block.variants.codex).toBe('old codex\n\nnew codex');
    expect(block.variants.opencode).toBe('new opencode');
  });
});

describe('replaceAdaptedVariants', () => {
  test('overwrites all three agent adapted variants including empty slots', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    const adapted = findBlockByMode(state.blocks, 'adapted')!;
    state = updateBlock(
      state,
      adapted.id,
      { variants: { claude: 'old claude', codex: 'old codex' } },
      AGENT,
    );

    const next = replaceAdaptedVariants(
      state,
      { claude: 'new claude', codex: 'new codex', opencode: 'new opencode' },
      AGENT,
    );
    const block = findBlockByMode(next.blocks, 'adapted')!;
    expect(block.variants.claude).toBe('new claude');
    expect(block.variants.codex).toBe('new codex');
    expect(block.variants.opencode).toBe('new opencode');
    expect(next.blocksDirty).toBe(true);
  });

  test('empty string overwrites an existing adapted variant', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    const adapted = findBlockByMode(state.blocks, 'adapted')!;
    state = updateBlock(
      state,
      adapted.id,
      { variants: { claude: 'keep? no', codex: 'old codex', opencode: 'old open' } },
      AGENT,
    );

    const next = replaceAdaptedVariants(
      state,
      { claude: '   ', codex: 'kept codex', opencode: 'kept open' },
      AGENT,
    );
    const block = findBlockByMode(next.blocks, 'adapted')!;
    expect(resolveAdaptedSlotText(block, 'claude')).toBe('');
    expect(block.variants.codex).toBe('kept codex');
    expect(block.variants.opencode).toBe('kept open');
    expect(block.commonMarkdown).toBe('');
  });

  test('does not rewrite shared or exclusive slots', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'shared', AGENT);
    state = ensureModeBlock(state, 'adapted', AGENT);
    state = ensureModeBlock(state, 'targetOnly', AGENT);
    const shared = findBlockByMode(state.blocks, 'shared')!;
    const exclusive = findBlockByMode(state.blocks, 'targetOnly')!;
    state = updateBlock(state, shared.id, { commonMarkdown: 'keep common' }, AGENT);
    state = updateBlock(
      state,
      exclusive.id,
      { variants: { claude: 'keep exclusive' }, sourceTarget: 'claude' },
      AGENT,
    );

    const next = replaceAdaptedVariants(
      state,
      { claude: 'new adapted' },
      AGENT,
    );
    expect(findBlockByMode(next.blocks, 'shared')?.commonMarkdown).toBe('keep common');
    expect(findBlockByMode(next.blocks, 'targetOnly')?.variants.claude).toBe('keep exclusive');
    expect(findBlockByMode(next.blocks, 'adapted')?.variants.claude).toBe('new adapted');
  });
});

describe('applyInstructionReviseResult', () => {
  test('common lane only writes shared markdown', () => {
    let state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    state = ensureModeBlock(state, 'shared', AGENT);
    state = ensureModeBlock(state, 'targetOnly', AGENT);
    const exclusive = findBlockByMode(state.blocks, 'targetOnly')!;
    state = updateBlock(
      state,
      exclusive.id,
      { variants: { claude: 'keep exclusive' }, sourceTarget: 'claude' },
      AGENT,
    );
    const next = applyInstructionReviseResult(state, 'common', AGENT, {
      common: 'revised common',
      exclusive: 'should ignore',
    });
    expect(findBlockByMode(next.blocks, 'shared')?.commonMarkdown).toBe('revised common');
    expect(findBlockByMode(next.blocks, 'targetOnly')?.variants.claude).toBe('keep exclusive');
  });

  test('exclusive lane only writes the current agent variant', () => {
    const state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    const next = applyInstructionReviseResult(state, 'exclusive', AGENT, {
      exclusive: 'revised exclusive',
      common: 'should ignore',
    });
    expect(findBlockByMode(next.blocks, 'targetOnly')?.variants.claude).toBe('revised exclusive');
    expect(findBlockByMode(next.blocks, 'shared')).toBeNull();
  });

  test('adapted lane overwrites every catalog hub target including grok and gemini', () => {
    const state = initialThreePaneFromDisk('/p.md', 'orig', null, AGENT);
    const next = applyInstructionReviseResult(state, 'adapted', AGENT, {
      variants: { claude: 'c', grok: 'g', gemini: 'm' },
    });
    const variants = findBlockByMode(next.blocks, 'adapted')?.variants;
    expect(variants?.claude).toBe('c');
    expect(variants?.grok).toBe('g');
    expect(variants?.gemini).toBe('m');
    expect(variants?.codex ?? '').toBe('');
    expect(variants?.opencode ?? '').toBe('');
  });
});

describe('findInstructionTextChangeRange', () => {
  test('selects the minimal changed range in the revised text', () => {
    expect(findInstructionTextChangeRange('before OLD after', 'before NEW after')).toEqual({
      start: 7,
      end: 10,
    });
  });

  test('returns a collapsed selection for deletion and null for equal text', () => {
    expect(findInstructionTextChangeRange('before remove after', 'before  after')).toEqual({
      start: 7,
      end: 7,
    });
    expect(findInstructionTextChangeRange('same', 'same')).toBeNull();
  });

  test('keeps DOM UTF-16 selection boundaries around complete emoji code points', () => {
    expect(findInstructionTextChangeRange('A😀Z', 'A😎Z')).toEqual({ start: 1, end: 3 });
  });
});

describe('parseBlocksFromOriginal (fallback)', () => {
  test('imports whole document as a single shared block; no ## splitting', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE_ORIGINAL, null, AGENT);
    expect(opened.blocks).toHaveLength(0);

    const parsed = parseBlocksFromOriginal(opened, AGENT);
    expect(parsed.blocks.length).toBe(1);
    expect(parsed.blocks[0]?.mode).toBe('shared');
    expect(parsed.blocks[0]?.commonMarkdown).toContain('Always use TypeScript');
    expect(parsed.blocks[0]?.commonMarkdown).toContain('Target notes');
    expect(parsed.previewText).toContain('Always use TypeScript');
    // 用户显式导入会生成待保存的 Hub 草稿。
    expect(parsed.blocksDirty).toBe(true);
    expect(parsed.originalDirty).toBe(false);
  });

  test('empty original yields empty blocks', () => {
    const state = initialThreePaneFromDisk('/e.md', '   \n  ', null, AGENT);
    expect(parseBlocksFromOriginal(state, AGENT).blocks).toEqual([]);
  });
});

describe('normalizeInstructionBlocks', () => {
  test('merges multiple same-mode blocks into three slots max', () => {
    const input = [
      makeBlock({ id: 's1', mode: 'shared', commonMarkdown: 'A' }),
      makeBlock({ id: 's2', mode: 'shared', commonMarkdown: 'B' }),
      makeBlock({
        id: 'a1',
        mode: 'adapted',
        commonMarkdown: 'base',
        variants: { claude: 'c1' },
      }),
      makeBlock({
        id: 'a2',
        mode: 'adapted',
        commonMarkdown: '',
        variants: { claude: 'c2', codex: 'x' },
      }),
      makeBlock({
        id: 't1',
        mode: 'targetOnly',
        variants: { claude: 'only-c' },
        sourceTarget: 'claude',
      }),
      makeBlock({
        id: 't2',
        mode: 'targetOnly',
        variants: { codex: 'only-x' },
        sourceTarget: 'codex',
      }),
    ];
    const out = normalizeInstructionBlocks(input);
    expect(out.map((b) => b.mode)).toEqual(['shared', 'adapted', 'targetOnly']);
    expect(out[0]?.commonMarkdown).toBe('A\n\nB');
    expect(out[1]?.variants.claude).toBe('c1\n\nc2');
    expect(out[1]?.variants.codex).toBe('x');
    expect(out[2]?.variants.claude).toBe('only-c');
    expect(out[2]?.variants.codex).toBe('only-x');
  });

  test('joinBlocksForTarget respects shared → adapted → targetOnly order', () => {
    const blocks = [
      makeBlock({
        id: 't',
        mode: 'targetOnly',
        variants: { claude: 'exclusive' },
        sourceTarget: 'claude',
      }),
      makeBlock({ id: 's', mode: 'shared', commonMarkdown: 'common' }),
      makeBlock({
        id: 'a',
        mode: 'adapted',
        commonMarkdown: 'adapt-common',
        variants: { claude: 'adapt-c' },
      }),
    ];
    expect(joinBlocksForTarget(blocks, 'claude')).toBe('common\n\nadapt-c\n\nexclusive');
  });
});

describe('recomputePreview', () => {
  test('joins blocks per-target without flipping dirty flags', () => {
    let state = initialThreePaneFromDisk(null, '', null, AGENT);
    state = {
      ...state,
      blocks: [
        makeBlock({ id: 'b1', commonMarkdown: 'alpha' }),
        makeBlock({
          id: 'b2',
          mode: 'targetOnly',
          variants: { claude: 'beta' },
          sourceTarget: 'claude',
        }),
      ],
      blocksDirty: true,
    };

    const next = recomputePreview(state, AGENT);
    expect(next.previewText).toBe('alpha\n\nbeta');
    expect(next.blocksDirty).toBe(true);
    expect(next.originalDirty).toBe(false);
  });
});

describe('resolveSyncContent', () => {
  test('dual dirty with diverging contents → dual_dirty_conflict', () => {
    let state = initialThreePaneFromDisk('/p.md', '## Original\n\nfrom disk\n', null, AGENT);
    state = parseBlocksFromOriginal(state, AGENT);
    // 块侧改动
    state = updateBlock(
      state,
      state.blocks[0]!.id,
      { commonMarkdown: 'edited blocks' },
      AGENT,
    );
    // 原文侧改动且内容分歧
    state = updateOriginalText(state, '## Original\n\nedited original differently\n');

    expect(state.blocksDirty).toBe(true);
    expect(state.originalDirty).toBe(true);

    expect(resolveSyncContent(state, AGENT)).toEqual({ ok: false, reason: 'dual_dirty_conflict' });
  });

  test('blocks-only dirty → baseline blocks with preview content', () => {
    let state = initialThreePaneFromDisk('/p.md', '## Keep me\n\ndisk\n', null, AGENT);
    state = parseBlocksFromOriginal(state, AGENT);
    state = updateBlock(
      state,
      state.blocks[0]!.id,
      { commonMarkdown: 'only blocks dirty' },
      AGENT,
    );
    state = recomputePreview(state, AGENT);

    expect(state.blocksDirty).toBe(true);
    expect(state.originalDirty).toBe(false);

    const result = resolveSyncContent(state, AGENT);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.baseline).toBe('blocks');
    expect(result.content).toBe(state.previewText);
    expect(result.content).toContain('only blocks dirty');
  });

  test('original-only dirty → baseline original', () => {
    let state = initialThreePaneFromDisk('/p.md', '## From disk\n\nold\n', null, AGENT);
    state = parseBlocksFromOriginal(state, AGENT);
    state = { ...state, blocksDirty: false };
    state = updateOriginalText(state, '## From disk\n\nonly original dirty\n');

    expect(resolveSyncContent(state, AGENT)).toEqual({
      ok: true,
      baseline: 'original',
      content: '## From disk\n\nonly original dirty\n',
    });
  });

  test('neither dirty with original content → baseline original', () => {
    const state = initialThreePaneFromDisk('/p.md', '## Clean\n\nok\n', null, AGENT);
    expect(resolveSyncContent(state, AGENT)).toEqual({
      ok: true,
      baseline: 'original',
      content: '## Clean\n\nok\n',
    });
  });

  test('neither dirty with blocks only → baseline blocks', () => {
    let state = initialThreePaneFromDisk(null, '', null, AGENT);
    state = addBlock(state, makeBlock({ id: 'manual-1', commonMarkdown: 'typed' }), AGENT);
    // 模拟已保存后清脏，只剩块模型有内容
    state = { ...state, blocksDirty: false, originalDirty: false, originalText: '' };

    const result = resolveSyncContent(state, AGENT);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.baseline).toBe('blocks');
    expect(result.content).toContain('typed');
  });

  test('empty both sides → empty', () => {
    const state = initialThreePaneFromDisk(null, '', null, AGENT);
    expect(resolveSyncContent(state, AGENT)).toEqual({ ok: false, reason: 'empty' });
  });
});

describe('update helpers (dirty flags)', () => {
  test('updateOriginalText marks originalDirty', () => {
    const base = initialThreePaneFromDisk('/p.md', 'a', null, AGENT);
    const next = updateOriginalText(base, 'b');
    expect(next.originalText).toBe('b');
    expect(next.originalDirty).toBe(true);
    expect(next.blocksDirty).toBe(false);
  });

  test('updateBlock marks blocksDirty and refreshes preview', () => {
    let state = initialThreePaneFromDisk(null, '', null, AGENT);
    state = addBlock(state, makeBlock({ id: 'x', commonMarkdown: 'old' }), AGENT);
    state = updateBlock(state, 'x', { commonMarkdown: 'new' }, AGENT);
    expect(state.blocksDirty).toBe(true);
    expect(state.blocks[0]?.commonMarkdown).toBe('new');
    expect(state.previewText).toContain('new');
  });

  test('addBlock appends and marks blocksDirty', () => {
    let state = initialThreePaneFromDisk(null, '', null, AGENT);
    state = addBlock(state, makeBlock({ id: 'a', commonMarkdown: 'one' }), AGENT);
    expect(state.blocks).toHaveLength(1);
    expect(state.blocksDirty).toBe(true);
    expect(state.previewText).toContain('one');
  });
});

describe('extractSlotText / replaceSlotText (history drawer helpers)', () => {
  /**
   * Business Logic: shared slot 读取/写入都使用 commonMarkdown；写入后 preview 跟随重算。
   * Code Logic: extractSlotText → shared.commonMarkdown；replaceSlotText → ensure shared → write common。
   */
  test('shared slot reads/writes commonMarkdown', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = replaceSlotText(state, { kind: 'shared' }, 'shared-text', AGENT);
    expect(extractSlotText(state, { kind: 'shared' })).toBe('shared-text');
    expect(state.blocksDirty).toBe(true);
    expect(state.previewText).toContain('shared-text');
  });

  /**
   * Business Logic: adapted slot 按 (agent) variant 读写；缺失 variant 回落 common。
   * Code Logic: 懒创建 adapted 块（确保存在），写入 variants[agent]，extract 返回该 variant。
   */
  test('adapted slot reads/writes variants[agent] with lazy create', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = replaceSlotText(state, { kind: 'adapted', agent: 'claude' }, 'claude-only', 'claude');
    expect(extractSlotText(state, { kind: 'adapted', agent: 'claude' })).toBe('claude-only');
    expect(extractSlotText(state, { kind: 'adapted', agent: 'codex' })).toBe('');
  });

  /**
   * Business Logic: targetOnly slot 按 (agent) variant 读写；缺失整个 block 返回空字符串。
   * Code Logic: replaceSlotText 懒创建 targetOnly block，extract 返回该 agent variant。
   */
  test('targetOnly slot reads/writes variants[agent]', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = replaceSlotText(state, { kind: 'targetOnly', agent: 'opencode' }, 'oc-only', 'opencode');
    expect(extractSlotText(state, { kind: 'targetOnly', agent: 'opencode' })).toBe('oc-only');
    expect(extractSlotText(state, { kind: 'targetOnly', agent: 'claude' })).toBe('');
  });

  /**
   * Business Logic: replaceSlotText 写空串仍写入 history 候选；后续编辑可见同样的 dirty 状态。
   * Code Logic: ensure + write commonMarkdown='' → previewText=空；state.blocksDirty=true。
   */
  test('empty replacement still marks dirty and clears slot', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = replaceSlotText(state, { kind: 'shared' }, 'x', AGENT);
    expect(state.blocksDirty).toBe(true);
    state = replaceSlotText(state, { kind: 'shared' }, '', AGENT);
    expect(extractSlotText(state, { kind: 'shared' })).toBe('');
    expect(state.blocksDirty).toBe(true);
  });
});
