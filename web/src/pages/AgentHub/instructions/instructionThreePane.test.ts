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
  dtoToDraft,
  draftToDto,
  initialThreePaneFromDisk,
  joinBlocksForTarget,
  normalizeInstructionBlocks,
  parseBlocksFromOriginal,
  recomputePreview,
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
    // 解析来自原文，块侧不应标脏
    expect(parsed.blocksDirty).toBe(false);
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
