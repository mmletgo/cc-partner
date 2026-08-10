// @vitest-environment jsdom
/**
 * 提示词三栏 pure view 测试（按 lane 布局）。
 *
 * Business Logic: 锁定公共单列、适配双列（Claude 单列）、独有三列与无 @/api 依赖。
 * Code Logic: 注入 labels + state + callbacks；静态扫描源文件。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { AgentTarget } from '@/lib/types/agentHub';
import {
  ensureModeBlock,
  findBlockByMode,
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
  updateBlock,
  type InstructionThreePaneState,
} from './instructionThreePane';

const AGENT: AgentTarget = 'claude';
import {
  InstructionThreePaneView,
  type InstructionThreePaneViewLabels,
  type InstructionThreePaneViewProps,
} from './InstructionThreePaneView';

const viewDir = dirname(fileURLToPath(import.meta.url));

afterEach(() => {
  cleanup();
});

const labels: InstructionThreePaneViewLabels = {
  blocksTitle: 'Current slot',
  previewTitle: 'Preview',
  originalTitle: 'Original file',
  reparseFromOriginal: 'Import original as common',
  syncToNative: 'Sync to native file…',
  emptyBlocks: 'No blocks yet',
  emptyPreview: 'Preview is empty until you reparse or fill blocks',
  emptyOriginal: 'No original content',
  pathLabel: 'Path',
  noPath: 'No path',
  loading: 'Loading instructions…',
  retry: 'Retry',
  previewReadOnly: 'Read-only composed preview',
  slotCommonHint: 'Shared by all agents',
  slotAdaptedHint: 'Adapted for current agent',
  slotExclusiveHint: 'Exclusive to current agent',
  adaptedCommonTitle: 'Common draft (Claude Code)',
  adaptedVariantTitle: 'Current agent variant',
  adaptedCommonHint: 'Claude is the common draft authority',
  adaptedVariantHint: 'Variant for the selected agent',
  dualDirtyTitle: 'Choose sync baseline',
  dualDirtyDescription: 'Blocks and original both changed.',
  useBlocksBaseline: 'Use composed blocks',
  useOriginalBaseline: 'Use original file',
  cancel: 'Cancel',
  blockBodyPlaceholder: 'Body',
  refresh: 'Rescan',
  commonMarkdown: 'Common body',
  saveBlocks: 'Save slots',
};

const SAMPLE = `## Shared

Rules

## Notes

CLI only
`;

/**
 * Business Logic: 构造可渲染 props，默认加载完成且有原文。
 * Code Logic: initialThreePaneFromDisk 样例 + 覆盖。
 */
function buildProps(
  overrides: Partial<InstructionThreePaneViewProps> = {},
): InstructionThreePaneViewProps {
  const state: InstructionThreePaneState = initialThreePaneFromDisk(
    '/home/user/CLAUDE.md',
    SAMPLE,
  );
  return {
    labels,
    state,
    agent: AGENT,
    instructionLane: 'common',
    loading: false,
    error: null,
    actionError: null,
    actionBusy: false,
    writeBlocked: false,
    writeBlockedReason: null,
    dualDirtyOpen: false,
    onReparse: vi.fn(),
    onSync: vi.fn(),
    onSaveBlocks: vi.fn(),
    onRetry: vi.fn(),
    onRefresh: vi.fn(),
    onOriginalChange: vi.fn(),
    onSlotTextChange: vi.fn(),
    onAdaptedCommonChange: vi.fn(),
    onAdaptedVariantChange: vi.fn(),
    onChooseBaseline: vi.fn(),
    onCancelDualDirty: vi.fn(),
    ...overrides,
  };
}

/**
 * Business Logic: 构造含三槽草稿的 state，便于布局断言。
 */
function stateWithSlots(): InstructionThreePaneState {
  let state = initialThreePaneFromDisk('/p.md', SAMPLE, null, AGENT);
  state = parseBlocksFromOriginal(state, AGENT);
  state = ensureModeBlock(state, 'adapted', AGENT);
  state = ensureModeBlock(state, 'targetOnly', AGENT);
  const adapted = findBlockByMode(state.blocks, 'adapted');
  if (adapted) {
    state = updateBlock(
      state,
      adapted.id,
      {
        commonMarkdown: 'adapted common draft',
        variants: { codex: 'codex variant' },
      },
      AGENT,
    );
  }
  const exclusive = findBlockByMode(state.blocks, 'targetOnly');
  if (exclusive) {
    state = updateBlock(
      state,
      exclusive.id,
      {
        variants: { claude: 'exclusive claude' },
        sourceTarget: 'claude',
      },
      AGENT,
    );
  }
  return state;
}

describe('InstructionThreePaneView', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(viewDir, './InstructionThreePaneView.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('common lane shows only the common slot (no preview/original)', () => {
    render(<InstructionThreePaneView {...buildProps({ instructionLane: 'common' })} />);

    expect(screen.getByTestId('instruction-three-pane')).toBeTruthy();
    expect(screen.getByTestId('instruction-panes-common')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks')).toBeTruthy();
    expect(screen.queryByTestId('instruction-pane-preview')).toBeNull();
    expect(screen.queryByTestId('instruction-pane-original')).toBeNull();
    expect(screen.getByTestId('instruction-sync-to-native')).toBeTruthy();
    expect(screen.queryByTestId('instruction-original-path')).toBeNull();
  });

  test('common slot textarea edits call onSlotTextChange', () => {
    const onSlotTextChange = vi.fn();
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE, null, AGENT);
    const parsed = parseBlocksFromOriginal(opened, AGENT);
    render(
      <InstructionThreePaneView
        {...buildProps({ state: parsed, onSlotTextChange, instructionLane: 'common' })}
      />,
    );
    fireEvent.change(screen.getByTestId('instruction-slot-textarea'), {
      target: { value: 'new common' },
    });
    expect(onSlotTextChange).toHaveBeenCalledWith('new common');
  });

  test('adapted lane with Claude shows only common draft column', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'adapted',
          agent: 'claude',
          state: stateWithSlots(),
        })}
      />,
    );
    expect(screen.getByTestId('instruction-panes-adapted')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-adapted-common')).toBeTruthy();
    expect(screen.queryByTestId('instruction-pane-adapted-variant')).toBeNull();
    expect(screen.queryByTestId('instruction-pane-preview')).toBeNull();
    expect(screen.queryByTestId('instruction-pane-original')).toBeNull();
  });

  test('adapted lane with Codex shows common draft + variant columns', () => {
    const onAdaptedCommonChange = vi.fn();
    const onAdaptedVariantChange = vi.fn();
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'adapted',
          agent: 'codex',
          state: stateWithSlots(),
          onAdaptedCommonChange,
          onAdaptedVariantChange,
        })}
      />,
    );
    expect(screen.getByTestId('instruction-pane-adapted-common')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-adapted-variant')).toBeTruthy();
    // 非 Claude 时公共底稿只读
    const commonInput = screen.getByTestId(
      'instruction-adapted-common-textarea',
    ) as HTMLTextAreaElement;
    expect(commonInput.readOnly).toBe(true);

    fireEvent.change(screen.getByTestId('instruction-adapted-variant-textarea'), {
      target: { value: 'codex next' },
    });
    expect(onAdaptedVariantChange).toHaveBeenCalledWith('codex next');
  });

  test('exclusive lane keeps three panes: blocks, preview, original', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: stateWithSlots(),
        })}
      />,
    );

    expect(screen.getByTestId('instruction-panes-exclusive')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-preview')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-original')).toBeTruthy();
    expect(screen.getByTestId('instruction-sync-to-native')).toBeTruthy();
  });

  test('reparse button exists only in exclusive original column', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', state: stateWithSlots() })}
      />,
    );

    const reparse = screen.getByTestId('instruction-reparse-from-original');
    expect(reparse.textContent).toContain('Import original as common');
    expect(screen.getByTestId('instruction-pane-original').contains(reparse)).toBe(true);
    expect(screen.queryAllByTestId('instruction-reparse-from-original')).toHaveLength(1);
  });

  test('exclusive sync button triggers onSync callback', () => {
    const onSync = vi.fn();
    render(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', onSync, state: stateWithSlots() })}
      />,
    );
    fireEvent.click(screen.getByTestId('instruction-sync-to-native'));
    expect(onSync).toHaveBeenCalledTimes(1);
  });

  test('write blocked disables exclusive sync button and shows reason', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          writeBlocked: true,
          writeBlockedReason: 'Writes are blocked for this agent',
          state: stateWithSlots(),
        })}
      />,
    );
    const sync = screen.getByTestId('instruction-sync-to-native') as HTMLButtonElement;
    expect(sync.disabled).toBe(true);
    expect(screen.getByTestId('instruction-write-blocked').textContent).toContain(
      'Writes are blocked for this agent',
    );
  });

  test('exclusive preview pane is read-only (no textarea for preview body)', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', state: stateWithSlots() })}
      />,
    );

    const preview = screen.getByTestId('instruction-pane-preview');
    expect(preview.querySelector('textarea')).toBeNull();
    expect(screen.getByTestId('instruction-preview-body')).toBeTruthy();
  });
});
