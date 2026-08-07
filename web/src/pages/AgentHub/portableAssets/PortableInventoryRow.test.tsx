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
    expect(screen.getByText('Hub')).toBeTruthy();
    expect(screen.getByText('/tmp/alpha')).toBeTruthy();

    fireEvent.click(screen.getByTestId('portable-inventory-row-claude-skill-alpha'));
    expect(onSelect).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    expect(onPrimaryAction).toHaveBeenCalledWith(expect.objectContaining({
      inventoryItemId: 'claude-skill-alpha',
    }), 'disable');
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
});
