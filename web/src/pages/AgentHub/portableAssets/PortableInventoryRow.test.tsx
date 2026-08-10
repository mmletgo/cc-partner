// @vitest-environment jsdom
/**
 * PortableInventoryRow pure view 测试。
 *
 * Business Logic: 行展示 observed 状态与单一主动作，不直接调 API。
 * Code Logic: 选中/primary action 点击委托回调。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import { PortableInventoryRow, type PortableInventoryRowLabels } from './PortableInventoryRow';

afterEach(() => {
  cleanup();
});

const labels: PortableInventoryRowLabels = {
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
};

function item(overrides: Partial<PortableInventoryItemDto> = {}): PortableInventoryItemDto {
  return {
    inventoryItemId: 'claude-skill-alpha',
    target: 'claude',
    kind: 'skill',
    nativeId: 'alpha',
    displayName: 'Alpha Skill',
    description: 'demo skill',
    version: '1.0.0',
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
    ...overrides,
  };
}

describe('PortableInventoryRow', () => {
  test('renders observed facts and fires select/primary action callbacks', () => {
    const onSelect = vi.fn();
    const onPrimaryAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item()}
        selected
        primaryAction="disable"
        labels={labels}
        onSelect={onSelect}
        onPrimaryAction={onPrimaryAction}
      />,
    );

    expect(screen.getByText('Alpha Skill')).toBeTruthy();
    expect(screen.getByText('Claude')).toBeTruthy();
    expect(screen.getByText('Enabled')).toBeTruthy();
    expect(screen.getByText('Consistent')).toBeTruthy();
    expect(screen.getByText('/tmp/alpha')).toBeTruthy();

    fireEvent.click(screen.getByTestId('portable-inventory-select-claude-skill-alpha'));
    expect(onSelect).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    expect(onPrimaryAction).toHaveBeenCalledWith(expect.objectContaining({
      inventoryItemId: 'claude-skill-alpha',
    }), 'disable');
  });

  test('uses a native selection button beside the primary action without nested button semantics', () => {
    const onSelect = vi.fn();
    const onPrimaryAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item()}
        selected
        primaryAction="disable"
        labels={labels}
        onSelect={onSelect}
        onPrimaryAction={onPrimaryAction}
      />,
    );

    const row = screen.getByTestId('portable-inventory-row-claude-skill-alpha');
    expect(row.getAttribute('role')).toBeNull();
    const selectionButton = screen.getByTestId(
      'portable-inventory-select-claude-skill-alpha',
    ) as HTMLButtonElement;
    expect(selectionButton.tagName).toBe('BUTTON');
    expect(selectionButton.getAttribute('aria-pressed')).toBe('true');
    expect(row.querySelectorAll('button')).toHaveLength(2);

    fireEvent.keyDown(selectionButton, { key: 'Enter' });
    fireEvent.click(selectionButton);
    expect(onSelect).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    expect(onPrimaryAction).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  test('hides primary action button when null', () => {
    render(
      <PortableInventoryRow
        item={item({ actualEnabled: null })}
        primaryAction={null}
        labels={labels}
      />,
    );
    expect(screen.queryByRole('button', { name: 'Disable' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Enable' })).toBeNull();
  });

  test('historical unmanaged without toggle shows refresh hint, never Adopt primary', () => {
    render(
      <PortableInventoryRow
        item={item({
          inventoryItemId: 'codex-skill-beta',
          target: 'codex',
          actualEnabled: false,
          managementState: 'unmanaged',
          capabilities: {
            canEnable: false,
            canDisable: false,
            canUninstall: false,
            canAdopt: true,
            canInstallToSourceTarget: false,
            reasonCode: null,
            evidenceIds: [],
          },
        })}
        primaryAction={null}
        labels={labels}
      />,
    );
    expect(screen.getByTestId('portable-row-unmanaged-refresh-hint').textContent).toContain(
      'Refresh inventory',
    );
    expect(screen.queryByRole('button', { name: 'Adopt' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Refresh to manage' })).toBeNull();
    expect(screen.getByText('Pending manage')).toBeTruthy();
  });

  test('keeps portability advisories out of the health row', () => {
    render(
      <PortableInventoryRow
        item={item({ warnings: ['targetExecutable', 'unknownSourceField', 'absolutePath'] })}
        primaryAction="disable"
        labels={labels}
      />,
    );

    expect(screen.getByText('Enabled')).toBeTruthy();
    expect(screen.queryByText('Problem')).toBeNull();
    expect(screen.queryByText('targetExecutable')).toBeNull();
    expect(screen.queryByText('unknownSourceField')).toBeNull();
    expect(screen.queryByText('absolutePath')).toBeNull();
  });
});
