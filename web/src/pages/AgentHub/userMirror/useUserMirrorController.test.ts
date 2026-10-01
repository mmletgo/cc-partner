// @vitest-environment jsdom

/**
 * useUserMirrorController 状态机测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   镜像 apply 必须先 preview、勾选破坏性确认；换设备必须作废 plan；
 *   apply 请求体仅 planToken + clientRequestId。
 *
 * Code Logic（这个测试做什么）:
 *   inject mock devices / userMirror API；renderHook 验证 preview/confirm/device/apply。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { Device } from '@/lib/types';
import type {
  UserMirrorApi,
  UserMirrorPlanDto,
  UserMirrorResultDto,
} from '@/lib/types/userMirror';
import { USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE } from '@/lib/types/userMirror';
import { useUserMirrorController } from './useUserMirrorController';

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
  {
    id: 'device-offline',
    name: 'Offline',
    address: '10.0.0.4',
    port: 62116,
    status: 'offline',
  },
];

function planFixture(overrides: Partial<UserMirrorPlanDto> = {}): UserMirrorPlanDto {
  return {
    planToken: 'plan-1',
    expiresAt: '2099-01-01T00:00:00.000Z',
    direction: 'pull',
    sourceDeviceId: 'dev-a',
    destinationDeviceId: 'dev-local',
    remoteInventorySnapshotHash: 'remote-hash',
    localInventorySnapshotHash: 'local-hash',
    credentialBearingCount: 1,
    hasCredentialBearingAssets: true,
    agents: [
      {
        target: 'claude',
        instructionWrites: [
          {
            logicalId: 'claude.native.CLAUDE.md',
            op: 'replace',
            sourceHash: 'src-hash',
            destHash: 'dst-hash',
          },
        ],
        portableUpserts: [
          {
            kind: 'skill',
            nativeId: 'skill-a',
            displayName: 'Skill A',
            op: 'write',
            credentialBearing: false,
          },
        ],
        portableDeletes: [
          {
            kind: 'command',
            nativeId: 'cmd-x',
            displayName: 'Cmd X',
            op: 'delete',
            credentialBearing: false,
          },
        ],
        pluginDisables: [
          {
            kind: 'plugin',
            nativeId: 'plug-x',
            displayName: 'Plug X',
            op: 'disable',
            credentialBearing: false,
          },
        ],
        mcpDeletes: [
          {
            kind: 'mcp',
            nativeId: 'github',
            displayName: 'GitHub MCP',
            op: 'delete',
            credentialBearing: true,
          },
        ],
      },
    ],
    blockingReasons: [],
    peerPlans: [],
    ...overrides,
  };
}

function resultFixture(overrides: Partial<UserMirrorResultDto> = {}): UserMirrorResultDto {
  return {
    planToken: 'plan-1',
    clientRequestId: 'req-1',
    sourceDeviceId: 'dev-a',
    destinationDeviceId: 'dev-local',
    partial: false,
    agents: [
      {
        target: 'claude',
        state: 'succeeded',
        errorCode: null,
        message: null,
      },
    ],
    peerResults: [],
    ...overrides,
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

function createMirrorApi(overrides: Partial<UserMirrorApi> = {}): UserMirrorApi {
  return {
    preview: vi.fn(async () => planFixture()),
    apply: vi.fn(async () => resultFixture()),
    get: vi.fn(async () => resultFixture()),
    ...overrides,
  };
}

afterEach(() => {
  vi.clearAllMocks();
});

describe('useUserMirrorController', () => {
  test('a globally blocked preview cannot apply and its selection remains editable', async () => {
    const mirrorApi = createMirrorApi({
      preview: vi.fn(async () => planFixture({ blockingReasons: ['USER_MIRROR_CAPABILITY_UNSUPPORTED'] })),
    });
    const { result } = renderHook(() => useUserMirrorController({
      open: true, direction: 'pull', mirrorApi, listDevices: vi.fn(async () => devices),
    }));
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => { await result.current.preview(); });
    act(() => { result.current.setConfirmed(true); });
    expect(result.current.canApply).toBe(false);
    await act(async () => { await result.current.apply(); });
    expect(mirrorApi.apply).not.toHaveBeenCalled();
    act(() => { result.current.setIncludeInstructions(false); });
    expect(result.current.includeInstructions).toBe(false);
    expect(result.current.submitted).toBe(false);
  });

  test('apply without preview is blocked and never calls apply', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));

    await act(async () => {
      result.current.setConfirmed(true);
      await result.current.apply();
    });

    expect(mirrorApi.apply).not.toHaveBeenCalled();
    expect(result.current.canApply).toBe(false);
    expect(result.current.error).toContain(USER_MIRROR_PREVIEW_REQUIRED);
  });

  test('confirm checkbox is required before apply even after preview', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));

    await act(async () => {
      await result.current.preview();
    });
    expect(result.current.plan?.planToken).toBe('plan-1');
    expect(result.current.confirmed).toBe(false);
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      await result.current.apply();
    });
    expect(mirrorApi.apply).not.toHaveBeenCalled();

    await act(async () => {
      result.current.setConfirmed(true);
    });
    expect(result.current.canApply).toBe(true);

    await act(async () => {
      await result.current.apply();
    });
    expect(mirrorApi.apply).toHaveBeenCalledTimes(1);
    const payload = (mirrorApi.apply as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as {
      planToken: string;
      clientRequestId: string;
    };
    expect(payload).toEqual({
      planToken: 'plan-1',
      clientRequestId: payload.clientRequestId,
    });
    expect(Object.keys(payload).sort()).toEqual(['clientRequestId', 'planToken']);
    expect(payload.clientRequestId.length).toBeGreaterThan(0);
  });

  test('changing pull source device clears the plan and disables apply', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));

    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });
    expect(result.current.canApply).toBe(true);
    expect(mirrorApi.preview).toHaveBeenCalledWith({
      direction: 'pull',
      sourceDeviceId: 'device-a',
      peerDeviceIds: [],
    });

    await act(async () => {
      result.current.selectSourceDevice('device-b');
    });
    expect(result.current.plan).toBeNull();
    expect(result.current.result).toBeNull();
    expect(result.current.confirmed).toBe(false);
    expect(result.current.canApply).toBe(false);
    expect(result.current.sourceDeviceId).toBe('device-b');
  });

  test('push preview sends peerDeviceIds and toggling a peer invalidates the plan', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'push',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));

    await act(async () => {
      result.current.togglePeer('device-a');
      result.current.togglePeer('device-b');
    });
    await act(async () => {
      await result.current.preview();
    });
    expect(mirrorApi.preview).toHaveBeenCalledWith({
      direction: 'push',
      peerDeviceIds: ['device-a', 'device-b'],
    });
    expect(result.current.plan).not.toBeNull();

    await act(async () => {
      result.current.togglePeer('device-b');
    });
    expect(result.current.plan).toBeNull();
    expect(result.current.canApply).toBe(false);
    expect(result.current.selectedPeerIds).toEqual(['device-a']);
  });

  test('stale apply error blocks canApply until a new preview', async () => {
    const mirrorApi = createMirrorApi({
      apply: vi.fn(async () => {
        throw Object.assign(new Error('inventory drifted'), { code: USER_MIRROR_STALE });
      }),
    });
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });
    await act(async () => {
      await result.current.apply();
    });
    expect(result.current.stale).toBe(true);
    expect(result.current.canApply).toBe(false);
    expect(result.current.error).toContain(USER_MIRROR_STALE);
  });

  test('confirmed defaults to false when the dialog is reopened', async () => {
    const mirrorApi = createMirrorApi();
    let open = true;
    const { result, rerender } = renderHook(() =>
      useUserMirrorController({
        open,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      result.current.setConfirmed(true);
    });
    expect(result.current.confirmed).toBe(true);

    open = false;
    rerender();
    open = true;
    rerender();
    await waitFor(() => expect(result.current.confirmed).toBe(false));
    expect(result.current.plan).toBeNull();
  });

  test('default selection applies without a selection field', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });

    // 默认全选 + 指令开：资产全部列出并勾选。
    expect(result.current.assetOptions.map((option) => option.key)).toEqual([
      'skill:skill-a',
      'command:cmd-x',
      'plugin:plug-x',
      'mcp:github',
    ]);
    expect(result.current.selectedAssetKeys).toHaveLength(4);
    expect(result.current.includeInstructions).toBe(true);

    await act(async () => {
      await result.current.apply();
    });
    const payload = (mirrorApi.apply as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as {
      planToken?: unknown;
      clientRequestId?: unknown;
      selection?: unknown;
    };
    expect('selection' in payload).toBe(false);
  });

  test('deselecting an asset sends a selection filter with the remaining keys', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });

    await act(async () => {
      result.current.toggleAsset('skill:skill-a');
    });
    expect(result.current.selectedAssetKeys).toEqual([
      'command:cmd-x',
      'plugin:plug-x',
      'mcp:github',
    ]);

    // 全选快捷操作回到默认，并按新范围重新确认。
    await act(async () => {
      result.current.selectAllAssets();
    });
    expect(result.current.selectedAssetKeys).toHaveLength(4);
    await act(async () => {
      result.current.toggleAsset('skill:skill-a');
    });
    await act(async () => {
      result.current.setConfirmed(true);
    });
    await act(async () => {
      await result.current.apply();
    });
    const payload = (mirrorApi.apply as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as {
      selection?: { includeInstructions: boolean; portableKeys: unknown };
    };
    expect(payload.selection).toEqual({
      includeInstructions: true,
      portableKeys: [
        { kind: 'command', nativeId: 'cmd-x' },
        { kind: 'plugin', nativeId: 'plug-x' },
        { kind: 'mcp', nativeId: 'github' },
      ],
    });
  });

  test('turning includeInstructions off sends includeInstructions=false', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });

    await act(async () => {
      result.current.setIncludeInstructions(false);
      result.current.setConfirmed(true);
    });
    expect(result.current.includeInstructions).toBe(false);

    await act(async () => {
      await result.current.apply();
    });
    const payload = (mirrorApi.apply as ReturnType<typeof vi.fn>).mock.calls[0]?.[0] as {
      selection?: { includeInstructions: boolean; portableKeys: unknown };
    };
    expect(payload.selection).toEqual({ includeInstructions: false, portableKeys: null });

    // 重新预览必须保留用户缩小后的范围，不能静默扩大回全量。
    await act(async () => {
      await result.current.preview();
    });
    expect(result.current.includeInstructions).toBe(false);
    expect(result.current.selectedAssetKeys).toHaveLength(4);
  });

  test('synchronous operation latch coalesces apply double-clicks', async () => {
    const pendingApply = deferred<UserMirrorResultDto>();
    const mirrorApi = createMirrorApi({
      apply: vi.fn(() => pendingApply.promise),
    });
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.setConfirmed(true);
    });

    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = result.current.apply();
      second = result.current.apply();
    });
    expect(mirrorApi.apply).toHaveBeenCalledTimes(1);

    pendingApply.resolve(resultFixture());
    await act(async () => {
      await Promise.all([first, second]);
    });
    expect(mirrorApi.apply).toHaveBeenCalledTimes(1);
  });

  test('apply transport failure exposes same-request reconciliation and recovers the result', async () => {
    const mirrorApi = createMirrorApi({
      apply: vi.fn(async () => {
        throw new Error('network timeout');
      }),
      get: vi.fn(async () => resultFixture()),
    });
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
    });
    await act(async () => {
      result.current.setConfirmed(true);
    });
    await act(async () => {
      await result.current.apply();
    });

    expect(result.current.clientRequestId).toBeTruthy();
    expect(result.current.canApply).toBe(false);
    expect(result.current.canReconcile).toBe(true);
    const failedRequestId = result.current.clientRequestId;

    await act(async () => {
      await result.current.reconcile();
    });
    expect(mirrorApi.get).toHaveBeenCalledWith(failedRequestId);
    expect(result.current.result?.partial).toBe(false);
    expect(result.current.canReconcile).toBe(false);
  });

  test('successful apply cannot be submitted again until a new preview', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
    });
    await act(async () => {
      result.current.setConfirmed(true);
    });
    await act(async () => {
      await result.current.apply();
    });
    await act(async () => {
      await result.current.apply();
    });

    expect(mirrorApi.apply).toHaveBeenCalledTimes(1);
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      await result.current.preview();
    });
    expect(result.current.result).toBeNull();
    expect(result.current.confirmed).toBe(false);
  });

  test('selection changes preserve the plan, reset confirmation, and empty selection cannot apply', async () => {
    const mirrorApi = createMirrorApi();
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
    });
    await act(async () => {
      result.current.setConfirmed(true);
      result.current.setIncludeInstructions(false);
      result.current.deselectAllAssets();
    });

    expect(result.current.plan?.planToken).toBe('plan-1');
    expect(result.current.confirmed).toBe(false);
    expect(result.current.selectedAssetKeys).toEqual([]);
    expect(result.current.canApply).toBe(false);
  });

  test('new preview preserves the selected scope and leaves newly discovered assets unselected', async () => {
    const secondPlan = planFixture({
      planToken: 'plan-2',
      agents: [
        {
          ...planFixture().agents[0],
          portableUpserts: [
            ...planFixture().agents[0].portableUpserts,
            {
              kind: 'skill',
              nativeId: 'skill-new',
              displayName: 'New Skill',
              op: 'write',
              credentialBearing: false,
            },
          ],
        },
      ],
    });
    const mirrorApi = createMirrorApi({
      preview: vi
        .fn<() => Promise<UserMirrorPlanDto>>()
        .mockResolvedValueOnce(planFixture())
        .mockResolvedValueOnce(secondPlan),
    });
    const { result } = renderHook(() =>
      useUserMirrorController({
        open: true,
        direction: 'pull',
        mirrorApi,
        listDevices: vi.fn(async () => devices),
      }),
    );
    await waitFor(() => expect(result.current.sourceDeviceId).toBe('device-a'));
    await act(async () => {
      await result.current.preview();
      result.current.toggleAsset('command:cmd-x');
    });
    await act(async () => {
      await result.current.preview();
    });

    expect(result.current.plan?.planToken).toBe('plan-2');
    expect(result.current.selectedAssetKeys).toEqual([
      'skill:skill-a',
      'plugin:plug-x',
      'mcp:github',
    ]);
  });

  test('reopening clears the previous peer list while devices reload', async () => {
    const secondLoad = deferred<Device[]>();
    const listDevices = vi
      .fn<() => Promise<Device[]>>()
      .mockResolvedValueOnce(devices)
      .mockImplementationOnce(() => secondLoad.promise);
    let open = true;
    const { result, rerender } = renderHook(() =>
      useUserMirrorController({
        open,
        direction: 'pull',
        mirrorApi: createMirrorApi(),
        listDevices,
      }),
    );
    await waitFor(() => expect(result.current.devices).toHaveLength(2));

    open = false;
    rerender();
    open = true;
    rerender();
    await waitFor(() => expect(result.current.devices).toEqual([]));
    expect(result.current.sourceDeviceId).toBe('');

    secondLoad.resolve(devices);
    await waitFor(() => expect(result.current.devices).toHaveLength(2));
  });
});
