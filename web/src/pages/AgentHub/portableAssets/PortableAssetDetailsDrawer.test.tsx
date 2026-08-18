// @vitest-environment jsdom
/**
 * PortableAssetDetailsDrawer four-kind rendering 合同。
 *
 * Business Logic: Skill/Command/Plugin/MCP 各有专属详情语义；MCP 不得渲染 secret。
 * Code Logic: RTL pure props；delete-everywhere 仅在 danger zone。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, test, vi } from 'vitest';
import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import { PortableAssetDetailsDrawer } from './PortableAssetDetailsDrawer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) => {
      if (opts && typeof opts === 'object') {
        const parts = Object.entries(opts)
          .filter(([k]) => k !== 'defaultValue')
          .map(([k, v]) => `${k}=${String(v)}`);
        return parts.length ? `${key}:${parts.join(',')}` : key;
      }
      return key;
    },
  }),
}));

function baseItem(
  kind: PortableInventoryItemDto['kind'],
  extra: Partial<PortableInventoryItemDto> = {},
): PortableInventoryItemDto {
  return {
    inventoryItemId: `claude-${kind}-demo`,
    target: 'claude',
    kind,
    nativeId: `${kind}-demo`,
    displayName: `${kind} demo`,
    description: `${kind} description`,
    version: '1.2.3',
    scopeId: 'user',
    scopeKind: 'user',
    projectId: null,
    projectOptedIn: true,
    sourcePath: `/tmp/claude/${kind}/demo`,
    sourceOrigin: kind === 'mcp' ? 'nativeConfig' : 'standalone',
    parentPluginInventoryItemId: null,
    actualEnabled: true,
    contentHash: `hash-${kind}`,
    treeHash: kind === 'skill' || kind === 'plugin' ? `tree-${kind}` : null,
    canonicalAssetId: `canon-${kind}`,
    canonicalRevisionId: `rev-${kind}`,
    managementState: 'hubManaged',
    desiredPresence: 'present',
    desiredEnabled: true,
    materializationStatus: 'verified',
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
    ownedBy: 'claude',
    loadedBy: 'claude',
    nativeOutputCandidate: true,
    ...extra,
  };
}

afterEach(() => {
  cleanup();
});

describe('PortableAssetDetailsDrawer four-kind rendering', () => {
  test('skill details show tree origin invocation and supporting facts', () => {
    const item = baseItem('skill', {
      sourceOrigin: 'standalone',
      treeHash: 'tree-skill-1',
      nativeId: 'skill-native',
      displayName: 'Skill Native',
      description: 'Does useful things',
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-asset-details-drawer')).toBeTruthy();
    expect(screen.getByTestId('portable-skill-details')).toBeTruthy();
    expect(screen.getByTestId('portable-skill-tree-hash').textContent).toContain('tree-skill-1');
    expect(screen.getByTestId('portable-skill-origin').getAttribute('data-origin')).toBe(
      'standalone',
    );
    expect(screen.getByTestId('portable-skill-invocation').textContent).toContain('skill-native');
    expect(screen.getByTestId('portable-skill-source-path').textContent).toContain(
      '/tmp/claude/skill/demo',
    );
    expect(screen.queryByTestId('portable-mcp-details')).toBeNull();
  });

  test('command details show native file invocation and compatibility', () => {
    const item = baseItem('command', {
      nativeId: '/review',
      displayName: 'Review Command',
      sourcePath: '/tmp/claude/commands/review.md',
      sourceOrigin: 'standalone',
      warnings: ['compat:skill_wrapper'],
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-command-details')).toBeTruthy();
    expect(screen.getByTestId('portable-command-native-id').textContent).toContain('/review');
    expect(screen.getByTestId('portable-command-source-file').textContent).toContain(
      '/tmp/claude/commands/review.md',
    );
    expect(screen.getByTestId('portable-command-invocation').textContent).toContain('/review');
    expect(screen.getByTestId('portable-command-compatibility').textContent).toContain(
      'compat:skill_wrapper',
    );
  });

  test('plugin details embed package components residual activation ownership groups', () => {
    const item = baseItem('plugin', {
      nativeId: 'mixed-plugin',
      displayName: 'mixed-plugin',
      sourceOrigin: 'standalone',
      managementState: 'hubManaged',
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        pluginReport={{
          packageDisplayName: 'mixed-plugin',
          activationState: 'planned',
          aggregateStatus: 'partial',
          componentCount: 2,
          residualCount: 1,
          deleteTombstoneCount: 1,
          deletePreserveCount: 1,
        }}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-plugin-details')).toBeTruthy();
    expect(screen.getByTestId('portable-plugin-package').textContent).toContain('mixed-plugin');
    expect(screen.getByTestId('portable-plugin-activation').textContent).toContain('planned');
    expect(screen.getByTestId('portable-plugin-components').textContent).toContain('2');
    expect(screen.getByTestId('portable-plugin-residuals').textContent).toContain('1');
    expect(screen.getByTestId('portable-plugin-delete-groups').textContent).toMatch(/1.*1|tombstone|preserve/i);
  });

  test('mcp details show transport source credential present/hash without secret', () => {
    const item = baseItem('mcp', {
      nativeId: 'github',
      displayName: 'GitHub MCP',
      sourceOrigin: 'nativeConfig',
      sourcePath: '/tmp/claude/.claude.json',
      mcpCredential: { present: true, hash: 'cred-abc' },
      warnings: ['transport:stdio'],
    });

    // 故意塞 secret 形状字段到 runtime 对象：typed DTO 不应渲染它。
    const polluted = {
      ...item,
      mcpCredential: {
        present: true,
        hash: 'cred-abc',
        secret: 'super-secret-token',
        token: 'should-not-render',
        value: 'also-secret',
      },
    } as PortableInventoryItemDto;

    render(
      <PortableAssetDetailsDrawer
        open
        item={polluted}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    const root = screen.getByTestId('portable-mcp-details');
    expect(root).toBeTruthy();
    expect(screen.getByTestId('portable-mcp-transport').textContent).toContain('stdio');
    expect(screen.getByTestId('portable-mcp-source').textContent).toContain(
      '/tmp/claude/.claude.json',
    );
    expect(screen.getByTestId('portable-mcp-credential-present').getAttribute('data-present')).toBe(
      'true',
    );
    expect(screen.getByTestId('portable-mcp-credential-hash').textContent).toContain('cred-abc');
    expect(root.textContent).not.toContain('super-secret-token');
    expect(root.textContent).not.toContain('should-not-render');
    expect(root.textContent).not.toContain('also-secret');
  });

  test('mcp fake store capabilities stay on native leaf enable/disable/uninstall', () => {
    const onRequestAction = vi.fn();
    const item = baseItem('mcp', {
      capabilities: {
        canEnable: true,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canMigrateToStore: true,
        canAttach: true,
        canDetach: true,
        canDestroyStore: true,
        reasonCode: null,
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
      />,
    );

    expect(screen.getByTestId('portable-action-disable')).toBeTruthy();
    expect(screen.getByTestId('portable-action-uninstall')).toBeTruthy();
    expect(screen.queryByTestId('portable-action-migrate-to-store')).toBeNull();
    expect(screen.queryByTestId('portable-action-attach')).toBeNull();
    expect(screen.queryByTestId('portable-action-detach')).toBeNull();
    expect(screen.queryByTestId('portable-action-destroy-store')).toBeNull();
  });

  test('unsupported management uses honest diagnostic not fabricated success', () => {
    const item = baseItem('skill', {
      managementState: 'unsupported',
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: false,
        canInstallToSourceTarget: false,
        reasonCode: 'PORTABLE_CLI_VERSION_UNSUPPORTED',
        evidenceIds: ['L2-PORTABLE-UNSUPPORTED-001'],
      },
      warnings: ['cli_version_unsupported'],
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-asset-diagnostic').textContent).toContain(
      'PORTABLE_CLI_VERSION_UNSUPPORTED',
    );
    expect(screen.queryByTestId('portable-action-enable')).toBeNull();
    expect(screen.getByTestId('portable-asset-management').getAttribute('data-state')).toBe(
      'unsupported',
    );
  });

  test('delete everywhere only appears in danger zone and requests uninstall action', () => {
    const onRequestAction = vi.fn();
    const item = baseItem('skill');

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
      />,
    );

    const danger = screen.getByTestId('portable-asset-danger-zone');
    const deleteBtn = screen.getByTestId('portable-action-uninstall');
    expect(danger.contains(deleteBtn)).toBe(true);
    fireEvent.click(deleteBtn);
    expect(onRequestAction).toHaveBeenCalledWith('uninstall');
  });

  test('empty item still renders drawer shell without kind body', () => {
    render(
      <PortableAssetDetailsDrawer
        open
        item={null}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );
    expect(screen.getByTestId('portable-asset-details-drawer')).toBeTruthy();
    expect(screen.getByTestId('portable-asset-details-empty')).toBeTruthy();
  });

  test('mutationBlocked hides mutation affordances', () => {
    const onRequestAction = vi.fn();
    const item = baseItem('skill');

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        mutationBlocked
        onClose={() => undefined}
        onRequestAction={onRequestAction}
      />,
    );

    expect(screen.queryByTestId('portable-action-enable')).toBeNull();
    expect(screen.queryByTestId('portable-action-disable')).toBeNull();
    expect(screen.queryByTestId('portable-action-uninstall')).toBeNull();
    expect(screen.getByTestId('portable-action-uninstall-blocked')).toBeTruthy();
  });

  test('never exposes adopt CTA; historical unmanaged shows refresh hint', () => {
    const onRequestAction = vi.fn();
    const item = baseItem('skill', {
      managementState: 'unmanaged',
      actualEnabled: false,
      capabilities: {
        canEnable: false,
        canDisable: false,
        canUninstall: false,
        canAdopt: true,
        canInstallToSourceTarget: false,
        reasonCode: null,
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
      />,
    );

    expect(screen.queryByTestId('portable-action-adopt')).toBeNull();
    expect(screen.getByTestId('portable-action-refresh-hint')).toBeTruthy();
    expect(screen.getByTestId('portable-asset-management').getAttribute('data-state')).toBe(
      'unmanaged',
    );
  });

  test('desired enabled uses i18n keys instead of hard-coded English', () => {
    const item = baseItem('skill', { desiredEnabled: true });
    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );
    expect(screen.getByTestId('portable-asset-desired').textContent).toContain(
      'agentHub:portable.details.desiredEnabled',
    );
    expect(screen.getByTestId('portable-asset-desired').textContent).not.toMatch(
      /\benabled\b|\bdisabled\b/,
    );
  });

  test('borrowed runtime item warns about owner impact and still offers mutations', () => {
    const onOpenOwner = vi.fn();
    const onRequestAction = vi.fn();
    const item = baseItem('skill', {
      inventoryItemId: 'grok-skill-from-claude',
      target: 'grok',
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
        reasonCode: 'borrowed_runtime_origin',
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
        onOpenOwner={onOpenOwner}
      />,
    );

    expect(screen.getByTestId('portable-asset-borrowed-banner').textContent).toContain(
      'agentHub:portable.details.borrowedHint',
    );
    expect(screen.queryByTestId('portable-asset-diagnostic')).toBeNull();
    expect(screen.getByTestId('portable-asset-actions')).toBeTruthy();
    expect(screen.getByTestId('portable-action-disable')).toBeTruthy();
    expect(screen.getByTestId('portable-action-uninstall')).toBeTruthy();
    fireEvent.click(screen.getByTestId('portable-action-open-owner'));
    expect(onOpenOwner).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByTestId('portable-action-disable'));
    expect(onRequestAction).toHaveBeenCalledWith('disable');
  });

  test('borrowed plugin hint does not claim owner flag rewrite', () => {
    const item = baseItem('plugin', {
      inventoryItemId: 'codex-plugin-from-claude',
      target: 'codex',
      originKind: 'compatibility',
      ownedBy: 'claude',
      loadedBy: 'codex',
      nativeOutputCandidate: false,
      capabilities: {
        canEnable: false,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        reasonCode: 'borrowed_runtime_origin',
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={() => undefined}
      />,
    );

    expect(screen.getByTestId('portable-asset-borrowed-banner').textContent).toContain(
      'agentHub:portable.details.borrowedHintPlugin',
    );
    expect(screen.queryByTestId('portable-asset-borrowed-banner')?.textContent).not.toContain(
      'borrowedHint:',
    );
  });

  test('sharedAgents borrowed item warns without an owner-agent switch and still mutates', () => {
    const onOpenOwner = vi.fn();
    const onRequestAction = vi.fn();
    const item = baseItem('skill', {
      inventoryItemId: 'grok-skill-shared',
      target: 'grok',
      ownedBy: 'sharedAgents',
      loadedBy: 'grok',
      originKind: 'native',
      nativeOutputCandidate: true,
      capabilities: {
        canEnable: false,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        reasonCode: 'borrowed_runtime_origin',
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
        onOpenOwner={onOpenOwner}
      />,
    );

    expect(screen.getByTestId('portable-asset-borrowed-banner')).toBeTruthy();
    expect(screen.queryByTestId('portable-action-open-owner')).toBeNull();
    expect(screen.getByTestId('portable-action-disable')).toBeTruthy();
    expect(screen.getByTestId('portable-action-uninstall')).toBeTruthy();
  });

  test('drifted item exposes confirm current version', () => {
    const onRequestAction = vi.fn();
    const item = baseItem('skill', {
      managementState: 'drifted',
      capabilities: {
        canEnable: false,
        canDisable: true,
        canUninstall: true,
        canAdopt: false,
        canInstallToSourceTarget: false,
        canConfirmCurrentVersion: true,
        reasonCode: null,
        evidenceIds: [],
      },
    });

    render(
      <PortableAssetDetailsDrawer
        open
        item={item}
        onClose={() => undefined}
        onRequestAction={onRequestAction}
      />,
    );

    fireEvent.click(screen.getByTestId('portable-action-confirm-current-version'));
    expect(onRequestAction).toHaveBeenCalledWith('confirmCurrentVersion');
  });
});
