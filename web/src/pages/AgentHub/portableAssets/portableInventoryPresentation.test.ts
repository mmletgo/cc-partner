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
  isPortableInventoryProblem,
  isPortableItemReadOnly,
  matchesPortableInventoryItem,
  resolvePortablePrimaryAction,
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
  return {
    target: 'claude',
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

function filters(patch: Partial<PortableInventoryFilters> = {}): PortableInventoryFilters {
  return { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, ...patch };
}

describe('portableInventoryPresentation filters', () => {
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

  test('prefers adopt, then enable/disable based on actualEnabled', () => {
    const unmanaged = catalog.find((i) => i.inventoryItemId === 'codex-skill-beta')!;
    expect(resolvePortablePrimaryAction(unmanaged, healthyCtx)).toBe('adopt');

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
