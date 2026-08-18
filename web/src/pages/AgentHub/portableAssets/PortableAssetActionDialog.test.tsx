// @vitest-environment jsdom
/**
 * PortableAssetActionDialog auto-preview / apply state-machine 合同。
 *
 * Business Logic: inspect→自动 preview→confirm→apply→rescan；用户不再点预览按钮。
 * Code Logic: pure props + user events；禁止 window.confirm。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type {
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableInventoryItemDto,
  PreviewPortableAssetActionRequest,
} from '@/lib/types/portableInventory';
import { PortableAssetActionDialog } from './PortableAssetActionDialog';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) => {
      if (opts && typeof opts === 'object') {
        const parts = Object.entries(opts)
          .filter(([k]) => k !== 'defaultValue')
          .map(([k, v]) => `${k}=${String(v)}`);
        return parts.length ? `${key}:${parts.join(',')}` : key;
      }
      return key;
    },
  }),
}));

const item: PortableInventoryItemDto = {
  inventoryItemId: 'claude-skill-skill-a',
  target: 'claude',
  kind: 'skill',
  nativeId: 'skill-a',
  displayName: 'Skill A',
  description: null,
  version: null,
  scopeId: 'user',
  scopeKind: 'user',
  projectId: null,
  projectOptedIn: true,
  sourcePath: '/tmp/claude/skills/skill-a',
  sourceOrigin: 'standalone',
  parentPluginInventoryItemId: null,
  actualEnabled: true,
  contentHash: 'hash-skill-a',
  treeHash: 'tree-skill-a',
  canonicalAssetId: 'canon-a',
  canonicalRevisionId: 'rev-a',
  managementState: 'hubManaged',
  desiredPresence: 'present',
  desiredEnabled: true,
  materializationStatus: 'verified',
  capabilities: {
    canEnable: true,
    canDisable: true,
    canUninstall: true,
    canAdopt: false,
    canInstallToSourceTarget: false,
    reasonCode: null,
    evidenceIds: [],
  },
    warnings: [],
    originKind: 'native',
    ownedBy: 'claude',
    loadedBy: 'claude',
    nativeOutputCandidate: true,
  };

function planFixture(
  overrides: Partial<PortableAssetActionPlanDto> = {},
): PortableAssetActionPlanDto {
  return {
    planToken: 'plan-token-1',
    expiresAt: '2026-08-07T12:15:00.000Z',
    inventorySnapshotHash: 'snap-hash-3x4',
    action: 'uninstall',
    keepData: false,
    conflictPolicy: 'skipExisting',
    changes: [
      {
        inventoryItemId: item.inventoryItemId,
        target: 'claude',
        kind: 'skill',
        path: item.sourcePath,
        operation: 'uninstall',
        expectedSourceHash: 'hash-skill-a',
        expectedTreeHash: 'tree-skill-a',
        expectedCanonicalRevisionId: 'rev-a',
        backupPolicy: 'recoverableBeforeDelete',
        createsOwnership: false,
        canonicalEffect: 'updateDesired',
        blockingReasons: [],
        warnings: [],
      },
    ],
    blockingReasons: [],
    ...overrides,
  };
}

function resultFixture(
  overrides: Partial<PortableAssetActionResultDto> = {},
): PortableAssetActionResultDto {
  return {
    planToken: 'plan-token-1',
    clientRequestId: 'req-1',
    items: [
      {
        inventoryItemId: item.inventoryItemId,
        state: 'succeeded',
        errorCode: null,
        message: null,
      },
    ],
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe('PortableAssetActionDialog state machine', () => {
  test('opening the dialog auto-previews and keepData change re-previews', () => {
    const onPreview = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={onPreview}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(onPreview).toHaveBeenCalledTimes(1);
    expect(screen.queryByTestId('portable-action-run-preview')).toBeNull();
    expect(screen.getByTestId('portable-action-confirm')).toBeTruthy();
    const first = onPreview.mock.calls[0][0] as PreviewPortableAssetActionRequest;
    expect(first).toEqual({
      inventorySnapshotHash: 'snap-hash-3x4',
      inventoryItemIds: [item.inventoryItemId],
      action: 'uninstall',
      keepData: false,
      conflictPolicy: 'skipExisting',
      expectedCanonicalRevisionId: 'rev-a',
    });

    fireEvent.click(screen.getByTestId('portable-action-keep-data'));
    expect(onPreview).toHaveBeenCalledTimes(2);
    const second = onPreview.mock.calls[1][0] as PreviewPortableAssetActionRequest;
    expect(second.keepData).toBe(true);
  });

  test('confirm passes planToken and clientRequestId', () => {
    const onConfirm = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture()}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-42"
        onPreview={() => undefined}
        onConfirm={onConfirm}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    fireEvent.click(screen.getByTestId('portable-action-confirm'));
    expect(onConfirm).toHaveBeenCalledWith('plan-token-1', 'req-42');
  });

  test('blocked plan disables confirm and surfaces blocking reasons', () => {
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture({
          blockingReasons: ['PORTABLE_INVENTORY_STALE'],
          changes: [
            {
              ...planFixture().changes[0],
              blockingReasons: ['PORTABLE_SOURCE_CHANGED'],
            },
          ],
        })}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-blocking').textContent).toContain(
      'PORTABLE_INVENTORY_STALE',
    );
    expect(
      (screen.getByTestId('portable-action-confirm') as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  test('partial item rows never render as full success', () => {
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="enable"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture({ action: 'enable' })}
        result={resultFixture({
          items: [
            {
              inventoryItemId: item.inventoryItemId,
              state: 'succeeded',
              errorCode: null,
              message: null,
            },
            {
              inventoryItemId: 'claude-skill-skill-b',
              state: 'failed',
              errorCode: 'PORTABLE_SOURCE_CHANGED',
              message: 'source changed',
            },
          ],
        })}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-result').getAttribute('data-outcome')).toBe(
      'partial',
    );
    expect(screen.getByTestId('portable-action-item-claude-skill-skill-a').getAttribute('data-state')).toBe(
      'succeeded',
    );
    expect(screen.getByTestId('portable-action-item-claude-skill-skill-b').getAttribute('data-state')).toBe(
      'failed',
    );
    expect(screen.queryByTestId('portable-action-full-success')).toBeNull();
  });

  test('outcomeUnknown exposes reconcile with same clientRequestId', () => {
    const onReconcile = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="disable"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture({ action: 'disable' })}
        result={resultFixture({
          clientRequestId: 'req-unknown',
          items: [
            {
              inventoryItemId: item.inventoryItemId,
              state: 'outcomeUnknown',
              errorCode: 'PORTABLE_ACTION_OUTCOME_UNKNOWN',
              message: 'timeout',
            },
          ],
        })}
        busy={false}
        error={null}
        clientRequestId="req-unknown"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={onReconcile}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-result').getAttribute('data-outcome')).toBe(
      'outcomeUnknown',
    );
    fireEvent.click(screen.getByTestId('portable-action-reconcile'));
    expect(onReconcile).toHaveBeenCalledWith('req-unknown');
  });

  test('busy applying prevents close handlers and disables close button', () => {
    const onClose = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture()}
        result={null}
        busy
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={onClose}
      />,
    );

    const closeBtn = screen.getByTestId('portable-action-close') as HTMLButtonElement;
    expect(closeBtn.disabled).toBe(true);
    fireEvent.click(closeBtn);
    expect(onClose).not.toHaveBeenCalled();
  });

  test('does not use window.confirm for destructive actions', () => {
    const confirmSpy = vi.spyOn(window, 'confirm');
    const onConfirm = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture()}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={onConfirm}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    fireEvent.click(screen.getByTestId('portable-action-confirm'));
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(onConfirm).toHaveBeenCalled();
  });

  test('error banner renders without clearing plan token context', () => {
    render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="enable"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture({ action: 'enable' })}
        result={null}
        busy={false}
        error="PORTABLE_INVENTORY_STALE"
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-error').textContent).toContain(
      'PORTABLE_INVENTORY_STALE',
    );
    expect(screen.getByTestId('portable-action-plan-token').textContent).toContain('plan-token-1');
  });

  test('mutationBlocked / stale disables auto-preview and confirm', () => {
    const onPreview = vi.fn();
    const onConfirm = vi.fn();
    const { rerender } = render(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId={null}
        mutationBlocked
        onPreview={onPreview}
        onConfirm={onConfirm}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.queryByTestId('portable-action-run-preview')).toBeNull();
    expect(onPreview).not.toHaveBeenCalled();
    expect(
      (screen.getByTestId('portable-action-confirm') as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(screen.getByTestId('portable-action-stale-banner')).toBeTruthy();

    rerender(
      <PortableAssetActionDialog
        open
        items={[item]}
        action="uninstall"
        inventorySnapshotHash="snap-hash-3x4"
        plan={planFixture()}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        stale
        onPreview={onPreview}
        onConfirm={onConfirm}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(
      (screen.getByTestId('portable-action-confirm') as HTMLButtonElement).disabled,
    ).toBe(true);
    fireEvent.click(screen.getByTestId('portable-action-confirm'));
    expect(onConfirm).not.toHaveBeenCalled();
  });

  test('borrowed item shows cross-agent impact before preview', () => {
    const borrowed: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'grok-skill-from-claude',
      target: 'grok',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
      capabilities: {
        ...item.capabilities,
        reasonCode: 'borrowed_runtime_origin',
      },
    };
    render(
      <PortableAssetActionDialog
        open
        items={[borrowed]}
        action="disable"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-borrowed-impact').textContent).toContain(
      'agentHub:portable.actionDialog.borrowedImpactDisable',
    );
    expect(screen.queryByTestId('portable-action-borrowed-impact')?.textContent).toContain(
      'owner=',
    );
  });

  test('borrowed plugin enablement copy does not claim owner flag rewrite', () => {
    const borrowed: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'grok-plugin-from-claude',
      target: 'grok',
      kind: 'plugin',
      nativeId: 'superpowers',
      displayName: 'superpowers',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
      capabilities: {
        ...item.capabilities,
        reasonCode: 'borrowed_runtime_origin',
      },
    };
    render(
      <PortableAssetActionDialog
        open
        items={[borrowed]}
        action="disable"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-borrowed-impact').textContent).toContain(
      'agentHub:portable.actionDialog.borrowedImpactDisablePlugin',
    );
    expect(screen.queryByTestId('portable-action-borrowed-impact')?.textContent).not.toContain(
      'borrowedImpactDisable:',
    );
  });

  test('store detach shows unlink copy and Grok-still-loaded-via-Claude hint', () => {
    const grokStore: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'grok-skill-store',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      nativeOutputCandidate: false,
      store: {
        storeId: 'skill:skill-a',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
    };
    render(
      <PortableAssetActionDialog
        open
        items={[grokStore]}
        action="detach"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={() => undefined}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-action-store-hint').textContent).toContain(
      'agentHub:portable.actionDialog.storeDetachHint',
    );
    expect(screen.getByTestId('portable-action-store-still-loaded').textContent).toContain(
      'storeStillLoadedVia',
    );
    expect(screen.queryByTestId('portable-action-borrowed-impact')).toBeNull();
  });

  test('batch confirm current version previews all item ids without canonical revision', () => {
    const second: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'claude-skill-skill-b',
      nativeId: 'skill-b',
      displayName: 'Skill B',
    };
    const onPreview = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item, second]}
        action="confirmCurrentVersion"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={onPreview}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(onPreview).toHaveBeenCalledTimes(1);
    const request = onPreview.mock.calls[0][0] as PreviewPortableAssetActionRequest;
    expect(request.inventoryItemIds).toEqual([item.inventoryItemId, second.inventoryItemId]);
    expect(request.expectedCanonicalRevisionId).toBeNull();
    expect(screen.getByTestId('portable-action-batch-summary').textContent).toContain('Skill B');
    expect(screen.getByTestId('portable-action-confirm-current-hint').textContent).toContain(
      'confirmAllCurrentVersionHint',
    );
  });

  test('batch migrate to store previews all item ids and shows the bulk hint', () => {
    const second: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'claude-skill-skill-b',
      nativeId: 'skill-b',
      displayName: 'Skill B',
    };
    const onPreview = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item, second]}
        action="migrateToStore"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={onPreview}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(onPreview).toHaveBeenCalledTimes(1);
    const request = onPreview.mock.calls[0][0] as PreviewPortableAssetActionRequest;
    expect(request.inventoryItemIds).toEqual([item.inventoryItemId, second.inventoryItemId]);
    expect(request.expectedCanonicalRevisionId).toBeNull();
    expect(screen.getByTestId('portable-action-batch-summary').textContent).toContain('Skill B');
    expect(screen.getByTestId('portable-action-store-hint').textContent).toContain(
      'migrateAllToStoreHint',
    );
  });

  test('batch materialize escape link shows the bulk repair hint', () => {
    const second: PortableInventoryItemDto = {
      ...item,
      inventoryItemId: 'claude-skill-skill-b',
      nativeId: 'skill-b',
      displayName: 'Skill B',
    };
    const onPreview = vi.fn();
    render(
      <PortableAssetActionDialog
        open
        items={[item, second]}
        action="materializeEscapeLink"
        inventorySnapshotHash="snap-hash-3x4"
        plan={null}
        result={null}
        busy={false}
        error={null}
        clientRequestId="req-1"
        onPreview={onPreview}
        onConfirm={() => undefined}
        onReconcile={() => undefined}
        onClose={() => undefined}
      />,
    );

    expect(onPreview).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('portable-action-materialize-escape-hint').textContent).toContain(
      'materializeAllEscapeLinksHint',
    );
  });
});
