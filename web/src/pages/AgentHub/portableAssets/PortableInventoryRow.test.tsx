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
    attach: 'Attach',
    detach: 'Detach',
    destroyStore: 'Destroy store',
    migrateToStore: 'Migrate to store',
  },
  sourceOrigin: {
    standalone: 'Standalone',
    pluginComponent: 'Plugin component',
    nativeConfig: 'Native config',
  },
  unmanagedRefreshHint: 'Refresh inventory to manage this asset.',
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
    portableStore: 'Portable store',
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
    originKind: 'native',
    ownedBy: target,
    loadedBy: target,
    nativeOutputCandidate: true,
    ...overrides,
  };
}

describe('PortableInventoryRow', () => {
  test('renders observed facts and fires select/action callbacks', () => {
    const onSelect = vi.fn();
    const onAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item()}
        selected
        actions={['disable']}
        labels={labels}
        onSelect={onSelect}
        onAction={onAction}
      />,
    );

    expect(screen.getByText('Alpha Skill')).toBeTruthy();
    expect(screen.getByText('Claude')).toBeTruthy();
    expect(screen.getByText('Enabled')).toBeTruthy();
    expect(screen.getByText('Consistent')).toBeTruthy();
    expect(screen.getByText('/tmp/alpha')).toBeTruthy();

    fireEvent.click(screen.getByTestId('portable-inventory-select-claude-skill-alpha'));
    expect(onSelect).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByTestId('portable-row-action-disable-claude-skill-alpha'));
    expect(onAction).toHaveBeenCalledWith(expect.objectContaining({
      inventoryItemId: 'claude-skill-alpha',
    }), 'disable');
  });

  test('uses a native selection button beside row actions without nested button semantics', () => {
    const onSelect = vi.fn();
    const onAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item()}
        selected
        actions={['disable']}
        labels={labels}
        onSelect={onSelect}
        onAction={onAction}
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

    fireEvent.click(screen.getByTestId('portable-row-action-disable-claude-skill-alpha'));
    expect(onAction).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  test('hides action buttons when actions array is empty', () => {
    render(
      <PortableInventoryRow
        item={item({ actualEnabled: null })}
        actions={[]}
        labels={labels}
      />,
    );
    expect(screen.queryByTestId('portable-row-action-disable-claude-skill-alpha')).toBeNull();
    expect(screen.queryByTestId('portable-row-action-enable-claude-skill-alpha')).toBeNull();
  });

  test('historical unmanaged without toggle shows refresh hint, never exposes action buttons', () => {
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
        actions={[]}
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
        actions={['disable']}
        labels={labels}
      />,
    );

    expect(screen.getByText('Enabled')).toBeTruthy();
    expect(screen.queryByText('Problem')).toBeNull();
    expect(screen.queryByText('targetExecutable')).toBeNull();
    expect(screen.queryByText('unknownSourceField')).toBeNull();
    expect(screen.queryByText('absolutePath')).toBeNull();
  });

  test('renders multiple action buttons (disable + uninstall) and each fires onAction', () => {
    const onAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item()}
        actions={['disable', 'uninstall']}
        labels={labels}
        onAction={onAction}
      />,
    );

    const disableBtn = screen.getByTestId('portable-row-action-disable-claude-skill-alpha');
    const uninstallBtn = screen.getByTestId('portable-row-action-uninstall-claude-skill-alpha');
    expect(disableBtn).toBeTruthy();
    expect(uninstallBtn).toBeTruthy();
    // uninstall 用 danger variant
    expect(uninstallBtn.className).toMatch(/danger/);

    fireEvent.click(disableBtn);
    expect(onAction).toHaveBeenLastCalledWith(expect.objectContaining({
      inventoryItemId: 'claude-skill-alpha',
    }), 'disable');

    fireEvent.click(uninstallBtn);
    expect(onAction).toHaveBeenLastCalledWith(expect.objectContaining({
      inventoryItemId: 'claude-skill-alpha',
    }), 'uninstall');
    expect(onAction).toHaveBeenCalledTimes(2);
  });

  test('borrowed row shows owner badge, mutations, and open-owner jump', () => {
    const onOpenOwner = vi.fn();
    const onAction = vi.fn();
    render(
      <PortableInventoryRow
        item={item({
          inventoryItemId: 'grok-skill-from-claude',
          target: 'grok',
          displayName: 'Borrowed Skill',
          originKind: 'compatibility',
          ownedBy: 'claude',
          loadedBy: 'grok',
          nativeOutputCandidate: false,
        })}
        actions={['disable', 'uninstall']}
        labels={labels}
        onAction={onAction}
        onOpenOwner={onOpenOwner}
      />,
    );

    expect(screen.getByTestId('portable-row-borrowed-badge').textContent).toContain(
      'From Claude Code',
    );
    expect(screen.getByTestId('portable-row-action-uninstall-grok-skill-from-claude')).toBeTruthy();
    expect(screen.getByTestId('portable-row-action-disable-grok-skill-from-claude')).toBeTruthy();
    fireEvent.click(screen.getByTestId('portable-row-open-owner'));
    expect(onOpenOwner).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId('portable-row-action-disable-grok-skill-from-claude'));
    expect(onAction).toHaveBeenCalledTimes(1);
  });
});
