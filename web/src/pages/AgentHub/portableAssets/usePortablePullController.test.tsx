// @vitest-environment jsdom

/**
 * usePortablePullController async/recovery tests.
 *
 * Business Logic（为什么需要这个测试）:
 *   device/target 切换必须取消 stale inventory 并清无效 selection；
 *   preview 失败保留 selection/policy；partial/outcomeUnknown 暴露 reconcile；
 *   重复 apply 复用 clientRequestId；stale remote inventory 禁止 confirm。
 *
 * Code Logic（这个测试做什么）:
 *   inject mock devices/pull API；renderHook + act/waitFor 验证状态机。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { Device } from '@/lib/types';
import type {
  PortablePullApi,
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryDto,
} from '@/lib/types/portableInventory';
import { usePortablePullController } from './usePortablePullController';

const devices: Device[] = [
  {
    id: 'device-a',
    name: 'Alpha',
    address: '10.0.0.2',
    port: 62116,
    status: 'online',
  },
  {
    id: 'device-b',
    name: 'Beta',
    address: '10.0.0.3',
    port: 62116,
    status: 'online',
  },
];

function remoteInventory(
  overrides: Partial<RemotePortableInventoryDto> = {},
): RemotePortableInventoryDto {
  return {
    sourceDeviceId: overrides.sourceDeviceId ?? 'device-a',
    sourceTarget: overrides.sourceTarget ?? 'claude',
    inventorySnapshotHash: overrides.inventorySnapshotHash ?? 'remote-snap-1',
    refreshedAt: overrides.refreshedAt ?? '2026-08-08T00:00:00.000Z',
    stale: overrides.stale ?? false,
    items: overrides.items ?? [
      {
        inventoryItemId: 'remote-skill-1',
        target: 'claude',
        kind: 'skill',
        nativeId: 'remote-skill',
        displayName: 'Remote Skill',
        description: null,
        version: '1.0.0',
        scopeId: 'user',
        projectId: null,
        projectOptedIn: true,
        sourceOrigin: 'standalone',
        actualEnabled: true,
        contentHash: 'c1',
        treeHash: null,
        warnings: [],
      },
      {
        inventoryItemId: 'remote-cmd-1',
        target: 'claude',
        kind: 'command',
        nativeId: 'remote-cmd',
        displayName: 'Remote Cmd',
        description: null,
        version: null,
        scopeId: 'user',
        projectId: null,
        projectOptedIn: true,
        sourceOrigin: 'standalone',
        actualEnabled: false,
        contentHash: 'c2',
        treeHash: null,
        warnings: [],
      },
    ],
  };
}

function planFixture(overrides: Partial<PortablePullPlanDto> = {}): PortablePullPlanDto {
  return {
    planToken: overrides.planToken ?? 'pull-plan-1',
    expiresAt: overrides.expiresAt ?? '2026-08-08T00:15:00.000Z',
    sourceDeviceId: overrides.sourceDeviceId ?? 'device-a',
    sourceTarget: overrides.sourceTarget ?? 'claude',
    destinationTarget: overrides.destinationTarget ?? 'claude',
    remoteInventorySnapshotHash: overrides.remoteInventorySnapshotHash ?? 'remote-snap-1',
    localInventorySnapshotHash: overrides.localInventorySnapshotHash ?? 'local-snap-1',
    conflictPolicy: overrides.conflictPolicy ?? 'skipExisting',
    selectionManifestHash: overrides.selectionManifestHash ?? 'sel-1',
    credentialBearingCount: overrides.credentialBearingCount ?? 0,
    hasCredentialBearingAssets: overrides.hasCredentialBearingAssets ?? false,
    changes: overrides.changes ?? [
      {
        inventoryItemId: 'remote-skill-1',
        kind: 'skill',
        nativeId: 'remote-skill',
        displayName: 'Remote Skill',
        installMode: 'installToTarget',
        conflict: false,
        legacyLossy: false,
        credentialBearing: false,
        blockingReasons: [],
        warnings: [],
      },
    ],
    blockingReasons: overrides.blockingReasons ?? [],
  };
}

function resultFixture(overrides: Partial<PortablePullResultDto> = {}): PortablePullResultDto {
  return {
    planToken: overrides.planToken ?? 'pull-plan-1',
    clientRequestId: overrides.clientRequestId ?? 'client-req-1',
    sourceDeviceId: overrides.sourceDeviceId ?? 'device-a',
    sourceTarget: overrides.sourceTarget ?? 'claude',
    destinationTarget: overrides.destinationTarget ?? 'claude',
    partial: overrides.partial ?? false,
    items: overrides.items ?? [
      {
        inventoryItemId: 'remote-skill-1',
        state: 'succeeded',
        installMode: 'installToTarget',
        errorCode: null,
        message: null,
      },
    ],
  };
}

function createPullApi(overrides: Partial<PortablePullApi> = {}): PortablePullApi {
  return {
    listRemote: vi.fn(async () => remoteInventory()),
    preview: vi.fn(async () => planFixture()),
    apply: vi.fn(async () => resultFixture()),
    get: vi.fn(async () => resultFixture()),
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

afterEach(() => {
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe('usePortablePullController', () => {
  test('resets conflict policy and mutation session when Pull is reopened', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    let open = true;
    const { result, rerender } = renderHook(() =>
      usePortablePullController({
        open,
        pullApi,
        listDevices,
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      result.current.setConflictPolicy('replaceAfterPreview');
    });
    expect(result.current.conflictPolicy).toBe('replaceAfterPreview');

    open = false;
    rerender();
    open = true;
    rerender();
    await waitFor(() => expect(result.current.conflictPolicy).toBe('skipExisting'));
    expect(result.current.plan).toBeNull();
    expect(result.current.result).toBeNull();
    expect(result.current.clientRequestId).toBeNull();
  });

  test('prefers initialSourceDeviceId and initialSourceTarget from hub shell context when opening', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
        initialSourceDeviceId: 'device-b',
        initialSourceTarget: 'codex',
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    // shell context device=peer → default source peer for same-agent pull
    expect(result.current.selectedDeviceId).toBe('device-b');
    expect(result.current.sourceTarget).toBe('codex');
  });

  test('fails closed when the explicitly selected source peer is offline', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
        initialSourceDeviceId: 'offline-peer',
        initialSourceTarget: 'claude',
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    expect(result.current.selectedDeviceId).toBe('');
    expect(result.current.error).toBe('AGENT_HUB_SELECTED_PEER_OFFLINE');
  });

  test('loads devices and remote inventory for selected device/target with destination fixed to source', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    expect(result.current.selectedDeviceId).toBe('device-a');
    expect(result.current.sourceTarget).toBe('claude');

    await act(async () => {
      await result.current.loadInventory();
    });

    expect(pullApi.listRemote).toHaveBeenCalledWith({
      sourceDeviceId: 'device-a',
      sourceTarget: 'claude',
    });
    expect(result.current.remoteInventory?.items).toHaveLength(2);
    expect(result.current.sourceTarget).toBe('claude');

    await act(async () => {
      result.current.toggleItem('remote-skill-1');
      result.current.setConflictPolicy('replaceAfterPreview');
    });
    await act(async () => {
      await result.current.preview();
    });

    expect(pullApi.preview).toHaveBeenCalledWith({
      sourceDeviceId: 'device-a',
      sourceTarget: 'claude',
      destinationTarget: 'claude',
      remoteInventorySnapshotHash: 'remote-snap-1',
      inventoryItemIds: ['remote-skill-1'],
      conflictPolicy: 'replaceAfterPreview',
    });
    expect(result.current.plan?.destinationTarget).toBe('claude');
  });

  test('project pull binds remote shortcut and exact local destination project', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
        initialSourceDeviceId: 'device-a',
        sourceProjectRef: 'remote:device-a:shortcut',
        destinationLocalProjectId: 'workbench-local-1',
      }),
    );
    await waitFor(() => expect(result.current.selectedDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.loadInventory();
      result.current.toggleItem('remote-skill-1');
    });
    await act(async () => {
      await result.current.preview();
    });
    expect(pullApi.listRemote).toHaveBeenCalledWith({
      sourceDeviceId: 'device-a',
      sourceTarget: 'claude',
      sourceProjectRef: 'remote:device-a:shortcut',
    });
    expect(pullApi.preview).toHaveBeenCalledWith(
      expect.objectContaining({
        sourceProjectRef: 'remote:device-a:shortcut',
        destinationLocalProjectId: 'workbench-local-1',
      }),
    );
  });

  test('device or target change cancels stale inventory and clears invalid selection', async () => {
    let resolveFirst: ((value: RemotePortableInventoryDto) => void) | null = null;
    const first = new Promise<RemotePortableInventoryDto>((resolve) => {
      resolveFirst = resolve;
    });
    const pullApi = createPullApi({
      listRemote: vi
        .fn()
        .mockImplementationOnce(() => first)
        .mockResolvedValueOnce(
          remoteInventory({
            sourceDeviceId: 'device-b',
            sourceTarget: 'codex',
            inventorySnapshotHash: 'remote-snap-b',
            items: [
              {
                inventoryItemId: 'codex-skill-1',
                target: 'codex',
                kind: 'skill',
                nativeId: 'codex-skill',
                displayName: 'Codex Skill',
                description: null,
                version: null,
                scopeId: 'user',
                projectId: null,
                projectOptedIn: true,
                sourceOrigin: 'standalone',
                actualEnabled: true,
                contentHash: 'x',
                treeHash: null,
                warnings: [],
              },
            ],
          }),
        ),
    });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));

    await act(async () => {
      void result.current.loadInventory();
    });
    await act(async () => {
      result.current.toggleItem('remote-skill-1');
    });
    expect(result.current.selectedItemIds.has('remote-skill-1')).toBe(true);

    await act(async () => {
      result.current.selectDevice('device-b');
      result.current.selectSourceTarget('codex');
    });
    expect(result.current.selectedItemIds.size).toBe(0);
    expect(result.current.remoteInventory).toBeNull();
    expect(result.current.plan).toBeNull();

    await act(async () => {
      await result.current.loadInventory();
    });

    // stale first response must not clobber newer inventory
    await act(async () => {
      resolveFirst?.(
        remoteInventory({
          sourceDeviceId: 'device-a',
          inventorySnapshotHash: 'stale-old',
        }),
      );
    });
    await waitFor(() =>
      expect(result.current.remoteInventory?.inventorySnapshotHash).toBe('remote-snap-b'),
    );
    expect(result.current.remoteInventory?.sourceDeviceId).toBe('device-b');
    expect(result.current.sourceTarget).toBe('codex');
  });

  test('preview failure retains selection and conflict policy', async () => {
    const pullApi = createPullApi({
      preview: vi.fn(async () => {
        throw Object.assign(new Error('preview failed'), { code: 'PORTABLE_PULL_PREVIEW_FAILED' });
      }),
    });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
    });
    await act(async () => {
      result.current.toggleItem('remote-skill-1');
      result.current.setConflictPolicy('replaceAfterPreview');
    });
    await act(async () => {
      await result.current.preview();
    });

    expect(result.current.selectedItemIds.has('remote-skill-1')).toBe(true);
    expect(result.current.conflictPolicy).toBe('replaceAfterPreview');
    expect(result.current.plan).toBeNull();
    expect(result.current.error).toBeTruthy();
  });

  test('selection changes invalidate a pending preview response', async () => {
    const pending = deferred<PortablePullPlanDto>();
    const preview = vi.fn(() => pending.promise);
    const pullApi = createPullApi({ preview });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
      result.current.toggleItem('remote-skill-1');
    });

    let previewPromise!: Promise<void>;
    act(() => {
      previewPromise = result.current.preview();
    });
    await waitFor(() => expect(preview).toHaveBeenCalledTimes(1));

    act(() => {
      result.current.clearSelection();
      result.current.toggleItem('remote-cmd-1');
    });
    expect([...result.current.selectedItemIds]).toEqual(['remote-cmd-1']);
    expect(result.current.plan).toBeNull();
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      pending.resolve(planFixture());
      await previewPromise;
    });

    expect(result.current.plan).toBeNull();
    expect(result.current.canApply).toBe(false);
    expect(result.current.busy).toBe(false);
  });

  test('conflict policy changes invalidate a pending preview response', async () => {
    const pending = deferred<PortablePullPlanDto>();
    const preview = vi.fn(() => pending.promise);
    const pullApi = createPullApi({ preview });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
      result.current.toggleItem('remote-skill-1');
    });

    let previewPromise!: Promise<void>;
    act(() => {
      previewPromise = result.current.preview();
    });
    await waitFor(() => expect(preview).toHaveBeenCalledTimes(1));

    act(() => {
      result.current.setConflictPolicy('replaceAfterPreview');
    });
    expect(result.current.conflictPolicy).toBe('replaceAfterPreview');
    expect(result.current.plan).toBeNull();
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      pending.resolve(planFixture());
      await previewPromise;
    });

    expect(result.current.plan).toBeNull();
    expect(result.current.canApply).toBe(false);
    expect(result.current.busy).toBe(false);
  });

  test('partial and outcomeUnknown expose reconcile; repeated apply reuses clientRequestId', async () => {
    const apply = vi.fn(async (request: { planToken: string; clientRequestId: string }) =>
      resultFixture({
        clientRequestId: request.clientRequestId,
        partial: true,
        items: [
          {
            inventoryItemId: 'remote-skill-1',
            state: 'outcomeUnknown',
            installMode: null,
            errorCode: 'timeout',
            message: null,
          },
        ],
      }),
    );
    const get = vi.fn(async (clientRequestId: string) =>
      resultFixture({
        clientRequestId,
        partial: false,
        items: [
          {
            inventoryItemId: 'remote-skill-1',
            state: 'succeeded',
            installMode: 'installToTarget',
            errorCode: null,
            message: null,
          },
        ],
      }),
    );
    const pullApi = createPullApi({ apply, get });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
    });
    await act(async () => {
      result.current.toggleItem('remote-skill-1');
    });
    await act(async () => {
      await result.current.preview();
    });
    expect(result.current.canReconcile).toBe(false);

    await act(async () => {
      await result.current.apply();
    });
    expect(result.current.canReconcile).toBe(true);
    const firstClientRequestId = result.current.clientRequestId;
    expect(firstClientRequestId).toBeTruthy();
    expect(apply).toHaveBeenCalledWith({
      planToken: 'pull-plan-1',
      clientRequestId: firstClientRequestId,
    });

    await act(async () => {
      await result.current.apply();
    });
    expect(apply).toHaveBeenLastCalledWith({
      planToken: 'pull-plan-1',
      clientRequestId: firstClientRequestId,
    });

    await act(async () => {
      await result.current.reconcile();
    });
    expect(get).toHaveBeenCalledWith(firstClientRequestId);
    expect(result.current.result?.partial).toBe(false);
    expect(result.current.canReconcile).toBe(false);
  });

  test('stale remote inventory disables confirm and apply', async () => {
    const pullApi = createPullApi({
      listRemote: vi.fn(async () => remoteInventory({ stale: true })),
    });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
    });
    await act(async () => {
      result.current.toggleItem('remote-skill-1');
    });
    expect(result.current.mutationBlocked).toBe(true);
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      await result.current.preview();
    });
    // stale inventory must not produce a usable confirm path
    expect(result.current.canApply).toBe(false);
    expect(pullApi.preview).not.toHaveBeenCalled();
  });

  test('refresh invalidates old plan immediately and keeps the old inventory stale on failure', async () => {
    let rejectRefresh: ((reason?: unknown) => void) | null = null;
    const initialSnapshot = remoteInventory();
    const pullApi = createPullApi({
      listRemote: vi
        .fn()
        .mockResolvedValueOnce(initialSnapshot)
        .mockImplementationOnce(
          () =>
            new Promise<RemotePortableInventoryDto>((_resolve, reject) => {
              rejectRefresh = reject;
            }),
        ),
    });
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );

    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
    });
    await act(async () => {
      result.current.toggleItem('remote-skill-1');
      result.current.setConflictPolicy('replaceAfterPreview');
    });
    await act(async () => {
      await result.current.preview();
    });
    await act(async () => {
      await result.current.apply();
    });
    expect(result.current.plan).not.toBeNull();
    expect(result.current.result).not.toBeNull();
    expect(result.current.clientRequestId).toBeTruthy();

    let refreshPromise: Promise<void> | null = null;
    act(() => {
      refreshPromise = result.current.loadInventory();
    });
    expect(result.current.plan).toBeNull();
    expect(result.current.result).toBeNull();
    expect(result.current.clientRequestId).toBeNull();

    await act(async () => {
      rejectRefresh?.(Object.assign(new Error('refresh failed'), { code: 'REFRESH_FAILED' }));
      await refreshPromise;
    });

    expect(result.current.remoteInventory?.items).toEqual(initialSnapshot.items);
    expect(result.current.remoteInventory?.stale).toBe(true);
    expect(result.current.mutationBlocked).toBe(true);
    expect(result.current.canApply).toBe(false);
    expect(result.current.plan).toBeNull();
    expect(result.current.result).toBeNull();
    expect(result.current.clientRequestId).toBeNull();
    expect(result.current.error).toContain('REFRESH_FAILED');
  });

  test('selectVisible and filters only affect current inventory selection helpers', async () => {
    const pullApi = createPullApi();
    const listDevices = vi.fn(async () => devices);
    const { result } = renderHook(() =>
      usePortablePullController({
        open: true,
        pullApi,
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));
    await act(async () => {
      await result.current.loadInventory();
    });
    await act(async () => {
      result.current.setFilters({ kind: 'skill', scope: 'all', actualState: 'all', search: '' });
    });
    await act(async () => {
      result.current.selectVisible();
    });
    expect([...result.current.selectedItemIds]).toEqual(['remote-skill-1']);
    expect(result.current.visibleItems.map((i) => i.inventoryItemId)).toEqual(['remote-skill-1']);
  });
});
