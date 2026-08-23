/**
 * Agent Hub 页面 — Multi-CLI 指令与资产工作区。
 *
 * Business Logic（为什么需要这个页面）:
 *   统一管理 Claude / Codex / OpenCode 指令与 portable 资产投影、冲突与项目 opt-in。
 *
 * Code Logic（这个页面做什么）:
 *   controller 持有数据/动作；AgentHubView 为 pure 视图（禁止 @/api/*）。
 */

import { useCallback, useEffect, useMemo } from 'react';
import { Button, StatusMessage } from '@/components/primitives';
import { GitImportDrawer } from './GitImportDrawer';
import {
  InstructionThreePaneView,
  type InstructionThreePaneViewLabels,
  type UseInstructionThreePaneControllerResult,
} from './instructions';
import {
  ProjectInstructionFilesView,
  type ProjectInstructionFilesViewLabels,
  type UseProjectInstructionFilesControllerResult,
} from './projectInstructions';
import {
  PortableAssetActionDialog,
  PortableInventoryView,
  portableBorrowedOwnerJumpTarget,
  type PortableInventoryViewLabels,
} from './portableAssets';
import { UserMirrorDialog } from './userMirror/UserMirrorDialog';
import { allHubTargets } from '@/lib/agentCatalog';
import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import { AgentHubShell } from './shell';
import { CrossAgentAdaptPage } from './crossAgent';
import {
  isPortableStoreTab,
  peerAllowsUserInstructionThreePane,
  peerAllowsUserPortableInventory,
} from './context/agentHubContext';
import {
  isAssetKindTab,
  useAgentHubController,
  type UseAgentHubControllerResult,
} from './useAgentHubController';
import { useAgentHubSession } from './useAgentHubSession';
import styles from './AgentHub.module.css';

/**
 * pure 视图 props（characterization 测试注入）。
 * instructionThreePane 在入口 hook 注入；测试可不传。
 */
export type AgentHubViewProps = UseAgentHubControllerResult & {
  instructionThreePane?: UseInstructionThreePaneControllerResult | null;
  projectInstructionFiles?: UseProjectInstructionFilesControllerResult | null;
  /** Workbench 嵌入时隐藏页面 H1，并使用铺满高度的布局。 */
  embedded?: boolean;
  /** 生产 Agent Hub 锁定 user；Workbench 项目 Agent 锁定 project。 */
  scopeLock?: 'user' | 'project';
  /** 文件工作区有未保存标签时的提示（可选）。 */
  unsavedFilesNotice?: string | null;
};

/**
 * Business Logic: 可测试的 pure 页面视图。
 * Code Logic: 只渲染 props；hooks 仅 useTranslation/useMemo/useCallback/useEffect。
 */
export function AgentHubView(props: AgentHubViewProps) {
  const {
    t,
    hubContext,
    contextMigrationNotice,
    onContextChange,
    shellPeers,
    portableInventory,
    portableActionOpen,
    portableActionKind,
    portableActionPlan,
    portableActionResult,
    portableActionBusy,
    portableActionError,
    portableActionClientRequestId,
    previewPortableAction,
    confirmPortableAction,
    reconcilePortableAction,
    closePortableAction,
    openUserMirrorPull,
    openUserMirrorPush,
    closeUserMirror,
    userMirrorOpen,
    userMirror,
    actionError,
    actionBusy,
    setInstructionRefresh,
    gitImportOpen,
    openGitImportDrawer,
    closeGitImportDrawer,
    gitInspectReport,
    gitSelectedLaneDeviceId,
    selectGitLane,
    gitPreview,
    gitSelectedAssetIds,
    gitAssetSelectionExplicit,
    toggleGitAsset,
    gitMappingDrafts,
    setGitMappingDraft,
    gitConfirmOutcome,
    gitLastMapping,
    runGitInspect,
    runGitPreview,
    runGitConfirmMapping,
    runGitConfirmImport,
    reload,
    writeBlocked,
    upgradeRequired,
    instructionThreePane,
    projectInstructionFiles,
    embedded = false,
    scopeLock,
    unsavedFilesNotice,
  } = props;
  const resolvedScopeLock =
    scopeLock ?? (hubContext.scope === 'project' ? 'project' : 'user');
  const projectLocked = resolvedScopeLock === 'project';

  const instructionApplyHasFailure = Boolean(
    instructionThreePane?.applyResult?.targets.some((target) =>
      ['stalePreview', 'blocked', 'conflict', 'failed'].includes(target.status),
    ),
  );

  const instructionThreePaneLabels: InstructionThreePaneViewLabels = useMemo(
    () => ({
      blocksTitle: t('agentHub:instructions.threePane.blocksTitle'),
      previewTitle: t('agentHub:instructions.threePane.previewTitle'),
      originalTitle: t('agentHub:instructions.threePane.originalTitle'),
      analyzeDecompose: t('agentHub:instructions.threePane.analyzeDecompose'),
      emptyBlocks: t('agentHub:instructions.threePane.emptyBlocks'),
      emptyPreview: t('agentHub:instructions.threePane.emptyPreview'),
      emptyOriginal: t('agentHub:instructions.threePane.emptyOriginal'),
      pathLabel: t('agentHub:instructions.threePane.pathLabel'),
      noPath: t('agentHub:instructions.threePane.noPath'),
      loading: t('agentHub:instructions.threePane.loading'),
      retry: t('common:action.retry'),
      previewReadOnly: t('agentHub:instructions.threePane.previewReadOnly'),
      slotCommonHint: t('agentHub:instructions.threePane.slotCommonHint'),
      slotAdaptedHint: t('agentHub:instructions.threePane.slotAdaptedHint'),
      slotExclusiveHint: t('agentHub:instructions.threePane.slotExclusiveHint'),
      dualDirtyTitle: t('agentHub:instructions.threePane.dualDirtyTitle'),
      dualDirtyDescription: t('agentHub:instructions.threePane.dualDirtyDescription'),
      useBlocksBaseline: t('agentHub:instructions.threePane.useBlocksBaseline'),
      useOriginalBaseline: t('agentHub:instructions.threePane.useOriginalBaseline'),
      cancel: t('common:action.cancel'),
      blockBodyPlaceholder: t('agentHub:instructions.threePane.blockBodyPlaceholder'),
      commonMarkdown: t('agentHub:instructions.threePane.commonMarkdown'),
      saveBlocks: t('agentHub:instructions.threePane.saveBlocks'),
      aiRevise: t('agentHub:instructions.threePane.aiRevise'),
      aiReviseTitle: t('agentHub:instructions.threePane.aiReviseTitle'),
      aiReviseDescriptionCommon: t('agentHub:instructions.threePane.aiReviseDescriptionCommon'),
      aiReviseDescriptionExclusive: t(
        'agentHub:instructions.threePane.aiReviseDescriptionExclusive',
      ),
      aiReviseDescriptionAdapted: t('agentHub:instructions.threePane.aiReviseDescriptionAdapted'),
      aiReviseDirectionLabel: t('agentHub:instructions.threePane.aiReviseDirectionLabel'),
      aiReviseDirectionPlaceholder: t(
        'agentHub:instructions.threePane.aiReviseDirectionPlaceholder',
      ),
      aiReviseConfirm: t('agentHub:instructions.threePane.aiReviseConfirm'),
      aiReviseSavedAndLocated: t(
        'agentHub:instructions.threePane.aiReviseSavedAndLocated',
      ),
      aiReviseSavedOtherAgents: t(
        'agentHub:instructions.threePane.aiReviseSavedOtherAgents',
      ),
      aiReviseSavedNoChange: t('agentHub:instructions.threePane.aiReviseSavedNoChange'),
      adaptToOtherAgents: t('agentHub:instructions.threePane.adaptToOtherAgents'),
      syncToNative: t('agentHub:instructions.threePane.syncToNative'),
      unsavedDraft: t('agentHub:instructions.threePane.unsavedDraft'),
      canonicalDrift: t('agentHub:instructions.threePane.canonicalDrift'),
      sourceDrift: t('agentHub:instructions.threePane.sourceDrift'),
      originalReadOnly: t('agentHub:instructions.threePane.originalReadOnly'),
      discardAndReload: t('agentHub:instructions.threePane.discardAndReload'),
      analyzeConfirmTitle: t('agentHub:instructions.threePane.analyzeConfirmTitle'),
      analyzeConfirmDescription: t(
        'agentHub:instructions.threePane.analyzeConfirmDescription',
      ),
      analyzeConfirm: t('agentHub:instructions.threePane.analyzeConfirm'),
      slotHistoryCommon: t('agentHub:instructions.threePane.slotHistoryCommon'),
      slotHistoryAdapted: t('agentHub:instructions.threePane.slotHistoryAdapted'),
      slotHistoryTargetOnly: t('agentHub:instructions.threePane.slotHistoryTargetOnly'),
      slotHistoryCopied: t('agentHub:userInstructions.errors.slotVersionCopyEmpty'),
    }),
    [t],
  );

  const projectInstructionFilesLabels: ProjectInstructionFilesViewLabels = useMemo(
    () => ({
      title: t('agentHub:instructions.projectFiles.title'),
      loading: t('agentHub:instructions.projectFiles.loading'),
      retry: t('common:action.retry'),
      save: t('agentHub:instructions.projectFiles.save'),
      unsaved: t('agentHub:instructions.projectFiles.unsaved'),
      missing: t('agentHub:instructions.projectFiles.missing'),
      editorAria: t('agentHub:instructions.projectFiles.editorAria'),
      placeholder: t('agentHub:instructions.projectFiles.placeholder'),
      pathLabel: t('agentHub:instructions.projectFiles.pathLabel'),
      filesAria: t('agentHub:instructions.projectFiles.filesAria'),
      truncated: t('agentHub:instructions.projectFiles.truncated'),
      sharedBy: (agents: string) =>
        t('agentHub:instructions.projectFiles.sharedBy', { agents }),
      exclusiveTo: (agent: string) =>
        t('agentHub:instructions.projectFiles.exclusiveTo', { agent }),
      agentSeparator: t('agentHub:instructions.projectFiles.agentSeparator'),
      agentName: (agent) => t(`agentHub:targets.${agent}`),
    }),
    [t],
  );

  const portableInventoryLabels: PortableInventoryViewLabels = useMemo(
    () => {
      const storeCatalog =
        isPortableStoreTab(hubContext.tab) && hubContext.assetLane === 'store';
      return {
      title: storeCatalog
        ? t('agentHub:portable.inventory.storeTitle')
        : t('agentHub:portable.inventory.title'),
      subtitle: storeCatalog
        ? t('agentHub:portable.inventory.storeSubtitle')
        : t('agentHub:portable.inventory.subtitle'),
      loading: t('agentHub:portable.inventory.loading'),
      empty: storeCatalog
        ? t('agentHub:portable.inventory.emptyStore')
        : t('agentHub:portable.inventory.empty'),
      migrateAllToStore: t('agentHub:portable.inventory.migrateAllToStore', {
        count: portableInventory.migratableToStoreItems.length,
      }),
      confirmAllVersions: t('agentHub:portable.inventory.confirmAllVersions', {
        count: portableInventory.confirmableCurrentVersionItems.length,
      }),
      materializeAllEscapeLinks: t('agentHub:portable.inventory.materializeAllEscapeLinks', {
        count: portableInventory.materializableEscapeLinkItems.length,
      }),
      retry: t('agentHub:portable.inventory.retry'),
      staleBanner: t('agentHub:portable.inventory.staleBanner'),
      searchPlaceholder: t('agentHub:portable.inventory.searchPlaceholder'),
      filterActual: t('agentHub:portable.inventory.filterActual'),
      filterManagement: t('agentHub:portable.inventory.filterManagement'),
      actualFilter: {
        all: t('agentHub:portable.inventory.actualFilter.all'),
        enabled: t('agentHub:portable.inventory.actualFilter.enabled'),
        disabled: t('agentHub:portable.inventory.actualFilter.disabled'),
        problem: t('agentHub:portable.inventory.actualFilter.problem'),
      },
      managementFilter: {
        all: t('agentHub:portable.inventory.managementFilter.all'),
        hubManaged: t('agentHub:portable.inventory.managementFilter.hubManaged'),
        drifted: t('agentHub:portable.inventory.managementFilter.drifted'),
        externalCollision: t('agentHub:portable.inventory.managementFilter.externalCollision'),
        unsupported: t('agentHub:portable.inventory.managementFilter.unsupported'),
        unmanaged: t('agentHub:portable.inventory.managementFilter.unmanaged'),
      },
      targets: Object.fromEntries(
        allHubTargets().map((target) => [target, t(`agentHub:targets.${target}`)]),
      ),
      kinds: {
        skill: t('agentHub:kinds.skill'),
        command: t('agentHub:kinds.command'),
        plugin: t('agentHub:kinds.plugin'),
        mcp: t('agentHub:kinds.mcp'),
      },
      actual: {
        enabled: t('agentHub:portable.inventory.actual.enabled'),
        disabled: t('agentHub:portable.inventory.actual.disabled'),
        problem: t('agentHub:portable.inventory.actual.problem'),
        unknown: t('agentHub:portable.inventory.actual.unknown'),
      },
      management: {
        hubManaged: t('agentHub:portable.inventory.management.hubManaged'),
        drifted: t('agentHub:portable.inventory.management.drifted'),
        externalCollision: t('agentHub:portable.inventory.management.externalCollision'),
        unsupported: t('agentHub:portable.inventory.management.unsupported'),
        unmanaged: t('agentHub:portable.inventory.management.unmanaged'),
      },
      scope: {
        user: t('agentHub:portable.inventory.scope.user'),
        project: t('agentHub:portable.inventory.scope.project'),
        directory: t('agentHub:portable.inventory.scope.directory'),
      },
      actions: {
        adopt: t('agentHub:portable.actions.adopt'),
        enable: t('agentHub:portable.actions.enable'),
        disable: t('agentHub:portable.actions.disable'),
        uninstall: t('agentHub:portable.actions.uninstall'),
        installToSourceTarget: t('agentHub:portable.actions.installToSourceTarget'),
        attach: t('agentHub:portable.actions.attach'),
        detach: t('agentHub:portable.actions.detach'),
        destroyStore: t('agentHub:portable.actions.destroyStore'),
        migrateToStore: t('agentHub:portable.actions.migrateToStore'),
        confirmCurrentVersion: t('agentHub:portable.actions.confirmCurrentVersion'),
        materializeEscapeLink: t('agentHub:portable.actions.materializeEscapeLink'),
      },
      sourceOrigin: {
        standalone: t('agentHub:portable.inventory.sourceOrigin.standalone'),
        pluginComponent: t('agentHub:portable.inventory.sourceOrigin.pluginComponent'),
        nativeConfig: t('agentHub:portable.inventory.sourceOrigin.nativeConfig'),
      },
      unmanagedRefreshHint: t('agentHub:portable.inventory.unmanagedRefreshHint'),
      groupInstalled: t('agentHub:portable.inventory.groupInstalled'),
      groupBorrowed: t('agentHub:portable.inventory.groupBorrowed'),
      groupStoreAttached: t('agentHub:portable.inventory.groupStoreAttached'),
      groupStoreAvailable: t('agentHub:portable.inventory.groupStoreAvailable'),
      storeBadge: t('agentHub:portable.inventory.storeBadge'),
      storeAgentGroupAria: t('agentHub:portable.inventory.storeAgentGroupAria'),
      storeAgentToggleAria: Object.fromEntries(
        allHubTargets().map((target) => [
          target,
          t('agentHub:portable.inventory.storeAgentToggle', {
            agent: t(`agentHub:targets.${target}`),
          }),
        ]),
      ),
      storeEnabledVia: Object.fromEntries(
        allHubTargets().map((target) => [
          target,
          t('agentHub:portable.inventory.storeEnabledVia', {
            agent: t(`agentHub:targets.${target}`),
          }),
        ]),
      ),
      emptyRuntimeHint: t('agentHub:portable.inventory.emptyRuntimeHint'),
      openInOwnerAgent: t('agentHub:portable.inventory.openInOwnerAgent'),
      borrowedFrom: {
        claude: t('agentHub:portable.inventory.borrowedFrom.claude'),
        codex: t('agentHub:portable.inventory.borrowedFrom.codex'),
        opencode: t('agentHub:portable.inventory.borrowedFrom.opencode'),
        grok: t('agentHub:portable.inventory.borrowedFrom.grok'),
        gemini: t('agentHub:portable.inventory.borrowedFrom.gemini'),
        cursor: t('agentHub:portable.inventory.borrowedFrom.cursor'),
        pi: t('agentHub:portable.inventory.borrowedFrom.pi'),
        sharedAgents: t('agentHub:portable.inventory.borrowedFrom.sharedAgents'),
        portableStore: t('agentHub:portable.inventory.borrowedFrom.portableStore'),
        unknown: t('agentHub:portable.inventory.borrowedFrom.unknown'),
      },
    };
    },
    [
      hubContext.assetLane,
      hubContext.tab,
      portableInventory.confirmableCurrentVersionItems.length,
      portableInventory.migratableToStoreItems.length,
      portableInventory.materializableEscapeLinkItems.length,
      t,
    ],
  );

  /**
   * Business Logic: 借用项可在当前列表启停/卸载；也可切到实际加载源 Agent。
   * Code Logic: Hub target 切 agent；`~/.agents` 写盘走 Codex；无 jump target 不渲染按钮。
   */
  const openPortableOwner = useCallback(
    (item: PortableInventoryItemDto) => {
      const owner = portableBorrowedOwnerJumpTarget(item);
      if (!owner) return;
      onContextChange({ agent: owner });
    },
    [onContextChange],
  );

  const isAssetTab = isAssetKindTab(hubContext.tab);
  const isRemoteContext =
    hubContext.deviceId !== null || hubContext.projectKey?.startsWith('remote:') === true;
  const isRemoteProject = hubContext.projectKey?.startsWith('remote:') === true;
  const canUseGitImport =
    !projectLocked && hubContext.scope === 'user' && hubContext.deviceId === null;
  const selectedPeer =
    hubContext.deviceId === null
      ? null
      : shellPeers.find((peer) => peer.deviceId === hubContext.deviceId) ?? null;
  /** 用户级对端在线且宣告 user-instructions 才挂三栏；否则保持远端 hint。 */
  const canMountRemoteUserThreePane =
    hubContext.scope === 'user' &&
    hubContext.deviceId !== null &&
    peerAllowsUserInstructionThreePane(selectedPeer);
  /** 用户级对端在线且宣告 portable-user 才挂资产主列表。 */
  const canMountRemoteUserPortable =
    hubContext.scope === 'user' &&
    hubContext.deviceId !== null &&
    peerAllowsUserPortableInventory(selectedPeer);
  const showProjectInstructionFiles =
    projectLocked &&
    hubContext.tab === 'instructions' &&
    hubContext.projectKey !== null &&
    Boolean(projectInstructionFiles);
  const showInstructionThreePane =
    !projectLocked &&
    hubContext.tab === 'instructions' &&
    hubContext.scope === 'user' &&
    Boolean(instructionThreePane) &&
    (!isRemoteContext || canMountRemoteUserThreePane);
  const showRemoteUserPortable =
    isAssetTab && canMountRemoteUserPortable;
  const canReloadCurrentTab =
    (isAssetTab && (!isRemoteContext || isRemoteProject || canMountRemoteUserPortable)) ||
    showInstructionThreePane ||
    showProjectInstructionFiles;

  /**
   * Business Logic: 用户级壳层保留 Pull/Push；项目 Agent 资产随项目走，不提供跨设备复制。
   *   提示词三栏 / 项目文件与资产列表共用壳层「刷新」，不再另开「刷新库存」。
   * Code Logic: 跨 Agent 适配入口改到适配页保存旁按钮，壳层不再提供。
   */
  const shellActions = useMemo(
    () => {
      const reloadAction = canReloadCurrentTab
        ? {
            onReload: () => {
              void reload();
            },
            reloadBusy:
              hubContext.tab === 'instructions'
                ? Boolean(
                    showProjectInstructionFiles
                      ? projectInstructionFiles?.refreshing
                      : instructionThreePane?.refreshing,
                  )
                : portableInventory.refreshing,
          }
        : {};
      if (projectLocked) {
        return {
          onPull: () => undefined,
          onPush: () => undefined,
          pullDisabledReason: null,
          pushDisabledReason: null,
          ...reloadAction,
        };
      }
      return {
        onPull: openUserMirrorPull,
        onPush: openUserMirrorPush,
        ...(canUseGitImport ? { onGitImport: openGitImportDrawer } : {}),
        pullDisabledReason: null,
        pushDisabledReason: null,
        ...reloadAction,
      };
    },
    [
      canReloadCurrentTab,
      canUseGitImport,
      hubContext.tab,
      openGitImportDrawer,
      openUserMirrorPull,
      openUserMirrorPush,
      portableInventory.refreshing,
      projectInstructionFiles?.refreshing,
      projectLocked,
      reload,
      showProjectInstructionFiles,
      instructionThreePane?.refreshing,
    ],
  );

  /**
   * Business Logic: 适配页优先使用三栏当前 original / preview 正文。
   * Code Logic: original 优先，其次合成 preview；皆空则交 controller inspect。
   */
  const adaptInitialMarkdown = useMemo(() => {
    const state = instructionThreePane?.state;
    if (!state) return null;
    if (state.originalText.trim().length > 0) return state.originalText;
    if (state.previewText.trim().length > 0) return state.previewText;
    return null;
  }, [instructionThreePane?.state]);

  /**
   * Business Logic: 把当前提示词 refresh 注入 hub controller，供壳层刷新分发。
   * Code Logic: 项目文件优先，其次用户级三栏；mount/update 写 ref；卸载清空。
   */
  useEffect(() => {
    if (projectLocked && projectInstructionFiles) {
      const refresh = (): Promise<void> => projectInstructionFiles.refresh();
      setInstructionRefresh(refresh);
      return () => setInstructionRefresh(null);
    }
    if (instructionThreePane) {
      const refresh = (): Promise<void> => instructionThreePane.refresh();
      setInstructionRefresh(refresh);
      return () => setInstructionRefresh(null);
    }
    setInstructionRefresh(null);
    return () => setInstructionRefresh(null);
  }, [instructionThreePane, projectInstructionFiles, projectLocked, setInstructionRefresh]);

  // 按需加载：禁止整页 loading/error 闸门挡住 shell 与 portable tab

  if (hubContext.adaptView) {
    return (
      <div
        className={embedded ? styles.pageEmbedded : styles.page}
        data-testid="agent-hub-page"
        data-embedded={embedded || undefined}
      >
        <div className={styles.container}>
          {embedded ? null : (
            <header className={styles.header}>
              <div className={styles.titleBlock}>
                <h1 className={styles.title}>{t('agentHub:title')}</h1>
                <p className={styles.subtitle}>{t('agentHub:crossAgent.pageTitle')}</p>
              </div>
            </header>
          )}
          <CrossAgentAdaptPage
            context={hubContext}
            initialSourceMarkdown={adaptInitialMarkdown}
            onExit={() => onContextChange({ adaptView: false })}
          />
        </div>
      </div>
    );
  }

  return (
    <div
      className={embedded ? styles.pageEmbedded : styles.page}
      data-testid="agent-hub-page"
      data-embedded={embedded || undefined}
    >
      <div className={styles.container}>
        {embedded ? null : (
          <header className={styles.header}>
            <div className={styles.titleBlock}>
              <h1 className={styles.title}>{t('agentHub:title')}</h1>
              <p className={styles.subtitle}>{t('agentHub:subtitle')}</p>
            </div>
          </header>
        )}

        {unsavedFilesNotice ? (
          <StatusMessage tone="info" data-testid="agent-hub-unsaved-files-notice">
            {unsavedFilesNotice}
          </StatusMessage>
        ) : null}

        {contextMigrationNotice ? (
          <StatusMessage tone="info" data-testid="agent-hub-context-migration-notice">
            {contextMigrationNotice}
          </StatusMessage>
        ) : null}

        <AgentHubShell
          context={hubContext}
          onContextChange={onContextChange}
          actions={shellActions}
          peers={shellPeers}
          tabCounts={portableInventory.kindCounts}
          scopeLock={resolvedScopeLock}
        >
        {showInstructionThreePane && instructionThreePane ? (
          <InstructionThreePaneView
            labels={instructionThreePaneLabels}
            state={instructionThreePane.state}
            agent={hubContext.agent}
            instructionLane={hubContext.instructionLane}
            loading={instructionThreePane.loading}
            error={instructionThreePane.error}
            actionError={instructionThreePane.actionError}
            actionBusy={instructionThreePane.actionBusy}
            busyAction={instructionThreePane.busyAction}
            writeBlocked={instructionThreePane.writeBlocked}
            writeBlockedReason={instructionThreePane.writeBlockedReason}
            dualDirtyOpen={instructionThreePane.dualDirtyOpen}
            analyzeConfirmOpen={instructionThreePane.analyzeConfirmOpen}
            aiReviseOpen={instructionThreePane.aiReviseOpen}
            aiReviseDirection={instructionThreePane.aiReviseDirection}
            aiReviseError={instructionThreePane.aiReviseError}
            aiReviseFeedback={instructionThreePane.aiReviseFeedback}
            aiReviseDisabled={instructionThreePane.aiReviseDisabled}
            onAnalyzeDecompose={instructionThreePane.analyzeDecompose}
            onAdaptToOtherAgents={() => {
              void instructionThreePane.adaptToOtherAgents();
            }}
            onSaveBlocks={() => {
              void instructionThreePane.saveBlocks();
            }}
            onOpenAiRevise={instructionThreePane.openAiRevise}
            onAiReviseDirectionChange={instructionThreePane.setAiReviseDirection}
            onCancelAiRevise={instructionThreePane.cancelAiRevise}
            onConfirmAiRevise={() => {
              void instructionThreePane.confirmAiRevise();
            }}
            onRequestSync={() => {
              void instructionThreePane.requestSync();
            }}
            onRetry={() => {
              void instructionThreePane.refresh();
            }}
            onDiscardAndReload={() => {
              void instructionThreePane.discardAndReload();
            }}
            onSlotTextChange={instructionThreePane.editCurrentSlot}
            onChooseBaseline={instructionThreePane.chooseBaseline}
            onCancelDualDirty={instructionThreePane.cancelDualDirty}
            onConfirmAnalyze={instructionThreePane.confirmAnalyzeDecompose}
            onCancelAnalyze={instructionThreePane.cancelAnalyzeDecompose}
            slotHistoryOpen={instructionThreePane.slotHistoryOpen}
            slotHistoryLoading={instructionThreePane.slotHistoryLoading}
            slotHistoryError={instructionThreePane.slotHistoryError}
            slotHistoryActionError={instructionThreePane.slotHistoryActionError}
            restoringSlotVersionId={instructionThreePane.restoringSlotVersionId}
            slotHistoryVersions={instructionThreePane.slotHistoryVersions}
            onOpenSlotHistory={() => {
              const slot =
                hubContext.instructionLane === 'common'
                  ? { kind: 'shared' as const }
                  : hubContext.instructionLane === 'adapted'
                    ? { kind: 'adapted' as const, agent: hubContext.agent }
                    : hubContext.instructionLane === 'exclusive'
                      ? { kind: 'targetOnly' as const, agent: hubContext.agent }
                      : null;
              if (!slot) return;
              instructionThreePane.openSlotHistory(slot);
            }}
            onCloseSlotHistory={instructionThreePane.closeSlotHistory}
            onCopySlotVersion={(version) => {
              void instructionThreePane.copySlotVersion(version);
            }}
            onRestoreSlotVersion={(version) => {
              void instructionThreePane.restoreSlotVersion(version);
            }}
          />
        ) : null}

        {hubContext.tab === 'instructions' && instructionThreePane?.applyResult ? (
          <StatusMessage
            tone={instructionApplyHasFailure ? 'warn' : 'success'}
            action={(
              <Button
                size="sm"
                variant="ghost"
                onClick={instructionThreePane.dismissApplyResult}
              >
                {t('common:action.confirm')}
              </Button>
            )}
            data-testid="instruction-three-pane-apply-result"
          >
            <span>
              {instructionApplyHasFailure
                ? t('agentHub:userInstructions.result.partial')
                : t('agentHub:userInstructions.result.success')}
            </span>
            <ul className={styles.userResultList}>
              {instructionThreePane.applyResult.targets.map((target) => (
                <li key={`${target.target}-${target.path}`}>
                  {t(`agentHub:targets.${target.target}`)} ·{' '}
                  {t(`agentHub:userInstructions.result.status.${target.status}`)} · {target.path}
                </li>
              ))}
            </ul>
          </StatusMessage>
        ) : null}

        {showProjectInstructionFiles && projectInstructionFiles ? (
          <ProjectInstructionFilesView
            labels={projectInstructionFilesLabels}
            controller={projectInstructionFiles}
            agent={hubContext.agent}
          />
        ) : null}

        {hubContext.tab === 'instructions' &&
        isRemoteContext &&
        hubContext.scope === 'user' &&
        !canMountRemoteUserThreePane ? (
          <StatusMessage tone="info" data-testid="agent-hub-remote-management">
            {t('agentHub:shell.remoteDeviceManageHint')}
          </StatusMessage>
        ) : null}

        {isAssetTab && (upgradeRequired || writeBlocked) ? (
          <StatusMessage tone="warn" data-testid="agent-hub-upgrade-required">
            {t('agentHub:upgradeRequired')}
          </StatusMessage>
        ) : null}

        {isAssetTab && actionError ? (
          <StatusMessage tone="danger" data-testid="agent-hub-action-error">
            {actionError}
          </StatusMessage>
        ) : null}

        {isAssetTab && isRemoteContext && !isRemoteProject && !showRemoteUserPortable ? (
          <StatusMessage tone="info" data-testid="agent-hub-remote-management">
            {hubContext.scope === 'user'
              ? t('agentHub:shell.remoteDeviceManageHint')
              : t('agentHub:shell.remoteProjectManageHint')}
          </StatusMessage>
        ) : null}

        {isAssetTab && showRemoteUserPortable ? (
          <StatusMessage tone="info" data-testid="agent-hub-remote-live">
            {t('agentHub:shell.remoteDeviceLiveHint')}
          </StatusMessage>
        ) : null}

        {isAssetTab && (!isRemoteContext || isRemoteProject || showRemoteUserPortable) ? (
          <div data-testid="agent-hub-assets-section">
            <PortableInventoryView
              controller={portableInventory}
              labels={portableInventoryLabels}
              onOpenOwner={openPortableOwner}
              hideScope={projectLocked}
            />
          </div>
        ) : null}
        </AgentHubShell>
      </div>

      {projectLocked ? null : (
      <UserMirrorDialog
        open={userMirrorOpen}
        direction={userMirror.direction}
        busy={userMirror.busy}
        error={userMirror.error}
        stale={userMirror.stale}
        devices={userMirror.devices.map((device) => ({
          deviceId: device.id,
          name: device.name,
        }))}
        sourceDeviceId={userMirror.sourceDeviceId}
        selectedPeerIds={userMirror.selectedPeerIds}
        plan={userMirror.plan}
        result={userMirror.result}
        confirmed={userMirror.confirmed}
        canApply={userMirror.canApply}
        canReconcile={userMirror.canReconcile}
        onSelectSourceDevice={(deviceId) => userMirror.selectSourceDevice(deviceId)}
        onTogglePeer={(deviceId) => userMirror.togglePeer(deviceId)}
        onConfirmChange={(value) => userMirror.setConfirmed(value)}
        onPreview={() => {
          void userMirror.preview();
        }}
        onApply={() => {
          void userMirror.apply();
        }}
        onReconcile={() => {
          void userMirror.reconcile();
        }}
        onClose={closeUserMirror}
      />
      )}

      {canUseGitImport ? (
        <GitImportDrawer
          open={gitImportOpen}
          busy={actionBusy}
          error={actionError}
          inspectReport={gitInspectReport}
          selectedLaneDeviceId={gitSelectedLaneDeviceId}
          preview={gitPreview}
          selectedAssetIds={gitSelectedAssetIds}
          hasExplicitAssetSelection={gitAssetSelectionExplicit}
          mappingDrafts={gitMappingDrafts}
          confirmOutcome={gitConfirmOutcome}
          lastMapping={gitLastMapping}
          onInspect={() => {
            void runGitInspect();
          }}
          onSelectLane={selectGitLane}
          onPreview={() => {
            void runGitPreview();
          }}
          onToggleAsset={toggleGitAsset}
          onMappingDraftChange={setGitMappingDraft}
          onConfirmMapping={(hubProjectId) => {
            void runGitConfirmMapping(hubProjectId);
          }}
          onConfirmImport={() => {
            void runGitConfirmImport();
          }}
          onClose={closeGitImportDrawer}
        />
      ) : null}

      <PortableAssetActionDialog
        open={portableActionOpen}
        items={
          portableInventory.pendingAction
            ? portableInventory.pendingAction.itemIds
                .map(
                  (itemId) =>
                    portableInventory.snapshot?.items.find(
                      (entry) => entry.inventoryItemId === itemId,
                    ) ?? null,
                )
                .filter((entry): entry is PortableInventoryItemDto => entry !== null)
            : []
        }
        action={portableActionKind}
        inventorySnapshotHash={portableInventory.snapshot?.inventorySnapshotHash ?? null}
        plan={portableActionPlan}
        result={portableActionResult}
        busy={portableActionBusy}
        error={portableActionError}
        clientRequestId={portableActionClientRequestId}
        mutationBlocked={portableInventory.mutationBlocked}
        stale={portableInventory.stale}
        onPreview={(request) => {
          void previewPortableAction(request);
        }}
        onConfirm={(planToken, clientRequestId) => {
          void confirmPortableAction(planToken, clientRequestId);
        }}
        onReconcile={(clientRequestId) => {
          void reconcilePortableAction(clientRequestId);
        }}
        onClose={closePortableAction}
      />



    </div>
  );
}

/**
 * 页面入口：注入 hub controller 与提示词 session（hooks 在 early return 前）。
 */
export function AgentHub() {
  const controller = useAgentHubController();
  const session = useAgentHubSession(controller);

  return (
    <>
      <AgentHubView
        {...controller}
        hubContext={session.committedHubContext}
        onContextChange={session.onContextChange}
        instructionThreePane={session.instructionThreePane}
        scopeLock="user"
      />
      {session.contextSwitchDialog}
    </>
  );
}
