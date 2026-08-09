// @vitest-environment jsdom
/**
 * 提示词三栏 pure view 测试。
 *
 * Business Logic: 锁定三栏可见、reparse 仅在原始栏、单槽编辑与无 @/api 依赖。
 * Code Logic: 注入 labels + state + callbacks；静态扫描源文件。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { AgentTarget } from '@/lib/types/agentHub';
import {
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
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
    onChooseBaseline: vi.fn(),
    onCancelDualDirty: vi.fn(),
    ...overrides,
  };
}

describe('InstructionThreePaneView', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(viewDir, './InstructionThreePaneView.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('renders three panes: blocks, preview, original', () => {
    render(<InstructionThreePaneView {...buildProps()} />);

    expect(screen.getByTestId('instruction-three-pane')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-preview')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-original')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks').textContent).toContain('Current slot');
    expect(screen.getByTestId('instruction-pane-preview').textContent).toContain('Preview');
    expect(screen.getByTestId('instruction-pane-original').textContent).toContain('Original file');
  });

  test('reparse button exists only in the original column', () => {
    render(<InstructionThreePaneView {...buildProps()} />);

    const reparse = screen.getByTestId('instruction-reparse-from-original');
    expect(reparse.textContent).toContain('Import original as common');
    expect(
      screen.getByTestId('instruction-pane-original').contains(reparse),
    ).toBe(true);
    expect(screen.queryAllByTestId('instruction-reparse-from-original')).toHaveLength(1);
    expect(screen.getByTestId('instruction-pane-blocks').textContent).not.toContain(
      'Import original as common',
    );
    expect(screen.getByTestId('instruction-pane-preview').textContent).not.toContain(
      'Import original as common',
    );
  });

  test('sync button triggers onSync callback', () => {
    const onSync = vi.fn();
    render(<InstructionThreePaneView {...buildProps({ onSync })} />);
    fireEvent.click(screen.getByTestId('instruction-sync-to-native'));
    expect(onSync).toHaveBeenCalledTimes(1);
  });

  test('slot textarea edits call onSlotTextChange', () => {
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

  test('write blocked disables sync button and shows reason', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          writeBlocked: true,
          writeBlockedReason: 'Writes are blocked for this agent',
        })}
      />,
    );
    const sync = screen.getByTestId('instruction-sync-to-native') as HTMLButtonElement;
    expect(sync.disabled).toBe(true);
    expect(screen.getByTestId('instruction-write-blocked').textContent).toContain(
      'Writes are blocked for this agent',
    );
  });

  test('after reparse, single shared slot is shown', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE, null, AGENT);
    const parsed = parseBlocksFromOriginal(opened, AGENT);
    render(<InstructionThreePaneView {...buildProps({ state: parsed })} />);

    expect(screen.getByTestId('instruction-block-list').children.length).toBe(1);
    expect(screen.getByTestId('instruction-slot-textarea')).toBeTruthy();
  });

  test('preview pane is read-only (no textarea for preview body)', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE, null, AGENT);
    const parsed = parseBlocksFromOriginal(opened, AGENT);
    render(<InstructionThreePaneView {...buildProps({ state: parsed })} />);

    const preview = screen.getByTestId('instruction-pane-preview');
    expect(preview.querySelector('textarea')).toBeNull();
    expect(screen.getByTestId('instruction-preview-body').textContent).toContain('Rules');
  });
});
