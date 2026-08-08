// @vitest-environment jsdom
/**
 * 提示词三栏 pure view 测试。
 *
 * Business Logic: 锁定三栏可见、reparse 仅在原始栏、同步回调与无 @/api 依赖。
 * Code Logic: 注入 labels + state + callbacks；静态扫描源文件。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import {
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
  type InstructionThreePaneState,
} from './instructionThreePane';
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
  blocksTitle: 'Blocks',
  previewTitle: 'Preview',
  originalTitle: 'Original file',
  reparseFromOriginal: 'Reparse blocks from original',
  syncToNative: 'Sync to native file…',
  emptyBlocks: 'No blocks yet',
  emptyPreview: 'Preview is empty until you reparse or fill blocks',
  emptyOriginal: 'No original content',
  pathLabel: 'Path',
  noPath: 'No path',
  loading: 'Loading instructions…',
  retry: 'Retry',
  previewReadOnly: 'Read-only composed preview',
  addBlock: 'Add block',
  dualDirtyTitle: 'Choose sync baseline',
  dualDirtyDescription: 'Blocks and original both changed.',
  useBlocksBaseline: 'Use composed blocks',
  useOriginalBaseline: 'Use original file',
  cancel: 'Cancel',
  blockTitlePlaceholder: 'Title',
  blockBodyPlaceholder: 'Body',
  refresh: 'Rescan',
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
    loading: false,
    error: null,
    actionError: null,
    actionBusy: false,
    writeBlocked: false,
    writeBlockedReason: null,
    dualDirtyOpen: false,
    onReparse: vi.fn(),
    onSync: vi.fn(),
    onRetry: vi.fn(),
    onRefresh: vi.fn(),
    onOriginalChange: vi.fn(),
    onBlockChange: vi.fn(),
    onAddBlock: vi.fn(),
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
    expect(screen.getByTestId('instruction-pane-blocks').textContent).toContain('Blocks');
    expect(screen.getByTestId('instruction-pane-preview').textContent).toContain('Preview');
    expect(screen.getByTestId('instruction-pane-original').textContent).toContain('Original file');
  });

  test('reparse button exists only in the original column', () => {
    render(<InstructionThreePaneView {...buildProps()} />);

    const reparse = screen.getByTestId('instruction-reparse-from-original');
    expect(reparse.textContent).toContain('Reparse blocks from original');
    expect(
      screen.getByTestId('instruction-pane-original').contains(reparse),
    ).toBe(true);
    expect(screen.queryAllByTestId('instruction-reparse-from-original')).toHaveLength(1);
    expect(screen.getByTestId('instruction-pane-blocks').textContent).not.toContain(
      'Reparse blocks from original',
    );
    expect(screen.getByTestId('instruction-pane-preview').textContent).not.toContain(
      'Reparse blocks from original',
    );
  });

  test('sync button triggers onSync callback', () => {
    const onSync = vi.fn();
    render(<InstructionThreePaneView {...buildProps({ onSync })} />);

    fireEvent.click(screen.getByTestId('instruction-sync-to-native'));
    expect(onSync).toHaveBeenCalledTimes(1);
  });

  test('reparse button triggers onReparse callback', () => {
    const onReparse = vi.fn();
    render(<InstructionThreePaneView {...buildProps({ onReparse })} />);

    fireEvent.click(screen.getByTestId('instruction-reparse-from-original'));
    expect(onReparse).toHaveBeenCalledTimes(1);
  });

  test('write blocked disables sync and shows reason', () => {
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

  test('after reparse, block list reflects state.blocks (view is pure)', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE);
    const parsed = parseBlocksFromOriginal(opened);
    render(<InstructionThreePaneView {...buildProps({ state: parsed })} />);

    expect(screen.getByTestId('instruction-block-list').children.length).toBe(2);
    expect(screen.queryByTestId('instruction-blocks-empty')).toBeNull();
  });

  test('preview pane is read-only (no textarea for preview body)', () => {
    const opened = initialThreePaneFromDisk('/p.md', SAMPLE);
    const parsed = parseBlocksFromOriginal(opened);
    render(<InstructionThreePaneView {...buildProps({ state: parsed })} />);

    const preview = screen.getByTestId('instruction-pane-preview');
    expect(preview.querySelector('textarea')).toBeNull();
    expect(screen.getByTestId('instruction-preview-body').textContent).toContain('Shared');
  });
});
