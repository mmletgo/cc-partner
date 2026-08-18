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
import { AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY } from './context/agentHubContext';
import { initialThreePaneFromDisk } from './instructions/instructionThreePane';
import type { UseInstructionThreePaneControllerResult } from './instructions';
import type { UseAgentHubControllerResult } from './useAgentHubController';
import { AgentHubView, type AgentHubViewProps } from './AgentHub';

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
    activeSection: 'assets',
    setActiveSection: vi.fn(),
    hubContext: {
      agent: 'claude',
      scope: 'user',
      deviceId: null,
      projectKey: null,
      tab: 'skill',
      instructionLane: 'common',
      assetLane: 'equipped',
      adaptView: false,
    },
    contextMigrationNotice: null,
    onContextChange: vi.fn(),
    shellPeers: [],
    shellProjects: [],
    userInstructions: {} as UseAgentHubControllerResult['userInstructions'],
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
    statusLoading: false,
    legacyLoadedOnce: true,
    legacyMatrixExpanded: true,
    expandLegacyMatrix: vi.fn(),
    instructionsLaneActive: false,
    portableLaneActive: true,
    setInstructionRefresh: vi.fn(),
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
      contentMarkdown: 'original body',
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
    pluginDrawerOpen: false,
    pluginReport: null,
    pluginReportAssetId: null,
    openPluginDrawer: vi.fn(),
    closePluginDrawer: vi.fn(),
    loadPluginReport: vi.fn(async () => undefined),
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
    deepLinkBridgePath: null,
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
    lanPushOpen: false,
    openLanPushDialog: vi.fn(),
    closeLanPushDialog: vi.fn(),
    lanPeers: [],
    lanSelectedPeerIds: [],
    toggleLanPeer: vi.fn(),
    lanMode: 'fullHub',
    setLanMode: vi.fn(),
    lanAssetIdsText: '',
    setLanAssetIdsText: vi.fn(),
    lanHubProjectIdsText: '',
    setLanHubProjectIdsText: vi.fn(),
    lanPreview: null,
    lanReport: null,
    runLanPreview: vi.fn(async () => undefined),
    runLanStart: vi.fn(async () => undefined),
    gitImportOpen: false,
    openGitImportDrawer: vi.fn(),
    closeGitImportDrawer: vi.fn(),
    gitInspectReport: null,
    gitSelectedLaneDeviceId: null,
    selectGitLane: vi.fn(),
    gitPreview: null,
    gitSelectedAssetIds: [],
    gitAssetSelectionExplicit: false,
    toggleGitAsset: vi.fn(),
    gitMappingDrafts: {},
    setGitMappingDraft: vi.fn(),
    gitConfirmOutcome: null,
    gitLastMapping: null,
    runGitInspect: vi.fn(async () => undefined),
    runGitPreview: vi.fn(async () => undefined),
    runGitConfirmMapping: vi.fn(async () => undefined),
    runGitConfirmImport: vi.fn(async () => undefined),
    portableInventory: {
      snapshot: null,
      visibleItems: [],
      kindCounts: { skill: 0, command: 0, plugin: 0, mcp: 0 },
      filters: {
        kind: 'skill',
        target: 'all',
        scope: 'all',
        actualState: 'all',
        management: 'all',
        search: '',
        assetLane: 'equipped',
      },
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
      getPrimaryAction: vi.fn(() => null),
      getRowActions: vi.fn(() => []),
      refresh: vi.fn(async () => undefined),
      requestContext: { deviceId: null, projectRef: null },
      inventoryQuery: { kind: 'skill' },
    } as UseAgentHubControllerResult['portableInventory'],
    requestPortableAction: vi.fn(),
    portableActionOpen: false,
    portableActionKind: null,
    portableActionPlan: null,
    portableActionResult: null,
    portableActionBusy: false,
    portableActionError: null,
    portableActionClientRequestId: null,
    previewPortableAction: vi.fn(async () => undefined),
    confirmPortableAction: vi.fn(async () => undefined),
    reconcilePortableAction: vi.fn(async () => undefined),
    closePortableAction: vi.fn(),
    portablePullOpen: false,
    openPortablePull: vi.fn(),
    closePortablePull: vi.fn(),
    portablePull: {
      devices: [],
      selectedDeviceId: '',
      sourceTarget: 'claude',
      remoteInventory: null,
      visibleItems: [],
      selectedItemIds: new Set(),
      filters: { kind: 'all', scope: 'all', actualState: 'all', search: '' },
      conflictPolicy: 'skipExisting',
      plan: null,
      result: null,
      clientRequestId: null,
      busy: false,
      error: null,
      mutationBlocked: false,
      canApply: false,
      canReconcile: false,
      loadInventory: vi.fn(async () => undefined),
      preview: vi.fn(async () => undefined),
      apply: vi.fn(async () => undefined),
      reconcile: vi.fn(async () => undefined),
      selectDevice: vi.fn(),
      selectSourceTarget: vi.fn(),
      setFilters: vi.fn(),
      setConflictPolicy: vi.fn(),
      toggleItem: vi.fn(),
      selectVisible: vi.fn(),
      clearSelection: vi.fn(),
    } as UseAgentHubControllerResult['portablePull'],
    writeBlocked: false,
    upgradeRequired: false,
    ...overrides,
  };
  return base;
}

/**
 * Business Logic: 远端用户级三栏门闩测试需要可渲染的三栏 stub。
 * Code Logic: 非 loading/error，足以露出 instruction-three-pane。
 */
function stubThreePane(): UseInstructionThreePaneControllerResult {
  return {
    state: initialThreePaneFromDisk(null, 'peer original'),
    workspace: null,
    loading: false,
    refreshing: false,
    error: null,
    actionError: null,
    actionBusy: false,
    busyAction: null,
    dirty: false,
    writeBlocked: false,
    writeBlockedReason: null,
    dualDirtyOpen: false,
    analyzeConfirmOpen: false,
    aiReviseOpen: false,
    aiReviseDirection: '',
    aiReviseError: null,
    aiReviseFeedback: null,
    aiReviseDisabled: false,
    openAiRevise: vi.fn(),
    setAiReviseDirection: vi.fn(),
    cancelAiRevise: vi.fn(),
    confirmAiRevise: vi.fn(async () => undefined),
    previewOpen: false,
    plan: null,
    applyResult: null,
    analyzeDecompose: vi.fn(),
    confirmAnalyzeDecompose: vi.fn(),
    cancelAnalyzeDecompose: vi.fn(),
    adaptToOtherAgents: vi.fn(async () => undefined),
    requestSync: vi.fn(async () => undefined),
    applyPlan: vi.fn(async () => undefined),
    saveBlocks: vi.fn(async () => true),
    closePreview: vi.fn(),
    refresh: vi.fn(async () => undefined),
    discardAndReload: vi.fn(async () => undefined),
    updateOriginal: vi.fn(),
    changeBlock: vi.fn(),
    appendBlock: vi.fn(),
    editCurrentSlot: vi.fn(),
    chooseBaseline: vi.fn(),
    cancelDualDirty: vi.fn(),
    dismissApplyResult: vi.fn(),
    discardDraftForContextChange: vi.fn(),
    slotHistoryOpen: false,
    slotHistorySlot: null,
    slotHistoryVersions: [],
    slotHistoryLoading: false,
    slotHistoryError: null,
    restoringSlotVersionId: null,
    slotHistoryActionError: null,
    openSlotHistory: vi.fn(),
    closeSlotHistory: vi.fn(),
    copySlotVersion: vi.fn(async () => undefined),
    restoreSlotVersion: vi.fn(async () => true),
  };
}

/**
 * Business Logic: i18n + view 统一挂载。
 * Code Logic: I18nextProvider 包装。
 */
function renderView(props: Partial<AgentHubViewProps> = {}) {
  const { instructionThreePane, ...controllerProps } = props;
  const merged = buildProps(controllerProps);
  return render(
    <I18nextProvider i18n={i18n}>
      <AgentHubView {...merged} instructionThreePane={instructionThreePane} />
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

  test('asset tab renders only portable inventory, even when legacy writer flags are injected', () => {
    renderView({
      activeSection: 'diagnostics',
      previewOpen: true,
      conflictDrawerOpen: true,
      blocksDrawerOpen: true,
      deleteEverywhereOpen: true,
      adoptionOpen: true,
    });
    expect(screen.getByTestId('agent-hub-page')).toBeTruthy();
    expect(screen.getByTestId('portable-inventory-workspace')).toBeTruthy();
    expect(screen.queryByTestId('agent-hub-filters')).toBeNull();
    expect(screen.queryByTestId('agent-hub-preview-dialog')).toBeNull();
    expect(screen.queryByTestId('agent-hub-conflict-drawer')).toBeNull();
    expect(screen.queryByTestId('instruction-blocks-drawer')).toBeNull();
    expect(screen.queryByTestId('agent-hub-delete-everywhere-dialog')).toBeNull();
    expect(screen.queryByTestId('agent-hub-adoption-dialog')).toBeNull();
    expect(screen.queryByTestId('portable-asset-details-drawer')).toBeNull();
    expect(screen.queryByTestId('agent-hub-status-card')).toBeNull();
  });

  test('portable secondary filters delegate only state and management changes', () => {
    const setFilters = vi.fn();
    renderView({
      portableInventory: {
        ...buildProps().portableInventory,
        setFilters,
      },
    });
    fireEvent.change(screen.getByTestId('portable-filter-actual'), {
      target: { value: 'problem' },
    });
    fireEvent.change(screen.getByTestId('portable-filter-management'), {
      target: { value: 'drifted' },
    });
    expect(setFilters).toHaveBeenCalledWith({ actualState: 'problem' });
    expect(setFilters).toHaveBeenCalledWith({ management: 'drifted' });
    expect(screen.queryByTestId('portable-filter-target')).toBeNull();
    expect(screen.queryByTestId('portable-filter-scope')).toBeNull();
  });

  test('instructions tab ignores stale legacy activeSection and never renders assets', () => {
    renderView({
      activeSection: 'assets',
      hubContext: {
        ...buildProps().hubContext,
        tab: 'instructions',
      },
    });
    expect(screen.queryByTestId('agent-hub-assets-section')).toBeNull();
    expect(screen.queryByTestId('agent-hub-lan-push-notice')).toBeNull();
    expect(screen.queryByTestId('agent-hub-project-opt-in-guard')).toBeNull();
  });

  test('local project instructions expose preview and explicit enable management', () => {
    const runPreviewProject = vi.fn(async () => undefined);
    const runEnableProject = vi.fn(async () => undefined);
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        scope: 'project',
        projectKey: 'wb-project-1',
        tab: 'instructions',
      },
      previewProjectId: 'wb-project-1',
      preview: {
        projectId: 'wb-project-1',
        hubProjectId: 'hub-project-1',
        optedIn: false,
        checkouts: [],
        plannedActions: [],
      },
      runPreviewProject,
      runEnableProject,
    });

    expect(screen.getByTestId('agent-hub-project-management')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Run preview' }));
    fireEvent.click(screen.getByRole('button', { name: 'Enable project' }));
    expect(runPreviewProject).toHaveBeenCalledOnce();
    expect(runEnableProject).toHaveBeenCalledOnce();
    expect(screen.queryByTestId('instruction-three-pane')).toBeNull();
  });

  test('remote device keeps management task callout without rendering local inventory', () => {
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        deviceId: 'peer-a',
        tab: 'skill',
      },
    });
    expect(screen.getByTestId('agent-hub-remote-management')).toBeTruthy();
    expect(screen.queryByTestId('portable-inventory-workspace')).toBeNull();
  });

  test('online peer with user-instructions capability mounts three-pane', () => {
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        deviceId: 'peer-a',
        tab: 'instructions',
      },
      shellPeers: [
        {
          deviceId: 'peer-a',
          name: 'Peer A',
          online: true,
          capabilities: [AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY],
        },
      ],
      instructionThreePane: stubThreePane(),
    });
    expect(screen.getByTestId('instruction-three-pane')).toBeTruthy();
    expect(screen.queryByTestId('agent-hub-remote-management')).toBeNull();
  });

  test('online peer missing user-instructions capability keeps remote hint', () => {
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        deviceId: 'peer-a',
        tab: 'instructions',
      },
      shellPeers: [{ deviceId: 'peer-a', name: 'Peer A', online: true, capabilities: [] }],
      instructionThreePane: stubThreePane(),
    });
    expect(screen.getByTestId('agent-hub-remote-management')).toBeTruthy();
    expect(screen.queryByTestId('instruction-three-pane')).toBeNull();
  });

  test('offline peer with user-instructions capability keeps remote hint', () => {
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        deviceId: 'peer-a',
        tab: 'instructions',
      },
      shellPeers: [
        {
          deviceId: 'peer-a',
          name: 'Peer A',
          online: false,
          capabilities: [AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY],
        },
      ],
      instructionThreePane: stubThreePane(),
    });
    expect(screen.getByTestId('agent-hub-remote-management')).toBeTruthy();
    expect(screen.queryByTestId('instruction-three-pane')).toBeNull();
  });

  test('remote project instructions keep hint and never mount three-pane', () => {
    renderView({
      hubContext: {
        ...buildProps().hubContext,
        scope: 'project',
        projectKey: 'remote:peer-a:inner',
        deviceId: null,
        tab: 'instructions',
      },
      shellPeers: [
        {
          deviceId: 'peer-a',
          name: 'Peer A',
          online: true,
          capabilities: [AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY],
        },
      ],
      instructionThreePane: stubThreePane(),
    });
    expect(screen.getByTestId('agent-hub-remote-management')).toBeTruthy();
    expect(screen.queryByTestId('instruction-three-pane')).toBeNull();
  });

  test('URL migration notice is visible without exposing a legacy action', () => {
    renderView({ contextMigrationNotice: 'Legacy link migrated safely.' });
    expect(screen.getByTestId('agent-hub-context-migration-notice').textContent).toContain(
      'Legacy link migrated safely.',
    );
    expect(screen.queryByTestId('agent-hub-legacy-error')).toBeNull();
  });

  test('shell tabs expose assets workspace, reset lane, and toolbar opens pull/push controls', () => {
    const onContextChange = vi.fn();
    const openPortablePull = vi.fn();
    const openLanPushDialog = vi.fn();
    renderView({
      onContextChange,
      openPortablePull,
      openLanPushDialog,
      activeSection: 'assets',
      hubContext: {
        agent: 'claude',
        scope: 'user',
        deviceId: null,
        projectKey: null,
        tab: 'instructions',
        instructionLane: 'common',
        assetLane: 'equipped',
        adaptView: false,
      },
    });
    expect(screen.getByTestId('agent-hub-shell')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-tab-skill')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-action-pull')).toBeTruthy();
    expect(screen.getByTestId('agent-hub-action-push')).toBeTruthy();
    fireEvent.click(screen.getByTestId('agent-hub-action-pull'));
    expect(openPortablePull).toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('agent-hub-action-push'));
    expect(openLanPushDialog).toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('agent-hub-tab-skill'));
    expect(onContextChange).toHaveBeenCalledWith({
      tab: 'skill',
      instructionLane: 'exclusive',
    });
    expect(screen.queryByTestId('agent-hub-section-assets')).toBeNull();
  });
});
