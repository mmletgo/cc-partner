/**
 * Agent Hub 页面 — Multi-CLI 指令与资产工作区。
 *
 * Business Logic（为什么需要这个页面）:
 *   统一管理 Claude / Codex / OpenCode 指令与 portable 资产投影、冲突与项目 opt-in。
 *
 * Code Logic（这个页面做什么）:
 *   controller 持有数据/动作；AgentHubView 为 pure 视图（禁止 @/api/*）。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import { LanPushDialog } from './LanPushDialog';
import {
  InstructionThreePaneView,
  useInstructionThreePaneController,
  type InstructionThreePaneViewLabels,
  type UseInstructionThreePaneControllerResult,
} from './instructions';
import {
  PortableAssetActionDialog,
  PortableInventoryView,
  PortablePullDrawer,
  type PortableInventoryViewLabels,
} from './portableAssets';
import { allHubTargets, isHubTarget } from '@/lib/agentCatalog';
import type { PortableInventoryItemDto } from '@/lib/types/portableInventory';
import { AgentHubShell } from './shell';
import { CrossAgentAdaptPage } from './crossAgent';
import { isPortableStoreTab, peerAllowsUserInstructionThreePane } from './context/agentHubContext';
import {
  isAssetKindTab,
  useAgentHubController,
  type UseAgentHubControllerResult,
} from './useAgentHubController';
import styles from './AgentHub.module.css';

/**
 * pure 视图 props（characterization 测试注入）。
 * instructionThreePane 在入口 hook 注入；测试可不传。
 */
export type AgentHubViewProps = UseAgentHubControllerResult & {
  instructionThreePane?: UseInstructionThreePaneControllerResult | null;
};

/**
 * Business Logic: 可测试的 pure 页面视图。
 * Code Logic: 只渲染 props；hooks 仅 useTranslation/useMemo/useRef。
 */
export function AgentHubView(props: AgentHubViewProps) {
  const {
    t,
    hubContext,
    contextMigrationNotice,
    onContextChange,
    shellPeers,
    shellProjects,
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
    portablePullOpen,
    openPortablePull,
    closePortablePull,
    portablePull,
    preview,
    previewProjectId,
    runPreviewProject,
    runEnableProject,
    actionError,
    actionBusy,
    setInstructionRefresh,
    openLanPushDialog,
    closeLanPushDialog,
    lanPushOpen,
    lanPeers,
    lanSelectedPeerIds,
    toggleLanPeer,
    lanMode,
    setLanMode,
    lanAssetIdsText,
    setLanAssetIdsText,
    lanHubProjectIdsText,
    setLanHubProjectIdsText,
    lanPreview,
    lanReport,
    runLanPreview,
    runLanStart,
    reload,
    writeBlocked,
    upgradeRequired,
    instructionThreePane,
  } = props;

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
      refresh: t('agentHub:portable.inventory.refresh'),
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
   * Business Logic: 借用项可在当前列表启停/卸载；也可切到所有者 Agent。
   * Code Logic: Hub target 只切 agent；sharedAgents/unknown 无详情面，保持当前列表。
   */
  const openPortableOwner = useCallback(
    (item: PortableInventoryItemDto) => {
      if (!isHubTarget(item.ownedBy)) return;
      onContextChange({ agent: item.ownedBy });
    },
    [onContextChange],
  );

  /**
   * Business Logic: 壳层工具栏动作 — 复用现有 Pull/Push 抽屉。
   * Code Logic: 跨 Agent 适配入口改到适配页保存旁按钮，壳层不再提供。
   */
  const shellActions = useMemo(
    () => {
      const remoteProject =
        hubContext.scope === 'project' && hubContext.projectKey?.startsWith('remote:');
      const localProject =
        hubContext.scope === 'project' && hubContext.projectKey && !remoteProject;
      return {
        onPull: openPortablePull,
        onPush: openLanPushDialog,
        pullDisabledReason: null,
        pushDisabledReason: remoteProject
          ? t('agentHub:shell.remoteProjectTaskUnavailable')
          : localProject &&
              (!preview?.hubProjectId ||
                previewProjectId !== hubContext.projectKey ||
                preview.optedIn !== true)
            ? t('agentHub:shell.projectPushRequiresPreview')
            : null,
      };
    },
    [hubContext, openLanPushDialog, openPortablePull, preview, previewProjectId, t],
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

  const isAssetTab = isAssetKindTab(hubContext.tab);
  const isRemoteContext =
    hubContext.deviceId !== null || hubContext.projectKey?.startsWith('remote:') === true;
  const isRemoteProject = hubContext.projectKey?.startsWith('remote:') === true;
  const isLocalProject =
    hubContext.scope === 'project' &&
    hubContext.projectKey !== null &&
    !hubContext.projectKey.startsWith('remote:');
  const isProject = hubContext.scope === 'project' && hubContext.projectKey !== null;
  const selectedPeer =
    hubContext.deviceId === null
      ? null
      : shellPeers.find((peer) => peer.deviceId === hubContext.deviceId) ?? null;
  /** 用户级对端在线且宣告 user-instructions 才挂三栏；否则保持远端 hint。 */
  const canMountRemoteUserThreePane =
    hubContext.scope === 'user' &&
    hubContext.deviceId !== null &&
    peerAllowsUserInstructionThreePane(selectedPeer);
  const showInstructionThreePane =
    hubContext.tab === 'instructions' &&
    !isLocalProject &&
    Boolean(instructionThreePane) &&
    (!isRemoteContext || canMountRemoteUserThreePane);

  /**
   * Business Logic: 把三栏 refresh 注入 hub controller，供 header reload 分发。
   * Code Logic: mount/update 写 ref；卸载清空。
   */
  useEffect(() => {
    if (!instructionThreePane) {
      setInstructionRefresh(null);
      return;
    }
    const refresh = (): Promise<void> => instructionThreePane.refresh();
    setInstructionRefresh(refresh);
    return () => setInstructionRefresh(null);
  }, [instructionThreePane, setInstructionRefresh]);

  // 按需加载：禁止整页 loading/error 闸门挡住 shell 与 portable tab

  if (hubContext.adaptView) {
    return (
      <div className={styles.page} data-testid="agent-hub-page">
        <div className={styles.container}>
          <header className={styles.header}>
            <div className={styles.titleBlock}>
              <h1 className={styles.title}>{t('agentHub:title')}</h1>
              <p className={styles.subtitle}>{t('agentHub:crossAgent.pageTitle')}</p>
            </div>
          </header>
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
    <div className={styles.page} data-testid="agent-hub-page">
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.titleBlock}>
            <h1 className={styles.title}>{t('agentHub:title')}</h1>
            <p className={styles.subtitle}>{t('agentHub:subtitle')}</p>
          </div>
          <div className={styles.headerActions}>
            <Button
              variant="secondary"
              size="sm"
              loading={
                hubContext.tab === 'instructions'
                  ? Boolean(instructionThreePane?.refreshing)
                  : portableInventory.refreshing
              }
              onClick={() => void reload()}
              data-testid="agent-hub-reload"
            >
              {t('common:action.refresh')}
            </Button>
          </div>
        </header>

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
          projects={shellProjects}
          tabCounts={portableInventory.kindCounts}
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

        {hubContext.tab === 'instructions' && isProject ? (
          <section className={styles.previewResult} data-testid="agent-hub-project-management">
            <StatusMessage tone={preview?.optedIn ? 'success' : 'info'}>
              {preview?.optedIn
                ? t('agentHub:preview.projectEnabled')
                : t('agentHub:preview.projectManageDescription')}
            </StatusMessage>
            <div className={styles.dialogActions}>
              <Button
                variant="secondary"
                size="sm"
                loading={actionBusy}
                onClick={() => void runPreviewProject()}
              >
                {t('agentHub:preview.run')}
              </Button>
              <Button
                variant="primary"
                size="sm"
                disabled={
                  !preview ||
                  previewProjectId !== hubContext.projectKey ||
                  preview.optedIn === true
                }
                loading={actionBusy}
                onClick={() => void runEnableProject()}
              >
                {t('agentHub:preview.enable')}
              </Button>
            </div>
            {preview ? (
              <div className={styles.metaBlock}>
                <span>
                  {t('agentHub:preview.hubProjectId')}: {preview.hubProjectId ?? '—'}
                </span>
                <span>
                  {t('agentHub:preview.checkouts')}: {preview.checkouts?.length ?? 0}
                </span>
                <span>
                  {t('agentHub:preview.plannedActions')}: {preview.plannedActions?.length ?? 0}
                </span>
                <span>{preview.noCommitNotice ?? t('agentHub:preview.noCommitDefault')}</span>
              </div>
            ) : null}
          </section>
        ) : null}

        {hubContext.tab === 'instructions' && isRemoteContext && !canMountRemoteUserThreePane ? (
          <StatusMessage tone="info" data-testid="agent-hub-remote-management">
            {hubContext.scope === 'user'
              ? t('agentHub:shell.remoteDeviceManageHint')
              : t('agentHub:shell.remoteProjectManageHint')}
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

        {isAssetTab && isRemoteContext && !isRemoteProject ? (
          <StatusMessage tone="info" data-testid="agent-hub-remote-management">
            {hubContext.scope === 'user'
              ? t('agentHub:shell.remoteDeviceManageHint')
              : t('agentHub:shell.remoteProjectManageHint')}
          </StatusMessage>
        ) : null}

        {isAssetTab && (!isRemoteContext || isRemoteProject) ? (
          <div data-testid="agent-hub-assets-section">
            <PortableInventoryView
              controller={portableInventory}
              labels={portableInventoryLabels}
              onOpenOwner={openPortableOwner}
            />
          </div>
        ) : null}
        </AgentHubShell>
      </div>

      <LanPushDialog
        open={lanPushOpen}
        busy={actionBusy}
        error={actionError}
        peers={lanPeers}
        selectedPeerIds={lanSelectedPeerIds}
        onTogglePeer={toggleLanPeer}
        mode={lanMode}
        onModeChange={setLanMode}
        assetIdsText={lanAssetIdsText}
        onAssetIdsTextChange={setLanAssetIdsText}
        hubProjectIdsText={lanHubProjectIdsText}
        onHubProjectIdsTextChange={setLanHubProjectIdsText}
        preview={lanPreview}
        report={lanReport}
        onPreview={() => void runLanPreview()}
        onStart={() => void runLanStart()}
        onClose={closeLanPushDialog}
      />

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

      <PortablePullDrawer
        open={portablePullOpen}
        busy={portablePull.busy}
        error={portablePull.error}
        devices={portablePull.devices}
        selectedDeviceId={portablePull.selectedDeviceId}
        sourceTarget={portablePull.sourceTarget}
        remoteInventory={portablePull.remoteInventory}
        visibleItems={portablePull.visibleItems}
        selectedItemIds={portablePull.selectedItemIds}
        filters={portablePull.filters}
        conflictPolicy={portablePull.conflictPolicy}
        plan={portablePull.plan}
        result={portablePull.result}
        mutationBlocked={portablePull.mutationBlocked}
        canApply={portablePull.canApply}
        canReconcile={portablePull.canReconcile}
        onSelectDevice={(deviceId) => portablePull.selectDevice(deviceId)}
        onSelectSourceTarget={(target) => portablePull.selectSourceTarget(target)}
        onSetFilters={(filters) => portablePull.setFilters(filters)}
        onToggleItem={(id) => portablePull.toggleItem(id)}
        onSelectVisible={() => portablePull.selectVisible()}
        onSetConflictPolicy={(policy) => portablePull.setConflictPolicy(policy)}
        onLoadInventory={() => {
          void portablePull.loadInventory();
        }}
        onPreview={() => {
          void portablePull.preview();
        }}
        onApply={() => {
          void portablePull.apply();
        }}
        onReconcile={() => {
          void portablePull.reconcile();
        }}
        onClose={closePortablePull}
      />

    </div>
  );
}

/**
 * 页面入口：注入 hub controller 与提示词三栏 controller（hooks 在 early return 前）。
 */
export function AgentHub() {
  const controller = useAgentHubController();
  const {
    hubContext: requestedHubContext,
    onContextChange: navigateContext,
    t,
  } = controller;
  const [committedHubContext, setCommittedHubContext] = useState(requestedHubContext);
  const [pendingHubContext, setPendingHubContext] = useState<
    UseAgentHubControllerResult['hubContext'] | null
  >(null);
  const contextStayRef = useRef<HTMLButtonElement | null>(null);
  const selectedCommittedPeer =
    committedHubContext.deviceId === null
      ? null
      : controller.shellPeers.find((peer) => peer.deviceId === committedHubContext.deviceId) ??
        null;
  const instructionThreePane = useInstructionThreePaneController({
    context: committedHubContext,
    t,
    enabled:
      (committedHubContext.tab === 'instructions' || committedHubContext.adaptView) &&
      committedHubContext.scope === 'user' &&
      (committedHubContext.deviceId === null ||
        peerAllowsUserInstructionThreePane(selectedCommittedPeer)),
  });

  const committedFingerprint = JSON.stringify(committedHubContext);
  const requestedFingerprint = JSON.stringify(requestedHubContext);

  /**
   * Business Logic: browser back/forward 与直接 deep link 也必须经过同一脏稿守卫。
   * Code Logic: dirty 时立即把 URL 恢复到 committed context，并暂存 requested context；
   *   clean 时才提交。正文和 Shell 始终只消费 committed context。
   */
  useEffect(() => {
    if (requestedFingerprint === committedFingerprint) return;
    const timeoutId = window.setTimeout(() => {
      if (instructionThreePane.dirty) {
        navigateContext(committedHubContext);
        setPendingHubContext(requestedHubContext);
        return;
      }
      setCommittedHubContext(requestedHubContext);
    }, 0);
    return () => window.clearTimeout(timeoutId);
  }, [
    committedFingerprint,
    committedHubContext,
    instructionThreePane.dirty,
    navigateContext,
    requestedHubContext,
    requestedFingerprint,
  ]);

  const onContextChange = useCallback(
    (patch: Partial<UseAgentHubControllerResult['hubContext']>) => {
      const next = {
        ...committedHubContext,
        ...patch,
      };
      if (next.scope === 'user') next.projectKey = null;
      else next.deviceId = null;
      if (JSON.stringify(next) === committedFingerprint) return;
      if (instructionThreePane.dirty) {
        setPendingHubContext(next);
        return;
      }
      setCommittedHubContext(next);
      navigateContext(next);
    },
    [
      committedFingerprint,
      committedHubContext,
      instructionThreePane.dirty,
      navigateContext,
    ],
  );

  const stayInCommittedContext = useCallback(() => {
    setPendingHubContext(null);
    navigateContext(committedHubContext);
  }, [committedHubContext, navigateContext]);

  const commitPendingContext = useCallback(() => {
    if (!pendingHubContext) return;
    instructionThreePane.discardDraftForContextChange();
    setCommittedHubContext(pendingHubContext);
    navigateContext(pendingHubContext);
    setPendingHubContext(null);
  }, [instructionThreePane, navigateContext, pendingHubContext]);

  const saveAndCommitPendingContext = useCallback(async () => {
    if (!pendingHubContext) return;
    const saved = await instructionThreePane.saveBlocks();
    if (!saved) return;
    setCommittedHubContext(pendingHubContext);
    navigateContext(pendingHubContext);
    setPendingHubContext(null);
  }, [instructionThreePane, navigateContext, pendingHubContext]);

  return (
    <>
      <AgentHubView
        {...controller}
        hubContext={committedHubContext}
        onContextChange={onContextChange}
        instructionThreePane={instructionThreePane}
      />
      <Dialog
        open={pendingHubContext !== null}
        titleId="agent-hub-context-change-title"
        onClose={stayInCommittedContext}
        initialFocusRef={contextStayRef}
      >
        <div className={styles.dialogBody} data-testid="agent-hub-context-change-dialog">
          <h2 id="agent-hub-context-change-title" className={styles.drawerTitle}>
            {t('agentHub:instructions.threePane.contextSwitchTitle')}
          </h2>
          <p className={styles.drawerSubtitle}>
            {t('agentHub:instructions.threePane.contextSwitchWarning')}
          </p>
          <div className={styles.dialogActions}>
            <Button
              ref={contextStayRef}
              variant="primary"
              size="sm"
              onClick={stayInCommittedContext}
              data-testid="agent-hub-context-stay"
            >
              {t('agentHub:instructions.threePane.contextStay')}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              disabled={
                instructionThreePane.actionBusy ||
                !instructionThreePane.state.blocksDirty ||
                instructionThreePane.state.externalDrift
              }
              loading={instructionThreePane.busyAction === 'save'}
              onClick={() => void saveAndCommitPendingContext()}
              data-testid="agent-hub-context-save"
            >
              {t('agentHub:instructions.threePane.contextSave')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              disabled={instructionThreePane.actionBusy}
              onClick={commitPendingContext}
              data-testid="agent-hub-context-discard"
            >
              {t('agentHub:instructions.threePane.contextDiscard')}
            </Button>
          </div>
        </div>
      </Dialog>
    </>
  );
}
