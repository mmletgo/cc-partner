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
  listConfirmableCurrentVersionItems,
  listMaterializableEscapeLinkItems,
  listMigratableToStoreItems,
  matchesPortableInventoryItem,
  needsPortableEnsureManagedRefresh,
  groupPortableStoreCatalog,
  portableStoreAgentChipState,
  portableStoreAgentChipStates,
  matchesPortableStoreCatalogGroup,
  partitionPortableInventoryItems,
  portableBorrowedOwnerJumpTarget,
  portableBorrowedOwnerLabelKey,
  resolvePortablePrimaryAction,
  resolvePortableRowActions,
  samePortableItemIds,
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
    capabilities: {
      ...baseCapabilities,
      canEnable: false,
      canDisable: false,
      canUninstall: false,
      canMigrateToStore: true,
    },
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
      'claude-skill-project',
      'opencode-skill-warn',
    ]);
    expect(visible.some((item) => item.sourceOrigin === 'pluginComponent')).toBe(false);
  });

  test('covers all four kind tabs independently', () => {
    expect(filterPortableInventoryItems(catalog, filters({ kind: 'skill' })).map((i) => i.kind)).toEqual([
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
    ).toEqual([]);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', scope: 'project' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['claude-skill-project']);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', scope: 'user' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['claude-skill-alpha', 'opencode-skill-warn']);

    const managementCases: Array<[PortableInventoryManagementState, string]> = [
      ['hubManaged', 'claude-skill-alpha'],
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
    ).toEqual([]);

    expect(
      filterPortableInventoryItems(catalog, filters({ kind: 'skill', actualState: 'problem' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['opencode-skill-warn']);

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

  test('equipped hides unattached store catalog; store shows catalog and hides native-only', () => {
    const native = makeItem({
      inventoryItemId: 'claude-skill-native',
      kind: 'skill',
      nativeId: 'native-only',
    });
    const attached = makeItem({
      inventoryItemId: 'claude-skill-attached',
      kind: 'skill',
      nativeId: 'attached',
      ownedBy: 'portableStore',
      store: { storeId: 'skill:attached', storeAttached: true },
    });
    const available = makeItem({
      inventoryItemId: 'claude-skill-available',
      kind: 'skill',
      nativeId: 'available',
      ownedBy: 'portableStore',
      store: { storeId: 'skill:available', storeAttached: false },
    });
    const mcp = makeItem({
      inventoryItemId: 'claude-mcp-native',
      kind: 'mcp',
      nativeId: 'native-mcp',
    });

    const borrowedViaClaude = makeItem({
      inventoryItemId: 'grok-skill-via-claude',
      kind: 'skill',
      nativeId: 'via-claude',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      store: {
        storeId: 'skill:via-claude',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
    });

    expect(
      filterPortableInventoryItems(
        [native, attached, available],
        filters({ kind: 'skill', assetLane: 'equipped' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-native', 'claude-skill-attached']);

    expect(
      filterPortableInventoryItems(
        [native, attached, available, borrowedViaClaude],
        filters({ kind: 'skill', target: 'grok', assetLane: 'equipped' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['grok-skill-via-claude']);

    expect(
      filterPortableInventoryItems(
        [native, attached, available],
        filters({ kind: 'skill', assetLane: 'store' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-attached', 'claude-skill-available']);

    expect(
      filterPortableInventoryItems([mcp], filters({ kind: 'mcp', assetLane: 'store' })).map(
        (item) => item.inventoryItemId,
      ),
    ).toEqual(['claude-mcp-native']);

    const disabledNative = makeItem({
      inventoryItemId: 'claude-skill-disabled',
      kind: 'skill',
      nativeId: 'disabled-native',
      actualEnabled: false,
    });
    expect(
      filterPortableInventoryItems(
        [native, disabledNative, attached],
        filters({ kind: 'skill', assetLane: 'equipped' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-native', 'claude-skill-attached']);

    expect(
      filterPortableInventoryItems(
        [attached, available, borrowedViaClaude],
        filters({ kind: 'skill', target: 'claude', assetLane: 'store' }),
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-attached', 'claude-skill-available', 'grok-skill-via-claude']);
  });

  test('store catalog groups one row per skill and derives Grok from the source agent', () => {
    const attachCaps = {
      ...baseCapabilities,
      canEnable: false,
      canDisable: false,
      canUninstall: false,
      canAttach: true,
      canDetach: true,
    };
    const claudeAttached = makeItem({
      inventoryItemId: 'claude-skill-shared',
      kind: 'skill',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      target: 'claude',
      ownedBy: 'portableStore',
      actualEnabled: true,
      capabilities: attachCaps,
      store: { storeId: 'skill:shared', storeAttached: true },
    });
    const grokViaClaude = makeItem({
      inventoryItemId: 'grok-skill-shared',
      kind: 'skill',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      actualEnabled: true,
      capabilities: { ...attachCaps, canAttach: false, canDetach: true },
      store: {
        storeId: 'skill:shared',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
    });
    const codexAvailable = makeItem({
      inventoryItemId: 'codex-skill-shared',
      kind: 'skill',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      target: 'codex',
      ownedBy: 'portableStore',
      actualEnabled: false,
      capabilities: attachCaps,
      store: { storeId: 'skill:shared', storeAttached: false },
    });
    const groups = groupPortableStoreCatalog([claudeAttached, grokViaClaude, codexAvailable]);
    expect(groups).toHaveLength(1);
    expect(groups[0]?.key).toBe('skill:shared');
    expect(groups[0]?.representative.inventoryItemId).toBe('claude-skill-shared');

    const claudeChip = portableStoreAgentChipState(groups[0]!, 'claude');
    const grokChip = portableStoreAgentChipState(groups[0]!, 'grok');
    const codexChip = portableStoreAgentChipState(groups[0]!, 'codex');
    expect(claudeChip).toMatchObject({
      enabled: true,
      derived: false,
      canToggle: true,
      item: expect.objectContaining({ inventoryItemId: 'claude-skill-shared' }),
    });
    expect(grokChip).toMatchObject({
      enabled: true,
      derived: true,
      derivedFrom: 'claude',
      canToggle: false,
    });
    expect(codexChip).toMatchObject({
      enabled: false,
      derived: false,
      canToggle: true,
      item: expect.objectContaining({ inventoryItemId: 'codex-skill-shared' }),
    });

    const afterClaudeDetached = groupPortableStoreCatalog([
      {
        ...claudeAttached,
        actualEnabled: false,
        store: { storeId: 'skill:shared', storeAttached: false },
        capabilities: attachCaps,
      },
      {
        ...grokViaClaude,
        actualEnabled: false,
        store: {
          storeId: 'skill:shared',
          storeAttached: false,
          loadedViaOtherPath: false,
          loadedViaTarget: 'claude',
        },
      },
      codexAvailable,
    ]);
    expect(portableStoreAgentChipState(afterClaudeDetached[0]!, 'claude').enabled).toBe(false);
    expect(portableStoreAgentChipState(afterClaudeDetached[0]!, 'grok').enabled).toBe(false);

    expect(
      matchesPortableStoreCatalogGroup(groups[0]!, filters({ kind: 'skill', assetLane: 'store' })),
    ).toBe(true);
    expect(portableStoreAgentChipStates(groups[0]!).map((chip) => chip.target)).toEqual(
      expect.arrayContaining(['claude', 'codex', 'grok']),
    );
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

  test('Codex cache plugin missing from config is disabled, not a health problem', () => {
    const cacheOnly = makeItem({
      inventoryItemId: 'codex-plugin-product-design',
      kind: 'plugin',
      nativeId: 'product-design@openai-curated-remote',
      displayName: 'product-design',
      target: 'codex',
      actualEnabled: false,
      warnings: ['codex_plugin_not_in_config'],
    });

    expect(isPortableInventoryProblem(cacheOnly)).toBe(false);
    expect(classifyPortableActualState(cacheOnly)).toBe('disabled');
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

  test('runtime load via another agent is not a health problem', () => {
    const borrowedViaOther = makeItem({
      inventoryItemId: 'grok-skill-via-claude',
      kind: 'skill',
      nativeId: 'via-claude',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      warnings: ['store_loaded_via_other_path', 'borrowed_runtime_origin'],
      store: {
        storeId: 'skill:via-claude',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
    });
    expect(isPortableInventoryProblem(borrowedViaOther)).toBe(false);
    expect(classifyPortableActualState(borrowedViaOther)).toBe('enabled');
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

    // 后端已 ensure_managed 后：unmanaged 残留但具备迁入仓库时直接走仓库
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
        canEnable: false,
        canDisable: false,
        canMigrateToStore: true,
      },
    });
    expect(needsPortableEnsureManagedRefresh(unmanagedAlreadyManaged)).toBe(false);
    expect(resolvePortablePrimaryAction(unmanagedAlreadyManaged, healthyCtx)).toBe(
      'migrateToStore',
    );
    expect(resolvePortablePrimaryAction(unmanagedAlreadyManaged, healthyCtx)).not.toBe('adopt');

    const enabled = catalog.find((i) => i.inventoryItemId === 'claude-skill-alpha')!;
    expect(resolvePortablePrimaryAction(enabled, healthyCtx)).toBe('migrateToStore');

    const disabled = makeItem({
      inventoryItemId: 'claude-skill-off',
      kind: 'skill',
      nativeId: 'off',
      actualEnabled: false,
      managementState: 'hubManaged',
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canMigrateToStore: true,
      },
    });
    expect(resolvePortablePrimaryAction(disabled, healthyCtx)).toBe('migrateToStore');
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

  test('escape-link items expose materialize even when unsupported', () => {
    const escaped = makeItem({
      inventoryItemId: 'claude-skill-escape',
      kind: 'skill',
      nativeId: 'huashu-design',
      managementState: 'unsupported',
      warnings: ['store_symlink_escape', 'source_blocked'],
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canMaterializeEscapeLink: true,
        reasonCode: 'source_blocked',
      },
    });
    expect(resolvePortablePrimaryAction(escaped, healthyCtx)).toBe('materializeEscapeLink');
    expect(resolvePortableRowActions(escaped, healthyCtx)).toEqual(['materializeEscapeLink']);
    expect(
      listMaterializableEscapeLinkItems([escaped], healthyCtx).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-escape']);
  });

  test('installToSourceTarget is available when capability allows and no enable/disable', () => {
    const item = makeItem({
      inventoryItemId: 'claude-mcp-install',
      kind: 'mcp',
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

  test('enabled plugin with canDisable+canUninstall exposes disable then uninstall', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-plugin-on',
      kind: 'plugin',
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

  test('disabled plugin with canEnable+canUninstall exposes enable then uninstall', () => {
    const disabled = makeItem({
      inventoryItemId: 'claude-plugin-off',
      kind: 'plugin',
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

  test('actualEnabled=null mcp with canInstallToSourceTarget+canUninstall exposes install then uninstall', () => {
    const nullState = makeItem({
      inventoryItemId: 'claude-mcp-null',
      kind: 'mcp',
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

  test('canUninstall=false omits uninstall from the plugin action list', () => {
    const enabled = makeItem({
      inventoryItemId: 'claude-plugin-nouninstall',
      kind: 'plugin',
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

  test('borrowed compatibility skill never exposes migrate, even if capability leaked', () => {
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
        canMigrateToStore: true,
      },
    });
    expect(isPortableBorrowedRuntimeItem(borrowed)).toBe(true);
    expect(resolvePortableRowActions(borrowed, healthyCtx)).toEqual([]);
    expect(resolvePortablePrimaryAction(borrowed, healthyCtx)).toBeNull();
  });

  test('shared ~/.agents skill without store exposes migrate, never disable or unload', () => {
    const agentsSkill = makeItem({
      inventoryItemId: 'codex-skill-agents-video',
      kind: 'skill',
      nativeId: 'web-video-presentation',
      target: 'codex',
      originKind: 'legacyStandalone',
      ownedBy: 'sharedAgents',
      loadedBy: 'codex',
      nativeOutputCandidate: false,
      sourcePath: '/Users/hans/.agents/skills/web-video-presentation',
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canMigrateToStore: true,
        canAttach: false,
        canDetach: false,
        canDestroyStore: false,
      },
    });
    expect(resolvePortableRowActions(agentsSkill, healthyCtx)).toEqual(['migrateToStore']);
    expect(resolvePortablePrimaryAction(agentsSkill, healthyCtx)).toBe('migrateToStore');
    expect(resolvePortableRowActions(agentsSkill, healthyCtx)).not.toContain('disable');
    expect(resolvePortableRowActions(agentsSkill, healthyCtx)).not.toContain('detach');
  });

  test('skill store capabilities expose migrate/attach/detach/destroy instead of enable/disable', () => {
    const storeSkill = makeItem({
      inventoryItemId: 'claude-skill-store',
      kind: 'skill',
      nativeId: 'store-skill',
      actualEnabled: true,
      ownedBy: 'portableStore',
      originKind: 'native',
      nativeOutputCandidate: true,
      store: { storeId: 'skill:store-skill', storeAttached: true },
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canMigrateToStore: false,
        canAttach: false,
        canDetach: true,
        canDestroyStore: true,
      },
    });
    expect(isPortableBorrowedRuntimeItem(storeSkill)).toBe(false);
    expect(resolvePortableRowActions(storeSkill, healthyCtx)).toEqual([
      'detach',
      'destroyStore',
    ]);
    expect(resolvePortablePrimaryAction(storeSkill, healthyCtx)).toBe('detach');
  });

  test('mcp store capabilities stay on native leaf enable/disable/uninstall', () => {
    const mcp = makeItem({
      inventoryItemId: 'claude-mcp-keep',
      kind: 'mcp',
      nativeId: 'good-api',
      actualEnabled: true,
      sourceOrigin: 'nativeConfig',
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canMigrateToStore: true,
        canAttach: true,
        canDetach: true,
        canDestroyStore: true,
      },
    });
    expect(resolvePortableRowActions(mcp, healthyCtx)).toEqual(['disable', 'uninstall']);
    expect(resolvePortablePrimaryAction(mcp, healthyCtx)).toBe('disable');
  });

  test('plugin store capabilities stay on viewing-agent enable flags', () => {
    const plugin = makeItem({
      inventoryItemId: 'claude-plugin-keep',
      kind: 'plugin',
      nativeId: 'superpowers',
      actualEnabled: true,
      capabilities: {
        ...baseCapabilities,
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canMigrateToStore: true,
        canAttach: true,
        canDetach: true,
        canDestroyStore: true,
      },
    });
    expect(resolvePortableRowActions(plugin, healthyCtx)).toEqual(['disable', 'uninstall']);
    expect(resolvePortablePrimaryAction(plugin, healthyCtx)).toBe('disable');
  });

  test('drifted item exposes confirm current version first', () => {
    const drifted = makeItem({
      inventoryItemId: 'claude-skill-updated',
      kind: 'skill',
      nativeId: 'updated-skill',
      managementState: 'drifted',
      capabilities: {
        ...baseCapabilities,
        canDisable: true,
        canUninstall: true,
        canConfirmCurrentVersion: true,
      },
    });
    expect(resolvePortablePrimaryAction(drifted, healthyCtx)).toBe('confirmCurrentVersion');
    expect(resolvePortableRowActions(drifted, healthyCtx)[0]).toBe('confirmCurrentVersion');
  });

  test('materialize escape link outranks confirm current version', () => {
    const escaped = makeItem({
      inventoryItemId: 'claude-skill-escape-drift',
      kind: 'skill',
      nativeId: 'updated-escape',
      managementState: 'drifted',
      capabilities: {
        ...baseCapabilities,
        canConfirmCurrentVersion: true,
        canMaterializeEscapeLink: true,
      },
    });
    expect(resolvePortablePrimaryAction(escaped, healthyCtx)).toBe('materializeEscapeLink');
    expect(resolvePortableRowActions(escaped, healthyCtx)).toEqual(['materializeEscapeLink']);
  });

  test('listConfirmableCurrentVersionItems takes snapshot drifted items and skips filters/components', () => {
    const driftedA = makeItem({
      inventoryItemId: 'claude-skill-a',
      kind: 'skill',
      nativeId: 'a',
      managementState: 'drifted',
      capabilities: { ...baseCapabilities, canConfirmCurrentVersion: true },
    });
    const driftedB = makeItem({
      inventoryItemId: 'claude-skill-b',
      kind: 'skill',
      nativeId: 'b',
      managementState: 'drifted',
      capabilities: { ...baseCapabilities, canConfirmCurrentVersion: true },
    });
    const consistent = makeItem({
      inventoryItemId: 'claude-skill-c',
      kind: 'skill',
      nativeId: 'c',
      managementState: 'hubManaged',
    });
    const component = makeItem({
      inventoryItemId: 'claude-skill-comp',
      kind: 'skill',
      nativeId: 'comp',
      sourceOrigin: 'pluginComponent',
      managementState: 'drifted',
      capabilities: { ...baseCapabilities, canConfirmCurrentVersion: true },
    });
    expect(
      listConfirmableCurrentVersionItems(
        [driftedA, consistent, driftedB, component],
        healthyCtx,
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-a', 'claude-skill-b']);
    expect(samePortableItemIds(['b', 'a'], ['a', 'b'])).toBe(true);
    expect(samePortableItemIds(['a'], ['a', 'b'])).toBe(false);
  });

  test('listMigratableToStoreItems takes snapshot native skill/command and skips filters/components/mcp', () => {
    const nativeA = makeItem({
      inventoryItemId: 'claude-skill-a',
      kind: 'skill',
      nativeId: 'a',
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    const nativeB = makeItem({
      inventoryItemId: 'claude-command-b',
      kind: 'command',
      nativeId: 'b',
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    const alreadyInStore = makeItem({
      inventoryItemId: 'claude-skill-store',
      kind: 'skill',
      nativeId: 'store',
      ownedBy: 'portableStore',
      store: { storeId: 'skill:store', storeAttached: true },
      capabilities: { ...baseCapabilities, canMigrateToStore: false, canDetach: true },
    });
    const component = makeItem({
      inventoryItemId: 'claude-skill-comp',
      kind: 'skill',
      nativeId: 'comp',
      sourceOrigin: 'pluginComponent',
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    const mcp = makeItem({
      inventoryItemId: 'claude-mcp-keep',
      kind: 'mcp',
      nativeId: 'good-api',
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    const grokBorrowed = makeItem({
      inventoryItemId: 'grok-skill-from-claude',
      kind: 'skill',
      nativeId: 'from-claude',
      target: 'grok',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    expect(
      listMigratableToStoreItems(
        [nativeA, alreadyInStore, nativeB, component, mcp, grokBorrowed],
        healthyCtx,
      ).map((item) => item.inventoryItemId),
    ).toEqual(['claude-skill-a', 'claude-command-b']);
  });

  test('store item loaded via other path is borrowed and offers detach, not attach', () => {
    const grokHint = makeItem({
      inventoryItemId: 'grok-skill-via-claude',
      kind: 'skill',
      nativeId: 'shared-skill',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      nativeOutputCandidate: false,
      store: {
        storeId: 'skill:shared-skill',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
      capabilities: {
        ...baseCapabilities,
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAttach: true,
        canDetach: true,
        canDestroyStore: true,
      },
    });
    expect(isPortableBorrowedRuntimeItem(grokHint)).toBe(true);
    expect(portableBorrowedOwnerLabelKey(grokHint)).toBe('claude');
    expect(portableBorrowedOwnerJumpTarget(grokHint)).toBe('claude');
    expect(resolvePortableRowActions(grokHint, healthyCtx)).toEqual(['detach']);
    expect(resolvePortablePrimaryAction(grokHint, healthyCtx)).toBe('detach');
  });
});

describe('portableInventoryPresentation ownership partition', () => {
  const healthyCtx: PortablePrimaryActionContext = {
    stale: false,
    mutationBlocked: false,
    lockedItemIds: new Set(),
  };

  test('native grok skill stays installed and can migrate into the store', () => {
    const grokNative = makeItem({
      inventoryItemId: 'grok-skill-native',
      kind: 'skill',
      nativeId: 'grok-native',
      target: 'grok',
      originKind: 'native',
      ownedBy: 'grok',
      loadedBy: 'grok',
      nativeOutputCandidate: true,
      capabilities: { ...baseCapabilities, canMigrateToStore: true },
    });
    expect(isPortableBorrowedRuntimeItem(grokNative)).toBe(false);
    expect(portableBorrowedOwnerLabelKey(grokNative)).toBe('grok');
    expect(resolvePortableRowActions(grokNative, healthyCtx)).toEqual(['migrateToStore']);
    expect(resolvePortablePrimaryAction(grokNative, healthyCtx)).toBe('migrateToStore');
  });

  test('same-agent drifted native item stays installed, not borrowed', () => {
    const drifted = makeItem({
      inventoryItemId: 'claude-skill-drifted',
      kind: 'skill',
      nativeId: 'drifted-skill',
      target: 'claude',
      originKind: 'native',
      ownedBy: 'claude',
      loadedBy: 'claude',
      nativeOutputCandidate: true,
      managementState: 'drifted',
    });
    expect(isPortableBorrowedRuntimeItem(drifted)).toBe(false);
    const { installed, borrowed } = partitionPortableInventoryItems([drifted]);
    expect(installed.map((item) => item.inventoryItemId)).toEqual(['claude-skill-drifted']);
    expect(borrowed).toEqual([]);
  });

  test('same-agent legacyStandalone drifted item stays installed', () => {
    const codexLegacy = makeItem({
      inventoryItemId: 'codex-skill-agents',
      kind: 'skill',
      nativeId: 'agents-skill',
      target: 'codex',
      originKind: 'legacyStandalone',
      ownedBy: 'codex',
      loadedBy: 'codex',
      nativeOutputCandidate: false,
      managementState: 'drifted',
    });
    expect(isPortableBorrowedRuntimeItem(codexLegacy)).toBe(false);
  });

  test('nativeOutputCandidate false does not by itself mean borrowed', () => {
    const blockedNative = makeItem({
      inventoryItemId: 'claude-skill-blocked',
      kind: 'skill',
      nativeId: 'blocked',
      originKind: 'native',
      ownedBy: 'claude',
      nativeOutputCandidate: false,
      managementState: 'unsupported',
    });
    expect(isPortableBorrowedRuntimeItem(blockedNative)).toBe(false);
  });

  test('cross-agent compatibility stays borrowed even when drifted', () => {
    const driftedBorrowed = makeItem({
      inventoryItemId: 'grok-skill-drifted-claude',
      kind: 'skill',
      nativeId: 'from-claude',
      target: 'grok',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
      managementState: 'drifted',
    });
    expect(isPortableBorrowedRuntimeItem(driftedBorrowed)).toBe(true);
  });

  test('partition splits installed vs borrowed by origin and owner, not nativeOutputCandidate', () => {
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
      inventoryItemId: 'codex-skill-legacy',
      kind: 'skill',
      nativeId: 'legacy',
      target: 'codex',
      originKind: 'legacyStandalone',
      ownedBy: 'codex',
      loadedBy: 'codex',
      nativeOutputCandidate: false,
    });
    const { installed: installedItems, borrowed } = partitionPortableInventoryItems([
      installed,
      fromClaude,
      shared,
      legacy,
    ]);
    expect(installedItems.map((item) => item.inventoryItemId)).toEqual([
      'grok-skill-own',
      'codex-skill-legacy',
    ]);
    expect(borrowed.map((item) => item.inventoryItemId)).toEqual([
      'grok-skill-from-claude',
      'grok-skill-shared',
    ]);
    expect(portableBorrowedOwnerLabelKey(fromClaude)).toBe('claude');
    expect(portableBorrowedOwnerLabelKey(shared)).toBe('sharedAgents');
    expect(isPortableBorrowedRuntimeItem(legacy)).toBe(false);
  });
});
