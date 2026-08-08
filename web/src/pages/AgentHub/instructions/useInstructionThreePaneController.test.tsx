// @vitest-environment jsdom
/**
 * 提示词三栏 controller 测试。
 *
 * Business Logic: inspect 加载原文且块为空；显式 reparse 才填块；同步单 destination。
 * Code Logic: mock agentHubApi；renderHook + waitFor。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import type { TFunction } from 'i18next';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type {
  UserInstructionApplyResultDto,
  UserInstructionPlanDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';

const apiMocks = vi.hoisted(() => ({
  inspectUserInstructionWorkspace: vi.fn(),
  previewUserInstructionSetup: vi.fn(),
  previewUserInstructionUpdate: vi.fn(),
  applyUserInstructionPlan: vi.fn(),
}));

vi.mock('@/api/agentHub', () => ({ agentHubApi: apiMocks }));

import {
  originalFromWorkspace,
  useInstructionThreePaneController,
} from './useInstructionThreePaneController';

const t = ((key: string) => key) as TFunction<['agentHub', 'common']>;

const SAMPLE_MARKDOWN = `## Shared rules

Always use TypeScript.

## Target notes

CLI-specific flags only.
`;

/** 构造可写入、带 canonical 原文的 workspace。 */
function workspaceFixture(
  overrides: Partial<UserInstructionWorkspaceDto> = {},
): UserInstructionWorkspaceDto {
  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'configured',
    healthState: 'healthy',
    inventorySnapshotHash: 'inventory-1',
    refreshedAt: '2026-08-08T00:00:00.000Z',
    canonical: {
      assetId: 'asset-user-instruction',
      displayName: 'User instructions',
      headRevisionId: 'rev-1',
      commonContent: SAMPLE_MARKDOWN,
      targetExtensions: {},
      deleted: false,
      contentTruncated: false,
    },
    targets: [
      {
        target: 'claude',
        cli: { installed: true, version: '1.0', configRoot: '/config/claude' },
        sources: [
          {
            sourceId: 'claude-managed',
            path: '/home/user/.claude/CLAUDE.md',
            role: 'native',
            active: true,
            exists: true,
            nonEmpty: true,
            hash: 'hash-1',
            modifiedAt: null,
            ownership: 'hubManaged',
          },
        ],
        effectiveSourceId: 'claude-managed',
        managedTargetPath: '/home/user/.claude/CLAUDE.md',
        managementMode: 'managedActive',
        capability: {
          scan: 'supported',
          write: 'supported',
          remove: 'supported',
          activate: 'newSession',
          reasonCode: null,
          evidenceIds: [],
        },
        projection: {
          state: 'inSync',
          desiredRevisionId: 'rev-1',
          appliedRevisionId: 'rev-1',
          observedHash: 'hash-1',
          lastErrorCode: null,
        },
        availableActions: ['pause'],
      },
      {
        target: 'codex',
        cli: { installed: true, version: '1.0', configRoot: '/config/codex' },
        sources: [],
        effectiveSourceId: null,
        managedTargetPath: '/home/user/.codex/AGENTS.md',
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
      {
        target: 'opencode',
        cli: { installed: false, version: null, configRoot: '' },
        sources: [],
        effectiveSourceId: null,
        managedTargetPath: null,
        managementMode: 'unmanaged',
        capability: {
          scan: 'blocked',
          write: 'blocked',
          remove: 'blocked',
          activate: 'blocked',
          reasonCode: 'cli_missing',
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
    ...overrides,
  };
}

function planFixture(): UserInstructionPlanDto {
  return {
    planToken: 'plan-1',
    expiresAt: '2026-08-08T00:05:00.000Z',
    baseRevisionId: 'rev-1',
    inventorySnapshotHash: 'inventory-1',
    blockingReasons: [],
    changes: [
      {
        target: 'claude',
        path: '/home/user/.claude/CLAUDE.md',
        operation: 'update',
        currentHash: 'hash-1',
        expectedHash: 'hash-1',
        renderedHash: 'hash-2',
        unifiedDiff: '-old\n+new',
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

function applyFixture(): UserInstructionApplyResultDto {
  return {
    planToken: 'plan-1',
    setupState: 'configured',
    healthState: 'healthy',
    targets: [
      {
        target: 'claude',
        path: '/home/user/.claude/CLAUDE.md',
        status: 'applied',
        errorCode: null,
        activation: 'newSession',
      },
    ],
  };
}

const baseContext: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  adaptView: false,
};

afterEach(() => {
  vi.clearAllMocks();
});

describe('originalFromWorkspace', () => {
  test('uses effective source path and common + extension content', () => {
    const workspace = workspaceFixture({
      canonical: {
        assetId: 'a',
        displayName: 'User',
        headRevisionId: 'r',
        commonContent: '## Common\n\nbody',
        targetExtensions: { claude: '## Extra\n\ncli' },
        deleted: false,
        contentTruncated: false,
      },
    });
    const result = originalFromWorkspace(workspace, 'claude');
    expect(result.path).toBe('/home/user/.claude/CLAUDE.md');
    expect(result.text).toContain('## Common');
    expect(result.text).toContain('## Extra');
  });
});

describe('useInstructionThreePaneController', () => {
  test('inspect loads original and leaves blocks empty (no auto-parse)', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalled();
    expect(result.current.state.originalPath).toBe('/home/user/.claude/CLAUDE.md');
    expect(result.current.state.originalText).toContain('## Shared rules');
    expect(result.current.state.blocks).toEqual([]);
    expect(result.current.state.previewText).toBe('');
    expect(result.current.state.blocksDirty).toBe(false);
  });

  test('reparse fills blocks from original', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.reparseFromOriginal();
    });

    expect(result.current.state.blocks.length).toBe(2);
    expect(result.current.state.blocks[0]?.title).toBe('Shared rules');
    expect(result.current.state.previewText).toContain('## Shared rules');
    expect(result.current.state.blocksDirty).toBe(false);
  });

  test('write blocked when target write is not supported', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        targets: workspaceFixture().targets.map((target) =>
          target.target === 'claude'
            ? {
                ...target,
                capability: { ...target.capability, write: 'blocked' as const },
              }
            : target,
        ),
      }),
    );
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.writeBlocked).toBe(true);
    expect(result.current.writeBlockedReason).toBeTruthy();
  });

  test('sync previews single destination = context.agent then apply reloads', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockResolvedValue(applyFixture());

    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.requestSync();
    });
    expect(apiMocks.previewUserInstructionUpdate).toHaveBeenCalledTimes(1);
    const previewArg = apiMocks.previewUserInstructionUpdate.mock.calls[0]?.[0];
    expect(previewArg?.targetSelections).toEqual({
      claude: 'managed',
      codex: 'unmanaged',
      opencode: 'unmanaged',
    });
    expect(previewArg?.commonContent).toContain('## Shared rules');
    expect(result.current.previewOpen).toBe(true);

    await act(async () => {
      await result.current.applyPlan();
    });
    expect(apiMocks.applyUserInstructionPlan).toHaveBeenCalledTimes(1);
    expect(apiMocks.inspectUserInstructionWorkspace.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  test('successful sync with original baseline auto re-parses blocks once', async () => {
    const afterWrite = workspaceFixture({
      canonical: {
        assetId: 'asset-user-instruction',
        displayName: 'User instructions',
        headRevisionId: 'rev-2',
        commonContent: SAMPLE_MARKDOWN,
        targetExtensions: {},
        deleted: false,
        contentTruncated: false,
      },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(afterWrite);
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockResolvedValue(applyFixture());

    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    // 初始块空
    expect(result.current.state.blocks).toEqual([]);

    await act(async () => {
      await result.current.requestSync();
    });
    await act(async () => {
      await result.current.applyPlan();
    });

    await waitFor(() => expect(result.current.state.blocks.length).toBe(2));
    expect(result.current.state.blocks[0]?.title).toBe('Shared rules');
  });

  test('changing deviceId retriggers inspect with peer context', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockImplementation(
      async (ctx?: { deviceId?: string | null }) => {
        if (ctx?.deviceId === 'peer-9') {
          throw Object.assign(new Error('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE'), {
            code: 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE',
          });
        }
        return workspaceFixture();
      },
    );

    const { result, rerender } = renderHook(
      (props: { context: AgentHubContext }) =>
        useInstructionThreePaneController({ context: props.context, t }),
      { initialProps: { context: baseContext } },
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledWith({
      deviceId: null,
      projectRef: null,
    });
    expect(result.current.workspace).not.toBeNull();

    rerender({
      context: { ...baseContext, deviceId: 'peer-9' },
    });

    await waitFor(() => {
      expect(result.current.error).toBe('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE');
    });
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledWith({
      deviceId: 'peer-9',
      projectRef: null,
    });
    expect(result.current.workspace).toBeNull();
  });

  test('local inspect passes null device context and remote project fails closed', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockImplementation(
      async (ctx?: { projectRef?: string | null }) => {
        if (ctx?.projectRef?.startsWith('remote:')) {
          throw Object.assign(new Error('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE'), {
            code: 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE',
          });
        }
        return workspaceFixture();
      },
    );

    const { result, rerender } = renderHook(
      (props: { context: AgentHubContext }) =>
        useInstructionThreePaneController({ context: props.context, t }),
      {
        initialProps: {
          context: {
            ...baseContext,
            scope: 'project' as const,
            projectKey: 'wb-local',
            deviceId: null,
          },
        },
      },
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledWith({
      deviceId: null,
      projectRef: 'wb-local',
    });

    rerender({
      context: {
        ...baseContext,
        scope: 'project',
        projectKey: 'remote:dev1:inner',
        deviceId: null,
      },
    });

    await waitFor(() => {
      expect(result.current.error).toBe('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE');
    });
  });
});
