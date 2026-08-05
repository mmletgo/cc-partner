import { describe, expect, test } from 'vitest';
import type {
  AgentTarget,
  UserInstructionSourceDto,
  UserInstructionTargetDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';
import {
  getEffectiveUserInstructionSource,
  getUserInstructionSummaryPresentation,
  getUserInstructionTargetPresentation,
} from './userInstructionPresentation';

/** 构造一个 target 事实快照，测试只覆盖 V2 展示归一化。 */
function targetFixture(
  target: AgentTarget,
  overrides: Partial<UserInstructionTargetDto> = {},
): UserInstructionTargetDto {
  return {
    target,
    cli: { installed: true, version: '1.0', configRoot: `/config/${target}` },
    sources: [],
    effectiveSourceId: null,
    managedTargetPath: `/managed/${target}.md`,
    managementMode: 'unmanaged',
    capability: {
      scan: 'readOnly',
      write: 'blocked',
      remove: 'blocked',
      activate: 'blocked',
      reasonCode: null,
      evidenceIds: [],
    },
    projection: {
      state: 'none',
      desiredRevisionId: null,
      appliedRevisionId: null,
      observedHash: null,
      lastErrorCode: null,
    },
    availableActions: [],
    ...overrides,
  };
}

/** 构造来源，显式标注 active/role，避免测试依赖路径猜测。 */
function sourceFixture(
  sourceId: string,
  path: string,
  role: UserInstructionSourceDto['role'],
  active: boolean,
): UserInstructionSourceDto {
  return {
    sourceId,
    path,
    role,
    active,
    exists: true,
    nonEmpty: true,
    hash: `hash-${sourceId}`,
    modifiedAt: null,
    ownership: 'external',
  };
}

/** 构造 workspace，用于验证 legacy 矛盾状态被归一为中性未纳管。 */
function workspaceFixture(targets: UserInstructionTargetDto[]): UserInstructionWorkspaceDto {
  return {
    scopeId: 'agent-hub-scope-user',
    setupState: 'readyToReview',
    healthState: 'healthy',
    canonical: null,
    targets,
    inventorySnapshotHash: 'inventory-1',
    refreshedAt: '2026-08-05T00:00:00.000Z',
  };
}

describe('user instruction presentation', () => {
  test('scan-only unmanaged targets never recreate contradictory legacy status pills', () => {
    const workspace = workspaceFixture([
      targetFixture('claude'),
      targetFixture('codex'),
      targetFixture('opencode'),
    ]);
    expect(getUserInstructionSummaryPresentation(workspace)).toEqual({
      key: 'readyToReview',
      managedCount: 0,
      actionCount: 0,
    });
    expect(workspace.targets.map(getUserInstructionTargetPresentation)).toMatchObject([
      { stateKey: 'unmanaged', capabilityKey: 'scanOnly' },
      { stateKey: 'unmanaged', capabilityKey: 'scanOnly' },
      { stateKey: 'unmanaged', capabilityKey: 'scanOnly' },
    ]);
  });

  test('Codex active override wins while the base file remains visible as shadowed', () => {
    const target = targetFixture('codex', {
      sources: [
        sourceFixture('base', '/Users/test/.codex/AGENTS.md', 'shadowed', false),
        sourceFixture('override', '/Users/test/.codex/AGENTS.override.md', 'override', true),
      ],
      effectiveSourceId: 'override',
    });
    expect(getEffectiveUserInstructionSource(target)?.path).toBe(
      '/Users/test/.codex/AGENTS.override.md',
    );
    expect(getUserInstructionTargetPresentation(target).shadowedSources).toHaveLength(1);
  });

  test('OpenCode fallback and disabled fallback are distinct explicit states', () => {
    const fallback = targetFixture('opencode', {
      sources: [sourceFixture('fallback', '/Users/test/.claude/CLAUDE.md', 'fallback', true)],
      effectiveSourceId: 'fallback',
    });
    const disabled = targetFixture('opencode', {
      capability: {
        ...targetFixture('opencode').capability,
        reasonCode: 'OPENCODE_FALLBACK_DISABLED',
      },
    });
    expect(getUserInstructionTargetPresentation(fallback).stateKey).toBe('fallback');
    expect(getUserInstructionTargetPresentation(disabled).stateKey).toBe('fallbackDisabled');
  });

  test('unmanaged blocked targets do not inflate configured action counts', () => {
    const target = targetFixture('claude', {
      projection: { ...targetFixture('claude').projection, state: 'blocked' },
    });
    expect(getUserInstructionSummaryPresentation(workspaceFixture([target])).actionCount).toBe(0);
  });
});
