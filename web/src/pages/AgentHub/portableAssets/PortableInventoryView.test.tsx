// @vitest-environment jsdom
/**
 * PortableInventoryView pure view 测试。
 *
 * Business Logic: view 只消费 controller props，切换 kind/filter 委托 setFilters。
 * Code Logic: 渲染 kind tabs、stale banner、列表行；无 @/api import。
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
  filterTarget: 'Target',
  filterScope: 'Scope',
  filterActual: 'State',
  filterManagement: 'Management',
  kindCounts: {
    skill: 'Skill',
    command: 'Command',
    plugin: 'Plugin',
    mcp: 'MCP',
  },
  targetFilter: {
    all: 'All targets',
    claude: 'Claude',
    codex: 'Codex',
    opencode: 'OpenCode',
  },
  scopeFilter: {
    all: 'All scopes',
    user: 'User',
    project: 'Project',
  },
  actualFilter: {
    all: 'All states',
    enabled: 'Enabled',
    disabled: 'Disabled',
    problem: 'Problem',
  },
  managementFilter: {
    all: 'All management',
    unmanaged: 'External',
    hubManaged: 'Hub',
    drifted: 'Drifted',
    externalCollision: 'Collision',
    unsupported: 'Unsupported',
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
    unmanaged: 'External',
    hubManaged: 'Hub',
    drifted: 'Drifted',
    externalCollision: 'Collision',
    unsupported: 'Unsupported',
  },
  scope: { user: 'User', project: 'Project', directory: 'Directory' },
  actions: {
    adopt: 'Adopt',
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
};

function item(): PortableInventoryItemDto {
  return {
    inventoryItemId: 'claude-skill-alpha',
    target: 'claude',
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
    refresh: vi.fn(async () => undefined),
    ...patch,
  };
}

describe('PortableInventoryView', () => {
  test('renders kind tabs, filters and rows; kind click patches filters', () => {
    const ctl = controller();
    render(<PortableInventoryView controller={ctl} labels={labels} />);

    expect(screen.getByTestId('portable-inventory-workspace')).toBeTruthy();
    expect(screen.getByTestId('portable-kind-tab-skill')).toBeTruthy();
    expect(screen.getByTestId('portable-kind-tab-command')).toBeTruthy();
    expect(screen.getByTestId('portable-kind-tab-plugin')).toBeTruthy();
    expect(screen.getByTestId('portable-kind-tab-mcp')).toBeTruthy();
    expect(screen.getByTestId('portable-inventory-row-claude-skill-alpha')).toBeTruthy();

    fireEvent.click(screen.getByTestId('portable-kind-tab-plugin'));
    expect(ctl.setFilters).toHaveBeenCalledWith({ kind: 'plugin' });
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
});
