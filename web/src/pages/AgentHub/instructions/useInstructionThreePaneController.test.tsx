// @vitest-environment jsdom
/**
 * 提示词三栏 controller 测试。
 *
 * Business Logic: inspect 加载原文且块为空；显式 reparse 才填块；公共槽同步全部可写目标。
 * Code Logic: mock agentHubApi；renderHook + waitFor。
 */

import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
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
  saveUserInstructionBlocks: vi.fn(),
  reviseInstructionSlot: vi.fn(),
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

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function workspaceWithSource(
  text: string,
  overrides: Partial<UserInstructionWorkspaceDto> = {},
): UserInstructionWorkspaceDto {
  const base = workspaceFixture(overrides);
  return {
    ...base,
    targets: base.targets.map((target) =>
      target.target === 'claude'
        ? {
            ...target,
            sources: target.sources.map((source) => ({
              ...source,
              content: text,
              contentTruncated: false,
            })),
          }
        : target,
    ),
  };
}

const baseContext: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  instructionLane: 'common',
  assetLane: 'equipped',
  adaptView: false,
};

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

describe('originalFromWorkspace', () => {
  test('prefers disk source.content over canonical for original pane', () => {
    const workspace = workspaceFixture({
      setupState: 'readyToReview',
      canonical: null,
      targets: [
        {
          ...workspaceFixture().targets[0]!,
          managementMode: 'unmanaged',
          sources: [
            {
              sourceId: 'claude-disk',
              path: '/home/user/.claude/CLAUDE.md',
              role: 'native',
              active: true,
              exists: true,
              nonEmpty: true,
              hash: 'hash-disk',
              modifiedAt: null,
              ownership: 'external',
              content: '## From disk\n\nAlways ship tests.\n',
              contentTruncated: false,
            },
          ],
          effectiveSourceId: 'claude-disk',
        },
        workspaceFixture().targets[1]!,
        workspaceFixture().targets[2]!,
      ],
    });
    const result = originalFromWorkspace(workspace, 'claude');
    expect(result.path).toBe('/home/user/.claude/CLAUDE.md');
    expect(result.text).toBe('## From disk\n\nAlways ship tests.\n');
    expect(result.contentTruncated).toBe(false);
  });

  test('falls back to canonical when source has no content field', () => {
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

  test('disk content wins even when canonical also present', () => {
    const workspace = workspaceFixture({
      targets: [
        {
          ...workspaceFixture().targets[0]!,
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
              content: '## Live disk\n\nnewer than hub\n',
              contentTruncated: false,
            },
          ],
        },
        workspaceFixture().targets[1]!,
        workspaceFixture().targets[2]!,
      ],
    });
    const result = originalFromWorkspace(workspace, 'claude');
    expect(result.text).toContain('## Live disk');
    expect(result.text).not.toContain('## Shared rules');
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

  test('inspect auto-loads unmanaged disk content into editable original pane', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        setupState: 'readyToReview',
        canonical: null,
        targets: [
          {
            ...workspaceFixture().targets[0]!,
            managementMode: 'unmanaged',
            sources: [
              {
                sourceId: 'claude-external',
                path: '/home/user/.claude/CLAUDE.md',
                role: 'native',
                active: true,
                exists: true,
                nonEmpty: true,
                hash: 'hash-ext',
                modifiedAt: null,
                ownership: 'external',
                content: '## Existing machine prompt\n\nDo not invent APIs.\n',
                contentTruncated: false,
              },
            ],
            effectiveSourceId: 'claude-external',
            availableActions: ['manage', 'adopt'],
          },
          workspaceFixture().targets[1]!,
          workspaceFixture().targets[2]!,
        ],
      }),
    );
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.state.originalText).toContain('## Existing machine prompt');
    expect(result.current.writeBlocked).toBe(false);

    act(() => {
      result.current.updateOriginal('## Existing machine prompt\n\nEdited locally.\n');
    });
    expect(result.current.state.originalText).toContain('Edited locally');
    expect(result.current.state.originalDirty).toBe(true);
  });

  test('editCurrentSlot fills shared slot from typed content', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.editCurrentSlot('Always use TypeScript');
    });

    expect(result.current.state.blocks.length).toBe(1);
    expect(result.current.state.blocks[0]?.mode).toBe('shared');
    expect(result.current.state.previewText).toContain('Always use TypeScript');
    expect(result.current.state.blocksDirty).toBe(true);
  });

  test('original-only edits do not reverse-write native text into canonical via requestSync', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        canonical: {
          ...workspaceFixture().canonical!,
          blocks: [
            {
              id: 'shared-1',
              mode: 'shared',
              commonMarkdown: '## Canonical body\n\nfrom hub\n',
              variants: null,
              headingPath: null,
              sourceTarget: null,
              needsAdaptation: false,
            },
          ],
        },
      }),
    );
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockResolvedValue(applyFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({
        context: { ...baseContext, instructionLane: 'exclusive' },
        t,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    const edited = '# Edited native document\n\nshould not become canonical.\n';
    act(() => {
      result.current.updateOriginal(edited);
    });
    await act(async () => {
      await result.current.requestSync();
    });

    // requestSync 固定 blocks 基线：不把原始栏正文 save 成 shared block。
    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();
    expect(apiMocks.previewUserInstructionUpdate).toHaveBeenCalledTimes(1);
    expect(apiMocks.applyUserInstructionPlan).toHaveBeenCalledTimes(1);
  });

  test('clean hydrated canonical blocks reuse the current head without an empty save', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        canonical: {
          ...workspaceFixture().canonical!,
          blocks: [
            {
              id: 'shared-1',
              mode: 'shared',
              commonMarkdown: SAMPLE_MARKDOWN,
              variants: null,
              headingPath: null,
              sourceTarget: null,
              needsAdaptation: false,
            },
          ],
        },
      }),
    );
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockResolvedValue(applyFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.requestSync();
    });

    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();
    expect(apiMocks.previewUserInstructionUpdate).toHaveBeenCalledTimes(1);
    expect(apiMocks.applyUserInstructionPlan).toHaveBeenCalledTimes(1);
  });

  test('requestSync rejects when composed preview is empty', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({
        context: { ...baseContext, instructionLane: 'exclusive' },
        t,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    // 初始块空、preview 空；即使 original 有内容也不能当写入源。
    expect(result.current.state.blocks).toEqual([]);
    expect(result.current.state.previewText).toBe('');

    await act(async () => {
      await result.current.requestSync();
    });

    expect(result.current.actionError).toBe(
      'agentHub:instructions.threePane.errors.emptySync',
    );
    expect(apiMocks.previewUserInstructionUpdate).not.toHaveBeenCalled();
    expect(apiMocks.applyUserInstructionPlan).not.toHaveBeenCalled();
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
      useInstructionThreePaneController({
        context: { ...baseContext, instructionLane: 'exclusive' },
        t,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.writeBlocked).toBe(true);
    expect(result.current.writeBlockedReason).toBeTruthy();
  });

  test('common lane prepares all destinations and writes them without a duplicate preview dialog', async () => {
    // requestSync 固定 blocks 基线：需要可合成的三槽 head，不能只靠原始文件正文。
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        canonical: {
          ...workspaceFixture().canonical!,
          blocks: [
            {
              id: 'shared-1',
              mode: 'shared',
              commonMarkdown: SAMPLE_MARKDOWN,
              variants: null,
              headingPath: null,
              sourceTarget: null,
              needsAdaptation: false,
            },
          ],
        },
      }),
    );
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
      codex: 'managed',
      opencode: 'unmanaged',
      grok: 'unmanaged',
      gemini: 'unmanaged',
      cursor: 'unmanaged',
      pi: 'unmanaged',
    });
    // backend preview 基于持久化 head 投影；前端只传 targetSelections/base/snapshot
    expect(previewArg?.commonContent).toBe('');
    expect(result.current.previewOpen).toBe(false);
    expect(apiMocks.applyUserInstructionPlan).toHaveBeenCalledTimes(1);
    expect(apiMocks.inspectUserInstructionWorkspace.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  test('successful blocks-baseline sync keeps hydrated slots without original reverse-parse', async () => {
    const hydrated = workspaceFixture({
      canonical: {
        ...workspaceFixture().canonical!,
        blocks: [
          {
            id: 'shared-1',
            mode: 'shared',
            commonMarkdown: SAMPLE_MARKDOWN,
            variants: null,
            headingPath: null,
            sourceTarget: null,
            needsAdaptation: false,
          },
        ],
      },
    });
    const afterWrite = {
      ...hydrated,
      inventorySnapshotHash: 'inventory-2',
      canonical: {
        ...hydrated.canonical!,
        headRevisionId: 'rev-2',
      },
    };
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(hydrated)
      .mockResolvedValue(afterWrite);
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    apiMocks.applyUserInstructionPlan.mockResolvedValue(applyFixture());

    const { result } = renderHook(() =>
      useInstructionThreePaneController({
        context: { ...baseContext, instructionLane: 'exclusive' },
        t,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.state.blocks).toHaveLength(1);

    await act(async () => {
      await result.current.requestSync();
    });

    await waitFor(() =>
      expect(apiMocks.applyUserInstructionPlan).toHaveBeenCalledTimes(1),
    );
    // blocks 基线写盘后 rescan hydrate canonical 块，不再从原文 auto re-parse。
    expect(result.current.state.blocks).toHaveLength(1);
    expect(result.current.state.blocks[0]?.mode).toBe('shared');
  });

  test('source-only refresh keeps canonical save available and uses latest observed snapshot', async () => {
    const initial = workspaceWithSource('## Disk\n\nold\n');
    const sourceChanged = workspaceWithSource('## Disk\n\nchanged elsewhere\n', {
      inventorySnapshotHash: 'inventory-2',
    });
    const afterSave = workspaceWithSource('## Disk\n\nchanged elsewhere\n', {
      inventorySnapshotHash: 'inventory-3',
      canonical: {
        ...workspaceFixture().canonical!,
        headRevisionId: 'rev-2',
      },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(initial)
      .mockResolvedValueOnce(sourceChanged)
      .mockResolvedValueOnce(afterSave);
    apiMocks.saveUserInstructionBlocks.mockResolvedValue(afterSave.canonical);
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledTimes(1);
    act(() => result.current.editCurrentSlot('seed draft'));

    await act(async () => result.current.refresh());
    expect(result.current.state.externalDrift).toBe(false);
    expect(result.current.state.sourceDrift).toBe(true);

    let saved = false;
    await act(async () => {
      saved = await result.current.saveBlocks();
    });
    expect(saved).toBe(true);
    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledWith(
      expect.objectContaining({
        baseRevisionId: 'rev-1',
        inventorySnapshotHash: 'inventory-2',
      }),
    );
    expect(result.current.state.originalText).toBe('## Disk\n\nold\n');
    expect(result.current.state.sourceDrift).toBe(true);
    expect(result.current.state.blocksDirty).toBe(false);
  });

  test('canonical refresh latches drift and never attempts a stale save', async () => {
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(
        workspaceFixture({
          inventorySnapshotHash: 'inventory-2',
          canonical: { ...workspaceFixture().canonical!, headRevisionId: 'rev-2' },
        }),
      );
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));
    await act(async () => result.current.refresh());
    expect(result.current.state.externalDrift).toBe(true);
    await act(async () => {
      expect(await result.current.saveBlocks()).toBe(false);
    });
    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();
  });

  test('Original-only Save is an honest no-op and preserves the draft', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.updateOriginal('## Local-only edit\n'));
    await act(async () => {
      expect(await result.current.saveBlocks()).toBe(false);
    });
    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();
    expect(result.current.state.originalText).toBe('## Local-only edit\n');
    expect(result.current.state.originalDirty).toBe(true);
  });

  test('edit during Save remains dirty after the older save response completes', async () => {
    const saveDeferred = deferred<unknown>();
    const afterSave = workspaceFixture({
      inventorySnapshotHash: 'inventory-2',
      canonical: { ...workspaceFixture().canonical!, headRevisionId: 'rev-2' },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(afterSave);
    apiMocks.saveUserInstructionBlocks.mockReturnValue(saveDeferred.promise);
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));

    let savePromise!: Promise<boolean>;
    act(() => {
      savePromise = result.current.saveBlocks();
    });
    await waitFor(() => expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledOnce());
    act(() => result.current.editCurrentSlot('newer unsaved text'));
    saveDeferred.resolve(afterSave.canonical);
    await act(async () => {
      await savePromise;
    });

    expect(result.current.state.previewText).toContain('newer unsaved text');
    expect(result.current.state.blocksDirty).toBe(true);
  });

  test('edit during the Save stage of Sync prevents previewing the older saved blocks', async () => {
    const saveDeferred = deferred<unknown>();
    const afterSave = workspaceFixture({
      inventorySnapshotHash: 'inventory-2',
      canonical: { ...workspaceFixture().canonical!, headRevisionId: 'rev-2' },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(afterSave);
    apiMocks.saveUserInstructionBlocks.mockReturnValue(saveDeferred.promise);
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));

    let syncPromise!: Promise<void>;
    act(() => {
      syncPromise = result.current.requestSync();
    });
    await waitFor(() => expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledOnce());
    act(() => result.current.editCurrentSlot('newer draft while save is pending'));
    saveDeferred.resolve(afterSave.canonical);
    await act(async () => syncPromise);

    expect(apiMocks.previewUserInstructionUpdate).not.toHaveBeenCalled();
    expect(apiMocks.previewUserInstructionSetup).not.toHaveBeenCalled();
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.plan).toBeNull();
    expect(result.current.state.previewText).toContain('newer draft while save is pending');
    expect(result.current.state.blocksDirty).toBe(true);
  });

  test('native source changing during Sync latches drift and blocks the old-source preview', async () => {
    const initial = workspaceWithSource('## Disk\n\nold source\n');
    const afterSave = workspaceWithSource('## Disk\n\nexternal change during save\n', {
      inventorySnapshotHash: 'inventory-2',
      canonical: { ...workspaceFixture().canonical!, headRevisionId: 'rev-2' },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(initial)
      .mockResolvedValueOnce(afterSave);
    apiMocks.saveUserInstructionBlocks.mockResolvedValue(afterSave.canonical);
    apiMocks.previewUserInstructionUpdate.mockResolvedValue(planFixture());
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));

    await act(async () => result.current.requestSync());

    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledOnce();
    expect(apiMocks.previewUserInstructionUpdate).not.toHaveBeenCalled();
    expect(apiMocks.previewUserInstructionSetup).not.toHaveBeenCalled();
    expect(result.current.state.originalText).toBe('## Disk\n\nold source\n');
    expect(result.current.state.sourceDrift).toBe(true);
    expect(result.current.state.externalDrift).toBe(false);
    expect(result.current.state.blocksDirty).toBe(false);
  });

  test('revision conflict latches canonical drift and blocks repeated Save attempts', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.saveUserInstructionBlocks.mockRejectedValue(
      Object.assign(new Error('revision changed'), {
        code: 'USER_INSTRUCTION_REVISION_CHANGED',
      }),
    );
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));
    await act(async () => {
      expect(await result.current.saveBlocks()).toBe(false);
    });
    expect(result.current.state.externalDrift).toBe(true);
    await act(async () => {
      expect(await result.current.saveBlocks()).toBe(false);
    });
    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledOnce();
  });

  test('blocked context change keeps the old lease and can still save after returning', async () => {
    const afterSave = workspaceFixture({
      inventorySnapshotHash: 'inventory-2',
      canonical: { ...workspaceFixture().canonical!, headRevisionId: 'rev-2' },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(workspaceFixture())
      .mockResolvedValueOnce(afterSave);
    apiMocks.saveUserInstructionBlocks.mockResolvedValue(afterSave.canonical);
    const { result, rerender } = renderHook(
      (props: { context: AgentHubContext }) =>
        useInstructionThreePaneController({ context: props.context, t }),
      { initialProps: { context: baseContext } },
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => result.current.editCurrentSlot('seed draft'));
    rerender({ context: { ...baseContext, agent: 'codex' } });
    expect(result.current.error).toBe('AGENT_HUB_CONTEXT_CHANGE_HAS_UNSAVED_DRAFT');
    expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledOnce();

    rerender({ context: baseContext });
    await waitFor(() =>
      expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledTimes(2),
    );
    await act(async () => {
      expect(await result.current.saveBlocks()).toBe(true);
    });
    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledWith(
      expect.objectContaining({ baseRevisionId: 'rev-1' }),
    );
  });

  test('editing while preview is pending prevents the old plan from opening', async () => {
    const previewDeferred = deferred<UserInstructionPlanDto>();
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(
      workspaceFixture({
        canonical: {
          ...workspaceFixture().canonical!,
          blocks: [
            {
              id: 'shared-1',
              mode: 'shared',
              commonMarkdown: SAMPLE_MARKDOWN,
              variants: null,
              headingPath: null,
              sourceTarget: null,
              needsAdaptation: false,
            },
          ],
        },
      }),
    );
    apiMocks.previewUserInstructionUpdate.mockReturnValue(previewDeferred.promise);
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    let previewPromise!: Promise<void>;
    act(() => {
      previewPromise = result.current.requestSync();
    });
    await waitFor(() => expect(apiMocks.previewUserInstructionUpdate).toHaveBeenCalledOnce());
    act(() => result.current.editCurrentSlot('changed while previewing'));
    previewDeferred.resolve(planFixture());
    await act(async () => previewPromise);
    expect(result.current.previewOpen).toBe(false);
    expect(result.current.plan).toBeNull();
  });

  test('changing deviceId inspects the peer workspace with deviceId', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockImplementation(
      async (ctx?: { deviceId?: string | null }) => {
        if (ctx?.deviceId === 'peer-9') {
          return {
            ...workspaceFixture(),
            inventorySnapshotHash: 'peer-hash',
          };
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
      expect(apiMocks.inspectUserInstructionWorkspace).toHaveBeenCalledWith({
        deviceId: 'peer-9',
        projectRef: null,
      });
      expect(result.current.workspace?.inventorySnapshotHash).toBe('peer-hash');
    });
    expect(result.current.error).toBeNull();
    expect(result.current.writeBlocked).toBe(false);
  });

  test('local and remote project contexts both fail closed before inspect', async () => {
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
    expect(apiMocks.inspectUserInstructionWorkspace).not.toHaveBeenCalled();
    expect(result.current.error).toBe('AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE');
    expect(result.current.writeBlocked).toBe(true);
    await act(async () => {
      await result.current.requestSync();
    });
    expect(result.current.actionError).toBe('AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE');
    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();

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
    expect(apiMocks.inspectUserInstructionWorkspace).not.toHaveBeenCalled();
  });

  test('confirmAiRevise on common lane revises shared slot then saves', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.reviseInstructionSlot.mockResolvedValue({ common: 'revised common' });
    apiMocks.saveUserInstructionBlocks.mockResolvedValue({
      ...workspaceFixture().canonical!,
      headRevisionId: 'rev-2',
    });
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.openAiRevise();
      result.current.setAiReviseDirection('make it shorter');
    });
    await act(async () => {
      await result.current.confirmAiRevise();
    });
    expect(apiMocks.reviseInstructionSlot).toHaveBeenCalledWith(
      expect.objectContaining({
        lane: 'common',
        agent: 'claude',
        direction: 'make it shorter',
      }),
    );
    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledTimes(1);
    expect(result.current.aiReviseOpen).toBe(false);
    expect(result.current.state.blocksDirty).toBe(false);
  });

  test('confirmAiRevise keeps the dialog open when Claude fails', async () => {
    apiMocks.inspectUserInstructionWorkspace.mockResolvedValue(workspaceFixture());
    apiMocks.reviseInstructionSlot.mockRejectedValue(new Error('claude down'));
    const { result } = renderHook(() =>
      useInstructionThreePaneController({ context: baseContext, t }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.openAiRevise();
      result.current.setAiReviseDirection('make it shorter');
    });
    await act(async () => {
      await result.current.confirmAiRevise();
    });
    expect(apiMocks.saveUserInstructionBlocks).not.toHaveBeenCalled();
    expect(result.current.aiReviseOpen).toBe(true);
    expect(result.current.aiReviseError).toBe('claude down');
  });

  test('confirmAiRevise on adapted lane sends all variants and replaces them', async () => {
    const initialWorkspace = workspaceFixture();
    const revisedWorkspace = workspaceFixture({
      inventorySnapshotHash: 'inventory-2',
      canonical: {
        ...initialWorkspace.canonical!,
        headRevisionId: 'rev-2',
        blocks: [
          {
            id: 'slot-adapted',
            mode: 'adapted',
            commonMarkdown: '',
            variants: { claude: 'c2', codex: 'x2', opencode: 'o2' },
            headingPath: null,
            sourceTarget: null,
            needsAdaptation: false,
          },
        ],
      },
    });
    apiMocks.inspectUserInstructionWorkspace
      .mockResolvedValueOnce(initialWorkspace)
      .mockResolvedValueOnce(revisedWorkspace);
    apiMocks.reviseInstructionSlot.mockResolvedValue({
      variants: { claude: 'c2', codex: 'x2', opencode: 'o2' },
    });
    apiMocks.saveUserInstructionBlocks.mockResolvedValue(revisedWorkspace.canonical);
    const adaptedContext: AgentHubContext = {
      ...baseContext,
      instructionLane: 'adapted',
    };
    const { result } = renderHook(() =>
      useInstructionThreePaneController({
        context: adaptedContext,
        t,
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.editCurrentSlot('old adapted');
      result.current.openAiRevise();
      result.current.setAiReviseDirection('rewrite for all agents');
    });
    await act(async () => {
      await result.current.confirmAiRevise();
    });
    expect(apiMocks.reviseInstructionSlot).toHaveBeenCalledWith(
      expect.objectContaining({
        lane: 'adapted',
        adaptedVariants: expect.objectContaining({ claude: 'old adapted' }),
      }),
    );
    expect(apiMocks.saveUserInstructionBlocks).toHaveBeenCalledTimes(1);
    expect(
      result.current.state.blocks.find((block) => block.mode === 'adapted')?.variants,
    ).toEqual({ claude: 'c2', codex: 'x2', opencode: 'o2' });
    expect(result.current.actionError).toBeNull();
    await waitFor(() => {
      expect(result.current.aiReviseFeedback).toEqual({
        currentSlotChanged: true,
        otherAdaptedSlotsChanged: true,
        selection: { start: 0, end: 2 },
      });
    });
    expect(result.current.state.blocksDirty).toBe(false);
  });
});
