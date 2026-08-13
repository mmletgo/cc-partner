// @vitest-environment jsdom
/**
 * 提示词三栏 pure view 测试（按 lane 布局）。
 *
 * Business Logic: 锁定公共单列、适配单列、独有三列与无 @/api 依赖。
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
  analyzeDecompose: 'Analyze & split',
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
  commonMarkdown: 'Common body',
  saveBlocks: 'Save slots',
  aiRevise: 'AI-assisted prompt edit',
  aiReviseTitle: 'AI-assisted prompt edit',
  aiReviseDescriptionCommon: 'Revise the common slot.',
  aiReviseDescriptionExclusive: 'Revise the exclusive slot.',
  aiReviseDescriptionAdapted: 'Revise all adapted slots.',
  aiReviseDirectionLabel: 'Direction',
  aiReviseDirectionPlaceholder: 'Type a direction',
  aiReviseConfirm: 'Revise and save',
  aiReviseSavedAndLocated: 'Saved and located the first change',
  aiReviseSavedOtherAgents: 'Saved changes for other agents',
  aiReviseSavedNoChange: 'Saved with no content change',
  adaptToOtherAgents: 'Adapt to other agents',
  syncToNative: 'Write to native file',
  unsavedDraft: 'Unsaved slot draft',
  canonicalDrift: 'Canonical changed',
  sourceDrift: 'Native source changed',
  originalReadOnly: 'Read-only source',
  discardAndReload: 'Discard and reload',
  analyzeConfirmTitle: 'Analyze and overwrite?',
  analyzeConfirmDescription: 'Overwrite slots with split parts.',
  analyzeConfirm: 'Start analyze',
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
    busyAction: null,
    writeBlocked: false,
    writeBlockedReason: null,
    dualDirtyOpen: false,
    analyzeConfirmOpen: false,
    aiReviseOpen: false,
    aiReviseDirection: '',
    aiReviseError: null,
    aiReviseFeedback: null,
    aiReviseDisabled: false,
    onAnalyzeDecompose: vi.fn(),
    onAdaptToOtherAgents: vi.fn(),
    onSaveBlocks: vi.fn(),
    onOpenAiRevise: vi.fn(),
    onAiReviseDirectionChange: vi.fn(),
    onCancelAiRevise: vi.fn(),
    onConfirmAiRevise: vi.fn(),
    onRequestSync: vi.fn(),
    onRetry: vi.fn(),
    onDiscardAndReload: vi.fn(),
    onSlotTextChange: vi.fn(),
    onChooseBaseline: vi.fn(),
    onCancelDualDirty: vi.fn(),
    onConfirmAnalyze: vi.fn(),
    onCancelAnalyze: vi.fn(),
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
        variants: { claude: 'claude adapted', codex: 'codex variant' },
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

  test('all three lanes expose the AI revise button', () => {
    for (const lane of ['common', 'adapted', 'exclusive'] as const) {
      cleanup();
      render(<InstructionThreePaneView {...buildProps({ instructionLane: lane })} />);
      expect(screen.getByTestId('instruction-ai-revise')).toBeTruthy();
    }
  });

  test('common lane shows only the common slot without rescan/sync/original', () => {
    render(<InstructionThreePaneView {...buildProps({ instructionLane: 'common' })} />);

    expect(screen.getByTestId('instruction-three-pane')).toBeTruthy();
    expect(screen.getByTestId('instruction-panes-common')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks')).toBeTruthy();
    expect(screen.queryByTestId('instruction-pane-preview')).toBeNull();
    expect(screen.queryByTestId('instruction-pane-original')).toBeNull();
    expect(screen.queryByTestId('instruction-rescan')).toBeNull();
    expect(screen.queryByTestId('instruction-sync-to-native')).toBeNull();
    expect(screen.queryByTestId('instruction-original-path')).toBeNull();
    expect(screen.queryByTestId('instruction-adapt-to-other-agents')).toBeNull();
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

  test('adapted lane is a single editable column with adapt-to-others next to save', () => {
    const onSlotTextChange = vi.fn();
    const onAdaptToOtherAgents = vi.fn();
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'adapted',
          agent: 'codex',
          state: stateWithSlots(),
          onSlotTextChange,
          onAdaptToOtherAgents,
        })}
      />,
    );
    expect(screen.getByTestId('instruction-panes-adapted')).toBeTruthy();
    expect(screen.getByTestId('instruction-adapted-textarea')).toBeTruthy();
    expect(screen.queryByTestId('instruction-pane-adapted-common')).toBeNull();
    expect(screen.queryByTestId('instruction-pane-preview')).toBeNull();
    expect(screen.queryByTestId('instruction-rescan')).toBeNull();
    expect(screen.queryByTestId('instruction-sync-to-native')).toBeNull();
    expect(screen.getByTestId('instruction-adapt-to-other-agents')).toBeTruthy();

    fireEvent.change(screen.getByTestId('instruction-adapted-textarea'), {
      target: { value: 'codex next' },
    });
    expect(onSlotTextChange).toHaveBeenCalledWith('codex next');
    fireEvent.click(screen.getByTestId('instruction-adapt-to-other-agents'));
    expect(onAdaptToOtherAgents).toHaveBeenCalledOnce();
  });

  test('AI revise success focuses and selects the current adapted-slot change', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'adapted',
          state: stateWithSlots(),
          aiReviseFeedback: {
            currentSlotChanged: true,
            otherAdaptedSlotsChanged: true,
            selection: { start: 7, end: 14 },
          },
        })}
      />,
    );

    const textarea = screen.getByTestId(
      'instruction-adapted-textarea',
    ) as HTMLTextAreaElement;
    expect(screen.getByTestId('instruction-ai-revise-success').textContent).toContain(
      'Saved and located the first change',
    );
    expect(document.activeElement).toBe(textarea);
    expect(textarea.selectionStart).toBe(7);
    expect(textarea.selectionEnd).toBe(14);
  });

  test('AI revise success explains other-agent-only changes without stealing focus', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'adapted',
          state: stateWithSlots(),
          aiReviseFeedback: {
            currentSlotChanged: false,
            otherAdaptedSlotsChanged: true,
            selection: null,
          },
        })}
      />,
    );

    expect(screen.getByTestId('instruction-ai-revise-success').textContent).toContain(
      'Saved changes for other agents',
    );
    expect(document.activeElement).not.toBe(
      screen.getByTestId('instruction-adapted-textarea'),
    );
  });

  test('exclusive lane keeps three panes, write-to-native, and analyze-decompose on original column', () => {
    const onAnalyzeDecompose = vi.fn();
    const onRequestSync = vi.fn();
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: stateWithSlots(),
          onAnalyzeDecompose,
          onRequestSync,
        })}
      />,
    );

    expect(screen.getByTestId('instruction-panes-exclusive')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-blocks')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-preview')).toBeTruthy();
    expect(screen.getByTestId('instruction-pane-original')).toBeTruthy();
    expect(screen.queryByTestId('instruction-rescan')).toBeNull();
    expect(screen.queryByTestId('instruction-reparse-from-original')).toBeNull();

    const sync = screen.getByTestId('instruction-sync-to-native');
    expect(sync.textContent).toContain('Write to native file');
    expect((sync as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(sync);
    expect(onRequestSync).toHaveBeenCalledOnce();

    const analyze = screen.getByTestId('instruction-analyze-decompose');
    expect(analyze.textContent).toContain('Analyze & split');
    expect(screen.getByTestId('instruction-pane-original').contains(analyze)).toBe(true);
    fireEvent.click(analyze);
    expect(onAnalyzeDecompose).toHaveBeenCalledOnce();
  });

  test('analyze busy shows spinner on analyze button only, not save slots', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: { ...stateWithSlots(), blocksDirty: true },
          actionBusy: true,
          busyAction: 'analyze',
        })}
      />,
    );

    const analyze = screen.getByTestId('instruction-analyze-decompose') as HTMLButtonElement;
    const save = screen.getByTestId('instruction-save-blocks') as HTMLButtonElement;
    const sync = screen.getByTestId('instruction-sync-to-native') as HTMLButtonElement;

    expect(analyze.getAttribute('data-loading')).toBe('true');
    expect(analyze.disabled).toBe(true);
    expect(save.getAttribute('data-loading')).toBeNull();
    expect(save.disabled).toBe(true);
    expect(sync.getAttribute('data-loading')).toBeNull();
    expect(sync.disabled).toBe(true);
  });

  test('save busy shows spinner on save slots only', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: { ...stateWithSlots(), blocksDirty: true },
          actionBusy: true,
          busyAction: 'save',
        })}
      />,
    );

    const analyze = screen.getByTestId('instruction-analyze-decompose') as HTMLButtonElement;
    const save = screen.getByTestId('instruction-save-blocks') as HTMLButtonElement;

    expect(save.getAttribute('data-loading')).toBe('true');
    expect(analyze.getAttribute('data-loading')).toBeNull();
  });

  test('exclusive write-to-native is disabled and explained when write is blocked', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: stateWithSlots(),
          writeBlocked: true,
          writeBlockedReason: 'write blocked reason',
        })}
      />,
    );
    expect((screen.getByTestId('instruction-sync-to-native') as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(screen.getByTestId('instruction-write-blocked').textContent).toContain(
      'write blocked reason',
    );
  });

  test('original source is read-only and save is gated by blocks dirty/drift', () => {
    const clean = stateWithSlots();
    clean.blocksDirty = false;
    const { rerender } = render(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', state: clean })}
      />,
    );
    expect((screen.getByTestId('instruction-original-textarea') as HTMLTextAreaElement).readOnly).toBe(
      true,
    );
    expect((screen.getByTestId('instruction-save-blocks') as HTMLButtonElement).disabled).toBe(true);

    const dirty = { ...clean, blocksDirty: true };
    rerender(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', state: dirty })}
      />,
    );
    expect((screen.getByTestId('instruction-save-blocks') as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByTestId('instruction-unsaved-draft')).toBeTruthy();

    rerender(
      <InstructionThreePaneView
        {...buildProps({
          instructionLane: 'exclusive',
          state: { ...dirty, externalDrift: true },
        })}
      />,
    );
    expect((screen.getByTestId('instruction-save-blocks') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId('instruction-canonical-drift')).toBeTruthy();
  });

  test('exclusive preview pane is read-only (no textarea for preview body)', () => {
    render(
      <InstructionThreePaneView
        {...buildProps({ instructionLane: 'exclusive', state: stateWithSlots() })}
      />,
    );
    expect(screen.getByTestId('instruction-preview-body').tagName.toLowerCase()).toBe('pre');
    expect(
      screen.getByTestId('instruction-pane-preview').querySelector('textarea'),
    ).toBeNull();
  });
});
