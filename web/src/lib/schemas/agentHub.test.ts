/**
 * Agent Hub schema 合同测试。
 *
 * Business Logic: required status enum 非法时 fail-closed，且错误不含 payload。
 * Code Logic: 覆盖 status/snapshot/Gate B aggregate + cell 解码。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  agentHubSnapshotDecoder,
  agentHubStatusDecoder,
  agentHubAssetSummaryListDecoder,
  agentHubAssetSummaryDecoder,
  assetAggregateStatusDecoder,
  pluginPackageReportDecoder,
  pluginComponentOwnershipDecoder,
  openCodeBridgeStatusDecoder,
} from './agentHub';

const validStatus = {
  enabled: true,
  backgroundEnabled: false,
  agentHubApiVersion: 1,
  ownerInstanceId: 'owner-1',
  writeCompatible: true,
  probes: [
    {
      target: 'claude',
      executable: '/usr/local/bin/claude',
      version: '1.0.0',
      support: 'supported',
      configRoot: '/Users/me/.claude',
    },
  ],
  conflictCount: 0,
  blockedMaterializationCount: 0,
};

const validTargetCell = {
  target: 'claude' as const,
  desiredPresence: 'present' as const,
  desiredEnabled: true,
  materializationStatus: 'synced',
  lastError: null,
  requested: true,
  supported: true,
  sourceOnly: false,
  verified: true,
};

const validAsset = {
  assetId: 'a1',
  scopeId: 'user',
  kind: 'instruction',
  displayName: 'User CLAUDE.md',
  logicalKey: 'user/instruction',
  originNamespace: 'claude',
  policy: 'shared',
  currentRevisionId: 'r1',
  targets: [validTargetCell],
  hasConflict: false,
  aggregateStatus: 'full' as const,
};

const AGGREGATE_STATUSES = [
  'full',
  'partial',
  'sourceOnly',
  'activationRequired',
  'externalCollision',
  'detached',
  'blocked',
] as const;

describe('agentHub schemas', () => {
  test('decodes valid status', () => {
    expect(agentHubStatusDecoder.decode(validStatus)).toEqual(validStatus);
  });

  test('decodes snapshot', () => {
    const snapshot = { status: validStatus, assets: [validAsset] };
    expect(agentHubSnapshotDecoder.decode(snapshot)).toEqual(snapshot);
  });

  test('decodes asset list', () => {
    expect(agentHubAssetSummaryListDecoder.decode([validAsset])).toEqual([validAsset]);
  });

  test('decodes every Gate B aggregate status', () => {
    for (const status of AGGREGATE_STATUSES) {
      expect(assetAggregateStatusDecoder.decode(status)).toBe(status);
      const asset = { ...validAsset, aggregateStatus: status };
      expect(agentHubAssetSummaryDecoder.decode(asset).aggregateStatus).toBe(status);
    }
  });

  test('requires Gate B cell fields requested/supported/sourceOnly/verified', () => {
    const missing = {
      ...validAsset,
      targets: [
        {
          target: 'claude',
          desiredPresence: 'present',
          desiredEnabled: true,
          materializationStatus: 'synced',
        },
      ],
    };
    expect(() => agentHubAssetSummaryDecoder.decode(missing)).toThrow(ContractDecodeError);
  });

  test('rejects unknown aggregate status without serializing payload', () => {
    const bad = {
      ...validAsset,
      aggregateStatus: 'totally-unknown-aggregate',
      displayName: 'secret-name-must-not-leak',
    };
    try {
      agentHubAssetSummaryDecoder.decode(bad);
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      expect(err.message).not.toContain('totally-unknown-aggregate');
      expect(JSON.stringify(err)).not.toContain('secret-name-must-not-leak');
    }
  });

  test('unknown required status enum rejects without serializing payload', () => {
    const bad = structuredClone(validStatus);
    bad.probes[0].support = 'totally-unknown-support-token';
    try {
      agentHubStatusDecoder.decode(bad);
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      expect(err.message).not.toContain('totally-unknown-support-token');
      expect(JSON.stringify(err)).not.toContain('/Users/me/.claude');
      expect(err.path).toContain('probes');
    }
  });

  test('agentHubSnapshotDecoder rejects unknown required status enum', () => {
    const bad = {
      status: {
        ...validStatus,
        probes: [{ ...validStatus.probes[0], support: 'not-a-real-level' }],
      },
      assets: [validAsset],
    };
    expect(() => agentHubSnapshotDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  const validPluginReport = {
    packageAssetId: 'pkg-1',
    packageDisplayName: 'demo',
    sourceTarget: 'claude',
    destinationTarget: 'codex',
    aggregateStatus: 'partial',
    activationState: 'none',
    diagnostics: [],
    components: [
      {
        kind: 'skill',
        assetId: 'c1',
        displayName: 'Skill',
        canonicalRevisionId: 'rev-1',
        ownership: 'packageOwned',
        sourceTarget: 'claude',
        residualReason: null,
        targets: [
          {
            target: 'claude',
            status: 'verified',
            reasons: [],
            projectedPaths: ['a'],
            materializedAlias: 'alias',
          },
        ],
      },
    ],
    residuals: [
      {
        residualTarget: 'claude',
        residualKind: 'runtime',
        treeManifestHash: 'hash',
        included: true,
        reasons: [],
      },
    ],
    partialBlockers: ['Skill@codex:partial'],
    deletePreview: {
      packageAssetId: 'pkg-1',
      packageDisplayName: 'demo',
      components: [
        {
          assetId: 'c1',
          displayName: 'Skill',
          kind: 'skill',
          ownership: 'packageOwned',
          decision: 'tombstoneOwned',
        },
      ],
    },
  };

  test('decodes plugin package report with ownership residual and delete preview', () => {
    const decoded = pluginPackageReportDecoder.decode(validPluginReport);
    expect(decoded.components[0].ownership).toBe('packageOwned');
    expect(decoded.components[0].canonicalRevisionId).toBe('rev-1');
    expect(decoded.residuals[0].residualKind).toBe('runtime');
    expect(decoded.deletePreview?.components[0].decision).toBe('tombstoneOwned');
    expect(decoded.partialBlockers).toEqual(['Skill@codex:partial']);
  });

  test('rejects unknown ownership without serializing payload', () => {
    const bad = structuredClone(validPluginReport);
    bad.components[0].ownership = 'totally-unknown-ownership';
    bad.packageDisplayName = 'secret-plugin-name';
    try {
      pluginPackageReportDecoder.decode(bad);
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      expect(JSON.stringify(err)).not.toContain('secret-plugin-name');
      expect(JSON.stringify(err)).not.toContain('totally-unknown-ownership');
    }
  });

  test('openCode bridge status enum is strict', () => {
    expect(openCodeBridgeStatusDecoder.decode('previewRequired')).toBe('previewRequired');
    expect(() => openCodeBridgeStatusDecoder.decode('maybe')).toThrow(ContractDecodeError);
    expect(pluginComponentOwnershipDecoder.decode('shared')).toBe('shared');
  });
});
