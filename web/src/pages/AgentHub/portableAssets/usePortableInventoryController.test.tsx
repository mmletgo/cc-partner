// @vitest-environment jsdom
/**
 * Portable inventory controller race / stale / lock 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   refresh generation 必须保证旧响应不覆盖新 snapshot；失败保留 stale 并禁止 mutation。
 *
 * Code Logic（这个测试做什么）:
 *   defer 两次 refresh 并后 resolve 旧响应；reject 后 mutationBlocked；单 item lock；
 *   unopted/unsupported/stale 不暴露 primary action。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type {
  PortableInventoryItemDto,
  PortableInventorySnapshotDto,
} from '@/lib/types/portableInventory';

const apiMocks = vi.hoisted(() => ({
  inspect: vi.fn(),
  previewAction: vi.fn(),
  applyAction: vi.fn(),
  getAction: vi.fn(),
}));

vi.mock('@/api/portableInventory', () => ({
  portableAssetApi: apiMocks,
}));

import { usePortableInventoryController } from './usePortableInventoryController';

const baseCapabilities = {
  canEnable: true,
  canDisable: true,
  canUninstall: true,
  canAdopt: false,
  canInstallToSourceTarget: false,
  reasonCode: null as string | null,
  evidenceIds: [] as string[],
};

function makeItem(
  overrides: Partial<PortableInventoryItemDto> &
    Pick<PortableInventoryItemDto, 'inventoryItemId' | 'kind' | 'nativeId'>,
): PortableInventoryItemDto {
  return {
    target: 'claude',
    displayName: overrides.nativeId,
    description: null,
    version: null,
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: `/tmp/${overrides.nativeId}`,
    sourceOrigin: 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: `hash-${overrides.nativeId}`,
    treeHash: null,
    canonicalAssetId: null,
    canonicalRevisionId: null,
    managementState: 'hubManaged',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'applied',
    capabilities: { ...baseCapabilities },
    warnings: [],
    ...overrides,
  };
}

function snapshot(
  hash: string,
  items: PortableInventoryItemDto[],
  stale = false,
): PortableInventorySnapshotDto {
  return {
    inventorySnapshotHash: hash,
    refreshedAt: '2026-08-07T12:00:00.000Z',
    stale,
    targets: [
      {
        target: 'claude',
        installed: true,
        version: '1.0.0',
        executable: '/usr/bin/claude',
        configRoot: '/home/.claude',
        scanCapability: 'supported',
        mutationCapability: 'supported',
        reasonCode: null,
        evidenceIds: [],
      },
    ],
    items,
  };
}

const alpha = makeItem({
  inventoryItemId: 'claude-skill-alpha',
  kind: 'skill',
  nativeId: 'alpha',
  actualEnabled: true,
});
const projectItem = makeItem({
  inventoryItemId: 'claude-skill-project',
  kind: 'skill',
  nativeId: 'project-skill',
  scopeKind: 'project',
  projectId: 'demo',
  projectOptedIn: false,
  capabilities: { ...baseCapabilities, canAdopt: true },
});
const unsupported = makeItem({
  inventoryItemId: 'claude-skill-unsupported',
  kind: 'skill',
  nativeId: 'unsupported-skill',
  managementState: 'unsupported',
  actualEnabled: false,
  capabilities: {
    ...baseCapabilities,
    canEnable: false,
    canDisable: false,
    canUninstall: false,
    canAdopt: false,
  },
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

afterEach(() => {
  vi.clearAllMocks();
});

describe('usePortableInventoryController', () => {
  test('newer refresh wins when older inspect resolves last', async () => {
    const first = deferred<PortableInventorySnapshotDto>();
    const second = deferred<PortableInventorySnapshotDto>();
    apiMocks.inspect
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));

    await act(async () => {
      // mount triggers first refresh; kick second while first is pending
      const pending = result.current.refresh();
      second.resolve(snapshot('snap-new', [alpha]));
      await Promise.resolve();
      first.resolve(snapshot('snap-old', [unsupported]));
      await pending;
    });

    await waitFor(() => {
      expect(result.current.snapshot?.inventorySnapshotHash).toBe('snap-new');
    });
    expect(result.current.stale).toBe(false);
    expect(result.current.mutationBlocked).toBe(false);
    expect(result.current.visibleItems.map((item) => item.inventoryItemId)).toEqual([
      'claude-skill-alpha',
    ]);
  });

  test('refresh rejection keeps old snapshot, marks stale and blocks mutation', async () => {
    apiMocks.inspect.mockResolvedValueOnce(snapshot('snap-ok', [alpha]));
    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));
    await waitFor(() => expect(result.current.snapshot?.inventorySnapshotHash).toBe('snap-ok'));
    expect(result.current.mutationBlocked).toBe(false);

    apiMocks.inspect.mockRejectedValueOnce(
      Object.assign(new Error('inspect failed'), { code: 'PORTABLE_CLI_UNAVAILABLE' }),
    );
    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.snapshot?.inventorySnapshotHash).toBe('snap-ok');
    expect(result.current.stale).toBe(true);
    expect(result.current.mutationBlocked).toBe(true);
    expect(result.current.error).toBeTruthy();
    expect(result.current.getPrimaryAction(alpha)).toBeNull();
  });

  test('locking one item only blocks that item primary action', async () => {
    const beta = makeItem({
      inventoryItemId: 'claude-skill-beta',
      kind: 'skill',
      nativeId: 'beta',
      actualEnabled: false,
      capabilities: { ...baseCapabilities, canEnable: true, canDisable: false },
    });
    apiMocks.inspect.mockResolvedValue(snapshot('snap-lock', [alpha, beta]));
    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));
    await waitFor(() => expect(result.current.snapshot).not.toBeNull());

    expect(result.current.getPrimaryAction(alpha)).toBe('disable');
    expect(result.current.getPrimaryAction(beta)).toBe('enable');

    act(() => {
      result.current.setItemLocked('claude-skill-alpha', true);
    });

    expect(result.current.lockedItemIds.has('claude-skill-alpha')).toBe(true);
    expect(result.current.getPrimaryAction(alpha)).toBeNull();
    expect(result.current.getPrimaryAction(beta)).toBe('enable');
  });

  test('unopted/unsupported/stale items never expose mutation action', async () => {
    apiMocks.inspect.mockResolvedValue(
      snapshot('snap-readonly', [alpha, projectItem, unsupported], true),
    );
    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));
    await waitFor(() => expect(result.current.snapshot).not.toBeNull());

    expect(result.current.stale).toBe(true);
    expect(result.current.mutationBlocked).toBe(true);
    expect(result.current.getPrimaryAction(alpha)).toBeNull();
    expect(result.current.getPrimaryAction(projectItem)).toBeNull();
    expect(result.current.getPrimaryAction(unsupported)).toBeNull();

    act(() => {
      result.current.openAction('claude-skill-alpha', 'disable');
    });
    expect(result.current.pendingAction).toBeNull();
  });

  test('kind filter change requests a narrowed inventory snapshot', async () => {
    const command = makeItem({
      inventoryItemId: 'claude-command-gamma',
      kind: 'command',
      nativeId: 'gamma',
      actualEnabled: null,
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
      },
    });
    apiMocks.inspect.mockResolvedValue(snapshot('snap-filter', [alpha, command]));
    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));
    await waitFor(() => expect(result.current.snapshot).not.toBeNull());
    const callsAfterLoad = apiMocks.inspect.mock.calls.length;

    act(() => {
      result.current.setFilters({ kind: 'command' });
      result.current.selectItem('claude-command-gamma');
    });

    await waitFor(() => {
      expect(apiMocks.inspect).toHaveBeenCalledTimes(callsAfterLoad + 1);
      expect(result.current.visibleItems.map((item) => item.inventoryItemId)).toEqual([
        'claude-command-gamma',
      ]);
    });
    expect(apiMocks.inspect).toHaveBeenLastCalledWith({
      deviceId: null,
      projectRef: null,
      target: 'claude',
      kind: 'command',
    });
    expect(result.current.filters.kind).toBe('command');
    // 查询域变化会清掉旧快照选择，避免把上一快照 item 交给新 hash 执行。
    expect(result.current.selectedItemId).toBeNull();

    act(() => result.current.setFilters({ kind: 'skill' }));
    await waitFor(() =>
      expect(result.current.visibleItems.map((item) => item.inventoryItemId)).toEqual([
        'claude-skill-alpha',
      ]),
    );
    expect(apiMocks.inspect).toHaveBeenCalledTimes(callsAfterLoad + 1);
  });

  test('openAction records pending action only when mutation is allowed', async () => {
    apiMocks.inspect.mockResolvedValue(snapshot('snap-action', [alpha]));
    const { result } = renderHook(() => usePortableInventoryController({ enabled: true }));
    await waitFor(() => expect(result.current.snapshot).not.toBeNull());

    act(() => {
      result.current.openAction('claude-skill-alpha', 'disable');
    });
    expect(result.current.pendingAction).toEqual({
      itemId: 'claude-skill-alpha',
      action: 'disable',
    });

    act(() => {
      result.current.clearPendingAction();
    });
    expect(result.current.pendingAction).toBeNull();
  });

  test('changing deviceId retriggers inspect with new context and clears prior snapshot', async () => {
    apiMocks.inspect.mockImplementation(async (ctx?: { deviceId?: string | null }) => {
      if (ctx?.deviceId === 'peer-1') {
        throw Object.assign(new Error('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE'), {
          code: 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE',
        });
      }
      return snapshot('snap-local', [alpha]);
    });

    const { result, rerender } = renderHook(
      (props: { deviceId: string | null }) =>
        usePortableInventoryController({ enabled: true,  deviceId: props.deviceId, projectRef: null }),
      { initialProps: { deviceId: null as string | null } },
    );

    await waitFor(() => expect(result.current.snapshot?.inventorySnapshotHash).toBe('snap-local'));
    expect(apiMocks.inspect).toHaveBeenCalledWith({
      deviceId: null,
      projectRef: null,
      target: 'claude',
      kind: 'skill',
    });
    const callsAfterLocal = apiMocks.inspect.mock.calls.length;

    rerender({ deviceId: 'peer-1' });

    await waitFor(() => {
      expect(apiMocks.inspect.mock.calls.length).toBeGreaterThan(callsAfterLocal);
    });
    await waitFor(() => {
      expect(result.current.error).toBe('AGENT_HUB_PEER_CONTEXT_UNAVAILABLE');
    });
    expect(apiMocks.inspect).toHaveBeenCalledWith({
      deviceId: 'peer-1',
      projectRef: null,
      target: 'claude',
      kind: 'skill',
    });
    // peer 切换不得保留本机 snapshot 冒充对端
    expect(result.current.snapshot).toBeNull();
    expect(result.current.mutationBlocked).toBe(true);
  });

  test('changing projectRef retriggers inspect', async () => {
    apiMocks.inspect.mockResolvedValue(snapshot('snap-proj', [alpha]));
    const { result, rerender } = renderHook(
      (props: { projectRef: string | null }) =>
        usePortableInventoryController({ enabled: true,  deviceId: null, projectRef: props.projectRef }),
      { initialProps: { projectRef: null as string | null } },
    );
    await waitFor(() => expect(result.current.snapshot).not.toBeNull());
    const callsAfter = apiMocks.inspect.mock.calls.length;

    rerender({ projectRef: 'wb-local-2' });
    await waitFor(() => {
      expect(apiMocks.inspect.mock.calls.length).toBeGreaterThan(callsAfter);
    });
    expect(apiMocks.inspect).toHaveBeenCalledWith({
      deviceId: null,
      projectRef: 'wb-local-2',
      target: 'claude',
      kind: 'skill',
      localProjectId: 'wb-local-2',
    });
    expect(result.current.requestContext.projectRef).toBe('wb-local-2');
  });
});
