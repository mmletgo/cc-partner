/**
 * AgentAssetRow 状态矩阵表驱动测试。
 *
 * Business Logic: 每个 Gate B 聚合态对应可见动作与文案。
 * Code Logic: 渲染行并断言 testid / 回调。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { AgentHubAssetSummary, AgentHubTargetCell, AssetAggregateStatus } from '@/lib/types/agentHub';
import { AgentAssetRow } from './AgentAssetRow';

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

afterEach(() => {
  cleanup();
});

function cell(
  target: 'claude' | 'codex' | 'opencode',
  overrides: Partial<AgentHubTargetCell> = {},
): AgentHubTargetCell {
  return {
    target,
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

function asset(
  aggregateStatus: AssetAggregateStatus,
  targets: AgentHubTargetCell[],
  extra: Partial<AgentHubAssetSummary> = {},
): AgentHubAssetSummary {
  return {
    assetId: 'row-1',
    scopeId: 'user',
    kind: 'skill',
    displayName: 'Canonical Skill',
    logicalKey: 'user/skill/my-skill',
    originNamespace: 'claude',
    policy: 'shared',
    currentRevisionId: 'r1',
    hasConflict: false,
    aggregateStatus,
    targets,
    ...extra,
  };
}

function renderRow(summary: AgentHubAssetSummary) {
  const onToggleTarget = vi.fn(
    (_asset: AgentHubAssetSummary, _target: 'claude' | 'codex' | 'opencode', _next: boolean) =>
      undefined,
  );
  const onRemoveTarget = vi.fn(
    (_asset: AgentHubAssetSummary, _target: 'claude' | 'codex' | 'opencode') => undefined,
  );
  const onRestoreTarget = vi.fn(
    (_asset: AgentHubAssetSummary, _target: 'claude' | 'codex' | 'opencode') => undefined,
  );
  const onOpenCollision = vi.fn(
    (_asset: AgentHubAssetSummary, _target: 'claude' | 'codex' | 'opencode') => undefined,
  );
  const onDeleteEverywhere = vi.fn((_asset: AgentHubAssetSummary) => undefined);
  render(
    <I18nextProvider i18n={i18n}>
      <AgentAssetRow
        asset={summary}
        onToggleTarget={onToggleTarget}
        onRemoveTarget={onRemoveTarget}
        onRestoreTarget={onRestoreTarget}
        onOpenCollision={onOpenCollision}
        onDeleteEverywhere={onDeleteEverywhere}
      />
    </I18nextProvider>,
  );
  return {
    onToggleTarget,
    onRemoveTarget,
    onRestoreTarget,
    onOpenCollision,
    onDeleteEverywhere,
  };
}

describe('AgentAssetRow matrix', () => {
  test('full -> verified invocation badge', () => {
    renderRow(
      asset('full', [
        cell('claude', { verified: true, invocationAlias: 'cc-partner__my-skill' }),
        cell('codex', { verified: true }),
        cell('opencode', { verified: true }),
      ]),
    );
    expect(screen.getByTestId('agent-asset-aggregate-row-1').textContent).toMatch(/full/i);
    expect(screen.getByTestId('agent-target-verified-claude')).toBeTruthy();
    expect(screen.getByTestId('agent-target-invocation-claude').textContent).toContain(
      'cc-partner__my-skill',
    );
    expect(screen.getByTestId('agent-asset-canonical-row-1').textContent).toContain(
      'Canonical Skill',
    );
  });

  test('partial -> lists missing/unequal components', () => {
    renderRow(
      asset('partial', [
        cell('claude', { verified: true }),
        cell('codex', {
          verified: false,
          supported: false,
          materializationStatus: 'unsupported',
        }),
        cell('opencode', {
          sourceOnly: true,
          verified: false,
          materializationStatus: null,
        }),
      ]),
    );
    const list = screen.getByTestId('agent-asset-partial-row-1');
    expect(list.textContent).toContain('codex:unsupported');
    expect(list.textContent).toContain('opencode:sourceOnly');
  });

  test('sourceOnly -> source target shown, no install action elsewhere', () => {
    renderRow(
      asset(
        'sourceOnly',
        [
          cell('claude', { sourceOnly: true, verified: false }),
          cell('codex', {
            desiredPresence: 'absent',
            desiredEnabled: false,
            sourceOnly: false,
            verified: false,
          }),
          cell('opencode', {
            desiredPresence: 'absent',
            desiredEnabled: false,
            sourceOnly: false,
            verified: false,
          }),
        ],
        { originNamespace: 'claude' },
      ),
    );
    expect(screen.getByTestId('agent-target-source-only-claude')).toBeTruthy();
    expect(screen.getByTestId('agent-target-no-install-row-1-codex')).toBeTruthy();
    expect(screen.queryByTestId('agent-target-toggle-row-1-codex')).toBeNull();
  });

  test('activationRequired -> only affected cell shows instructions', () => {
    renderRow(
      asset('activationRequired', [
        cell('claude', {
          materializationStatus: 'activationRequired',
          verified: false,
        }),
        cell('codex', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
        cell('opencode', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
      ]),
    );
    expect(screen.getByTestId('agent-target-activation-claude')).toBeTruthy();
    expect(screen.queryByTestId('agent-target-activation-codex')).toBeNull();
    expect(screen.queryByTestId('agent-target-activation-opencode')).toBeNull();
  });

  test('externalCollision -> only affected cell opens collision', () => {
    const { onOpenCollision } = renderRow(
      asset('externalCollision', [
        cell('claude', {
          materializationStatus: 'externalCollision',
          verified: false,
          lastError: 'collision',
        }),
        cell('codex', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
        cell('opencode', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
      ]),
    );
    fireEvent.click(screen.getByTestId('agent-target-collision-row-1-claude'));
    expect(onOpenCollision).toHaveBeenCalled();
    expect(screen.queryByTestId('agent-target-collision-row-1-codex')).toBeNull();
    expect(screen.queryByTestId('agent-target-collision-row-1-opencode')).toBeNull();
  });

  test('detached -> restore/remove only on detached cell; everywhere once at row', () => {
    const { onRestoreTarget, onRemoveTarget, onDeleteEverywhere } = renderRow(
      asset('detached', [
        cell('claude', { materializationStatus: 'detached', verified: false }),
        cell('codex', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
        cell('opencode', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
        }),
      ]),
    );
    fireEvent.click(screen.getByTestId('agent-target-restore-row-1-claude'));
    fireEvent.click(screen.getByTestId('agent-target-remove-row-1-claude'));
    fireEvent.click(screen.getByTestId('agent-asset-delete-everywhere-row-1'));
    expect(onRestoreTarget).toHaveBeenCalled();
    expect(onRemoveTarget).toHaveBeenCalled();
    expect(onDeleteEverywhere).toHaveBeenCalled();
    // 无关 target 不得出现 restore/remove（aggregate detached 不再波及）
    expect(screen.queryByTestId('agent-target-restore-row-1-codex')).toBeNull();
    expect(screen.queryByTestId('agent-target-remove-row-1-codex')).toBeNull();
    expect(screen.queryByTestId('agent-target-restore-row-1-opencode')).toBeNull();
    expect(screen.queryByTestId('agent-target-remove-row-1-opencode')).toBeNull();
  });

  test('blocked -> only affected cell shows support/evidence reason', () => {
    renderRow(
      asset('blocked', [
        cell('claude', {
          materializationStatus: 'blocked',
          verified: false,
          lastError: 'support_blocked:scanOnly',
        }),
        cell('codex', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
          lastError: null,
        }),
        cell('opencode', {
          desiredPresence: 'absent',
          desiredEnabled: false,
          verified: false,
          materializationStatus: null,
          lastError: null,
        }),
      ]),
    );
    expect(screen.getByTestId('agent-target-blocked-claude').textContent).toContain(
      'support_blocked:scanOnly',
    );
    expect(screen.queryByTestId('agent-target-blocked-codex')).toBeNull();
    expect(screen.queryByTestId('agent-target-blocked-opencode')).toBeNull();
  });

  test('enable/disable one target callback', () => {
    const { onToggleTarget } = renderRow(
      asset('full', [
        cell('claude', { desiredEnabled: true }),
        cell('codex', { desiredPresence: 'absent', desiredEnabled: false }),
        cell('opencode', { desiredPresence: 'absent', desiredEnabled: false }),
      ]),
    );
    fireEvent.click(screen.getByTestId('agent-target-toggle-row-1-claude'));
    expect(onToggleTarget).toHaveBeenCalled();
  });
});
