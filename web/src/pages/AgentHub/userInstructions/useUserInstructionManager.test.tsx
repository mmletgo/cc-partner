// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import type { TFunction } from 'i18next';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type {
  UserInstructionPlanDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';

const apiMocks = vi.hoisted(() => ({
  inspectUserInstructionWorkspace: vi.fn(),
  previewUserInstructionSetup: vi.fn(),
  previewUserInstructionUpdate: vi.fn(),
  applyUserInstructionPlan: vi.fn(),
  previewPauseUserInstructionTarget: vi.fn(),
  previewStopManagingUserInstructionTarget: vi.fn(),
  previewRemoveUserInstructionTarget: vi.fn(),
  previewAdoptUserInstructionSource: vi.fn(),
  previewDeleteUserInstructionAsset: vi.fn(),
  openUserInstructionPath: vi.fn(),
}));

vi.mock('@/api/agentHub', () => ({ agentHubApi: apiMocks }));

import { useUserInstructionManager } from './useUserInstructionManager';

const t = ((key: string) => key) as TFunction<['agentHub', 'common']>;

/** 构造支持写入的首次设置 workspace。 */
function workspaceFixture(): UserInstructionWorkspaceDto {
  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'readyToReview',
    healthState: 'healthy',
    canonical: null,
    inventorySnapshotHash: 'inventory-1',
    refreshedAt: '2026-08-05T00:00:00.000Z',
    targets: [
      {
        target: 'claude',
        cli: { installed: true, version: '1.0', configRoot: '/config/claude' },
        sources: [],
        effectiveSourceId: null,
        managedTargetPath: '/managed/CLAUDE.md',
        managementMode: 'unmanaged',
        capability: {
          scan: 'supported',
          write: 'supported',
          remove: 'supported',
          activate: 'newSession',
          reasonCode: null,
          evidenceIds: [],
        },
        projection: {
          state: 'none',
          desiredRevisionId: null,
          appliedRevisionId: null,
          observedHash: null,
          lastErrorCode: null,
        },
        availableActions: ['manage'],
      },
    ],
  };
}

/** 构造可应用的零写入预览结果。 */
function planFixture(): UserInstructionPlanDto {
  return {
    planToken: 'plan-1',
    expiresAt: '2026-08-05T00:05:00.000Z',
    baseRevisionId: null,
    inventorySnapshotHash: 'inventory-1',
    blockingReasons: [],
    changes: [
      {
        target: 'claude',
        path: '/managed/CLAUDE.md',
        operation: 'create',
        currentHash: null,
        expectedHash: null,
        renderedHash: 'rendered-1',
        unifiedDiff: '+shared rule',
        ownershipRequired: false,
        willShadowSourcePath: null,
        willReplaceFallbackSourcePath: null,
        emptyDueToTargetOnly: false,
        activation: 'newSession',
        warnings: [],
      },
    ],
  };
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

afterEach(() => {
  vi.clearAllMocks();
});

describe('useUserInstructionManager', () => {
  test('editing and setup selection stay local until preview is requested', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.previewUserInstructionSetup.mockResolvedValue(planFixture());
    const { result } = renderHook(() => useUserInstructionManager(t));
    await act(async () => {
      await result.current.refresh();
    });
    await waitFor(() => expect(result.current.workspace).not.toBeNull());

    act(() => {
      result.current.updateDraftContent('common', 'shared rule');
      result.current.openSetup();
      result.current.setTargetSelection('claude', 'managed');
    });
    expect(apiMocks.previewUserInstructionSetup).not.toHaveBeenCalled();
    expect(apiMocks.applyUserInstructionPlan).not.toHaveBeenCalled();

    await act(async () => {
      await result.current.previewDraft();
    });
    expect(apiMocks.previewUserInstructionSetup).toHaveBeenCalledTimes(1);
    expect(apiMocks.applyUserInstructionPlan).not.toHaveBeenCalled();
  });

  test('stale apply closes the old plan and preserves the local draft', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.previewUserInstructionSetup.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockRejectedValue(
      Object.assign(new Error('stale'), { code: 'USER_INSTRUCTION_PREVIEW_STALE' }),
    );
    const { result } = renderHook(() => useUserInstructionManager(t));
    await act(async () => {
      await result.current.refresh();
    });
    await waitFor(() => expect(result.current.workspace).not.toBeNull());
    act(() => {
      result.current.updateDraftContent('common', 'keep this draft');
      result.current.setTargetSelection('claude', 'managed');
    });
    await act(async () => {
      await result.current.previewDraft();
    });
    await act(async () => {
      await result.current.applyPlan();
    });

    expect(result.current.draft.commonContent).toBe('keep this draft');
    expect(result.current.dirty).toBe(true);
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.actionError).toBe(
      'agentHub:userInstructions.errors.previewStale',
    );
  });

  test('editing while preview is pending prevents the old plan from landing', async () => {
    const previewDeferred = deferred<UserInstructionPlanDto>();
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.previewUserInstructionSetup.mockReturnValue(previewDeferred.promise);
    const { result } = renderHook(() => useUserInstructionManager(t));
    await act(async () => {
      await result.current.refresh();
    });
    act(() => {
      result.current.updateDraftContent('common', 'draft A');
      result.current.setTargetSelection('claude', 'managed');
    });

    let previewPromise!: Promise<void>;
    act(() => {
      previewPromise = result.current.previewDraft();
    });
    await waitFor(() => expect(apiMocks.previewUserInstructionSetup).toHaveBeenCalledOnce());
    act(() => result.current.updateDraftContent('common', 'draft B'));
    previewDeferred.resolve(planFixture());
    await act(async () => previewPromise);

    expect(result.current.draft.commonContent).toBe('draft B');
    expect(result.current.plan).toBeNull();
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.actionBusy).toBe(false);
  });
});
