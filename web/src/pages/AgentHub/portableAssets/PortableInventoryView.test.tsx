// @vitest-environment jsdom
/**
 * PortableInventoryView pure view 测试。
 *
 * Business Logic: view 只消费 controller props；kind 由壳层 tab 驱动，本视图委托其它 filter。
 * Code Logic: 渲染 filters、stale banner、列表行；无 @/api import。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import { DEFAULT_PORTABLE_INVENTORY_FILTERS } from './portableInventoryPresentation';
import { PortableInventoryView, type PortableInventoryViewLabels } from './PortableInventoryView';
import type { UsePortableInventoryControllerResult } from './usePortableInventoryController';

afterEach(() => {
  cleanup();
});

const labels: PortableInventoryViewLabels = {
  title: 'Agent assets',
  subtitle: 'Observed inventory',
  loading: 'Loading inventory',
  empty: 'No assets',
  migrateAllToStore: 'Migrate all to store',
  confirmAllVersions: 'Confirm all versions',
  materializeAllEscapeLinks: 'Replace all escape symlinks',
  retry: 'Retry',
  staleBanner: 'Inventory is stale',
  searchPlaceholder: 'Search assets',
  filterActual: 'State',
  filterManagement: 'Management',
  actualFilter: {
    all: 'All states',
    enabled: 'Enabled',
    disabled: 'Disabled',
    problem: 'Problem',
  },
  managementFilter: {
    all: 'All consistency',
    hubManaged: 'Consistent',
    drifted: 'Drifted',
    externalCollision: 'Conflict',
    unsupported: 'Unsupported',
    unmanaged: 'Pending manage',
  },
  targets: { claude: 'Claude', codex: 'Codex', opencode: 'OpenCode' },
  kinds: { skill: 'Skill', command: 'Command', plugin: 'Plugin', mcp: 'MCP' },
  actual: {
    enabled: 'Enabled',
    disabled: 'Disabled',
    problem: 'Problem',
    unknown: 'Unknown',
  },
  management: {
    unmanaged: 'Pending manage',
    hubManaged: 'Consistent',
    drifted: 'Drifted',
    externalCollision: 'Conflict',
    unsupported: 'Unsupported',
  },
  scope: { user: 'User', project: 'Project', directory: 'Directory' },
  actions: {
    adopt: 'Refresh to manage',
    enable: 'Enable',
    disable: 'Disable',
    uninstall: 'Uninstall',
    installToSourceTarget: 'Install',
    attach: 'Attach',
    detach: 'Detach',
    destroyStore: 'Destroy store',
    migrateToStore: 'Migrate to store',
    confirmCurrentVersion: 'Confirm current version',
    materializeEscapeLink: 'Replace symlink with a copy',
  },
  sourceOrigin: {
    standalone: 'Standalone',
    pluginComponent: 'Plugin component',
    nativeConfig: 'Native config',
  },
  unmanagedRefreshHint: 'Refresh inventory to manage this asset.',
  groupInstalled: 'Installed here',
  groupBorrowed: 'Loaded at runtime from other agents',
  groupStoreAttached: 'Attached to this agent',
  groupStoreAvailable: 'Not attached',
  emptyRuntimeHint: 'Runtime still loads assets from other agents.',
  openInOwnerAgent: 'Open in owner Agent',
  borrowedFrom: {
    claude: 'From Claude Code',
    codex: 'From Codex',
    opencode: 'From OpenCode',
    grok: 'From Grok Build',
    gemini: 'From Gemini CLI',
    cursor: 'From Cursor CLI',
    pi: 'From Pi',
    sharedAgents: 'Shared ~/.agents',
    portableStore: 'Store',
    unknown: 'From unknown owner',
  },
};

function item(overrides: Partial<PortableInventoryItemDto> = {}): PortableInventoryItemDto {
  const target = overrides.target ?? 'claude';
  return {
    inventoryItemId: 'claude-skill-alpha',
    target,
    kind: 'skill',
    nativeId: 'alpha',
    displayName: 'Alpha Skill',
    description: null,
    version: null,
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: '/tmp/alpha',
    sourceOrigin: 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: 'hash-alpha',
    treeHash: null,
    canonicalAssetId: null,
    canonicalRevisionId: null,
    managementState: 'hubManaged',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'applied',
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
    ownedBy: target,
    loadedBy: target,
    nativeOutputCandidate: true,
    ...overrides,
  };
}

function controller(
  patch: Partial<UsePortableInventoryControllerResult> = {},
): UsePortableInventoryControllerResult {
  const alpha = item();
  return {
    snapshot: {
      inventorySnapshotHash: 'snap-1',
      refreshedAt: '2026-08-07T12:00:00.000Z',
      stale: false,
      targets: [],
      items: [alpha],
    },
    visibleItems: [alpha],
    kindCounts: { skill: 1, command: 0, plugin: 0, mcp: 0 },
    filters: { ...DEFAULT_PORTABLE_INVENTORY_FILTERS },
    setFilters: vi.fn(),
    loading: false,
    refreshing: false,
    stale: false,
    mutationBlocked: false,
    error: null,
    selectedItemId: null,
    selectItem: vi.fn(),
    lockedItemIds: new Set(),
    setItemLocked: vi.fn(),
    pendingAction: null,
    openAction: vi.fn(),
    confirmableCurrentVersionItems: [],
    openConfirmAllCurrentVersions: vi.fn(),
    migratableToStoreItems: [],
    openMigrateAllToStore: vi.fn(),
    materializableEscapeLinkItems: [],
    openMaterializeAllEscapeLinks: vi.fn(),
    clearPendingAction: vi.fn(),
    getPrimaryAction: () => 'disable',
    getRowActions: () => ['disable', 'uninstall'],
    refresh: vi.fn(async () => undefined),
    requestContext: { deviceId: null, projectRef: null },
    ...patch,
    inventoryQuery: patch.inventoryQuery ?? { target: 'claude', kind: 'skill' },
  };
}

describe('PortableInventoryView', () => {
  test('renders only search/state/management filters; Agent, kind and scope stay shell-owned', () => {
    const ctl = controller();
    render(<PortableInventoryView controller={ctl} labels={labels} />);

    expect(screen.getByTestId('portable-inventory-workspace')).toBeTruthy();
    expect(screen.queryByTestId('portable-kind-tab-skill')).toBeNull();
    expect(screen.queryByTestId('portable-kind-tab-command')).toBeNull();
    expect(screen.queryByTestId('portable-kind-tab-plugin')).toBeNull();
    expect(screen.queryByTestId('portable-kind-tab-mcp')).toBeNull();
    expect(screen.getByTestId('portable-inventory-row-claude-skill-alpha')).toBeTruthy();

    expect(screen.queryByTestId('portable-filter-target')).toBeNull();
    expect(screen.queryByTestId('portable-filter-scope')).toBeNull();
    // 筛选 select 必须挂主题 class，避免暗色主题下回落系统白底
    expect(screen.getByTestId('portable-filter-actual').className).toMatch(/filterSelect/);
    expect(screen.getByTestId('portable-filter-management').className).toMatch(
      /filterSelect/,
    );
    fireEvent.change(screen.getByTestId('portable-filter-actual'), {
      target: { value: 'problem' },
    });
    expect(ctl.setFilters).toHaveBeenCalledWith({ actualState: 'problem' });
  });

  test('migrate-all button sits before confirm-all and opens the batch action', () => {
    const nativeA = item({
      inventoryItemId: 'claude-skill-alpha',
      capabilities: {
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canMigrateToStore: true,
        reasonCode: null,
        evidenceIds: [],
      },
    });
    const openMigrateAllToStore = vi.fn();
    render(
      <PortableInventoryView
        controller={controller({
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [nativeA],
          },
          visibleItems: [nativeA],
          migratableToStoreItems: [nativeA],
          openMigrateAllToStore,
        })}
        labels={labels}
      />,
    );

    const migrateAll = screen.getByTestId('portable-inventory-migrate-all-to-store');
    const confirmAll = screen.getByTestId('portable-inventory-confirm-all-versions');
    expect(migrateAll.compareDocumentPosition(confirmAll) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect((migrateAll as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(migrateAll);
    expect(openMigrateAllToStore).toHaveBeenCalledTimes(1);
  });

  test('hides migrate-all on plugin and mcp inventory', () => {
    const { rerender } = render(
      <PortableInventoryView
        controller={controller({
          filters: { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, kind: 'plugin' },
          inventoryQuery: { target: 'claude', kind: 'plugin' },
        })}
        labels={labels}
      />,
    );
    expect(screen.queryByTestId('portable-inventory-migrate-all-to-store')).toBeNull();

    rerender(
      <PortableInventoryView
        controller={controller({
          filters: { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, kind: 'mcp' },
          inventoryQuery: { target: 'claude', kind: 'mcp' },
        })}
        labels={labels}
      />,
    );
    expect(screen.queryByTestId('portable-inventory-migrate-all-to-store')).toBeNull();
    expect(screen.getByTestId('portable-inventory-confirm-all-versions')).toBeTruthy();
  });

  test('shows restore-all escape links on equipped and hides it on store lane', () => {
    const escaped = item({
      inventoryItemId: 'claude-skill-escape',
      actualEnabled: false,
      managementState: 'unsupported',
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canMaterializeEscapeLink: true,
        reasonCode: 'source_blocked',
        evidenceIds: [],
      },
    });
    const { rerender } = render(
      <PortableInventoryView
        controller={controller({
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [escaped],
          },
          visibleItems: [escaped],
          materializableEscapeLinkItems: [escaped],
        })}
        labels={labels}
      />,
    );
    expect(screen.getByTestId('portable-inventory-materialize-all-escape-links')).toBeTruthy();

    rerender(
      <PortableInventoryView
        controller={controller({
          filters: { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, assetLane: 'store' },
          materializableEscapeLinkItems: [escaped],
        })}
        labels={labels}
      />,
    );
    expect(screen.queryByTestId('portable-inventory-materialize-all-escape-links')).toBeNull();
  });

  test('confirm-all button opens the batch action', () => {
    const drifted = item({
      inventoryItemId: 'claude-skill-alpha',
      managementState: 'drifted',
      capabilities: {
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canConfirmCurrentVersion: true,
        reasonCode: null,
        evidenceIds: [],
      },
    });
    const openConfirmAllCurrentVersions = vi.fn();
    render(
      <PortableInventoryView
        controller={controller({
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [drifted],
          },
          visibleItems: [drifted],
          confirmableCurrentVersionItems: [drifted],
          openConfirmAllCurrentVersions,
        })}
        labels={labels}
      />,
    );

    const confirmAll = screen.getByTestId('portable-inventory-confirm-all-versions');
    expect((confirmAll as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(confirmAll);
    expect(openConfirmAllCurrentVersions).toHaveBeenCalledTimes(1);
  });

  test('shows stale banner and empty state', () => {
    render(
      <PortableInventoryView
        controller={controller({
          stale: true,
          mutationBlocked: true,
          visibleItems: [],
        })}
        labels={labels}
      />,
    );
    expect(screen.getByTestId('portable-inventory-stale').textContent).toContain(
      'Inventory is stale',
    );
    expect(screen.getByTestId('portable-inventory-empty').textContent).toContain('No assets');
  });

  test('loading and hard error states', () => {
    const { rerender } = render(
      <PortableInventoryView
        controller={controller({
          loading: true,
          snapshot: null,
          visibleItems: [],
        })}
        labels={labels}
      />,
    );
    expect(screen.getByTestId('portable-inventory-loading')).toBeTruthy();

    rerender(
      <PortableInventoryView
        controller={controller({
          loading: false,
          snapshot: null,
          visibleItems: [],
          error: 'boom',
        })}
        labels={labels}
      />,
    );
    expect(screen.getByTestId('portable-inventory-error').textContent).toContain('boom');
  });

  test('partitions borrowed items into a runtime group with mutation buttons', () => {
    const installed = item();
    const borrowed = item({
      inventoryItemId: 'grok-skill-from-claude',
      target: 'grok',
      nativeId: 'from-claude',
      displayName: 'Borrowed Skill',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'grok',
      nativeOutputCandidate: false,
      capabilities: {
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        reasonCode: null,
        evidenceIds: [],
      },
    });
    render(
      <PortableInventoryView
        controller={controller({
          visibleItems: [installed, borrowed],
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [installed, borrowed],
          },
          getRowActions: () => ['disable', 'uninstall'],
        })}
        labels={labels}
      />,
    );

    expect(screen.getByTestId('portable-inventory-group-installed')).toBeTruthy();
    expect(screen.getByTestId('portable-inventory-group-borrowed')).toBeTruthy();
    expect(screen.getByTestId('portable-row-borrowed-badge').textContent).toContain(
      'From Claude Code',
    );
    expect(
      screen.getByTestId('portable-row-action-uninstall-grok-skill-from-claude'),
    ).toBeTruthy();
    expect(screen.queryByTestId('portable-inventory-empty')).toBeNull();
  });

  test('borrowed-only filter shows runtime group instead of empty installed copy', () => {
    const borrowed = item({
      inventoryItemId: 'grok-skill-shared',
      target: 'grok',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      ownedBy: 'sharedAgents',
      loadedBy: 'grok',
    });
    render(
      <PortableInventoryView
        controller={controller({
          visibleItems: [borrowed],
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [borrowed],
          },
          getRowActions: () => ['uninstall'],
        })}
        labels={labels}
      />,
    );

    expect(screen.queryByTestId('portable-inventory-empty')).toBeNull();
    expect(screen.queryByTestId('portable-inventory-group-installed')).toBeNull();
    expect(screen.getByTestId('portable-inventory-group-borrowed')).toBeTruthy();
    expect(screen.getByTestId('portable-inventory-runtime-hint').textContent).toContain(
      'Runtime still loads',
    );
    expect(screen.getByTestId('portable-row-borrowed-badge').textContent).toContain(
      'Shared ~/.agents',
    );
    expect(screen.getByTestId('portable-row-action-uninstall-grok-skill-shared')).toBeTruthy();
  });

  test('store lane lists each skill once with per-agent enable chips', () => {
    const attached = item({
      inventoryItemId: 'claude-skill-shared',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      ownedBy: 'portableStore',
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canAttach: true,
        canDetach: true,
        reasonCode: null,
        evidenceIds: [],
      },
      store: { storeId: 'skill:shared', storeAttached: true },
    });
    const grokViaClaude = item({
      inventoryItemId: 'grok-skill-shared',
      nativeId: 'shared',
      displayName: 'Shared Skill',
      target: 'grok',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      actualEnabled: true,
      store: {
        storeId: 'skill:shared',
        storeAttached: false,
        loadedViaOtherPath: true,
        loadedViaTarget: 'claude',
      },
    });
    const openAction = vi.fn();
    render(
      <PortableInventoryView
        controller={controller({
          visibleItems: [attached, grokViaClaude],
          filters: { ...DEFAULT_PORTABLE_INVENTORY_FILTERS, assetLane: 'store' },
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [attached, grokViaClaude],
          },
          getRowActions: () => ['attach', 'detach', 'destroyStore'],
          openAction,
        })}
        labels={labels}
      />,
    );

    expect(screen.getByTestId('portable-inventory-group-store-catalog')).toBeTruthy();
    expect(screen.queryByTestId('portable-inventory-group-store-attached')).toBeNull();
    expect(screen.queryByTestId('portable-inventory-group-store-available')).toBeNull();
    expect(screen.getAllByTestId('portable-store-agent-chips')).toHaveLength(1);
    expect(screen.getByTestId('portable-store-agent-chip-claude').getAttribute('data-enabled')).toBe(
      'true',
    );
    expect(screen.getByTestId('portable-store-agent-chip-grok').getAttribute('data-derived')).toBe(
      'true',
    );
    fireEvent.click(screen.getByTestId('portable-store-agent-chip-claude'));
    expect(openAction).toHaveBeenCalledWith('claude-skill-shared', 'detach');
    expect(screen.getByTestId('portable-row-action-destroyStore-claude-skill-shared')).toBeTruthy();
  });

  test('equipped rows hide destroyStore even if getRowActions still offers it', () => {
    render(
      <PortableInventoryView
        controller={controller({
          getRowActions: () => ['detach', 'destroyStore'],
        })}
        labels={labels}
      />,
    );

    expect(screen.getByTestId('portable-row-action-detach-claude-skill-alpha')).toBeTruthy();
    expect(screen.queryByTestId('portable-row-action-destroyStore-claude-skill-alpha')).toBeNull();
  });

  test('equipped nested store members render as one pack row with detach and destroy', () => {
    const usingSuperpowers = item({
      inventoryItemId: 'grok-skill-using-superpowers',
      target: 'grok',
      nativeId: 'using-superpowers',
      displayName: 'using-superpowers',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      sourcePath: '/home/.agents/skills/superpowers/using-superpowers',
      warnings: ['nested_skill_package'],
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canDetach: true,
        canDestroyStore: true,
        reasonCode: null,
        evidenceIds: [],
      },
      store: { storeId: 'skill:superpowers', storeAttached: true },
    });
    const brainstorming = item({
      inventoryItemId: 'grok-skill-brainstorming',
      target: 'grok',
      nativeId: 'brainstorming',
      displayName: 'brainstorming',
      ownedBy: 'portableStore',
      originKind: 'compatibility',
      sourcePath: '/home/.agents/skills/superpowers/brainstorming',
      warnings: ['nested_skill_package'],
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canDetach: true,
        canDestroyStore: true,
        reasonCode: null,
        evidenceIds: [],
      },
      store: { storeId: 'skill:superpowers', storeAttached: true },
    });
    render(
      <PortableInventoryView
        controller={controller({
          visibleItems: [usingSuperpowers, brainstorming],
          snapshot: {
            inventorySnapshotHash: 'snap-1',
            refreshedAt: '2026-08-07T12:00:00.000Z',
            stale: false,
            targets: [],
            items: [usingSuperpowers, brainstorming],
          },
          getRowActions: () => ['detach', 'destroyStore'],
        })}
        labels={labels}
      />,
    );

    expect(screen.getByText('superpowers')).toBeTruthy();
    expect(screen.queryByText('using-superpowers')).toBeNull();
    expect(screen.getByText('brainstorming · using-superpowers')).toBeTruthy();
    expect(
      screen.getByTestId('portable-row-action-detach-grok-skill-using-superpowers'),
    ).toBeTruthy();
    expect(
      screen.getByTestId('portable-row-action-destroyStore-grok-skill-using-superpowers'),
    ).toBeTruthy();
    expect(screen.queryByTestId('portable-inventory-row-grok-skill-brainstorming')).toBeNull();
  });
});
