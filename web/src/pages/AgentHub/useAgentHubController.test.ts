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
const previewLanPush = vi.fn();
const startLanPush = vi.fn();
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
    previewLanPush: (...args: unknown[]) => previewLanPush(...args),
    startLanPush: (...args: unknown[]) => startLanPush(...args),
  },
}));

const setSearchParamsMock = vi.hoisted(() => vi.fn());

vi.mock('react-router-dom', async () => {
  const actual = await vi.importActual<typeof import('react-router-dom')>('react-router-dom');
  return {
    ...actual,
    useSearchParams: () => [searchParamsMock.current, setSearchParamsMock],
  };
});

const portableApiMocks = vi.hoisted(() => ({
  inspect: vi.fn(),
  previewAction: vi.fn(),
  applyAction: vi.fn(),
  getAction: vi.fn(),
  listRemoteInventory: vi.fn(),
  previewPull: vi.fn(),
  applyPull: vi.fn(),
  getPull: vi.fn(),
}));

vi.mock('@/api/portableInventory', () => ({
  portableAssetApi: {
    inspect: (...args: unknown[]) => portableApiMocks.inspect(...args),
    previewAction: (...args: unknown[]) => portableApiMocks.previewAction(...args),
    applyAction: (...args: unknown[]) => portableApiMocks.applyAction(...args),
    getAction: (...args: unknown[]) => portableApiMocks.getAction(...args),
  },
  portablePullApi: {
    listRemoteInventory: (...args: unknown[]) => portableApiMocks.listRemoteInventory(...args),
    previewPull: (...args: unknown[]) => portableApiMocks.previewPull(...args),
    applyPull: (...args: unknown[]) => portableApiMocks.applyPull(...args),
    getPull: (...args: unknown[]) => portableApiMocks.getPull(...args),
  },
}));

const devicesListMock = vi.hoisted(() =>
  vi.fn(async () => [
    { id: 'peer-online', name: 'Peer Online', status: 'online' as const },
    { id: 'peer-offline', name: 'Peer Offline', status: 'offline' as const },
  ]),
);

vi.mock('@/api/devices', () => ({
  devicesApi: {
    list: () => devicesListMock(),
  },
}));

const workbenchProjectsListMock = vi.hoisted(() =>
  vi.fn(async () => [
    {
      id: 'local-1',
      name: 'Local Repo',
      kind: 'local',
      deviceId: 'self',
      deviceName: 'This Mac',
      path: '/tmp/local',
      lastOpenedAt: '2026-08-08T00:00:00.000Z',
    },
    {
      id: 'remote:dev-hk:inner',
      name: 'Remote Repo',
      kind: 'remote',
      deviceId: 'dev-hk',
      deviceName: 'HK Peer',
      path: '/remote/repo',
      lastOpenedAt: '2026-08-08T00:00:00.000Z',
    },
  ]),
);

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    projects: {
      list: () => workbenchProjectsListMock(),
    },
  },
}));

import { useAgentHubController } from './useAgentHubController';
import type {
  PortableAssetActionPlanDto,
  PortableInventoryItemDto,
  PortableInventorySnapshotDto,
} from '@/lib/types/portableInventory';

function makePortableItem(
  overrides: Partial<PortableInventoryItemDto> = {},
): PortableInventoryItemDto {
  return {
    inventoryItemId: 'claude-skill-alpha',
    target: 'claude',
    kind: 'skill',
    nativeId: 'alpha',
    displayName: 'Alpha',
    description: null,
    version: null,
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: '/tmp/alpha',
    sourceOrigin: 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: 'hash-alpha',
    treeHash: null,
    canonicalAssetId: 'canon-alpha',
    canonicalRevisionId: 'rev-alpha',
    managementState: 'hubManaged',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'verified',
    capabilities: {
      canEnable: true,
      canDisable: true,
      canUninstall: true,
      canAdopt: false,
      canInstallToSourceTarget: false,
      reasonCode: null,
      evidenceIds: [],
    },
    warnings: [],
    ...overrides,
  };
}

function portableSnapshot(
  items: PortableInventoryItemDto[],
  stale = false,
): PortableInventorySnapshotDto {
  return {
    inventorySnapshotHash: 'snap-ok',
    refreshedAt: '2026-08-07T00:00:00.000Z',
    stale,
    targets: [],
    items,
  };
}

function portablePlanFixture(): PortableAssetActionPlanDto {
  return {
    planToken: 'plan-token-1',
    expiresAt: '2026-08-07T12:15:00.000Z',
    inventorySnapshotHash: 'snap-ok',
    action: 'disable',
    keepData: false,
    conflictPolicy: 'skipExisting',
    changes: [
      {
        inventoryItemId: 'claude-skill-alpha',
        target: 'claude',
        kind: 'skill',
        path: '/tmp/alpha',
        operation: 'disable',
        expectedSourceHash: 'hash-alpha',
        expectedTreeHash: null,
        expectedCanonicalRevisionId: 'rev-alpha',
        backupPolicy: 'none',
        createsOwnership: false,
        canonicalEffect: 'updateDesired',
        blockingReasons: [],
        warnings: [],
      },
    ],
    blockingReasons: [],
  };
}

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
    devicesListMock.mockResolvedValue([
      { id: 'peer-online', name: 'Peer Online', status: 'online' as const },
      { id: 'peer-offline', name: 'Peer Offline', status: 'offline' as const },
    ]);
    workbenchProjectsListMock.mockResolvedValue([
      {
        id: 'local-1',
        name: 'Local Repo',
        kind: 'local',
        deviceId: 'self',
        deviceName: 'This Mac',
        path: '/tmp/local',
        lastOpenedAt: '2026-08-08T00:00:00.000Z',
      },
      {
        id: 'remote:dev-hk:inner',
        name: 'Remote Repo',
        kind: 'remote',
        deviceId: 'dev-hk',
        deviceName: 'HK Peer',
        path: '/remote/repo',
        lastOpenedAt: '2026-08-08T00:00:00.000Z',
      },
    ]);
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
    previewLanPush.mockResolvedValue({
      previewToken: 'lan-preview-token',
      snapshotHash: 'snapshot-hash',
      snapshotId: 'snapshot-id',
      selectionHash: 'selection-hash',
      assetCount: 1,
      revisionCount: 1,
      credentialBearingAssetCount: 0,
      peerDeviceIds: ['peer-online'],
      mode: 'userScope',
      plaintextBackupDisclosure: 'disclosure',
      hasCredentialBearingAssets: false,
    });
    startLanPush.mockResolvedValue({
      requestId: 'push-request',
      selectionHash: 'selection-hash',
      snapshotHash: 'snapshot-hash',
      status: 'completed',
      targets: [],
    });
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    portableApiMocks.previewAction.mockResolvedValue(portablePlanFixture());
    portableApiMocks.applyAction.mockResolvedValue({
      planToken: 'plan-token-1',
      clientRequestId: 'req-1',
      items: [
        {
          inventoryItemId: 'claude-skill-alpha',
          state: 'succeeded',
          errorCode: null,
          message: null,
        },
      ],
    });
    portableApiMocks.getAction.mockResolvedValue({
      planToken: 'plan-token-1',
      clientRequestId: 'req-1',
      items: [
        {
          inventoryItemId: 'claude-skill-alpha',
          state: 'succeeded',
          errorCode: null,
          message: null,
        },
      ],
    });
  });

  test('cold shell loads lightweight peer and project selectors without scanning assets', async () => {
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(devicesListMock).toHaveBeenCalledOnce();
    expect(workbenchProjectsListMock).toHaveBeenCalledOnce();
    expect(portableApiMocks.inspect).not.toHaveBeenCalled();
  });

  test('peer context is preserved and never falls back to local inspect (T2)', async () => {
    searchParamsMock.current = new URLSearchParams('deviceId=peer-online');
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.hubContext.deviceId).toBe('peer-online');
    expect(result.current.hubContext.scope).toBe('user');
    expect(result.current.portableInventory.requestContext.deviceId).toBe('peer-online');
    expect(portableApiMocks.inspect).not.toHaveBeenCalled();
    expect(listAssets).not.toHaveBeenCalled();
    expect(getStatus).not.toHaveBeenCalled();
  });

  test('tab=skill preserves peer ownership and does not inspect local inventory (T3)', async () => {
    searchParamsMock.current = new URLSearchParams('tab=skill&deviceId=peer-online');
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    const { result } = renderHook(() => useAgentHubController());
    expect(result.current.activeSection).toBe('assets');
    expect(result.current.portableLaneActive).toBe(true);
    await waitFor(() => expect(result.current.shellPeers.length).toBeGreaterThan(0));
    expect(result.current.hubContext.deviceId).toBe('peer-online');
    expect(portableApiMocks.inspect).not.toHaveBeenCalled();
    expect(listAssets).not.toHaveBeenCalled();
  });

  test('switching command → instructions is not stuck by portable filter URL sync', async () => {
    // 真实 RR：setSearchParams 后 searchParams 同步更新，触发 re-parse。
    setSearchParamsMock.mockImplementation((updater: unknown) => {
      if (typeof updater === 'function') {
        searchParamsMock.current = (updater as (prev: URLSearchParams) => URLSearchParams)(
          searchParamsMock.current,
        );
      } else if (updater instanceof URLSearchParams) {
        searchParamsMock.current = new URLSearchParams(updater);
      }
    });
    // 现代 URL 的 tab 是唯一导航来源；portable filter 不再写 section/kind。
    searchParamsMock.current = new URLSearchParams('tab=command');
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem({ kind: 'command' })]));
    const { result, rerender } = renderHook(() => useAgentHubController());

    await waitFor(() => expect(result.current.activeSection).toBe('assets'));
    await waitFor(() =>
      expect(result.current.portableInventory.filters.kind).toBe('command'),
    );
    // 资产页只保留现代 tab；legacy 导航键始终被清除。
    await waitFor(() => {
      expect(searchParamsMock.current.get('tab')).toBe('command');
      expect(searchParamsMock.current.get('section')).toBeNull();
      expect(searchParamsMock.current.get('kind')).toBeNull();
    });

    act(() => {
      result.current.onContextChange({ tab: 'instructions' });
    });
    // 模拟 useSearchParams 订阅到新 URL 后的 re-render
    rerender();

    expect(result.current.hubContext.tab).toBe('instructions');
    expect(result.current.activeSection).toBe('userInstructions');
    expect(result.current.instructionsLaneActive).toBe(true);
    // 切回提示词后不得再残留会把 tab 解析成资产的 legacy 导航键
    expect(searchParamsMock.current.get('tab')).toBeNull();
    expect(searchParamsMock.current.get('section')).toBeNull();
    expect(searchParamsMock.current.get('kind')).toBeNull();
    expect(searchParamsMock.current.get('target')).toBeNull();

    // 竞态回归：离开资产区后 filters 再变，也不得写回 kind/section 把 tab 拉回资产
    act(() => {
      result.current.portableInventory.setFilters({ kind: 'mcp' });
    });
    rerender();
    expect(result.current.hubContext.tab).toBe('instructions');
    expect(searchParamsMock.current.get('kind')).toBeNull();
    expect(searchParamsMock.current.get('section')).toBeNull();
    expect(result.current.activeSection).toBe('userInstructions');
  });

  test('history changes rehydrate shell-owned inventory filters and selection without legacy echo', async () => {
    setSearchParamsMock.mockImplementation((updater: unknown) => {
      if (typeof updater === 'function') {
        searchParamsMock.current = (updater as (prev: URLSearchParams) => URLSearchParams)(
          searchParamsMock.current,
        );
      } else if (updater instanceof URLSearchParams) {
        searchParamsMock.current = new URLSearchParams(updater);
      }
    });
    portableApiMocks.inspect.mockResolvedValue(
      portableSnapshot([
        makePortableItem({ inventoryItemId: 'i1' }),
        makePortableItem({
          inventoryItemId: 'i2',
          target: 'codex',
          kind: 'mcp',
          nativeId: 'mcp-two',
        }),
      ]),
    );
    searchParamsMock.current = new URLSearchParams(
      'agent=claude&tab=skill&state=enabled&inventoryItemId=i1',
    );
    const { result, rerender } = renderHook(() => useAgentHubController());

    await waitFor(() => expect(result.current.portableInventory.selectedItemId).toBe('i1'));
    expect(result.current.portableInventory.filters).toEqual(
      expect.objectContaining({
        target: 'claude',
        kind: 'skill',
        scope: 'user',
        actualState: 'enabled',
        management: 'all',
      }),
    );

    searchParamsMock.current = new URLSearchParams(
      'agent=codex&tab=mcp&state=problem&management=drifted&inventoryItemId=i2',
    );
    rerender();

    await waitFor(() => {
      expect(result.current.hubContext.agent).toBe('codex');
      expect(result.current.hubContext.tab).toBe('mcp');
      expect(result.current.portableInventory.filters).toEqual(
        expect.objectContaining({
          target: 'codex',
          kind: 'mcp',
          scope: 'user',
          actualState: 'problem',
          management: 'drifted',
        }),
      );
      expect(result.current.portableInventory.selectedItemId).toBe('i2');
    });
    expect(searchParamsMock.current.get('section')).toBeNull();
    expect(searchParamsMock.current.get('target')).toBeNull();
    expect(searchParamsMock.current.get('kind')).toBeNull();

    searchParamsMock.current = new URLSearchParams('agent=codex');
    rerender();
    await waitFor(() => expect(result.current.hubContext.tab).toBe('instructions'));
    expect(result.current.activeSection).toBe('userInstructions');
    expect(searchParamsMock.current.get('state')).toBeNull();
    expect(searchParamsMock.current.get('inventoryItemId')).toBeNull();
  });

  test('optimistic instructions switch survives delayed URL while section=assets remains', async () => {
    // 复现根因：setSearchParams 尚未把 tab 从 command 刷掉时，乐观 setActiveSection
    // 不得被 hubContext/deepLinkSection effect 盖回 assets。
    setSearchParamsMock.mockImplementation(() => {
      /* 故意不更新 searchParamsMock —— 模拟 URL 滞后 */
    });
    searchParamsMock.current = new URLSearchParams('tab=command&section=assets&kind=command');
    portableApiMocks.inspect.mockResolvedValue(
      portableSnapshot([makePortableItem({ kind: 'command' })]),
    );
    const { result, rerender } = renderHook(() => useAgentHubController());

    await waitFor(() => expect(result.current.activeSection).toBe('assets'));
    expect(result.current.hubContext.tab).toBe('command');

    act(() => {
      result.current.onContextChange({ tab: 'instructions' });
    });
    // URL 仍旧：tab=command&section=assets —— 但乐观 activeSection 必须已是提示词
    rerender();
    expect(result.current.hubContext.tab).toBe('command'); // URL 滞后
    expect(result.current.activeSection).toBe('userInstructions');

    // 再渲染一帧仍不得被 effect 回滚
    rerender();
    expect(result.current.activeSection).toBe('userInstructions');
  });

  test('cold default instructions skips listAssets and status (T1)', async () => {
    searchParamsMock.current = new URLSearchParams();
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.loading).toBe(false);
    expect(result.current.legacyLoadedOnce).toBe(false);
    expect(listAssets).not.toHaveBeenCalled();
    expect(getStatus).not.toHaveBeenCalled();
    expect(portableApiMocks.inspect).not.toHaveBeenCalled();
  });

  test('openPortablePull uses the selected peer and same-agent source (toolbar pull)', async () => {
    searchParamsMock.current = new URLSearchParams('deviceId=peer-1&agent=codex');
    devicesListMock.mockResolvedValue([
      { id: 'other-peer', name: 'Other Peer', status: 'online' as const },
      { id: 'peer-1', name: 'Peer One', status: 'online' as const },
      { id: 'peer-offline', name: 'Peer Offline', status: 'offline' as const },
    ]);
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.hubContext.deviceId).toBe('peer-1');
    expect(result.current.hubContext.agent).toBe('codex');

    act(() => {
      result.current.openPortablePull();
    });
    expect(result.current.portablePullOpen).toBe(true);
    await waitFor(() =>
      expect(result.current.portablePull.selectedDeviceId).toBe('peer-1'),
    );
    expect(result.current.portablePull.sourceTarget).toBe('codex');
  });

  test('openLanPushDialog keeps project mode but never treats Workbench id as Hub id', async () => {
    searchParamsMock.current = new URLSearchParams('scope=project&project=local-1&agent=claude');
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));

    act(() => {
      result.current.openLanPushDialog();
    });
    expect(result.current.lanPushOpen).toBe(true);
    expect(result.current.hubContext.scope).toBe('project');
    expect(result.current.hubContext.projectKey).toBe('local-1');
    expect(result.current.lanMode).toBe('project');
    expect(result.current.lanHubProjectIdsText).toBe('');
  });

  test('LAN push forwards the token returned by the matching preview', async () => {
    const { result } = renderHook(() => useAgentHubController());
    act(() => {
      result.current.openLanPushDialog();
    });
    await waitFor(() =>
      expect(result.current.lanPeers.some((peer) => peer.deviceId === 'peer-online')).toBe(true),
    );
    act(() => {
      result.current.toggleLanPeer('peer-online');
    });
    await act(async () => {
      await result.current.runLanPreview();
    });
    await waitFor(() => expect(result.current.lanPreview?.previewToken).toBe('lan-preview-token'));

    await act(async () => {
      await result.current.runLanStart();
    });

    expect(startLanPush).toHaveBeenCalledWith({
      peerDeviceIds: ['peer-online'],
      mode: 'userScope',
      scopeIds: [],
      assetIds: [],
      hubProjectIds: [],
      includeHistory: true,
      previewToken: 'lan-preview-token',
    });
  });

  test('legacy deep links only migrate to modern workspaces and never load old writers', async () => {
    searchParamsMock.current = new URLSearchParams('assetId=asset-1&conflictId=c1');
    const assetLink = renderHook(() => useAgentHubController());
    expect(assetLink.result.current.activeSection).toBe('assets');
    await waitFor(() => expect(portableApiMocks.inspect).toHaveBeenCalled());
    expect(listAssets).not.toHaveBeenCalled();
    expect(getAsset).not.toHaveBeenCalled();
    expect(assetLink.result.current.conflictDrawerOpen).toBe(false);
    assetLink.unmount();

    searchParamsMock.current = new URLSearchParams('preview=1&projectId=project-1');
    const projectLink = renderHook(() => useAgentHubController());
    expect(projectLink.result.current.activeSection).toBe('userInstructions');
    expect(projectLink.result.current.previewOpen).toBe(false);
    expect(previewProject).not.toHaveBeenCalled();
    projectLink.unmount();
  });

  test('section=assets and legacy portableAssets alias restore assets workspace', () => {
    searchParamsMock.current = new URLSearchParams('section=assets&target=claude&kind=skill');
    const direct = renderHook(() => useAgentHubController());
    expect(direct.result.current.activeSection).toBe('assets');
    expect(direct.result.current.portableInventory.filters.target).toBe('claude');
    expect(direct.result.current.portableInventory.filters.kind).toBe('skill');
    direct.unmount();

    searchParamsMock.current = new URLSearchParams('section=portableAssets&target=codex');
    const alias = renderHook(() => useAgentHubController());
    expect(alias.result.current.activeSection).toBe('assets');
    expect(alias.result.current.portableInventory.filters.target).toBe('codex');
    alias.unmount();
  });

  test('setActiveSection writes modern tab and removes retired mutation params', async () => {
    searchParamsMock.current = new URLSearchParams('conflictId=c1&bridge=/tmp/bridge');
    setSearchParamsMock.mockImplementation((updater: unknown) => {
      if (typeof updater === 'function') {
        const next = (updater as (prev: URLSearchParams) => URLSearchParams)(
          searchParamsMock.current,
        );
        searchParamsMock.current = next;
      }
    });
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    act(() => {
      result.current.setActiveSection('assets');
    });
    expect(setSearchParamsMock).toHaveBeenCalled();
    // 其他同步 effect 也可能随后写 URL；断言真实 router mock 的最终状态，
    // 不依赖“最后一次 setter callback 恰好来自 setActiveSection”的脆弱顺序。
    expect(searchParamsMock.current.get('tab')).toBe('skill');
    expect(searchParamsMock.current.get('section')).toBeNull();
    expect(searchParamsMock.current.get('conflictId')).toBeNull();
    expect(searchParamsMock.current.get('bridge')).toBeNull();
  });

  test('legacy asset identity survives a transient first scan and migrates after Retry', async () => {
    const item = makePortableItem({
      inventoryItemId: 'portable-alpha',
      canonicalAssetId: 'canon-alpha',
    });
    searchParamsMock.current = new URLSearchParams('tab=skill&assetId=canon-alpha');
    setSearchParamsMock.mockImplementation((updater: unknown) => {
      if (typeof updater === 'function') {
        searchParamsMock.current = (updater as (prev: URLSearchParams) => URLSearchParams)(
          searchParamsMock.current,
        );
      } else if (updater instanceof URLSearchParams) {
        searchParamsMock.current = new URLSearchParams(updater);
      }
    });
    portableApiMocks.inspect
      .mockRejectedValueOnce(new Error('transient inventory failure'))
      .mockResolvedValue(portableSnapshot([item]));

    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() =>
      expect(result.current.portableInventory.error).toContain('transient inventory failure'),
    );
    expect(searchParamsMock.current.get('assetId')).toBe('canon-alpha');
    expect(searchParamsMock.current.get('inventoryItemId')).toBeNull();

    await act(async () => {
      await result.current.portableInventory.refresh();
    });
    await waitFor(() => {
      expect(searchParamsMock.current.get('assetId')).toBeNull();
      expect(searchParamsMock.current.get('inventoryItemId')).toBe('portable-alpha');
    });
  });

  test('first-load error surfaces error without assets', async () => {
    listAssets.mockRejectedValueOnce(new Error('status boom'));
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await act(async () => {
      result.current.expandLegacyMatrix();
    });
    await waitFor(() => expect(result.current.error).toContain('status boom'));
    expect(result.current.assets).toEqual([]);
  });

  test('stale refresh keeps previous assets when later load fails', async () => {
    searchParamsMock.current = new URLSearchParams('tab=skill');
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([makePortableItem()]));
    const { result } = renderHook(() => useAgentHubController());
    await act(async () => {
      result.current.expandLegacyMatrix();
    });
    await waitFor(() => expect(result.current.assets).toHaveLength(1));

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
    // 先展开 legacy 一次，后续 filter 才会触发 listAssets
    await act(async () => {
      result.current.expandLegacyMatrix();
    });
    await waitFor(() => expect(result.current.legacyLoadedOnce).toBe(true));
    listAssets.mockClear();

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

    await waitFor(() => expect(listAssets).toHaveBeenCalledTimes(2));

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

  test('H1: preview then inventory refresh fail must not allow applyAction on confirm', async () => {
    const item = makePortableItem();
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([item]));
    searchParamsMock.current = new URLSearchParams('tab=skill');
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.loading).toBe(false));
    await waitFor(() => expect(result.current.portableInventory.snapshot).not.toBeNull());

    act(() => {
      result.current.requestPortableAction(item.inventoryItemId, 'disable');
    });
    expect(result.current.portableActionOpen).toBe(true);

    await act(async () => {
      await result.current.previewPortableAction({
        inventorySnapshotHash: 'snap-ok',
        inventoryItemIds: [item.inventoryItemId],
        action: 'disable',
        keepData: false,
        conflictPolicy: 'skipExisting',
        expectedCanonicalRevisionId: item.canonicalRevisionId,
      });
    });
    expect(portableApiMocks.previewAction).toHaveBeenCalledTimes(1);
    expect(result.current.portableActionPlan?.planToken).toBe('plan-token-1');
    const clientRequestId = result.current.portableActionClientRequestId;
    expect(clientRequestId).toBeTruthy();

    portableApiMocks.inspect.mockRejectedValueOnce(new Error('inspect failed'));
    await act(async () => {
      await result.current.portableInventory.refresh();
    });
    expect(result.current.portableInventory.stale).toBe(true);
    expect(result.current.portableInventory.mutationBlocked).toBe(true);

    await act(async () => {
      await result.current.confirmPortableAction('plan-token-1', clientRequestId!);
    });

    expect(portableApiMocks.applyAction).not.toHaveBeenCalled();
    expect(result.current.portableActionError).toBeTruthy();
    expect(result.current.portableActionBusy).toBe(false);
  });

  test('M1: double preview before re-render only starts one previewAction', async () => {
    const item = makePortableItem();
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([item]));
    let resolvePreview!: (value: PortableAssetActionPlanDto) => void;
    const previewPromise = new Promise<PortableAssetActionPlanDto>((resolve) => {
      resolvePreview = resolve;
    });
    portableApiMocks.previewAction.mockReturnValueOnce(previewPromise);

    searchParamsMock.current = new URLSearchParams('tab=skill');
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.portableInventory.snapshot).not.toBeNull());

    act(() => {
      result.current.requestPortableAction(item.inventoryItemId, 'disable');
    });

    const request = {
      inventorySnapshotHash: 'snap-ok',
      inventoryItemIds: [item.inventoryItemId],
      action: 'disable' as const,
      keepData: false,
      conflictPolicy: 'skipExisting' as const,
      expectedCanonicalRevisionId: item.canonicalRevisionId,
    };

    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => {
      first = result.current.previewPortableAction(request);
      second = result.current.previewPortableAction(request);
      await Promise.resolve();
    });
    expect(portableApiMocks.previewAction).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolvePreview(portablePlanFixture());
      await Promise.all([first, second]);
    });
    expect(result.current.portableActionPlan?.planToken).toBe('plan-token-1');
    expect(result.current.portableActionBusy).toBe(false);
  });

  test('history context change discards a pending portable action preview response', async () => {
    const item = makePortableItem();
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([item]));
    let resolvePreview!: (value: PortableAssetActionPlanDto) => void;
    portableApiMocks.previewAction.mockReturnValueOnce(
      new Promise<PortableAssetActionPlanDto>((resolve) => {
        resolvePreview = resolve;
      }),
    );
    searchParamsMock.current = new URLSearchParams('agent=claude&tab=skill');
    const { result, rerender } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.portableInventory.snapshot).not.toBeNull());
    act(() => result.current.requestPortableAction(item.inventoryItemId, 'disable'));

    let previewPromise!: Promise<void>;
    act(() => {
      previewPromise = result.current.previewPortableAction({
        inventorySnapshotHash: 'snap-ok',
        inventoryItemIds: [item.inventoryItemId],
        action: 'disable',
        keepData: false,
        conflictPolicy: 'skipExisting',
        expectedCanonicalRevisionId: item.canonicalRevisionId,
      });
    });
    await waitFor(() => expect(portableApiMocks.previewAction).toHaveBeenCalledOnce());

    searchParamsMock.current = new URLSearchParams('agent=codex&tab=skill');
    rerender();
    await waitFor(() => expect(result.current.hubContext.agent).toBe('codex'));
    resolvePreview(portablePlanFixture());
    await act(async () => previewPromise);

    expect(result.current.portableActionOpen).toBe(false);
    expect(result.current.portableActionPlan).toBeNull();
    expect(result.current.portableActionClientRequestId).toBeNull();
    expect(result.current.portableActionBusy).toBe(false);
    expect(portableApiMocks.applyAction).not.toHaveBeenCalled();
  });

  test('M1: confirm refuses entry when already busy', async () => {
    const item = makePortableItem();
    portableApiMocks.inspect.mockResolvedValue(portableSnapshot([item]));
    let resolveApply!: (value: {
      planToken: string;
      clientRequestId: string;
      items: Array<{
        inventoryItemId: string;
        state: 'succeeded';
        errorCode: null;
        message: null;
      }>;
    }) => void;
    const applyPromise = new Promise<{
      planToken: string;
      clientRequestId: string;
      items: Array<{
        inventoryItemId: string;
        state: 'succeeded';
        errorCode: null;
        message: null;
      }>;
    }>((resolve) => {
      resolveApply = resolve;
    });
    portableApiMocks.applyAction.mockReturnValueOnce(applyPromise);

    searchParamsMock.current = new URLSearchParams('tab=skill');
    const { result } = renderHook(() => useAgentHubController());
    await waitFor(() => expect(result.current.portableInventory.snapshot).not.toBeNull());

    act(() => {
      result.current.requestPortableAction(item.inventoryItemId, 'disable');
    });
    await act(async () => {
      await result.current.previewPortableAction({
        inventorySnapshotHash: 'snap-ok',
        inventoryItemIds: [item.inventoryItemId],
        action: 'disable',
        keepData: false,
        conflictPolicy: 'skipExisting',
        expectedCanonicalRevisionId: item.canonicalRevisionId,
      });
    });
    const clientRequestId = result.current.portableActionClientRequestId!;

    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => {
      first = result.current.confirmPortableAction('plan-token-1', clientRequestId);
      second = result.current.confirmPortableAction('plan-token-1', clientRequestId);
      await Promise.resolve();
    });
    expect(portableApiMocks.applyAction).toHaveBeenCalledTimes(1);

    await act(async () => {
      resolveApply({
        planToken: 'plan-token-1',
        clientRequestId,
        items: [
          {
            inventoryItemId: item.inventoryItemId,
            state: 'succeeded',
            errorCode: null,
            message: null,
          },
        ],
      });
      await Promise.all([first, second]);
    });
    expect(portableApiMocks.applyAction).toHaveBeenCalledTimes(1);
  });
});

import {
  clearPortableFilterSearchParams,
  normalizeAgentHubSection,
  parsePortableFiltersFromSearchParams,
  writePortableFiltersToSearchParams,
} from './useAgentHubController';
import { DEFAULT_PORTABLE_INVENTORY_FILTERS } from './portableAssets';
import { parseAgentHubContext, writeAgentHubContext } from './context/agentHubContext';

describe('Agent Hub URL helpers', () => {
  test('normalizeAgentHubSection maps legacy portableAssets to assets', () => {
    expect(normalizeAgentHubSection('portableAssets')).toBe('assets');
    expect(normalizeAgentHubSection('assets')).toBe('assets');
    expect(normalizeAgentHubSection('nope', 'diagnostics')).toBe('diagnostics');
  });

  test('parse and write portable filter query contract', () => {
    const parsed = parsePortableFiltersFromSearchParams(
      new URLSearchParams(
        'target=claude&kind=mcp&inventoryScope=project&state=problem&management=drifted',
      ),
    );
    expect(parsed).toEqual({
      target: 'claude',
      kind: 'mcp',
      scope: 'project',
      actualState: 'problem',
      management: 'drifted',
    });
    const written = writePortableFiltersToSearchParams(
      new URLSearchParams('conflictId=c9&scope=project'),
      {
        ...DEFAULT_PORTABLE_INVENTORY_FILTERS,
        target: 'claude',
        kind: 'command',
        scope: 'user',
        actualState: 'enabled',
        management: 'hubManaged',
      },
      'item-1',
    );
    expect(written.get('section')).toBeNull();
    expect(written.get('target')).toBeNull();
    expect(written.get('kind')).toBeNull();
    expect(written.get('inventoryScope')).toBeNull();
    // inventory scope is a portable filter; shell context scope remains intact.
    expect(written.get('scope')).toBe('project');
    expect(written.get('state')).toBe('enabled');
    expect(written.get('management')).toBe('hubManaged');
    expect(written.get('inventoryItemId')).toBe('item-1');
    expect(written.get('conflictId')).toBe('c9');
  });

  test('clearPortableFilterSearchParams drops assets section and filter keys only', () => {
    const polluted = new URLSearchParams(
      'section=assets&kind=command&target=claude&state=enabled&management=hubManaged&inventoryItemId=i1&bridge=/tmp&inventoryScope=user&scope=project',
    );
    const cleared = clearPortableFilterSearchParams(polluted);
    expect(cleared.get('section')).toBeNull();
    expect(cleared.get('kind')).toBeNull();
    expect(cleared.get('target')).toBeNull();
    expect(cleared.get('state')).toBeNull();
    expect(cleared.get('management')).toBeNull();
    expect(cleared.get('inventoryItemId')).toBeNull();
    expect(cleared.get('bridge')).toBe('/tmp');
    expect(cleared.get('inventoryScope')).toBeNull();
    // scope 是壳层键，clear 不删；诊断 section 保留
    expect(cleared.get('scope')).toBe('project');

    const diagnostics = clearPortableFilterSearchParams(
      new URLSearchParams('section=diagnostics&kind=mcp'),
    );
    expect(diagnostics.get('section')).toBe('diagnostics');
    expect(diagnostics.get('kind')).toBeNull();
  });

  test('write instructions then clear prevents legacy kind from re-parsing to asset tab', () => {
    // 模拟：filters 曾写入 section/kind，再切回 instructions
    const polluted = writePortableFiltersToSearchParams(
      new URLSearchParams('tab=command'),
      {
        ...DEFAULT_PORTABLE_INVENTORY_FILTERS,
        kind: 'command',
        target: 'claude',
      },
      null,
    );
    expect(parseAgentHubContext(polluted).tab).toBe('command');

    const toInstructions = writeAgentHubContext(polluted, {
      agent: 'claude',
      scope: 'user',
      deviceId: null,
      projectKey: null,
      tab: 'instructions',
      instructionLane: 'common',
      adaptView: false,
    });
    // writeAgentHubContext 已剥 legacy；再 clear 是防御双写
    const cleaned = clearPortableFilterSearchParams(toInstructions);
    expect(parseAgentHubContext(cleaned).tab).toBe('instructions');
    expect(cleaned.get('kind')).toBeNull();
    expect(cleaned.get('section')).toBeNull();
  });
});
