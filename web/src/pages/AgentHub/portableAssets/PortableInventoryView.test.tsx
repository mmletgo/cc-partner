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
  refresh: 'Refresh',
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
  },
  sourceOrigin: {
    standalone: 'Standalone',
    pluginComponent: 'Plugin component',
    nativeConfig: 'Native config',
  },
  unmanagedRefreshHint: 'Refresh inventory to manage this asset.',
  groupInstalled: 'Installed here',
  groupBorrowed: 'Loaded at runtime from other agents',
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

  test('partitions borrowed items into a runtime group without uninstall buttons', () => {
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
          getRowActions: (row) =>
            row.inventoryItemId === borrowed.inventoryItemId ? [] : ['disable', 'uninstall'],
        })}
        labels={labels}
      />,
    );

    expect(screen.getByTestId('portable-inventory-group-installed')).toBeTruthy();
    expect(screen.getByTestId('portable-inventory-group-borrowed')).toBeTruthy();
    expect(screen.getByTestId('portable-row-borrowed-badge').textContent).toContain(
      'From Claude Code',
    );
    expect(screen.queryByTestId('portable-row-action-uninstall-grok-skill-from-claude')).toBeNull();
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
    expect(screen.queryByTestId('portable-row-action-uninstall-grok-skill-shared')).toBeNull();
  });
});
