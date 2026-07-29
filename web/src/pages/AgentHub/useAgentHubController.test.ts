/**
 * useAgentHubController 单元测试。
 *
 * Business Logic: 锁定首载错误、stale refresh、preview/enable/conflict/sequence。
 * Code Logic: mock agentHubApi + renderHook。
 */

// @vitest-environment jsdom

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { AgentHubAssetDetail, AgentHubStatus } from '@/lib/types/agentHub';

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
  },
}));

vi.mock('react-router-dom', async () => {
  const actual = await vi.importActual<typeof import('react-router-dom')>('react-router-dom');
  return {
    ...actual,
    useSearchParams: () => [new URLSearchParams(), vi.fn()],
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

const assetSummary = {
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
      target: 'claude' as const,
      desiredPresence: 'present' as const,
      desiredEnabled: true,
      materializationStatus: 'synced',
      lastError: null,
    },
  ],
  hasConflict: true,
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
});
