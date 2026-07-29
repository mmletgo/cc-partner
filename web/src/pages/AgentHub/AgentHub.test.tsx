/**
 * AgentHub 页面 characterization 测试。
 *
 * Business Logic: 锁定 probe/filters/target cells/dialog/drawers/blocked 态与 pure view 无 api 导入。
 * Code Logic: 注入 AgentHubView props；静态扫描源文件。
 */

// @vitest-environment jsdom

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { UseAgentHubControllerResult } from './useAgentHubController';
import { AgentHubView } from './AgentHub';

const pageDir = dirname(fileURLToPath(import.meta.url));

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

afterEach(() => {
  cleanup();
});

function baseTargets() {
  return [
    {
      target: 'claude' as const,
      desiredPresence: 'present' as const,
      desiredEnabled: true,
      materializationStatus: 'synced',
      lastError: null,
      requested: true,
      supported: true,
      sourceOnly: false,
      verified: true,
    },
    {
      target: 'codex' as const,
      desiredPresence: 'present' as const,
      desiredEnabled: true,
      materializationStatus: 'blocked',
      lastError: 'blocked',
      requested: true,
      supported: true,
      sourceOnly: false,
      verified: false,
    },
    {
      target: 'opencode' as const,
      desiredPresence: 'absent' as const,
      desiredEnabled: false,
      materializationStatus: 'unsupported',
      lastError: null,
      requested: false,
      supported: false,
      sourceOnly: false,
      verified: false,
    },
  ];
}

/**
 * Business Logic: 构造可渲染的 controller 快照。
 * Code Logic: 覆盖 status/assets/drawers 默认值，允许 overrides。
 */
function buildProps(
  overrides: Partial<UseAgentHubControllerResult> = {},
): UseAgentHubControllerResult {
  const base: UseAgentHubControllerResult = {
    t: i18n.t.bind(i18n) as unknown as UseAgentHubControllerResult['t'],
    loading: false,
    refreshing: false,
    stale: false,
    error: null,
    actionError: null,
    actionBusy: false,
    status: {
      enabled: true,
      backgroundEnabled: false,
      agentHubApiVersion: 1,
      ownerInstanceId: 'owner',
      writeCompatible: true,
      probes: [
        { target: 'claude', support: 'supported', version: '1.0', executable: 'claude' },
        { target: 'codex', support: 'scanOnly', version: null, executable: null },
        { target: 'opencode', support: 'unsupported', version: null, executable: null },
      ],
      conflictCount: 1,
      blockedMaterializationCount: 1,
    },
    assets: [],
    filteredAssets: [
      {
        assetId: 'asset-1',
        scopeId: 'user',
        kind: 'instruction',
        displayName: 'User instruction',
        logicalKey: 'user/instruction',
        originNamespace: 'claude',
        policy: 'shared',
        currentRevisionId: 'r1',
        hasConflict: true,
        aggregateStatus: 'partial',
        targets: baseTargets(),
      },
    ],
    scopeFilter: '',
    kindFilter: '',
    setScopeFilter: vi.fn(),
    setKindFilter: vi.fn(),
    selectedAssetId: 'asset-1',
    selectedAsset: {
      assetId: 'asset-1',
      scopeId: 'user',
      kind: 'instruction',
      displayName: 'User instruction',
      logicalKey: 'user/instruction',
      originNamespace: 'claude',
      policy: 'shared',
      currentRevisionId: 'r1',
      hasConflict: true,
      aggregateStatus: 'partial',
      targets: baseTargets(),
      blocks: [
        {
          id: 'b1',
          mode: 'shared',
          commonMarkdown: 'shared body',
        },
        {
          id: 'b2',
          mode: 'targetOnly',
          commonMarkdown: 'only claude',
          sourceTarget: 'claude',
          variants: { claude: 'only claude' },
        },
        {
          id: 'b3',
          mode: 'adapted',
          commonMarkdown: 'common',
          variants: { claude: 'c', codex: 'x', opencode: 'o' },
        },
      ],
      conflicts: [
        {
          id: 'c1',
          createdAt: '2026-07-29T00:00:00.000Z',
          detailJson: '{"reason":"drift"}',
        },
      ],
    },
    selectAsset: vi.fn(),
    preview: {
      projectId: 'p1',
      checkouts: [{ path: '/tmp/p' }],
      plannedActions: [{ action: 'create' }],
      noCommitNotice: 'no commit',
      warnings: ['warn-1'],
    },
    previewOpen: false,
    previewProjectId: 'p1',
    setPreviewProjectId: vi.fn(),
    openPreviewDialog: vi.fn(),
    closePreviewDialog: vi.fn(),
    runPreviewProject: vi.fn(async () => undefined),
    runEnableProject: vi.fn(async () => undefined),
    conflictDrawerOpen: false,
    openConflictDrawer: vi.fn(),
    closeConflictDrawer: vi.fn(),
    blocksDrawerOpen: false,
    openBlocksDrawer: vi.fn(),
    closeBlocksDrawer: vi.fn(),
    adoptionOpen: false,
    adoptionPreview: null,
    openAdoptionPreview: vi.fn(),
    closeAdoptionDialog: vi.fn(),
    deleteEverywhereOpen: false,
    deleteEverywhereAssetId: null,
    openDeleteEverywhere: vi.fn(),
    closeDeleteEverywhere: vi.fn(),
    confirmDeleteEverywhere: vi.fn(async () => undefined),
    deepLinkConflictId: null,
    reload: vi.fn(async () => undefined),
    resolveConflict: vi.fn(async () => undefined),
    updateInstruction: vi.fn(async () => undefined),
    updateInstructionBlock: vi.fn(async () => undefined),
    pairInstructionVariants: vi.fn(async () => undefined),
    setTargetBinding: vi.fn(async () => undefined),
    setTargetEnabled: vi.fn(async () => undefined),
    setTargetPresence: vi.fn(async () => undefined),
    restoreDetachedTarget: vi.fn(async () => undefined),
    removeTarget: vi.fn(async () => undefined),
    writeBlocked: false,
    upgradeRequired: false,
    ...overrides,
  };
  return base;
}

/**
 * Business Logic: i18n + view 统一挂载。
 * Code Logic: I18nextProvider 包装。
 */
function renderView(props: Partial<UseAgentHubControllerResult> = {}) {
  const merged = buildProps(props);
  return render(
    <I18nextProvider i18n={i18n}>
      <AgentHubView {...merged} />
    </I18nextProvider>,
  );
}

describe('AgentHub page characterization', () => {
  test('pure view source does not import @/api/', () => {
    const source = readFileSync(resolve(pageDir, './AgentHub.tsx'), 'utf8');
    const drawerSource = readFileSync(resolve(pageDir, './InstructionBlocksDrawer.tsx'), 'utf8');
    const adoptionSource = readFileSync(resolve(pageDir, './AssetAdoptionDialog.tsx'), 'utf8');
    const cellSource = readFileSync(resolve(pageDir, './TargetStatusCell.tsx'), 'utf8');
    expect(source).not.toMatch(/from\s+['"]@\/api\//);
    expect(drawerSource).not.toMatch(/from\s+['"]@\/api\//);
    expect(adoptionSource).not.toMatch(/from\s+['"]@\/api\//);
    expect(cellSource).not.toMatch(/from\s+['"]@\/api\//);
  });

  test('renders probe summary, filters, and target cells', () => {
    renderView();
    expect(screen.getByTestId('agent-hub-page')).toBeTruthy();
    expect(screen.getByTestId('probe-claude')).toBeTruthy();
    expect(screen.getByTestId('probe-codex')).toBeTruthy();
    expect(screen.getByTestId('probe-opencode')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-filters')).toBeTruthy();
    expect(screen.getByTestId('agent-target-claude')).toBeTruthy();
    expect(screen.getByTestId('agent-target-codex')).toBeTruthy();
    expect(screen.getByTestId('agent-target-opencode')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-lan-push-notice')).toBeTruthy();
    expect(screen.getByTestId('agent-asset-aggregate-asset-1')).toBeTruthy();
  });

  test('filter inputs call controller setters', () => {
    const setScopeFilter = vi.fn();
    const setKindFilter = vi.fn();
    renderView({ setScopeFilter, setKindFilter });
    fireEvent.change(screen.getByTestId('agent-hub-filter-scope'), {
      target: { value: 'user' },
    });
    fireEvent.change(screen.getByTestId('agent-hub-filter-kind'), {
      target: { value: 'instruction' },
    });
    expect(setScopeFilter).toHaveBeenCalledWith('user');
    expect(setKindFilter).toHaveBeenCalledWith('instruction');
  });

  test('preview dialog and conflict/blocks drawers render states', () => {
    renderView({
      previewOpen: true,
      conflictDrawerOpen: true,
      blocksDrawerOpen: true,
      writeBlocked: true,
      upgradeRequired: true,
    });
    expect(screen.getByTestId('agent-hub-preview-dialog')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-conflict-drawer')).toBeTruthy();
    expect(screen.getByTestId('instruction-blocks-drawer')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-upgrade-required')).toBeTruthy();
    expect(screen.getByTestId('block-item-b1')).toBeTruthy();
    expect(screen.getByTestId('block-item-b2')).toBeTruthy();
    expect(screen.getByTestId('block-item-b3')).toBeTruthy();
    expect(screen.getByTestId('blocks-diff-preview')).toBeTruthy();
    expect(screen.getByTestId('conflict-c1')).toBeTruthy();
  });

  test('blocked/unsupported probe state is visible', () => {
    renderView();
    expect(screen.getByTestId('probe-opencode').textContent?.toLowerCase()).toMatch(
      /unsupported|不支持/,
    );
  });

  test('detached cells expose restore/remove actions', () => {
    const restoreDetachedTarget = vi.fn(async () => undefined);
    const removeTarget = vi.fn(async () => undefined);
    renderView({
      restoreDetachedTarget,
      removeTarget,
      filteredAssets: [
        {
          assetId: 'asset-detached',
          scopeId: 'user',
          kind: 'skill',
          displayName: 'Detached skill',
          logicalKey: 'user/skill/d',
          originNamespace: 'claude',
          policy: 'shared',
          currentRevisionId: 'r2',
          hasConflict: false,
          aggregateStatus: 'detached',
          targets: [
            {
              target: 'claude',
              desiredPresence: 'present',
              desiredEnabled: true,
              materializationStatus: 'detached',
              lastError: null,
              requested: true,
              supported: true,
              sourceOnly: false,
              verified: false,
            },
            {
              target: 'codex',
              desiredPresence: 'absent',
              desiredEnabled: false,
              materializationStatus: null,
              lastError: null,
              requested: false,
              supported: true,
              sourceOnly: false,
              verified: false,
            },
            {
              target: 'opencode',
              desiredPresence: 'absent',
              desiredEnabled: false,
              materializationStatus: null,
              lastError: null,
              requested: false,
              supported: true,
              sourceOnly: false,
              verified: false,
            },
          ],
        },
      ],
    });
    fireEvent.click(screen.getByTestId('agent-target-restore-asset-detached-claude'));
    fireEvent.click(screen.getByTestId('agent-target-remove-asset-detached-claude'));
    expect(restoreDetachedTarget).toHaveBeenCalled();
    expect(removeTarget).toHaveBeenCalled();
  });

  test('delete everywhere confirmation dialog', () => {
    const confirmDeleteEverywhere = vi.fn(async () => undefined);
    renderView({
      deleteEverywhereOpen: true,
      deleteEverywhereAssetId: 'asset-1',
      confirmDeleteEverywhere,
    });
    expect(screen.getByTestId('agent-hub-delete-everywhere-dialog')).toBeTruthy();
    fireEvent.click(screen.getByTestId('agent-hub-delete-everywhere-confirm'));
    expect(confirmDeleteEverywhere).toHaveBeenCalled();
  });

  test('external collision opens adoption dialog', () => {
    renderView({
      adoptionOpen: true,
      adoptionPreview: {
        assetId: 'asset-1',
        displayName: 'User instruction',
        logicalKey: 'user/instruction',
        originNamespace: 'claude',
        target: 'claude',
        diagnostics: ['materialization:externalCollision'],
        aggregateStatus: 'externalCollision',
      },
    });
    expect(screen.getByTestId('agent-hub-adoption-dialog')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-lan-push-gate-c')).toBeTruthy();
    expect(screen.getByTestId('adoption-canonical').textContent).toContain('User instruction');
  });
});
