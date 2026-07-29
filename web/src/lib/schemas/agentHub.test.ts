/**
 * Agent Hub schema 合同测试。
 *
 * Business Logic: required status enum 非法时 fail-closed，且错误不含 payload。
 * Code Logic: 覆盖 status/snapshot 解码与未知 support enum。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  agentHubSnapshotDecoder,
  agentHubStatusDecoder,
  agentHubAssetSummaryListDecoder,
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

const validAsset = {
  assetId: 'a1',
  scopeId: 'user',
  kind: 'instruction',
  displayName: 'User CLAUDE.md',
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
    },
  ],
  hasConflict: false,
};

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
