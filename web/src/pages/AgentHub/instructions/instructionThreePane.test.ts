/**
 * 提示词三栏 pure 状态机测试。
 *
 * Business Logic: 锁定打开不自动 parse、显式解析、双脏合流与同步基线选择。
 * Code Logic: 无 React / 无 api；仅调用 pure helpers 断言 state 与 resolve 结果。
 */

import { describe, expect, test } from 'vitest';
import {
  addBlock,
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
  recomputePreview,
  resolveSyncContent,
  updateBlock,
  updateOriginalText,
  type InstructionBlockDraft,
  type InstructionThreePaneState,
} from './instructionThreePane';

/** 带 ## 标题的样例原文，解析后应得到两块。 */
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
  overrides: Partial<InstructionBlockDraft> & Pick<InstructionBlockDraft, 'id' | 'title' | 'body'>,
): InstructionBlockDraft {
  return {
    mode: 'shared',
    ...overrides,
  };
}

describe('initialThreePaneFromDisk', () => {
  test('fills original path/text and leaves blocks + preview empty (no auto-parse)', () => {
    const state = initialThreePaneFromDisk('/home/user/CLAUDE.md', SAMPLE_ORIGINAL);

    expect(state.originalPath).toBe('/home/user/CLAUDE.md');
    expect(state.originalText).toBe(SAMPLE_ORIGINAL);
    expect(state.blocks).toEqual([]);
    expect(state.previewText).toBe('');
    expect(state.blocksDirty).toBe(false);
    expect(state.originalDirty).toBe(false);
    expect(state.externalDrift).toBe(false);
  });

  test('accepts null path and empty text', () => {
    const state = initialThreePaneFromDisk(null, '');
    expect(state.originalPath).toBeNull();
    expect(state.originalText).toBe('');
    expect(state.blocks).toEqual([]);
    expect(state.previewText).toBe('');
  });
});

describe('parseBlocksFromOriginal', () => {
  test('is not applied by initialThreePaneFromDisk (only when explicitly called)', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE_ORIGINAL);
    expect(opened.blocks).toHaveLength(0);
    expect(opened.previewText).toBe('');

    const parsed = parseBlocksFromOriginal(opened);
    expect(parsed.blocks.length).toBeGreaterThan(0);
    expect(parsed.blocks.map((b) => b.title)).toEqual(['Shared rules', 'Target notes']);
    expect(parsed.blocks[0]?.body).toContain('Always use TypeScript');
    expect(parsed.blocks[1]?.body).toContain('CLI-specific flags');
    expect(parsed.previewText.length).toBeGreaterThan(0);
    // 解析来自原文，块侧不应标脏
    expect(parsed.blocksDirty).toBe(false);
    expect(parsed.originalDirty).toBe(false);
  });

  test('empty original yields empty blocks and empty preview', () => {
    const state = initialThreePaneFromDisk('/empty.md', '   \n  ');
    const parsed = parseBlocksFromOriginal(state);
    expect(parsed.blocks).toEqual([]);
    expect(parsed.previewText).toBe('');
  });
});

describe('recomputePreview', () => {
  test('joins blocks into markdown previewText without flipping dirty flags', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = {
      ...state,
      blocks: [
        makeBlock({ id: 'b1', title: 'A', body: 'alpha' }),
        makeBlock({ id: 'b2', title: 'B', body: 'beta', mode: 'targetOnly' }),
      ],
      blocksDirty: true,
    };

    const next = recomputePreview(state);
    expect(next.previewText).toContain('## A');
    expect(next.previewText).toContain('alpha');
    expect(next.previewText).toContain('## B');
    expect(next.previewText).toContain('beta');
    expect(next.blocksDirty).toBe(true);
    expect(next.originalDirty).toBe(false);
  });
});

describe('resolveSyncContent', () => {
  test('dual dirty with diverging contents → dual_dirty_conflict', () => {
    let state = initialThreePaneFromDisk('/p.md', '## Original\n\nfrom disk\n');
    state = parseBlocksFromOriginal(state);
    // 块侧改动
    state = updateBlock(state, state.blocks[0]!.id, { body: 'edited blocks' });
    // 原文侧改动且内容分歧
    state = updateOriginalText(state, '## Original\n\nedited original differently\n');

    expect(state.blocksDirty).toBe(true);
    expect(state.originalDirty).toBe(true);

    const result = resolveSyncContent(state);
    expect(result).toEqual({ ok: false, reason: 'dual_dirty_conflict' });
  });

  test('blocks-only dirty → baseline blocks with preview content', () => {
    let state = initialThreePaneFromDisk('/p.md', '## Keep me\n\ndisk\n');
    state = parseBlocksFromOriginal(state);
    state = updateBlock(state, state.blocks[0]!.id, { body: 'only blocks dirty' });
    // recompute so preview matches blocks (updateBlock may already do this)
    state = recomputePreview(state);

    expect(state.blocksDirty).toBe(true);
    expect(state.originalDirty).toBe(false);

    const result = resolveSyncContent(state);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.baseline).toBe('blocks');
    expect(result.content).toBe(state.previewText);
    expect(result.content).toContain('only blocks dirty');
  });

  test('original-only dirty → baseline original', () => {
    let state = initialThreePaneFromDisk('/p.md', '## From disk\n\nold\n');
    state = parseBlocksFromOriginal(state);
    state = updateOriginalText(state, '## From disk\n\nonly original dirty\n');

    expect(state.blocksDirty).toBe(false);
    expect(state.originalDirty).toBe(true);

    const result = resolveSyncContent(state);
    expect(result).toEqual({
      ok: true,
      baseline: 'original',
      content: '## From disk\n\nonly original dirty\n',
    });
  });

  test('neither dirty with original content → baseline original', () => {
    const state = initialThreePaneFromDisk('/p.md', '## Clean\n\nok\n');
    const result = resolveSyncContent(state);
    expect(result).toEqual({
      ok: true,
      baseline: 'original',
      content: '## Clean\n\nok\n',
    });
  });

  test('neither dirty with blocks only → baseline blocks', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = addBlock(
      state,
      makeBlock({ id: 'manual-1', title: 'Hand', body: 'typed' }),
    );
    state = recomputePreview(state);
    // 模拟已保存后清脏，只剩块模型有内容
    state = { ...state, blocksDirty: false, originalDirty: false, originalText: '' };

    const result = resolveSyncContent(state);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.baseline).toBe('blocks');
    expect(result.content).toContain('typed');
  });

  test('empty both sides → empty', () => {
    const state = initialThreePaneFromDisk(null, '');
    expect(resolveSyncContent(state)).toEqual({ ok: false, reason: 'empty' });
  });
});

describe('update helpers (dirty flags)', () => {
  test('updateOriginalText marks originalDirty', () => {
    const base = initialThreePaneFromDisk('/p.md', 'a');
    const next = updateOriginalText(base, 'b');
    expect(next.originalText).toBe('b');
    expect(next.originalDirty).toBe(true);
    expect(next.blocksDirty).toBe(false);
  });

  test('updateBlock marks blocksDirty and refreshes preview', () => {
    let state: InstructionThreePaneState = initialThreePaneFromDisk(null, '');
    state = addBlock(state, makeBlock({ id: 'x', title: 'T', body: 'old' }));
    state = updateBlock(state, 'x', { body: 'new' });
    expect(state.blocksDirty).toBe(true);
    expect(state.blocks[0]?.body).toBe('new');
    expect(state.previewText).toContain('new');
  });

  test('addBlock appends and marks blocksDirty', () => {
    let state = initialThreePaneFromDisk(null, '');
    state = addBlock(state, makeBlock({ id: 'a', title: 'One', body: '1' }));
    expect(state.blocks).toHaveLength(1);
    expect(state.blocksDirty).toBe(true);
    expect(state.previewText).toContain('## One');
  });
});
