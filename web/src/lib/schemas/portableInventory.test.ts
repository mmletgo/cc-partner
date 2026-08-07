/**
 * Portable inventory / action / pull schema 合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   IPC 边界必须 fail-closed 拒绝损坏/混合版本 DTO；MCP secret 不得进入 typed 模型。
 *
 * Code Logic（这个测试做什么）:
 *   3×4 合法 fixture；拒绝 missing hash/target/kind/capability、非法枚举、
 *   非有限 size、畸形 item result；允许未知额外字段；断言 secret 不能出现在 MCP DTO 类型。
 */

import { describe, expect, expectTypeOf, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import type {
  PortableInventorySnapshotDto,
  PortableMcpCredentialFactDto,
  PortableAssetActionResultDto,
  PortablePullResultDto,
} from '../types/portableInventory';
import {
  portableInventorySnapshotDecoder,
  portableInventoryItemDecoder,
  portableInventoryTargetDecoder,
  portableMcpCredentialFactDecoder,
  portableAssetActionPlanDecoder,
  portableAssetActionResultDecoder,
  portableAssetActionItemResultDecoder,
  remotePortableInventoryDecoder,
  portablePullPlanDecoder,
  portablePullResultDecoder,
  portablePullItemResultDecoder,
} from './portableInventory';

const baseCapabilities = {
  canEnable: true,
  canDisable: true,
  canUninstall: true,
  canAdopt: false,
  canInstallToSourceTarget: true,
  reasonCode: null,
  evidenceIds: [] as string[],
};

function makeItem(
  target: 'claude' | 'codex' | 'opencode',
  kind: 'skill' | 'command' | 'plugin' | 'mcp',
  nativeId: string,
  extra: Record<string, unknown> = {},
) {
  return {
    inventoryItemId: `${target}-${kind}-${nativeId}`,
    target,
    kind,
    nativeId,
    displayName: nativeId,
    description: null,
    version: null,
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: `/tmp/${target}/${kind}/${nativeId}`,
    sourceOrigin: kind === 'mcp' ? 'nativeConfig' : 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: `hash-${nativeId}`,
    treeHash: null,
    canonicalAssetId: null,
    canonicalRevisionId: null,
    managementState: 'unmanaged',
    desiredPresence: null,
    desiredEnabled: null,
    materializationStatus: null,
    capabilities: { ...baseCapabilities },
    warnings: [] as string[],
    mcpCredential:
      kind === 'mcp'
        ? { present: true, hash: `cred-${nativeId}` }
        : undefined,
    ...extra,
  };
}

const validTargets = (
  ['claude', 'codex', 'opencode'] as const
).map((target) => ({
  target,
  installed: true,
  version: '1.0.0',
  executable: `/usr/bin/${target}`,
  configRoot: `/home/.${target}`,
  scanCapability: 'supported' as const,
  mutationCapability: 'supported' as const,
  reasonCode: null,
  evidenceIds: [] as string[],
}));

const KINDS = ['skill', 'command', 'plugin', 'mcp'] as const;

/** 合法 3×4 inventory fixture（三 target × 四 kind）。 */
export const validInventorySnapshot = {
  inventorySnapshotHash: 'snap-hash-3x4',
  refreshedAt: '2026-08-07T12:00:00.000Z',
  stale: false,
  targets: validTargets,
  items: (['claude', 'codex', 'opencode'] as const).flatMap((target) =>
    KINDS.map((kind) => makeItem(target, kind, `${kind}-a`)),
  ),
};

const validActionPlan = {
  planToken: 'plan-token-1',
  expiresAt: '2026-08-07T12:15:00.000Z',
  inventorySnapshotHash: 'snap-hash-3x4',
  action: 'enable' as const,
  keepData: false,
  conflictPolicy: 'skipExisting' as const,
  changes: [
    {
      inventoryItemId: 'claude-skill-skill-a',
      target: 'claude' as const,
      kind: 'skill' as const,
      path: '/tmp/claude/skills/skill-a',
      operation: 'enable' as const,
      expectedSourceHash: 'hash-skill-a',
      expectedTreeHash: null,
      expectedCanonicalRevisionId: null,
      backupPolicy: 'none' as const,
      createsOwnership: false,
      canonicalEffect: 'none' as const,
      blockingReasons: [] as string[],
      warnings: [] as string[],
    },
  ],
  blockingReasons: [] as string[],
};

const validActionResult = {
  planToken: 'plan-token-1',
  clientRequestId: 'req-1',
  items: [
    {
      inventoryItemId: 'claude-skill-skill-a',
      state: 'succeeded' as const,
      errorCode: null,
      message: null,
    },
  ],
};

const validRemoteInventory = {
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude' as const,
  inventorySnapshotHash: 'remote-snap-1',
  refreshedAt: '2026-08-07T12:00:00.000Z',
  stale: false,
  items: [
    {
      inventoryItemId: 'remote-skill-1',
      target: 'claude' as const,
      kind: 'skill' as const,
      nativeId: 'remote-skill',
      displayName: 'Remote Skill',
      description: null,
      version: '1.0.0',
      scopeId: 'user',
      projectId: null,
      projectOptedIn: true,
      sourceOrigin: 'standalone' as const,
      actualEnabled: true,
      contentHash: 'c1',
      treeHash: null,
      warnings: [] as string[],
    },
  ],
};

const validPullPlan = {
  planToken: 'pull-plan-1',
  expiresAt: '2026-08-07T12:15:00.000Z',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude' as const,
  destinationTarget: 'claude' as const,
  remoteInventorySnapshotHash: 'remote-snap-1',
  localInventorySnapshotHash: 'snap-hash-3x4',
  conflictPolicy: 'skipExisting' as const,
  selectionManifestHash: 'sel-1',
  credentialBearingCount: 0,
  hasCredentialBearingAssets: false,
  changes: [
    {
      inventoryItemId: 'remote-skill-1',
      kind: 'skill' as const,
      nativeId: 'remote-skill',
      displayName: 'Remote Skill',
      installMode: 'installToTarget' as const,
      conflict: false,
      legacyLossy: false,
      credentialBearing: false,
      blockingReasons: [] as string[],
      warnings: [] as string[],
    },
  ],
  blockingReasons: [] as string[],
};

const validPullResult = {
  planToken: 'pull-plan-1',
  clientRequestId: 'pull-req-1',
  sourceDeviceId: 'device-a',
  sourceTarget: 'claude' as const,
  destinationTarget: 'claude' as const,
  partial: false,
  items: [
    {
      inventoryItemId: 'remote-skill-1',
      state: 'succeeded' as const,
      installMode: 'installToTarget' as const,
      errorCode: null,
      message: null,
    },
  ],
};

describe('portable inventory schemas', () => {
  test('decodes valid 3×4 inventory fixture', () => {
    const decoded = portableInventorySnapshotDecoder.decode(validInventorySnapshot);
    expect(decoded.inventorySnapshotHash).toBe('snap-hash-3x4');
    expect(decoded.targets).toHaveLength(3);
    expect(decoded.items).toHaveLength(12);
    expect(decoded.items.filter((i) => i.kind === 'mcp')).toHaveLength(3);
    expect(
      decoded.items.find((i) => i.kind === 'mcp' && i.target === 'claude')?.mcpCredential,
    ).toEqual({ present: true, hash: 'cred-mcp-a' });
  });

  test('rejects missing inventorySnapshotHash', () => {
    const { inventorySnapshotHash: _drop, ...rest } = validInventorySnapshot;
    expect(() => portableInventorySnapshotDecoder.decode(rest)).toThrow(ContractDecodeError);
  });

  test('rejects missing target on item', () => {
    const bad = {
      ...validInventorySnapshot,
      items: [{ ...makeItem('claude', 'skill', 'x'), target: undefined }],
    };
    expect(() => portableInventorySnapshotDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('rejects missing kind on item', () => {
    const { kind: _k, ...noKind } = makeItem('claude', 'skill', 'x');
    expect(() => portableInventoryItemDecoder.decode(noKind)).toThrow(ContractDecodeError);
  });

  test('rejects missing scanCapability on target', () => {
    const { scanCapability: _s, ...noCap } = validTargets[0];
    expect(() => portableInventoryTargetDecoder.decode(noCap)).toThrow(ContractDecodeError);
  });

  test('rejects missing mutationCapability on target', () => {
    const { mutationCapability: _m, ...noCap } = validTargets[0];
    expect(() => portableInventoryTargetDecoder.decode(noCap)).toThrow(ContractDecodeError);
  });

  test('rejects invalid enums', () => {
    expect(() =>
      portableInventoryItemDecoder.decode({
        ...makeItem('claude', 'skill', 'x'),
        kind: 'instruction',
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portableInventoryItemDecoder.decode({
        ...makeItem('claude', 'skill', 'x'),
        managementState: 'managed',
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portableInventoryTargetDecoder.decode({
        ...validTargets[0],
        scanCapability: 'full',
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portableAssetActionPlanDecoder.decode({
        ...validActionPlan,
        action: 'delete',
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portablePullPlanDecoder.decode({
        ...validPullPlan,
        changes: [{ ...validPullPlan.changes[0], installMode: 'overwrite' }],
      }),
    ).toThrow(ContractDecodeError);
  });

  test('rejects non-finite size fields', () => {
    expect(() =>
      portablePullPlanDecoder.decode({
        ...validPullPlan,
        credentialBearingCount: Number.NaN,
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portablePullPlanDecoder.decode({
        ...validPullPlan,
        credentialBearingCount: Number.POSITIVE_INFINITY,
      }),
    ).toThrow(ContractDecodeError);
  });

  test('rejects malformed item result', () => {
    expect(() =>
      portableAssetActionItemResultDecoder.decode({
        inventoryItemId: 'x',
        // missing state
        errorCode: null,
        message: null,
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portableAssetActionItemResultDecoder.decode({
        inventoryItemId: 'x',
        state: 'ok',
        errorCode: null,
        message: null,
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      portablePullItemResultDecoder.decode({
        inventoryItemId: 'x',
        state: 'imported',
        installMode: null,
        errorCode: null,
        message: null,
      }),
    ).toThrow(ContractDecodeError);
  });

  test('allows unknown extra fields for forward compatibility', () => {
    const withExtra = {
      ...validInventorySnapshot,
      futureField: { nested: true },
      items: [
        {
          ...makeItem('claude', 'skill', 'extra'),
          experimentalBadge: 'beta',
        },
      ],
    };
    const decoded = portableInventorySnapshotDecoder.decode(withExtra);
    expect(decoded.inventorySnapshotHash).toBe('snap-hash-3x4');
    expect(decoded.items[0].nativeId).toBe('extra');
    expect('futureField' in decoded).toBe(false);
    expect('experimentalBadge' in decoded.items[0]).toBe(false);
  });

  test('MCP credential only exposes present/hash; secret-shaped extras are not typed', () => {
    const withSecret = {
      present: true,
      hash: 'abc123',
      secret: 'sk-live-leaked',
      token: 'should-not-type',
      apiKey: 'x',
    };
    const decoded = portableMcpCredentialFactDecoder.decode(withSecret);
    expect(decoded).toEqual({ present: true, hash: 'abc123' });
    expect('secret' in decoded).toBe(false);
    expect('token' in decoded).toBe(false);
    // 编译期：typed DTO 不得含 secret 字段
    expectTypeOf<PortableMcpCredentialFactDto>().toEqualTypeOf<{
      present: boolean;
      hash: string | null;
    }>();
    expectTypeOf(decoded).not.toHaveProperty('secret');
  });

  test('decodes action plan/result and pull plan/result', () => {
    expect(portableAssetActionPlanDecoder.decode(validActionPlan).planToken).toBe('plan-token-1');
    const actionResult = portableAssetActionResultDecoder.decode(
      validActionResult,
    ) as PortableAssetActionResultDto;
    expect(actionResult.items[0].state).toBe('succeeded');
    expect(remotePortableInventoryDecoder.decode(validRemoteInventory).sourceDeviceId).toBe(
      'device-a',
    );
    expect(portablePullPlanDecoder.decode(validPullPlan).selectionManifestHash).toBe('sel-1');
    const pullResult = portablePullResultDecoder.decode(validPullResult) as PortablePullResultDto;
    expect(pullResult.partial).toBe(false);
    expect(pullResult.items[0].state).toBe('succeeded');
  });

  test('rejects missing hash on snapshot without leaking payload', () => {
    try {
      portableInventorySnapshotDecoder.decode({
        ...validInventorySnapshot,
        inventorySnapshotHash: undefined,
        secretPayload: 'must-not-appear',
      });
      expect.unreachable('should throw');
    } catch (err) {
      expect(err).toBeInstanceOf(ContractDecodeError);
      const message = String(err);
      expect(message).not.toContain('must-not-appear');
      expect(message).not.toContain('secretPayload');
    }
  });

  test('snapshot type is PortableInventorySnapshotDto', () => {
    const decoded = portableInventorySnapshotDecoder.decode(validInventorySnapshot);
    expectTypeOf(decoded).toEqualTypeOf<PortableInventorySnapshotDto>();
  });
});
