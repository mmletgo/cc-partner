// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import type { TFunction } from 'i18next';
import i18n from '@/i18n';
import type { UseUserInstructionManagerResult } from './useUserInstructionManager';
import { UserInstructionView } from './UserInstructionView';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(cleanup);

/** 构造只读发现模式 manager，确保页面不会伪装写入能力。 */
function readOnlyManager(): UseUserInstructionManagerResult {
  return {
    workspace: {
      scopeId: 'agent-hub-scope-user',
      setupState: 'readyToReview',
      healthState: 'healthy',
      canonical: null,
      inventorySnapshotHash: 'legacy-read-only:owner',
      refreshedAt: '2026-08-05T00:00:00.000Z',
      targets: [
        {
          target: 'claude',
          cli: { installed: true, version: '1.0', configRoot: '/config/claude' },
          sources: [],
          effectiveSourceId: null,
          managedTargetPath: null,
          managementMode: 'unmanaged',
          capability: {
            scan: 'readOnly',
            write: 'blocked',
            remove: 'blocked',
            activate: 'blocked',
            reasonCode: 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE',
            evidenceIds: [],
          },
          projection: {
            state: 'none',
            desiredRevisionId: null,
            appliedRevisionId: null,
            observedHash: null,
            lastErrorCode: null,
          },
          availableActions: [],
        },
      ],
    },
    loading: false,
    refreshing: false,
    error: null,
    actionError: null,
    actionBusy: false,
    draft: {
      commonContent: '',
      targetExtensions: {},
      targetSelections: {
        claude: 'unmanaged',
        codex: 'unmanaged',
        opencode: 'unmanaged',
        grok: 'unmanaged',
        gemini: 'unmanaged',
      },
    },
    dirty: false,
    activePane: 'common',
    setActivePane: vi.fn(),
    updateDraftContent: vi.fn(),
    resetDraft: vi.fn(),
    setupOpen: false,
    openSetup: vi.fn(),
    closeSetup: vi.fn(),
    setTargetSelection: vi.fn(),
    promoteTargetExtensionToCommon: vi.fn(),
    previewOpen: false,
    plan: null,
    closePreview: vi.fn(),
    previewDraft: vi.fn(async () => undefined),
    applyPlan: vi.fn(async () => undefined),
    applyResult: null,
    dismissApplyResult: vi.fn(),
    runTargetIntent: vi.fn(async () => undefined),
    openPath: vi.fn(async () => undefined),
    copyPath: vi.fn(async () => undefined),
    refresh: vi.fn(async () => undefined),
    canPreview: false,
    canonicalContentTruncated: false,
    deleteDialogOpen: false,
    deleteConfirmation: '',
    setDeleteConfirmation: vi.fn(),
    openDeleteDialog: vi.fn(),
    closeDeleteDialog: vi.fn(),
    previewDeleteAsset: vi.fn(async () => undefined),
  };
}

describe('UserInstructionView scan-only mode', () => {
  test('explains read-only discovery and disables all write CTAs', () => {
    const manager = readOnlyManager();
    render(
      <UserInstructionView
        t={i18n.t.bind(i18n) as unknown as TFunction<['agentHub', 'common']>}
        manager={manager}
      />,
    );

    expect(screen.getByTestId('user-instruction-scan-only').textContent).toContain('暂不支持自动写入');
    expect(screen.getByTestId('user-instruction-editor-read-only')).toBeTruthy();
    expect((screen.getByTestId('user-instruction-primary-action') as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByTestId('user-instruction-preview-draft') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId('user-instruction-target-claude').textContent).toContain(
      '不会猜测文件路径',
    );
  });
});
