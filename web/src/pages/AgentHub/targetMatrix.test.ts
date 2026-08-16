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
      makeCell({ target: 'grok', verified: true }),
      makeCell({ target: 'gemini', verified: true }),
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
    const claude = makeCell({ target: 'claude', sourceOnly: true, verified: false });
    const codex = makeCell({
      target: 'codex',
      desiredPresence: 'absent',
      desiredEnabled: false,
      sourceOnly: false,
      verified: false,
    });
    const opencode = makeCell({
      target: 'opencode',
      desiredPresence: 'absent',
      desiredEnabled: false,
      sourceOnly: false,
      verified: false,
    });
    const asset = makeAsset('sourceOnly', [claude, codex, opencode], {
      originNamespace: 'claude',
    });
    expect(isSourceTarget(asset, 'claude')).toBe(true);
    expect(isSourceTarget(asset, 'codex')).toBe(false);
    // sourceOnly cell 不允许 toggle；非源 target 在 sourceOnly 聚合下也不允许
    expect(canToggleEnabled(asset, 'claude', claude)).toBe(false);
    expect(canToggleEnabled(asset, 'codex', codex)).toBe(false);
    // full 聚合下 supported 非 sourceOnly cell 仍可 toggle
    const fullAsset = makeAsset('full', [
      makeCell({ target: 'claude' }),
      makeCell({ target: 'codex', supported: true, sourceOnly: false }),
    ]);
    expect(
      canToggleEnabled(
        fullAsset,
        'codex',
        makeCell({ target: 'codex', supported: true, sourceOnly: false }),
      ),
    ).toBe(true);
  });

  test('activationRequired -> only affected cell needs activation', () => {
    const activated = makeCell({
      target: 'codex',
      materializationStatus: 'activationRequired',
      verified: false,
    });
    const absent = makeCell({
      target: 'opencode',
      desiredPresence: 'absent',
      desiredEnabled: false,
      verified: false,
      materializationStatus: null,
    });
    const asset = makeAsset('activationRequired', [activated, absent]);
    expect(needsActivation(asset, activated)).toBe(true);
    expect(needsActivation(asset, absent)).toBe(false);
  });

  test('externalCollision -> only affected cell opens collision', () => {
    const collided = makeCell({
      target: 'claude',
      materializationStatus: 'externalCollision',
      verified: false,
    });
    const absent = makeCell({
      target: 'codex',
      desiredPresence: 'absent',
      desiredEnabled: false,
      verified: false,
      materializationStatus: null,
    });
    const asset = makeAsset('externalCollision', [collided, absent]);
    expect(hasExternalCollision(asset, collided)).toBe(true);
    expect(hasExternalCollision(asset, absent)).toBe(false);
  });

  test('detached -> only detached cell exposes restore/remove', () => {
    const detached = makeCell({
      target: 'claude',
      materializationStatus: 'detached',
      verified: false,
    });
    const absent = makeCell({
      target: 'codex',
      desiredPresence: 'absent',
      desiredEnabled: false,
      verified: false,
      materializationStatus: null,
    });
    const synced = makeCell({
      target: 'opencode',
      materializationStatus: 'synced',
      verified: true,
    });
    const asset = makeAsset('detached', [detached, absent, synced]);
    expect(isDetachedCell(asset, detached)).toBe(true);
    expect(isDetachedCell(asset, absent)).toBe(false);
    expect(isDetachedCell(asset, synced)).toBe(false);
  });

  test('blocked -> only affected cell shows reason', () => {
    const blocked = makeCell({
      target: 'claude',
      materializationStatus: 'blocked',
      lastError: 'support_blocked:scanOnly',
      verified: false,
    });
    const absent = makeCell({
      target: 'codex',
      desiredPresence: 'absent',
      desiredEnabled: false,
      verified: false,
      materializationStatus: null,
      lastError: null,
    });
    const asset = makeAsset('blocked', [blocked, absent]);
    expect(blockedReason(asset, blocked)).toBe('support_blocked:scanOnly');
    expect(blockedReason(asset, absent)).toBeNull();
  });

  test('canonical name is separate from invocation alias label', () => {
    const asset = makeAsset('full', [
      makeCell({ target: 'claude', invocationAlias: 'cc-partner__my-skill' }),
    ]);
    expect(asset.displayName).toBe('Canonical Skill');
    expect(resolveInvocationLabel(asset, 'claude')).toBe('cc-partner__my-skill');
  });
});
