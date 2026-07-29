/**
 * 目标矩阵 pure helper 表驱动测试。
 *
 * Business Logic: 锁定每个 Gate B 聚合态的 UI 语义与动作可用性。
 * Code Logic: 构造 summary + cell 断言 helpers。
 */

import { describe, expect, test } from 'vitest';
import type {
  AgentHubAssetSummary,
  AgentHubTargetCell,
  AssetAggregateStatus,
} from '@/lib/types/agentHub';
import {
  blockedReason,
  canToggleEnabled,
  hasExternalCollision,
  isDetachedCell,
  isSourceTarget,
  isVerifiedInvocation,
  listPartialReasons,
  needsActivation,
  resolveInvocationLabel,
} from './targetMatrix';

function makeCell(overrides: Partial<AgentHubTargetCell> = {}): AgentHubTargetCell {
  return {
    target: 'claude',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'synced',
    lastError: null,
    requested: true,
    supported: true,
    sourceOnly: false,
    verified: true,
    ...overrides,
  };
}

function makeAsset(
  aggregateStatus: AssetAggregateStatus,
  targets: AgentHubTargetCell[],
  extra: Partial<AgentHubAssetSummary> = {},
): AgentHubAssetSummary {
  return {
    assetId: 'asset-1',
    scopeId: 'user',
    kind: 'skill',
    displayName: 'Canonical Skill',
    logicalKey: 'user/skill/my-skill',
    originNamespace: 'claude',
    policy: 'shared',
    currentRevisionId: 'r1',
    targets,
    hasConflict: false,
    aggregateStatus,
    ...extra,
  };
}

describe('targetMatrix status table', () => {
  test('full -> verified invocation', () => {
    const asset = makeAsset('full', [
      makeCell({ target: 'claude', verified: true }),
      makeCell({ target: 'codex', verified: true }),
      makeCell({ target: 'opencode', verified: true }),
    ]);
    expect(isVerifiedInvocation(asset.aggregateStatus)).toBe(true);
    expect(listPartialReasons(asset)).toEqual([]);
  });

  test('partial -> missing/unequal components listed', () => {
    const asset = makeAsset('partial', [
      makeCell({ target: 'claude', verified: true }),
      makeCell({
        target: 'codex',
        verified: false,
        supported: false,
        materializationStatus: 'unsupported',
      }),
      makeCell({
        target: 'opencode',
        sourceOnly: true,
        verified: false,
        materializationStatus: null,
      }),
    ]);
    const reasons = listPartialReasons(asset);
    expect(reasons).toEqual(
      expect.arrayContaining([
        'codex:unsupported',
        'codex:not-verified',
        'opencode:sourceOnly',
        'opencode:not-verified',
      ]),
    );
  });

  test('sourceOnly -> source target shown, no install action elsewhere', () => {
    const asset = makeAsset(
      'sourceOnly',
      [
        makeCell({ target: 'claude', sourceOnly: true, verified: false }),
        makeCell({
          target: 'codex',
          desiredPresence: 'absent',
          desiredEnabled: false,
          sourceOnly: false,
          verified: false,
        }),
        makeCell({
          target: 'opencode',
          desiredPresence: 'absent',
          desiredEnabled: false,
          sourceOnly: false,
          verified: false,
        }),
      ],
      { originNamespace: 'claude' },
    );
    expect(isSourceTarget(asset, 'claude')).toBe(true);
    expect(isSourceTarget(asset, 'codex')).toBe(false);
    // sourceOnly cell 不允许 toggle install
    expect(canToggleEnabled(makeCell({ sourceOnly: true }))).toBe(false);
    expect(canToggleEnabled(makeCell({ target: 'codex', supported: true, sourceOnly: false }))).toBe(
      true,
    );
  });

  test('activationRequired -> manual activation instructions', () => {
    const cell = makeCell({
      target: 'codex',
      materializationStatus: 'activationRequired',
      verified: false,
    });
    const asset = makeAsset('activationRequired', [cell]);
    expect(needsActivation(asset, cell)).toBe(true);
  });

  test('externalCollision -> adoption/collision preview', () => {
    const cell = makeCell({
      materializationStatus: 'externalCollision',
      verified: false,
    });
    const asset = makeAsset('externalCollision', [cell]);
    expect(hasExternalCollision(asset, cell)).toBe(true);
  });

  test('detached -> restore/remove/everywhere choices', () => {
    const cell = makeCell({ materializationStatus: 'detached', verified: false });
    const asset = makeAsset('detached', [cell]);
    expect(isDetachedCell(asset, cell)).toBe(true);
  });

  test('blocked -> support/evidence reason', () => {
    const cell = makeCell({
      materializationStatus: 'blocked',
      lastError: 'support_blocked:scanOnly',
      verified: false,
    });
    const asset = makeAsset('blocked', [cell]);
    expect(blockedReason(asset, cell)).toBe('support_blocked:scanOnly');
  });

  test('canonical name is separate from invocation alias label', () => {
    const asset = makeAsset('full', [
      makeCell({ target: 'claude', invocationAlias: 'cc-partner__my-skill' }),
    ]);
    expect(asset.displayName).toBe('Canonical Skill');
    expect(resolveInvocationLabel(asset, 'claude')).toBe('cc-partner__my-skill');
  });
});
