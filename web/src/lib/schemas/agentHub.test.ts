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
});
