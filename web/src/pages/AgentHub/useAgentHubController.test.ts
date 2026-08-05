/**
 * useAgentHubController 单元测试。
 *
 * Business Logic: 锁定首载错误、stale refresh、preview/enable/conflict/sequence、矩阵 mutation。
 * Code Logic: mock agentHubApi + renderHook。
 */

// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { AgentHubAssetDetail, AgentHubAssetSummary, AgentHubStatus } from '@/lib/types/agentHub';

const getStatus = vi.fn();
const listAssets = vi.fn();
const getAsset = vi.fn();
const previewProject = vi.fn();
const enableProject = vi.fn();
const resolveConflict = vi.fn();
const updateInstruction = vi.fn();
const updateInstructionBlock = vi.fn();
const pairInstructionVariants = vi.fn();
const setTargetBinding = vi.fn();
const setTargetPresence = vi.fn();
const setTargetEnabled = vi.fn();
const restoreDetachedTarget = vi.fn();
const deleteAssetEverywhere = vi.fn();
const getPluginPackageReport = vi.fn();
const previewPluginDelete = vi.fn();
const searchParamsMock = vi.hoisted(() => ({ current: new URLSearchParams() }));

vi.mock('@/api/agentHub', () => ({
  agentHubApi: {
    getStatus: (...args: unknown[]) => getStatus(...args),
    listAssets: (...args: unknown[]) => listAssets(...args),
    getAsset: (...args: unknown[]) => getAsset(...args),
    previewProject: (...args: unknown[]) => previewProject(...args),
    enableProject: (...args: unknown[]) => enableProject(...args),
    resolveConflict: (...args: unknown[]) => resolveConflict(...args),
    updateInstruction: (...args: unknown[]) => updateInstruction(...args),
    updateInstructionBlock: (...args: unknown[]) => updateInstructionBlock(...args),
    pairInstructionVariants: (...args: unknown[]) => pairInstructionVariants(...args),
    setTargetBinding: (...args: unknown[]) => setTargetBinding(...args),
    setTargetPresence: (...args: unknown[]) => setTargetPresence(...args),
    setTargetEnabled: (...args: unknown[]) => setTargetEnabled(...args),
    restoreDetachedTarget: (...args: unknown[]) => restoreDetachedTarget(...args),
    deleteAssetEverywhere: (...args: unknown[]) => deleteAssetEverywhere(...args),
    getPluginPackageReport: (...args: unknown[]) => getPluginPackageReport(...args),
    previewPluginDelete: (...args: unknown[]) => previewPluginDelete(...args),
  },
}));

vi.mock('react-router-dom', async () => {
  const actual = await vi.importActual<typeof import('react-router-dom')>('react-router-dom');
  return {
    ...actual,
    useSearchParams: () => [searchParamsMock.current, vi.fn()],
  };
});

import { useAgentHubController } from './useAgentHubController';

const statusOk: AgentHubStatus = {
  enabled: true,
  backgroundEnabled: false,
  agentHubApiVersion: 1,
  ownerInstanceId: 'o1',
  writeCompatible: true,
  probes: [
    {
      target: 'claude',
      support: 'supported',
      executable: 'claude',
      version: '1.0',
    },
  ],
  conflictCount: 1,
  blockedMaterializationCount: 0,
};

const assetSummary: AgentHubAssetSummary = {
  assetId: 'asset-1',
  scopeId: 'user',
  kind: 'instruction',
  displayName: 'User instruction',
  logicalKey: 'user/instruction',
  originNamespace: 'claude',
  policy: 'shared',
  currentRevisionId: 'r1',
  targets: [
    {
      target: 'claude',
      desiredPresence: 'present',
      desiredEnabled: true,
      materializationStatus: 'synced',
      lastError: null,
      requested: true,
      supported: true,
      sourceOnly: false,
      verified: true,
    },
  ],
  hasConflict: true,
  aggregateStatus: 'full',
};

const assetDetail: AgentHubAssetDetail = {
  ...assetSummary,
  blocks: [
    {
      id: 'b1',
      mode: 'shared',
      commonMarkdown: 'hello',
    },
  ],
  conflicts: [
    {
      id: 'c1',
      createdAt: '2026-07-29T00:00:00.000Z',
      detailJson: '{}',
    },
  ],
};

describe('useAgentHubController', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    searchParamsMock.current = new URLSearchParams();
    getStatus.mockResolvedValue(statusOk);
    listAssets.mockResolvedValue([assetSummary]);
    getAsset.mockResolvedValue(assetDetail);
    previewProject.mockResolvedValue({
      projectId: 'p1',
      plannedActions: [],
      checkouts: [],
      noCommitNotice: 'no commit',
    });
    enableProject.mockResolvedValue({ projectId: 'p1', optedIn: true });
    resolveConflict.mockResolvedValue(assetDetail);
    setTargetEnabled.mockResolvedValue(assetSummary);
    setTargetPresence.mockResolvedValue(assetSummary);
    restoreDetachedTarget.mockResolvedValue(assetSummary);
    deleteAssetEverywhere.mockResolvedValue(assetSummary);
  });

  test('deep links select the advanced workspace that owns the requested surface', () => {
    searchParamsMock.current = new URLSearchParams('assetId=asset-1&conflictId=c1');
    const assetLink = renderHook(() => useAgentHubController());
    expect(assetLink.result.current.activeSection).toBe('portableAssets');
    assetLink.unmount();

    searchParamsMock.current = new URLSearchParams('preview=1&projectId=project-1');
    const projectLink = renderHook(() => useAgentHubController());
    expect(projectLink.result.current.activeSection).toBe('projectInstructions');
    projectLink.unmount();
  });

  test('first-load error surfaces error without assets', async () => {
    getStatus.mockRejectedValueOnce(new Error('status boom'));
    listAssets.mockRejectedValueOnce(new Error('status boom'));
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toContain('status boom');
    expect(result.current.assets).toEqual([]);
  });

  test('stale refresh keeps previous assets when later load fails', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.assets).toHaveLength(1));

    getStatus.mockRejectedValueOnce(new Error('network'));
    listAssets.mockRejectedValueOnce(new Error('network'));
    await act(async () => {
      await result.current.reload();
    });
    expect(result.current.assets).toHaveLength(1);
    expect(result.current.stale || result.current.error).toBeTruthy();
  });

  test('preview and enable project actions', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.setPreviewProjectId('proj-1');
      result.current.openPreviewDialog();
    });
    await act(async () => {
      await result.current.runPreviewProject();
    });
    expect(previewProject).toHaveBeenCalledWith('proj-1');
    expect(result.current.preview?.projectId).toBe('p1');

    await act(async () => {
      await result.current.runEnableProject();
    });
    expect(enableProject).toHaveBeenCalledWith('proj-1');
  });

  test('loadPluginReport always calls previewPluginDelete and merges deletePreview', async () => {
    getPluginPackageReport.mockResolvedValue({
      packageAssetId: 'plugin-1',
      packageDisplayName: 'Demo Plugin',
      sourceTarget: 'claude',
      aggregateStatus: 'partial',
      activationState: 'inactive',
      diagnostics: [],
      components: [],
      residuals: [],
      partialBlockers: ['x'],
      deletePreview: null,
    });
    previewPluginDelete.mockResolvedValue({
      packageAssetId: 'plugin-1',
      packageDisplayName: 'Demo Plugin',
      sourceTarget: 'claude',
      aggregateStatus: 'partial',
      activationState: 'inactive',
      diagnostics: [],
      components: [],
      residuals: [],
      partialBlockers: ['x'],
      deletePreview: {
        packageAssetId: 'plugin-1',
        components: [
          {
            componentId: 'c1',
            displayName: 'Hook A',
            decision: 'tombstoneOwned',
            reasons: ['owned'],
          },
        ],
      },
    });

    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.loadPluginReport('plugin-1');
    });

    expect(getPluginPackageReport).toHaveBeenCalledWith('plugin-1');
    expect(previewPluginDelete).toHaveBeenCalledWith('plugin-1');
    expect(result.current.pluginReport?.deletePreview?.components[0].decision).toBe(
      'tombstoneOwned',
    );
  });

  test('conflict resolve and request sequence for select asset', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let resolveSlow: (value: AgentHubAssetDetail) => void = () => undefined;
    const slow = new Promise<AgentHubAssetDetail>((resolve) => {
      resolveSlow = resolve;
    });
    getAsset.mockImplementationOnce(() => slow);
    getAsset.mockResolvedValueOnce({ ...assetDetail, assetId: 'asset-2', displayName: 'Second' });

    act(() => {
      result.current.selectAsset('asset-1');
      result.current.selectAsset('asset-2');
    });
    await waitFor(() => expect(getAsset).toHaveBeenCalledTimes(2));

    // 慢响应后到，不得覆盖 asset-2
    await act(async () => {
      resolveSlow(assetDetail);
      await Promise.resolve();
    });
    await waitFor(() => expect(result.current.selectedAsset?.assetId).toBe('asset-2'));

    act(() => {
      result.current.selectAsset('asset-1');
    });
    await waitFor(() => expect(result.current.selectedAsset?.assetId).toBe('asset-1'));

    await act(async () => {
      await result.current.resolveConflict({ conflictId: 'c1', resolution: 'keepHub' });
    });
    expect(resolveConflict).toHaveBeenCalledWith(
      expect.objectContaining({ assetId: 'asset-1', conflictId: 'c1' }),
    );
  });

  test('enable/disable one target refreshes via revision cursor', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.setTargetEnabled({
        assetId: 'asset-1',
        target: 'claude',
        desiredEnabled: false,
      });
    });
    expect(setTargetEnabled).toHaveBeenCalledWith(
      expect.objectContaining({ assetId: 'asset-1', desiredEnabled: false }),
    );
    expect(listAssets).toHaveBeenCalled();
  });

  test('target removal and restore call presence/restore APIs', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      await result.current.removeTarget({ assetId: 'asset-1', target: 'claude' });
    });
    expect(setTargetPresence).toHaveBeenCalledWith(
      expect.objectContaining({ desiredPresence: 'absent', target: 'claude' }),
    );
    await act(async () => {
      await result.current.restoreDetachedTarget({ assetId: 'asset-1', target: 'claude' });
    });
    expect(restoreDetachedTarget).toHaveBeenCalledWith(
      expect.objectContaining({ assetId: 'asset-1', target: 'claude' }),
    );
  });

  test('delete everywhere confirmation mutates then reloads', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.openDeleteEverywhere('asset-1');
    });
    expect(result.current.deleteEverywhereOpen).toBe(true);
    await act(async () => {
      await result.current.confirmDeleteEverywhere();
    });
    expect(deleteAssetEverywhere).toHaveBeenCalledWith({ assetId: 'asset-1' });
    expect(result.current.deleteEverywhereOpen).toBe(false);
  });

  test('rapid scope switching drops stale list responses', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    let resolveSlow: (value: AgentHubAssetSummary[]) => void = () => undefined;
    const slow = new Promise<AgentHubAssetSummary[]>((resolve) => {
      resolveSlow = resolve;
    });
    listAssets.mockImplementationOnce(() => slow);
    listAssets.mockResolvedValueOnce([
      {
        ...assetSummary,
        assetId: 'asset-scope-b',
        scopeId: 'project-b',
        displayName: 'Project B',
      },
    ]);

    act(() => {
      result.current.setScopeFilter('project-a');
    });
    act(() => {
      result.current.setScopeFilter('project-b');
    });

    await waitFor(() => expect(listAssets).toHaveBeenCalledTimes(3));

    await act(async () => {
      resolveSlow([{ ...assetSummary, assetId: 'asset-scope-a', scopeId: 'project-a' }]);
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(result.current.assets[0]?.assetId).toBe('asset-scope-b');
    });
    expect(result.current.assets.some((a) => a.assetId === 'asset-scope-a')).toBe(false);
  });

  test('openAdoptionPreview builds collision diagnostics', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.openAdoptionPreview(
        {
          ...assetSummary,
          aggregateStatus: 'externalCollision',
          targets: [
            {
              ...assetSummary.targets[0],
              materializationStatus: 'externalCollision',
              lastError: 'collision-path',
              verified: false,
            },
          ],
        },
        'claude',
      );
    });
    expect(result.current.adoptionOpen).toBe(true);
    expect(result.current.adoptionPreview?.diagnostics).toEqual(
      expect.arrayContaining(['collision-path', 'materialization:externalCollision']),
    );
  });
});
