/**
 * Portable inventory / action / pull API 契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   前端命令名与 request 形状必须与后端 committed Tauri 命令完全一致；
 *   成功 body fail-closed 解码；稳定 backend code 原样透传。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke/invokeDecoded；锁定 8 个命令常量、参数与 decoder 路径。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Decoder } from '@/lib/runtimeSchema';
import { ContractDecodeError } from '@/lib/runtimeSchema';
import { validInventorySnapshot } from '@/lib/schemas/portableInventory.test';

const mockInvoke = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
  invokeDecoded: async <T>(
    cmd: string,
    args: Record<string, unknown> | undefined,
    decoder: Decoder<T>,
  ): Promise<T> => {
    const raw = await mockInvoke(cmd, args);
    return decoder.decode(raw, '$');
  },
  normalizeError: (reason: unknown) => {
    if (reason instanceof Error) return reason;
    if (reason && typeof reason === 'object') {
      const obj = reason as Record<string, unknown>;
      const msg = typeof obj.error === 'string' ? obj.error : String(reason);
      const err = new Error(msg);
      if (typeof obj.code === 'string') return Object.assign(err, { code: obj.code });
      return err;
    }
    return new Error(String(reason));
  },
}));

import {
  AGENT_HUB_PEER_CONTEXT_UNAVAILABLE,
  AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE,
  PORTABLE_INVENTORY_COMMANDS,
  portableAssetApi,
  portablePullApi,
  requiresPeerAgentHubPath,
} from './portableInventory';

const validActionPlan = {
  planToken: 'plan-token-1',
  expiresAt: '2026-08-07T12:15:00.000Z',
  inventorySnapshotHash: 'snap-hash-3x4',
  action: 'enable',
  keepData: false,
  conflictPolicy: 'skipExisting',
  changes: [
    {
      inventoryItemId: 'claude-skill-skill-a',
      target: 'claude',
      kind: 'skill',
      path: '/tmp/x',
      operation: 'enable',
      expectedSourceHash: 'h1',
      expectedTreeHash: null,
      expectedCanonicalRevisionId: null,
      backupPolicy: 'none',
      createsOwnership: false,
      canonicalEffect: 'none',
      blockingReasons: [],
      warnings: [],
    },
  ],
  blockingReasons: [],
};

const validActionResult = {
  planToken: 'plan-token-1',
  clientRequestId: 'req-1',
  items: [
    {
      inventoryItemId: 'claude-skill-skill-a',
      state: 'succeeded',
      errorCode: null,
      message: null,
    },
  ],
};

const validRemoteInventory = {
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  inventorySnapshotHash: 'remote-snap-1',
  refreshedAt: '2026-08-07T12:00:00.000Z',
  stale: false,
  items: [
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
  ],
};

const validPullPlan = {
  planToken: 'pull-plan-1',
  expiresAt: '2026-08-07T12:15:00.000Z',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  destinationTarget: 'claude',
  remoteInventorySnapshotHash: 'remote-snap-1',
  localInventorySnapshotHash: 'snap-hash-3x4',
  conflictPolicy: 'skipExisting',
  selectionManifestHash: 'sel-1',
  credentialBearingCount: 0,
  hasCredentialBearingAssets: false,
  changes: [
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
  blockingReasons: [],
};

const validPullResult = {
  planToken: 'pull-plan-1',
  clientRequestId: 'pull-req-1',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude',
  destinationTarget: 'claude',
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
};

describe('portable inventory API', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('command constants match backend snake_case names', () => {
    expect(PORTABLE_INVENTORY_COMMANDS).toEqual({
      inspect: 'agent_hub_inspect_portable_inventory',
      previewAction: 'agent_hub_preview_portable_asset_action',
      applyAction: 'agent_hub_apply_portable_asset_action',
      getAction: 'agent_hub_get_portable_asset_action',
      listRemoteInventory: 'agent_hub_list_remote_portable_inventory',
      previewPull: 'agent_hub_preview_portable_pull',
      applyPull: 'agent_hub_apply_portable_pull',
      getPull: 'agent_hub_get_portable_pull',
    });
  });

  test('inspect decodes PortableInventorySnapshotDto', async () => {
    mockInvoke.mockResolvedValueOnce(validInventorySnapshot);
    const result = await portableAssetApi.inspect();
    expect(mockInvoke).toHaveBeenCalledWith(
      'agent_hub_inspect_portable_inventory',
      undefined,
    );
    expect(result.inventorySnapshotHash).toBe('snap-hash-3x4');
    expect(result.items).toHaveLength(12);
  });

  test('inspect with local null context still uses no-arg invoke', async () => {
    mockInvoke.mockResolvedValueOnce(validInventorySnapshot);
    await portableAssetApi.inspect({ deviceId: null, projectRef: null });
    expect(mockInvoke).toHaveBeenCalledWith(
      'agent_hub_inspect_portable_inventory',
      undefined,
    );
  });

  test('inspect forwards target kind and scope as a typed request', async () => {
    mockInvoke.mockResolvedValueOnce(validInventorySnapshot);
    await portableAssetApi.inspect({
      deviceId: null,
      projectRef: null,
      target: 'codex',
      kind: 'plugin',
      scopeKind: 'user',
    });
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_inspect_portable_inventory', {
      request: { target: 'codex', kind: 'plugin', scopeKind: 'user' },
    });
  });

  test('inspect with local projectRef fails closed until project route exists', async () => {
    await expect(
      Promise.resolve().then(() =>
        portableAssetApi.inspect({ deviceId: null, projectRef: 'wb-local-1' }),
      ),
    ).rejects.toMatchObject({ code: AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE });
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('inspect with peer deviceId fails closed without invoke', async () => {
    let thrown: unknown;
    try {
      await portableAssetApi.inspect({ deviceId: 'peer-1', projectRef: null });
    } catch (reason) {
      thrown = reason;
    }
    expect(thrown).toBeInstanceOf(Error);
    expect((thrown as Error).message).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('inspect with remote projectRef fails closed without invoke', async () => {
    let thrown: unknown;
    try {
      await portableAssetApi.inspect({
        deviceId: null,
        projectRef: 'remote:dev-a:/path/to/repo',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('requiresPeerAgentHubPath classifies local vs peer context', () => {
    expect(requiresPeerAgentHubPath(undefined)).toBe(false);
    expect(requiresPeerAgentHubPath({ deviceId: null, projectRef: null })).toBe(false);
    expect(requiresPeerAgentHubPath({ deviceId: '', projectRef: 'local-proj' })).toBe(false);
    expect(requiresPeerAgentHubPath({ deviceId: 'peer-x' })).toBe(true);
    expect(requiresPeerAgentHubPath({ projectRef: 'remote:p:inner' })).toBe(true);
  });

  test('previewAction passes request object and decodes plan', async () => {
    mockInvoke.mockResolvedValueOnce(validActionPlan);
    const request = {
      inventorySnapshotHash: 'snap-hash-3x4',
      inventoryItemIds: ['claude-skill-skill-a'],
      action: 'enable' as const,
      keepData: false,
      conflictPolicy: 'skipExisting' as const,
      expectedCanonicalRevisionId: null,
    };
    const result = await portableAssetApi.previewAction(request);
    expect(mockInvoke).toHaveBeenCalledWith(
      'agent_hub_preview_portable_asset_action',
      { request },
    );
    expect(result.planToken).toBe('plan-token-1');
  });

  test('previewAction with local projectRef fails closed before write path', async () => {
    await expect(
      Promise.resolve().then(() =>
        portableAssetApi.previewAction({
          inventorySnapshotHash: 'snap-hash-3x4',
          inventoryItemIds: ['claude-skill-skill-a'],
          action: 'enable',
          keepData: false,
          conflictPolicy: 'skipExisting',
          expectedCanonicalRevisionId: null,
          deviceId: null,
          projectRef: 'wb-local',
        }),
      ),
    ).rejects.toMatchObject({ code: AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE });
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('previewAction with peer deviceId fails closed without invoke', async () => {
    let thrown: unknown;
    try {
      await portableAssetApi.previewAction({
        inventorySnapshotHash: 'snap-hash-3x4',
        inventoryItemIds: ['claude-skill-skill-a'],
        action: 'enable',
        keepData: false,
        conflictPolicy: 'skipExisting',
        expectedCanonicalRevisionId: null,
        deviceId: 'peer-1',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('applyAction and getAction use planToken/clientRequestId shapes', async () => {
    mockInvoke.mockResolvedValueOnce(validActionResult);
    await portableAssetApi.applyAction({
      planToken: 'plan-token-1',
      clientRequestId: 'req-1',
    });
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_apply_portable_asset_action', {
      request: { planToken: 'plan-token-1', clientRequestId: 'req-1' },
    });

    mockInvoke.mockResolvedValueOnce(validActionResult);
    await portableAssetApi.getAction('req-1');
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_get_portable_asset_action', {
      clientRequestId: 'req-1',
    });
  });

  test('applyAction with peer deviceId fails closed without local write', async () => {
    let thrown: unknown;
    try {
      await portableAssetApi.applyAction({
        planToken: 'plan-token-1',
        clientRequestId: 'req-1',
        deviceId: 'peer-1',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('pull API list/preview/apply/get command wiring', async () => {
    mockInvoke.mockResolvedValueOnce(validRemoteInventory);
    await portablePullApi.listRemote({
      sourceDeviceId: 'device-a',
      sourceTarget: 'claude',
    });
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_list_remote_portable_inventory', {
      request: { sourceDeviceId: 'device-a', sourceTarget: 'claude' },
    });

    mockInvoke.mockResolvedValueOnce(validPullPlan);
    const pullRequest = {
      sourceDeviceId: 'device-a',
      sourceTarget: 'claude' as const,
      destinationTarget: 'claude' as const,
      remoteInventorySnapshotHash: 'remote-snap-1',
      inventoryItemIds: ['remote-skill-1'],
      conflictPolicy: 'skipExisting' as const,
    };
    await portablePullApi.preview(pullRequest);
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_preview_portable_pull', {
      request: pullRequest,
    });

    mockInvoke.mockResolvedValueOnce(validPullResult);
    await portablePullApi.apply({
      planToken: 'pull-plan-1',
      clientRequestId: 'pull-req-1',
    });
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_apply_portable_pull', {
      request: { planToken: 'pull-plan-1', clientRequestId: 'pull-req-1' },
    });

    mockInvoke.mockResolvedValueOnce(validPullResult);
    await portablePullApi.get('pull-req-1');
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_get_portable_pull', {
      clientRequestId: 'pull-req-1',
    });
  });

  test('fail-closed success body decode rejects malformed inventory', async () => {
    mockInvoke.mockResolvedValueOnce({ stale: false, items: [] });
    await expect(portableAssetApi.inspect()).rejects.toBeInstanceOf(ContractDecodeError);
  });

  test('preserves stable backend error codes on transport failures', async () => {
    mockInvoke.mockRejectedValueOnce(
      Object.assign(new Error('inventory is stale'), {
        code: 'PORTABLE_INVENTORY_STALE',
      }),
    );
    try {
      await portableAssetApi.inspect();
      expect.unreachable('should throw');
    } catch (err) {
      expect(err).toBeInstanceOf(Error);
      expect((err as Error & { code?: string }).code).toBe('PORTABLE_INVENTORY_STALE');
      expect((err as Error).message).toContain('stale');
    }

    mockInvoke.mockRejectedValueOnce(
      Object.assign(new Error('outcome unknown'), {
        code: 'PORTABLE_ACTION_OUTCOME_UNKNOWN',
      }),
    );
    try {
      await portableAssetApi.getAction('req-x');
      expect.unreachable('should throw');
    } catch (err) {
      expect((err as Error & { code?: string }).code).toBe('PORTABLE_ACTION_OUTCOME_UNKNOWN');
    }
  });

  test('source does not invent optional defaults and has no any', () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const src = readFileSync(join(here, 'portableInventory.ts'), 'utf8');
    expect(src).not.toMatch(/\bany\b/);
    expect(src).toContain('agent_hub_inspect_portable_inventory');
    expect(src).toContain('agent_hub_get_portable_pull');
  });
});
