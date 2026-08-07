/**
 * portablePullPresentation pure helper tests.
 *
 * Business Logic（为什么需要这个测试）:
 *   Same-agent Pull UI 必须在本地正确筛选远端 inventory、表达 same-target 目标、
 *   映射 canonical-only、比较 skip/replace 差异，并只披露 credential boolean。
 *
 * Code Logic（这个测试做什么）:
 *   直接断言 pure function 行为；不挂 React / API。
 */

import { describe, expect, test } from 'vitest';
import type {
  PortablePullChangeDto,
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import {
  canConfirmPortablePull,
  credentialDisclosureFromPlan,
  filterRemotePortableItems,
  formatPullInstallModeLabelKey,
  mapCanonicalOnlyChanges,
  needsPullReconcile,
  portablePullItemResultTone,
  sameAgentDestinationLabelKey,
  selectVisibleRemoteItemIds,
  summarizeConflictPolicyDiff,
  summarizePullResultProgress,
  type PortablePullFilters,
} from './portablePullPresentation';

function remoteItem(
  overrides: Partial<RemotePortableInventoryItemDto> & Pick<RemotePortableInventoryItemDto, 'inventoryItemId'>,
): RemotePortableInventoryItemDto {
  return {
    inventoryItemId: overrides.inventoryItemId,
    target: overrides.target ?? 'claude',
    kind: overrides.kind ?? 'skill',
    nativeId: overrides.nativeId ?? overrides.inventoryItemId,
    displayName: overrides.displayName ?? overrides.inventoryItemId,
    description: overrides.description ?? null,
    version: overrides.version ?? null,
    scopeId: overrides.scopeId ?? 'user',
    projectId: overrides.projectId ?? null,
    projectOptedIn: overrides.projectOptedIn ?? true,
    sourceOrigin: overrides.sourceOrigin ?? 'standalone',
    // 允许显式 null（problem 态）；仅 undefined 时默认 true
    actualEnabled: overrides.actualEnabled === undefined ? true : overrides.actualEnabled,
    contentHash: overrides.contentHash ?? 'h1',
    treeHash: overrides.treeHash ?? null,
    warnings: overrides.warnings ?? [],
    mcpCredential: overrides.mcpCredential,
  };
}

const fourKindItems: RemotePortableInventoryItemDto[] = [
  remoteItem({ inventoryItemId: 's1', kind: 'skill', displayName: 'Alpha Skill' }),
  remoteItem({ inventoryItemId: 'c1', kind: 'command', displayName: 'Beta Command', actualEnabled: false }),
  remoteItem({
    inventoryItemId: 'p1',
    kind: 'plugin',
    displayName: 'Gamma Plugin',
    projectId: 'proj-1',
    scopeId: 'project:proj-1',
  }),
  remoteItem({
    inventoryItemId: 'm1',
    kind: 'mcp',
    displayName: 'Delta MCP',
    warnings: ['credential-check'],
    actualEnabled: null,
    mcpCredential: { present: true, hash: 'abc' },
  }),
];

describe('portablePullPresentation filters', () => {
  test('filters by kind, scope, actual state, search and all four kinds', () => {
    const base: PortablePullFilters = {
      kind: 'all',
      scope: 'all',
      actualState: 'all',
      search: '',
    };
    expect(filterRemotePortableItems(fourKindItems, base)).toHaveLength(4);

    expect(filterRemotePortableItems(fourKindItems, { ...base, kind: 'skill' }).map((i) => i.inventoryItemId)).toEqual([
      's1',
    ]);
    expect(filterRemotePortableItems(fourKindItems, { ...base, kind: 'command' }).map((i) => i.inventoryItemId)).toEqual([
      'c1',
    ]);
    expect(filterRemotePortableItems(fourKindItems, { ...base, kind: 'plugin' }).map((i) => i.inventoryItemId)).toEqual([
      'p1',
    ]);
    expect(filterRemotePortableItems(fourKindItems, { ...base, kind: 'mcp' }).map((i) => i.inventoryItemId)).toEqual([
      'm1',
    ]);

    expect(filterRemotePortableItems(fourKindItems, { ...base, scope: 'user' }).map((i) => i.inventoryItemId)).toEqual([
      's1',
      'c1',
      'm1',
    ]);
    expect(filterRemotePortableItems(fourKindItems, { ...base, scope: 'project' }).map((i) => i.inventoryItemId)).toEqual([
      'p1',
    ]);

    expect(
      filterRemotePortableItems(fourKindItems, { ...base, actualState: 'enabled' }).map((i) => i.inventoryItemId),
    ).toEqual(['s1', 'p1']);
    expect(
      filterRemotePortableItems(fourKindItems, { ...base, actualState: 'disabled' }).map((i) => i.inventoryItemId),
    ).toEqual(['c1']);
    expect(
      filterRemotePortableItems(fourKindItems, { ...base, actualState: 'problem' }).map((i) => i.inventoryItemId),
    ).toEqual(['m1']);

    expect(
      filterRemotePortableItems(fourKindItems, { ...base, search: 'delta' }).map((i) => i.inventoryItemId),
    ).toEqual(['m1']);
    expect(
      filterRemotePortableItems(fourKindItems, { ...base, search: 'proj-1' }).map((i) => i.inventoryItemId),
    ).toEqual(['p1']);
  });

  test('selectVisibleRemoteItemIds returns only currently visible ids', () => {
    const visible = filterRemotePortableItems(fourKindItems, {
      kind: 'skill',
      scope: 'all',
      actualState: 'all',
      search: '',
    });
    expect([...selectVisibleRemoteItemIds(visible)]).toEqual(['s1']);
  });
});

describe('portablePullPresentation labels and plan helpers', () => {
  test('same-target destination label key is fixed to source target', () => {
    expect(sameAgentDestinationLabelKey('claude')).toBe('agentHub:portablePull.destination.sameAsClaude');
    expect(sameAgentDestinationLabelKey('codex')).toBe('agentHub:portablePull.destination.sameAsCodex');
    expect(sameAgentDestinationLabelKey('opencode')).toBe('agentHub:portablePull.destination.sameAsOpenCode');
  });

  test('maps canonical-only install mode changes explicitly', () => {
    const changes: PortablePullChangeDto[] = [
      {
        inventoryItemId: 's1',
        kind: 'skill',
        nativeId: 's1',
        displayName: 'Skill',
        installMode: 'installToTarget',
        conflict: false,
        legacyLossy: false,
        credentialBearing: false,
        blockingReasons: [],
        warnings: [],
      },
      {
        inventoryItemId: 'p1',
        kind: 'plugin',
        nativeId: 'p1',
        displayName: 'Plugin',
        installMode: 'importedCanonicalOnly',
        conflict: false,
        legacyLossy: false,
        credentialBearing: false,
        blockingReasons: ['projectUnmapped'],
        warnings: [],
      },
    ];
    expect(mapCanonicalOnlyChanges(changes).map((c) => c.inventoryItemId)).toEqual(['p1']);
    expect(formatPullInstallModeLabelKey('importedCanonicalOnly')).toBe(
      'agentHub:portablePull.installMode.importedCanonicalOnly',
    );
  });

  test('summarizes skip vs replace conflict policy diffs', () => {
    const conflictChange: PortablePullChangeDto = {
      inventoryItemId: 's1',
      kind: 'skill',
      nativeId: 's1',
      displayName: 'Skill',
      installMode: 'skipExisting',
      conflict: true,
      legacyLossy: false,
      credentialBearing: false,
      blockingReasons: [],
      warnings: [],
    };
    expect(summarizeConflictPolicyDiff('skipExisting', [conflictChange])).toEqual({
      conflictCount: 1,
      skippedByPolicy: 1,
      replaceCandidates: 0,
    });
    expect(
      summarizeConflictPolicyDiff('replaceAfterPreview', [
        { ...conflictChange, installMode: 'installToTarget' },
      ]),
    ).toEqual({
      conflictCount: 1,
      skippedByPolicy: 0,
      replaceCandidates: 1,
    });
  });

  test('credential disclosure is boolean-only from plan flags', () => {
    const plan: PortablePullPlanDto = {
      planToken: 'p1',
      expiresAt: '2026-08-08T00:00:00.000Z',
      sourceDeviceId: 'd1',
      sourceTarget: 'claude',
      destinationTarget: 'claude',
      remoteInventorySnapshotHash: 'r1',
      localInventorySnapshotHash: 'l1',
      conflictPolicy: 'skipExisting',
      selectionManifestHash: 'sel',
      credentialBearingCount: 2,
      hasCredentialBearingAssets: true,
      changes: [],
      blockingReasons: [],
    };
    expect(credentialDisclosureFromPlan(plan)).toEqual({
      hasCredentialBearingAssets: true,
      credentialBearingCount: 2,
    });
  });

  test('progress summary and reconcile needs for partial/outcomeUnknown', () => {
    const result: PortablePullResultDto = {
      planToken: 'p1',
      clientRequestId: 'req-1',
      sourceDeviceId: 'd1',
      sourceTarget: 'claude',
      destinationTarget: 'claude',
      partial: true,
      items: [
        {
          inventoryItemId: 's1',
          state: 'succeeded',
          installMode: 'installToTarget',
          errorCode: null,
          message: null,
        },
        {
          inventoryItemId: 'm1',
          state: 'outcomeUnknown',
          installMode: null,
          errorCode: 'timeout',
          message: null,
        },
        {
          inventoryItemId: 'c1',
          state: 'importedCanonicalOnly',
          installMode: 'importedCanonicalOnly',
          errorCode: null,
          message: null,
        },
      ],
    };
    expect(summarizePullResultProgress(result)).toEqual({
      total: 3,
      succeeded: 1,
      skipped: 0,
      failed: 0,
      blocked: 0,
      importedCanonicalOnly: 1,
      outcomeUnknown: 1,
      partial: true,
    });
    expect(needsPullReconcile(result)).toBe(true);
    expect(portablePullItemResultTone('succeeded')).toBe('success');
    expect(portablePullItemResultTone('outcomeUnknown')).toBe('warn');
    expect(portablePullItemResultTone('failed')).toBe('danger');
  });

  test('canConfirmPortablePull blocks stale inventory or empty selection', () => {
    expect(
      canConfirmPortablePull({
        remoteInventory: {
          sourceDeviceId: 'd1',
          sourceTarget: 'claude',
          inventorySnapshotHash: 'r1',
          refreshedAt: '2026-08-08T00:00:00.000Z',
          stale: true,
          items: fourKindItems,
        },
        selectedItemIds: new Set(['s1']),
        plan: null,
        busy: false,
      }).ok,
    ).toBe(false);

    expect(
      canConfirmPortablePull({
        remoteInventory: {
          sourceDeviceId: 'd1',
          sourceTarget: 'claude',
          inventorySnapshotHash: 'r1',
          refreshedAt: '2026-08-08T00:00:00.000Z',
          stale: false,
          items: fourKindItems,
        },
        selectedItemIds: new Set(),
        plan: {
          planToken: 'p1',
          expiresAt: '2026-08-08T00:00:00.000Z',
          sourceDeviceId: 'd1',
          sourceTarget: 'claude',
          destinationTarget: 'claude',
          remoteInventorySnapshotHash: 'r1',
          localInventorySnapshotHash: 'l1',
          conflictPolicy: 'skipExisting',
          selectionManifestHash: 'sel',
          credentialBearingCount: 0,
          hasCredentialBearingAssets: false,
          changes: [],
          blockingReasons: [],
        },
        busy: false,
      }).ok,
    ).toBe(false);

    expect(
      canConfirmPortablePull({
        remoteInventory: {
          sourceDeviceId: 'd1',
          sourceTarget: 'claude',
          inventorySnapshotHash: 'r1',
          refreshedAt: '2026-08-08T00:00:00.000Z',
          stale: false,
          items: fourKindItems,
        },
        selectedItemIds: new Set(['s1']),
        plan: {
          planToken: 'p1',
          expiresAt: '2026-08-08T00:00:00.000Z',
          sourceDeviceId: 'd1',
          sourceTarget: 'claude',
          destinationTarget: 'claude',
          remoteInventorySnapshotHash: 'r1',
          localInventorySnapshotHash: 'l1',
          conflictPolicy: 'skipExisting',
          selectionManifestHash: 'sel',
          credentialBearingCount: 0,
          hasCredentialBearingAssets: false,
          changes: [],
          blockingReasons: [],
        },
        busy: false,
      }).ok,
    ).toBe(true);
  });
});
