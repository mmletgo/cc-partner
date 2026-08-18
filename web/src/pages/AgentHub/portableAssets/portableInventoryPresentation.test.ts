/**
 * Portable inventory pure filter / status presentation 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   列表筛选与问题分类必须纯函数、可复现，且不得把 Plugin component 计入 standalone 主列表。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖四类 kind tab、target/scope/actual/management/search、problem 分类、
 *   projectOptedIn 只读与 actualEnabled=null。
 */

import { describe, expect, test } from 'vitest';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
  PortableInventoryManagementState,
} from '@/lib/types/portableInventory';
import {
  classifyPortableActualState,
  countPortableItemsByKind,
  DEFAULT_PORTABLE_INVENTORY_FILTERS,
  filterPortableInventoryItems,
  isPortableBorrowedRuntimeItem,
  isPortableInventoryProblem,
  isPortableItemReadOnly,
  matchesPortableInventoryItem,
  needsPortableEnsureManagedRefresh,
  partitionPortableInventoryItems,
  portableBorrowedOwnerLabelKey,
  resolvePortablePrimaryAction,
  resolvePortableRowActions,
  type PortableInventoryFilters,
  type PortablePrimaryActionContext,
} from './portableInventoryPresentation';

const baseCapabilities = {
  canEnable: true,
  canDisable: true,
  canUninstall: true,
  canAdopt: false,
  canInstallToSourceTarget: false,
  reasonCode: null as string | null,
  evidenceIds: [] as string[],
};

function makeItem(
  overrides: Partial<PortableInventoryItemDto> &
    Pick<PortableInventoryItemDto, 'inventoryItemId' | 'kind' | 'nativeId'>,
): PortableInventoryItemDto {
  const target = overrides.target ?? 'claude';
  return {
    target,
    displayName: overrides.nativeId,
    description: null,
    version: null,
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: `/tmp/${overrides.nativeId}`,
    sourceOrigin: 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: `hash-${overrides.nativeId}`,
    treeHash: null,
    canonicalAssetId: null,
    canonicalRevisionId: null,
    managementState: 'hubManaged',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'applied',
    capabilities: { ...baseCapabilities },
    warnings: [],
    originKind: 'native',
    ownedBy: target,
    loadedBy: target,
    nativeOutputCandidate: true,
    ...overrides,
  };
}

const catalog: PortableInventoryItemDto[] = [
  makeItem({
    inventoryItemId: 'claude-skill-alpha',
    kind: 'skill',
    nativeId: 'alpha',
    displayName: 'Alpha Skill',
    target: 'claude',
    actualEnabled: true,
    managementState: 'hubManaged',
  }),
  makeItem({
    inventoryItemId: 'codex-skill-beta',
    kind: 'skill',
    nativeId: 'beta',
    displayName: 'Beta Skill',
    target: 'codex',
    actualEnabled: false,
    managementState: 'unmanaged',
    capabilities: { ...baseCapabilities, canAdopt: true, canEnable: false, canDisable: false },
  }),
  makeItem({
    inventoryItemId: 'claude-command-gamma',
    kind: 'command',
    nativeId: 'gamma',
    displayName: 'Gamma Command',
    actualEnabled: null,
    managementState: 'hubManaged',
    capabilities: {
      ...baseCapabilities,
      canEnable: false,
      canDisable: false,
      canUninstall: true,
    },
  }),
  makeItem({
    inventoryItemId: 'claude-plugin-delta',
    kind: 'plugin',
    nativeId: 'delta',
    displayName: 'Delta Plugin',
    actualEnabled: true,
    managementState: 'drifted',
  }),
  makeItem({
    inventoryItemId: 'claude-plugin-delta-skill',
    kind: 'skill',
    nativeId: 'delta-skill',
    displayName: 'Delta Nested Skill',
    sourceOrigin: 'pluginComponent',
    parentPluginInventoryItemId: 'claude-plugin-delta',
    actualEnabled: true,
    managementState: 'hubManaged',
  }),
  makeItem({
    inventoryItemId: 'claude-mcp-echo',
    kind: 'mcp',
    nativeId: 'echo',
    displayName: 'Echo MCP',
    sourceOrigin: 'nativeConfig',
    actualEnabled: true,
    managementState: 'externalCollision',
    warnings: ['PORTABLE_EXTERNAL_COLLISION'],
  }),
  makeItem({
    inventoryItemId: 'claude-skill-project',
    kind: 'skill',
    nativeId: 'project-skill',
    displayName: 'Project Skill',
    scopeKind: 'project',
    scopeId: 'project:demo',
    projectId: 'demo',
    projectOptedIn: false,
    actualEnabled: true,
    managementState: 'unmanaged',
    capabilities: { ...baseCapabilities, canAdopt: true },
  }),
  makeItem({
    inventoryItemId: 'opencode-skill-warn',
    kind: 'skill',
    nativeId: 'warn-skill',
    displayName: 'Warn Skill',
    target: 'opencode',
    actualEnabled: true,
    managementState: 'hubManaged',
    warnings: ['PATH_SHADOWED'],
  }),
  makeItem({
    inventoryItemId: 'claude-skill-unsupported',
    kind: 'skill',
    nativeId: 'unsupported-skill',
    displayName: 'Unsupported Skill',
    actualEnabled: false,
    managementState: 'unsupported',
    capabilities: {
      ...baseCapabilities,
      canEnable: false,
      canDisable: false,
      canUninstall: false,
      canAdopt: false,
    },
  }),
];

/** 跨 target 断言时显式 target:all；默认 DEFAULT 已是 claude 单 agent。 */
function filters(patch: Partial<PortableInventoryFilters> = {}): PortableInventoryFilters {
  return { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, target: 'all', ...patch };
}

describe('portableInventoryPresentation filters', () => {
  test('defaults to skill tab + claude target (single-agent workstation)', () => {
    expect(DEFAULT_PORTABLE_INVENTORY_FILTERS.kind).toBe('skill');
    expect(DEFAULT_PORTABLE_INVENTORY_FILTERS.target).toBe('claude');
  });

  test('defaults to skill tab and excludes plugin components from standalone list', () => {
    const visible = filterPortableInventoryItems(catalog, filters({ kind: 'skill' }));
    expect(visible.map((item) => item.inventoryItemId)).toEqual([
      'claude-skill-alpha',
      'codex-skill-beta',
      'claude-skill-project',
      'opencode-skill-warn',
      'claude-skill-unsupported',
    ]);
    expect(visible.some((item) => item.sourceOrigin === 'pluginComponent')).toBe(false);
  });

  test('covers all four kind tabs independently', () => {
    expect(filterPortableInventoryItems(catalog, filters({ kind: 'skill' })).map((i) => i.kind)).toEqual([
      'skill',
      'skill',
      'skill',
      'skill',
      'skill',
    ]);
    expect(filterPortableInventoryItems(catalog, filters({ kind: 'command' })).map((i) => i.kind)).toEqual([
      'command',
    ]);
    expect(filterPortableInventoryItems(catalog, filters({ kind: 'plugin' })).map((i) => i.kind)).toEqual([
      'plugin',
    ]);
    expect(filterPortableInventoryItems(catalog, filters({ kind: 'mcp' })).map((i) => i.kind)).toEqual([
      'mcp',
    ]);
  });

  test('kind counts exclude plugin components from standalone tallies', () => {
    expect(countPortableItemsByKind(catalog)).toEqual({
      skill: 5,
      command: 1,
      plugin: 1,
      mcp: 1,
    });
  });

  test('filters by target, scope, management and search', () => {
    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', target: 'codex' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['codex-skill-beta']);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', scope: 'project' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['claude-skill-project']);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', scope: 'user' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual([
      'claude-skill-alpha',
      'codex-skill-beta',
      'opencode-skill-warn',
      'claude-skill-unsupported',
    ]);

    const managementCases: Array<[PortableInventoryManagementState, string]> = [
      ['hubManaged', 'claude-skill-alpha'],
      ['unmanaged', 'codex-skill-beta'],
      ['unsupported', 'claude-skill-unsupported'],
      ['drifted', 'claude-plugin-delta'],
      ['externalCollision', 'claude-mcp-echo'],
    ];
    for (const [management, expectedId] of managementCases) {
      const kind =
        management === 'drifted' ? 'plugin' : management === 'externalCollision' ? 'mcp' : 'skill';
      expect(
        filterPortableInventoryItems(catalog, filters({ kind, management })).map(
          (item) => item.inventoryItemId,
        ),
      ).toContain(expectedId);
    }

    expect(
      filterPortableInventoryItems(
        catalog,
        filters({ kind: 'skill', search: '  ALPHA  ' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-alpha']);
  });

  test('actualState enabled/disabled/problem combinations and null actualEnabled', () => {
    expect(classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'claude-skill-alpha')!)).toBe(
      'enabled',
    );
    expect(classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'codex-skill-beta')!)).toBe(
      'disabled',
    );
    expect(
      classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'claude-command-gamma')!),
    ).toBe('unknown');
    expect(
      classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'claude-plugin-delta')!),
    ).toBe('problem');
    expect(
      classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'opencode-skill-warn')!),
    ).toBe('problem');
    expect(
      classifyPortableActualState(catalog.find((i) => i.inventoryItemId === 'claude-skill-unsupported')!),
    ).toBe('problem');

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', actualState: 'enabled' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['claude-skill-alpha', 'claude-skill-project']);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', actualState: 'disabled' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['codex-skill-beta']);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', actualState: 'problem' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['opencode-skill-warn', 'claude-skill-unsupported']);

    // actualEnabled=null is not enabled/disabled/problem unless other problem signals
    expect(
      matchesPortableInventoryItem(
        catalog.find((i) => i.inventoryItemId === 'claude-command-gamma')!,
        filters({ kind: 'command', actualState: 'enabled' }),
      ),
    ).toBe(false);
    expect(
      matchesPortableInventoryItem(
        catalog.find((i) => i.inventoryItemId === 'claude-command-gamma')!,
        filters({ kind: 'command', actualState: 'all' }),
      ),
    ).toBe(true);
  });

  test('problem classification covers management and warnings', () => {
    expect(isPortableInventoryProblem(catalog.find((i) => i.inventoryItemId === 'claude-skill-alpha')!)).toBe(
      false,
    );
    expect(isPortableInventoryProblem(catalog.find((i) => i.inventoryItemId === 'claude-plugin-delta')!)).toBe(
      true,
    );
    expect(isPortableInventoryProblem(catalog.find((i) => i.inventoryItemId === 'claude-mcp-echo')!)).toBe(
      true,
    );
    expect(isPortableInventoryProblem(catalog.find((i) => i.inventoryItemId === 'opencode-skill-warn')!)).toBe(
      true,
    );
  });

  test('portability notes do not mark a healthy local item as problem', () => {
    const healthyWithNotes = makeItem({
      inventoryItemId: 'claude-skill-portability-notes',
      kind: 'skill',
      nativeId: 'portability-notes',
      warnings: ['targetExecutable', 'unknownSourceField', 'absolutePath'],
    });

    expect(isPortableInventoryProblem(healthyWithNotes)).toBe(false);
    expect(classifyPortableActualState(healthyWithNotes)).toBe('enabled');
  });

  test('projectOptedIn=false is read-only and never exposes mutation primary action', () => {
    const projectItem = catalog.find((i) => i.inventoryItemId === 'claude-skill-project')!;
    expect(isPortableItemReadOnly(projectItem)).toBe(true);
    expect(
      resolvePortablePrimaryAction(projectItem, {
        stale: false,
        mutationBlocked: false,
        lockedItemIds: new Set(),
      }),
    ).toBeNull();
  });
});

describe('portableInventoryPresentation primary action', () => {
  const healthyCtx: PortablePrimaryActionContext = {
    stale: false,
    mutationBlocked: false,
    lockedItemIds: new Set(),
  };

  test('never returns adopt; historical unmanaged prefers refresh or enable/disable', () => {
    const unmanagedAdoptOnly = catalog.find((i) => i.inventoryItemId === 'codex-skill-beta')!;
    expect(unmanagedAdoptOnly.capabilities.canAdopt).toBe(true);
    expect(needsPortableEnsureManagedRefresh(unmanagedAdoptOnly)).toBe(true);
    expect(resolvePortablePrimaryAction(unmanagedAdoptOnly, healthyCtx)).not.toBe('adopt');
    expect(resolvePortablePrimaryAction(unmanagedAdoptOnly, healthyCtx)).toBeNull();

    // 后端已 ensure_managed 后：unmanaged 残留但具备 enable/disable 时直接启停
    const unmanagedAlreadyManaged = makeItem({
      inventoryItemId: 'codex-skill-managed-path',
      kind: 'skill',
      nativeId: 'managed-path',
      target: 'codex',
      actualEnabled: false,
      managementState: 'unmanaged',
      capabilities: {
        ...baseCapabilities,
        canAdopt: true,
        canEnable: true,
        canDisable: false,
      },
    });
    expect(needsPortableEnsureManagedRefresh(unmanagedAlreadyManaged)).toBe(false);
    expect(resolvePortablePrimaryAction(unmanagedAlreadyManaged, healthyCtx)).toBe('enable');
    expect(resolvePortablePrimaryAction(unmanagedAlreadyManaged, healthyCtx)).not.toBe('adopt');

    const enabled = catalog.find((i) => i.inventoryItemId === 'claude-skill-alpha')!;
    expect(resolvePortablePrimaryAction(enabled, healthyCtx)).toBe('disable');

    const disabled = makeItem({
      inventoryItemId: 'claude-skill-off',
      kind: 'skill',
      nativeId: 'off',
      actualEnabled: false,
      managementState: 'hubManaged',
      capabilities: { ...baseCapabilities, canEnable: true, canDisable: false },
    });
    expect(resolvePortablePrimaryAction(disabled, healthyCtx)).toBe('enable');
  });

  test('stale / mutationBlocked / unsupported / locked never expose mutation action', () => {
    const enabled = catalog.find((i) => i.inventoryItemId === 'claude-skill-alpha')!;
    expect(
      resolvePortablePrimaryAction(enabled, { ...healthyCtx, stale: true }),
    ).toBeNull();
    expect(
      resolvePortablePrimaryAction(enabled, { ...healthyCtx, mutationBlocked: true }),
    ).toBeNull();
    expect(
      resolvePortablePrimaryAction(enabled, {
        ...healthyCtx,
        lockedItemIds: new Set([enabled.inventoryItemId]),
      }),
    ).toBeNull();

    const unsupported = catalog.find((i) => i.inventoryItemId === 'claude-skill-unsupported')!;
    expect(resolvePortablePrimaryAction(unsupported, healthyCtx)).toBeNull();
  });

  test('installToSourceTarget is available when capability allows and no enable/disable', () => {
    const item = makeItem({
      inventoryItemId: 'claude-skill-install',
      kind: 'skill',
      nativeId: 'install-me',
      actualEnabled: null,
      managementState: 'hubManaged',
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canInstallToSourceTarget: true,
      },
    });
    expect(resolvePortablePrimaryAction(item, healthyCtx)).toBe(
      'installToSourceTarget' satisfies PortableAssetActionKind,
    );
  });
});

describe('portableInventoryPresentation row actions', () => {
  const healthyCtx: PortablePrimaryActionContext = {
    stale: false,
    mutationBlocked: false,
    lockedItemIds: new Set(),
  };

  test('enabled item with canDisable+canUninstall exposes disable then uninstall', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-skill-on',
      kind: 'skill',
      nativeId: 'on',
      actualEnabled: true,
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
      },
    });
    expect(resolvePortableRowActions(enabled, healthyCtx)).toEqual(['disable', 'uninstall']);
  });

  test('disabled item with canEnable+canUninstall exposes enable then uninstall', () => {
    const disabled = makeItem({
      inventoryItemId: 'claude-skill-off',
      kind: 'skill',
      nativeId: 'off',
      actualEnabled: false,
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
      },
    });
    expect(resolvePortableRowActions(disabled, healthyCtx)).toEqual(['enable', 'uninstall']);
  });

  test('actualEnabled=null with canInstallToSourceTarget+canUninstall exposes install then uninstall', () => {
    const nullState = makeItem({
      inventoryItemId: 'claude-skill-null',
      kind: 'skill',
      nativeId: 'null',
      actualEnabled: null,
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canUninstall: true,
        canInstallToSourceTarget: true,
      },
    });
    expect(resolvePortableRowActions(nullState, healthyCtx)).toEqual([
      'installToSourceTarget',
      'uninstall',
    ]);
  });

  test('stale context returns empty array', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-skill-stale',
      kind: 'skill',
      nativeId: 'stale',
      actualEnabled: true,
    });
    expect(resolvePortableRowActions(enabled, { ...healthyCtx, stale: true })).toEqual([]);
  });

  test('locked item returns empty array', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-skill-locked',
      kind: 'skill',
      nativeId: 'locked',
      actualEnabled: true,
    });
    expect(
      resolvePortableRowActions(enabled, {
        ...healthyCtx,
        lockedItemIds: new Set(['claude-skill-locked']),
      }),
    ).toEqual([]);
  });

  test('unsupported management state returns empty array', () => {
    const unsupported = makeItem({
      inventoryItemId: 'claude-skill-unsup',
      kind: 'skill',
      nativeId: 'unsup',
      actualEnabled: false,
      managementState: 'unsupported',
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canUninstall: false,
      },
    });
    expect(resolvePortableRowActions(unsupported, healthyCtx)).toEqual([]);
  });

  test('canUninstall=false omits uninstall from the action list', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-skill-nouninstall',
      kind: 'skill',
      nativeId: 'nouninstall',
      actualEnabled: true,
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: false,
      },
    });
    expect(resolvePortableRowActions(enabled, healthyCtx)).toEqual(['disable']);
  });

  test('borrowed item exposes row actions from owner capabilities', () => {
    const borrowed = makeItem({
      inventoryItemId: 'grok-skill-borrowed-claude',
      kind: 'skill',
      nativeId: 'borrowed-claude',
      target: 'grok',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
      },
    });
    expect(isPortableBorrowedRuntimeItem(borrowed)).toBe(true);
    expect(resolvePortableRowActions(borrowed, healthyCtx)).toEqual(['disable', 'uninstall']);
    expect(resolvePortablePrimaryAction(borrowed, healthyCtx)).toBe('disable');
  });
});

describe('portableInventoryPresentation ownership partition', () => {
  const healthyCtx: PortablePrimaryActionContext = {
    stale: false,
    mutationBlocked: false,
    lockedItemIds: new Set(),
  };

  test('native grok skill stays installed and keeps mutation actions', () => {
    const grokNative = makeItem({
      inventoryItemId: 'grok-skill-native',
      kind: 'skill',
      nativeId: 'grok-native',
      target: 'grok',
      originKind: 'native',
      ownedBy: 'grok',
      loadedBy: 'grok',
      nativeOutputCandidate: true,
    });
    expect(isPortableBorrowedRuntimeItem(grokNative)).toBe(false);
    expect(portableBorrowedOwnerLabelKey(grokNative)).toBe('grok');
    expect(resolvePortableRowActions(grokNative, healthyCtx)).toEqual(['disable', 'uninstall']);
  });

  test('partition splits installed vs borrowed by origin, owner and nativeOutputCandidate', () => {
    const installed = makeItem({
      inventoryItemId: 'grok-skill-own',
      kind: 'skill',
      nativeId: 'own',
      target: 'grok',
    });
    const fromClaude = makeItem({
      inventoryItemId: 'grok-skill-from-claude',
      kind: 'skill',
      nativeId: 'from-claude',
      target: 'grok',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
    });
    const shared = makeItem({
      inventoryItemId: 'grok-skill-shared',
      kind: 'skill',
      nativeId: 'shared',
      target: 'grok',
      originKind: 'native',
      ownedBy: 'sharedAgents',
      loadedBy: 'grok',
      nativeOutputCandidate: true,
    });
    const legacy = makeItem({
      inventoryItemId: 'grok-skill-legacy',
      kind: 'skill',
      nativeId: 'legacy',
      target: 'grok',
      originKind: 'legacyStandalone',
      ownedBy: 'unknown',
      loadedBy: 'grok',
      nativeOutputCandidate: true,
    });
    const { installed: installedItems, borrowed } = partitionPortableInventoryItems([
      installed,
      fromClaude,
      shared,
      legacy,
    ]);
    expect(installedItems.map((item) => item.inventoryItemId)).toEqual(['grok-skill-own']);
    expect(borrowed.map((item) => item.inventoryItemId)).toEqual([
      'grok-skill-from-claude',
      'grok-skill-shared',
      'grok-skill-legacy',
    ]);
    expect(portableBorrowedOwnerLabelKey(fromClaude)).toBe('claude');
    expect(portableBorrowedOwnerLabelKey(shared)).toBe('sharedAgents');
    expect(portableBorrowedOwnerLabelKey(legacy)).toBe('unknown');
  });
});
