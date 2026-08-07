/**
 * Agent Hub 页面 — Multi-CLI 指令与资产工作区。
 *
 * Business Logic（为什么需要这个页面）:
 *   统一管理 Claude / Codex / OpenCode 指令与 portable 资产投影、冲突与项目 opt-in。
 *
 * Code Logic（这个页面做什么）:
 *   controller 持有数据/动作；AgentHubView 为 pure 视图（禁止 @/api/*）。
 */

import { useMemo, useRef } from 'react';
import { AgentAssetRow } from '@/components/domain/AgentAssetRow';
import { Button, Card, Dialog, Drawer, Input, Pill, StatusMessage } from '@/components/primitives';
import type {
  AgentHubAssetDetail,
  AgentHubAssetSummary,
  AgentHubProbe,
  AgentHubProjectPreview,
  AgentHubStatus,
  AgentTarget,
} from '@/lib/types/agentHub';
import { AssetAdoptionDialog } from './AssetAdoptionDialog';
import { GitImportDrawer } from './GitImportDrawer';
import { LanPushDialog } from './LanPushDialog';
import { InstructionBlocksDrawer } from './InstructionBlocksDrawer';
import { PluginComponentsDrawer } from './PluginComponentsDrawer';
import { UserInstructionView } from './userInstructions/UserInstructionView';
import {
  PortableAssetActionDialog,
  PortableAssetDetailsDrawer,
  PortableInventoryView,
  PortablePullDrawer,
  type PortableInventoryViewLabels,
  type PortablePluginDetailsSummary,
} from './portableAssets';
import { summarizeDeletePreview } from './pluginPackagePresentation';
import {
  useAgentHubController,
  type UseAgentHubControllerResult,
} from './useAgentHubController';
import styles from './AgentHub.module.css';

/**
 * pure 视图 props（characterization 测试注入）。
 */
export type AgentHubViewProps = UseAgentHubControllerResult;

/**
 * Business Logic: probe.support 映射 tone。
 * Code Logic: supported/full → success；scanOnly/partial → warn；否则 danger。
 */
function supportTone(support: string): 'success' | 'warn' | 'danger' | 'neutral' {
  if (support === 'supported' || support === 'full') return 'success';
  if (support === 'scanOnly' || support === 'partial') return 'warn';
  if (support === 'unsupported') return 'danger';
  return 'neutral';
}

/**
 * Business Logic: 可测试的 pure 页面视图。
 * Code Logic: 只渲染 props；hooks 仅 useTranslation/useMemo/useRef。
 */
export function AgentHubView(props: AgentHubViewProps) {
  const {
    t,
    activeSection,
    setActiveSection,
    userInstructions,
    portableInventory,
    portableDetailsOpen,
    portableSelectedItem,
    closePortableDetails,
    requestPortableAction,
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
    loading,
    refreshing,
    stale,
    error,
    actionError,
    actionBusy,
    status,
    filteredAssets,
    scopeFilter,
    kindFilter,
    setScopeFilter,
    setKindFilter,
    selectedAssetId,
    selectedAsset,
    selectAsset,
    preview,
    previewOpen,
    previewProjectId,
    setPreviewProjectId,
    openPreviewDialog,
    closePreviewDialog,
    runPreviewProject,
    runEnableProject,
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
    openGitImportDrawer,
    closeGitImportDrawer,
    gitImportOpen,
    gitInspectReport,
    gitSelectedLaneDeviceId,
    selectGitLane,
    gitPreview,
    gitSelectedAssetIds,
    toggleGitAsset,
    gitMappingDrafts,
    setGitMappingDraft,
    gitConfirmOutcome,
    gitLastMapping,
    runGitInspect,
    runGitPreview,
    runGitConfirmMapping,
    runGitConfirmImport,
    conflictDrawerOpen,
    openConflictDrawer,
    closeConflictDrawer,
    blocksDrawerOpen,
    openBlocksDrawer,
    closeBlocksDrawer,
    pluginDrawerOpen,
    pluginReport,
    openPluginDrawer,
    closePluginDrawer,
    adoptionOpen,
    adoptionPreview,
    openAdoptionPreview,
    closeAdoptionDialog,
    deleteEverywhereOpen,
    closeDeleteEverywhere,
    confirmDeleteEverywhere,
    deepLinkConflictId,
    deepLinkBridgePath,
    reload,
    resolveConflict,
    updateInstructionBlock,
    updateInstruction,
    pairInstructionVariants,
    setTargetEnabled,
    restoreDetachedTarget,
    removeTarget,
    openDeleteEverywhere,
    writeBlocked,
    upgradeRequired,
  } = props;

  const previewFocusRef = useRef<HTMLInputElement | null>(null);

  const probes: AgentHubProbe[] = status?.probes ?? [];

  const selectedConflicts = selectedAsset?.conflicts ?? [];

  /**
   * Business Logic: 行选中后打开块抽屉。
   * Code Logic: select + openBlocksDrawer。
   */
  function handleOpenBlocks(asset: AgentHubAssetSummary) {
    selectAsset(asset.assetId);
    openBlocksDrawer();
  }

  /**
   * Business Logic: Plugin 资产打开组件矩阵 Drawer。
   * Code Logic: select + openPluginDrawer。
   */
  function handleOpenPlugin(asset: AgentHubAssetSummary) {
    selectAsset(asset.assetId);
    openPluginDrawer(asset.assetId);
  }

  /**
   * Business Logic: 行冲突入口。
   * Code Logic: select + openConflictDrawer。
   */
  function handleOpenConflicts(asset: AgentHubAssetSummary) {
    selectAsset(asset.assetId);
    openConflictDrawer();
  }

  /**
   * Business Logic: 切换 target enabled（target-local）。
   * Code Logic: setTargetEnabled。
   */
  function handleToggleTarget(
    asset: AgentHubAssetSummary,
    target: AgentTarget,
    nextEnabled: boolean,
  ) {
    void setTargetEnabled({
      assetId: asset.assetId,
      target,
      desiredEnabled: nextEnabled,
    });
  }

  const previewCheckouts = useMemo(() => {
    const list = preview?.checkouts;
    return Array.isArray(list) ? list : [];
  }, [preview]);

  const previewActions = useMemo(() => {
    const list = preview?.plannedActions;
    return Array.isArray(list) ? list : [];
  }, [preview]);

  const portableInventoryLabels: PortableInventoryViewLabels = useMemo(
    () => ({
      title: t('agentHub:portable.inventory.title'),
      subtitle: t('agentHub:portable.inventory.subtitle'),
      loading: t('agentHub:portable.inventory.loading'),
      empty: t('agentHub:portable.inventory.empty'),
      refresh: t('agentHub:portable.inventory.refresh'),
      retry: t('agentHub:portable.inventory.retry'),
      staleBanner: t('agentHub:portable.inventory.staleBanner'),
      searchPlaceholder: t('agentHub:portable.inventory.searchPlaceholder'),
      filterTarget: t('agentHub:portable.inventory.filterTarget'),
      filterScope: t('agentHub:portable.inventory.filterScope'),
      filterActual: t('agentHub:portable.inventory.filterActual'),
      filterManagement: t('agentHub:portable.inventory.filterManagement'),
      kindCounts: {
        skill: t('agentHub:portable.inventory.kindCounts.skill'),
        command: t('agentHub:portable.inventory.kindCounts.command'),
        plugin: t('agentHub:portable.inventory.kindCounts.plugin'),
        mcp: t('agentHub:portable.inventory.kindCounts.mcp'),
      },
      targetFilter: {
        all: t('agentHub:portable.inventory.targetFilter.all'),
        claude: t('agentHub:portable.inventory.targetFilter.claude'),
        codex: t('agentHub:portable.inventory.targetFilter.codex'),
        opencode: t('agentHub:portable.inventory.targetFilter.opencode'),
      },
      scopeFilter: {
        all: t('agentHub:portable.inventory.scopeFilter.all'),
        user: t('agentHub:portable.inventory.scopeFilter.user'),
        project: t('agentHub:portable.inventory.scopeFilter.project'),
      },
      actualFilter: {
        all: t('agentHub:portable.inventory.actualFilter.all'),
        enabled: t('agentHub:portable.inventory.actualFilter.enabled'),
        disabled: t('agentHub:portable.inventory.actualFilter.disabled'),
        problem: t('agentHub:portable.inventory.actualFilter.problem'),
      },
      managementFilter: {
        all: t('agentHub:portable.inventory.managementFilter.all'),
        unmanaged: t('agentHub:portable.inventory.managementFilter.unmanaged'),
        hubManaged: t('agentHub:portable.inventory.managementFilter.hubManaged'),
        drifted: t('agentHub:portable.inventory.managementFilter.drifted'),
        externalCollision: t('agentHub:portable.inventory.managementFilter.externalCollision'),
        unsupported: t('agentHub:portable.inventory.managementFilter.unsupported'),
      },
      targets: {
        claude: t('agentHub:targets.claude'),
        codex: t('agentHub:targets.codex'),
        opencode: t('agentHub:targets.opencode'),
      },
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
        unmanaged: t('agentHub:portable.inventory.management.unmanaged'),
        hubManaged: t('agentHub:portable.inventory.management.hubManaged'),
        drifted: t('agentHub:portable.inventory.management.drifted'),
        externalCollision: t('agentHub:portable.inventory.management.externalCollision'),
        unsupported: t('agentHub:portable.inventory.management.unsupported'),
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
      },
      sourceOrigin: {
        standalone: t('agentHub:portable.inventory.sourceOrigin.standalone'),
        pluginComponent: t('agentHub:portable.inventory.sourceOrigin.pluginComponent'),
        nativeConfig: t('agentHub:portable.inventory.sourceOrigin.nativeConfig'),
      },
    }),
    [t],
  );

  const portablePluginSummary: PortablePluginDetailsSummary | null = useMemo(() => {
    if (!portableSelectedItem || portableSelectedItem.kind !== 'plugin' || !pluginReport) {
      return null;
    }
    const deleteSummary = summarizeDeletePreview(pluginReport.deletePreview ?? null);
    return {
      packageDisplayName: pluginReport.packageDisplayName || portableSelectedItem.displayName,
      activationState: pluginReport.activationState,
      aggregateStatus: pluginReport.aggregateStatus,
      componentCount: pluginReport.components.length,
      residualCount: pluginReport.residuals.length,
      deleteTombstoneCount: deleteSummary.tombstoneCount,
      deletePreserveCount: deleteSummary.preserveCount,
    };
  }, [portableSelectedItem, pluginReport]);

  if (activeSection !== 'userInstructions' && loading && !status) {
    return (
      <div className={styles.page} data-testid="agent-hub-loading">
        <div className={styles.container}>
          <StatusMessage tone="info">{t('agentHub:loading')}</StatusMessage>
        </div>
      </div>
    );
  }

  if (activeSection !== 'userInstructions' && error && !status) {
    return (
      <div className={styles.page} data-testid="agent-hub-error">
        <div className={styles.container}>
          <StatusMessage
            tone="danger"
            action={
              <Button size="sm" onClick={() => void reload()}>
                {t('common:action.retry')}
              </Button>
            }
          >
            {error}
          </StatusMessage>
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
            {activeSection !== 'userInstructions' ? (
              <Button
                variant="secondary"
                size="sm"
                loading={refreshing}
                onClick={() => void reload()}
                data-testid="agent-hub-reload"
              >
                {t('common:action.refresh')}
              </Button>
            ) : null}
            {activeSection === 'projectInstructions' ? (
              <Button
                variant="primary"
                size="sm"
                onClick={openPreviewDialog}
                data-testid="agent-hub-open-preview"
              >
                {t('agentHub:actions.previewProject')}
              </Button>
            ) : null}
            {activeSection === 'syncImport' ? (
              <>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={openLanPushDialog}
                  data-testid="agent-hub-open-lan-push"
                >
                  {t('agentHub:lanPush.open')}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={openGitImportDrawer}
                  data-testid="agent-hub-open-git-import"
                >
                  {t('agentHub:gitImport.open')}
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  onClick={openPortablePull}
                  data-testid="agent-hub-open-portable-pull"
                >
                  {t('agentHub:portablePull.title')}
                </Button>
              </>
            ) : null}
          </div>
        </header>

        <nav className={styles.hubSectionNav} aria-label={t('agentHub:sections.aria')}>
          {(['userInstructions', 'projectInstructions', 'assets', 'syncImport', 'diagnostics'] as const).map((section) => (
            <Button
              key={section}
              variant={activeSection === section ? 'secondary' : 'ghost'}
              size="sm"
              role="tab"
              aria-selected={activeSection === section}
              onClick={() => setActiveSection(section)}
              data-testid={`agent-hub-section-${section}`}
            >
              {t(`agentHub:sections.${section}`)}
            </Button>
          ))}
        </nav>

        {activeSection === 'userInstructions' ? (
          <UserInstructionView t={t} manager={userInstructions} />
        ) : null}

        {activeSection === 'syncImport' ? (
          <StatusMessage tone="info" live="off" data-testid="agent-hub-lan-push-notice">
            {t('agentHub:lanPushGateC')}
          </StatusMessage>
        ) : null}

        {activeSection !== 'userInstructions' && (upgradeRequired || writeBlocked) ? (
          <StatusMessage tone="warn" data-testid="agent-hub-upgrade-required">
            {t('agentHub:upgradeRequired')}
          </StatusMessage>
        ) : null}

        {activeSection !== 'userInstructions' && stale ? (
          <StatusMessage tone="warn" data-testid="agent-hub-stale">
            {t('agentHub:stale')}
          </StatusMessage>
        ) : null}

        {activeSection !== 'userInstructions' && actionError ? (
          <StatusMessage tone="danger" data-testid="agent-hub-action-error">
            {actionError}
          </StatusMessage>
        ) : null}

        {activeSection === 'diagnostics' && status ? (
          <Card variant="outlined" padding="md" data-testid="agent-hub-status-card">
            <Card.Header>
              <div className={styles.statusHeader}>
                <span className={styles.sectionTitle}>{t('agentHub:probes.title')}</span>
                <div className={styles.statusPills}>
                  <Pill tone={status.enabled ? 'success' : 'neutral'} dot>
                    {status.enabled ? t('agentHub:status.enabled') : t('agentHub:status.disabled')}
                  </Pill>
                  <Pill tone={status.writeCompatible ? 'success' : 'danger'}>
                    {status.writeCompatible
                      ? t('agentHub:status.writeOk')
                      : t('agentHub:status.writeBlocked')}
                  </Pill>
                  <Pill tone={status.conflictCount > 0 ? 'danger' : 'neutral'}>
                    {t('agentHub:status.conflicts', { count: status.conflictCount })}
                  </Pill>
                  <Pill tone={status.blockedMaterializationCount > 0 ? 'warn' : 'neutral'}>
                    {t('agentHub:status.blocked', {
                      count: status.blockedMaterializationCount,
                    })}
                  </Pill>
                </div>
              </div>
            </Card.Header>
            <Card.Body>
              <div className={styles.probeGrid}>
                {probes.length === 0 ? (
                  <p className={styles.emptyInline}>{t('agentHub:probes.empty')}</p>
                ) : (
                  probes.map((probe) => (
                    <div
                      key={probe.target}
                      className={styles.probeCell}
                      data-testid={`probe-${probe.target}`}
                    >
                      <div className={styles.probeName}>{t(`agentHub:targets.${probe.target}`)}</div>
                      <Pill tone={supportTone(probe.support)}>
                        {t(`agentHub:probes.support.${probe.support}`, {
                          defaultValue: probe.support,
                        })}
                      </Pill>
                      <div className={styles.probeMeta}>
                        <span>{probe.version || t('agentHub:probes.unknownVersion')}</span>
                        <span>{probe.executable || t('agentHub:probes.unknownExecutable')}</span>
                      </div>
                      {probe.support === 'unsupported' ? (
                        <StatusMessage tone="warn" live="off">
                          {t('agentHub:probes.unsupportedHint')}
                        </StatusMessage>
                      ) : null}
                    </div>
                  ))
                )}
              </div>
            </Card.Body>
          </Card>
        ) : null}

        {activeSection === 'assets' ? (
          <div data-testid="agent-hub-assets-section">
            <PortableInventoryView
              controller={portableInventory}
              labels={portableInventoryLabels}
            />
            {/* Legacy canonical matrix retained for conflict/plugin deep links until F6 cleanup. */}
            <section className={styles.legacyMatrix} data-testid="agent-hub-legacy-matrix">
              <div className={styles.filters} data-testid="agent-hub-filters">
                <label className={styles.filterField}>
                  <span>{t('agentHub:filters.scope')}</span>
                  <Input
                    value={scopeFilter}
                    onChange={(event) => setScopeFilter(event.currentTarget.value)}
                    placeholder={t('agentHub:filters.scopePlaceholder')}
                    data-testid="agent-hub-filter-scope"
                  />
                </label>
                <label className={styles.filterField}>
                  <span>{t('agentHub:filters.kind')}</span>
                  <Input
                    value={kindFilter}
                    onChange={(event) => setKindFilter(event.currentTarget.value)}
                    placeholder={t('agentHub:filters.kindPlaceholder')}
                    data-testid="agent-hub-filter-kind"
                  />
                </label>
              </div>
              <section className={styles.list} data-testid="agent-hub-asset-list" aria-label={t('agentHub:listAria')}>
                {filteredAssets.length === 0 ? (
                  <p className={styles.empty} data-testid="agent-hub-empty">
                    {t('agentHub:empty')}
                  </p>
                ) : (
                  filteredAssets.map((asset) => (
                    <AgentAssetRow
                      key={asset.assetId}
                      asset={asset}
                      selected={selectedAssetId === asset.assetId}
                      busy={actionBusy}
                      writeBlocked={writeBlocked}
                      onSelect={(item) => selectAsset(item.assetId)}
                      onOpenBlocks={handleOpenBlocks}
                      onOpenPlugin={handleOpenPlugin}
                      onOpenConflicts={handleOpenConflicts}
                      onToggleTarget={handleToggleTarget}
                      onRemoveTarget={(item, target) => {
                        void removeTarget({ assetId: item.assetId, target });
                      }}
                      onRestoreTarget={(item, target) => {
                        void restoreDetachedTarget({ assetId: item.assetId, target });
                      }}
                      onOpenCollision={(item, target) => openAdoptionPreview(item, target)}
                      onDeleteEverywhere={(item) => openDeleteEverywhere(item.assetId)}
                    />
                  ))
                )}
              </section>
            </section>
          </div>
        ) : null}

        {activeSection === 'projectInstructions' ? (
          <Card variant="outlined" padding="md">
            <Card.Header>
              <span className={styles.sectionTitle}>{t('agentHub:sections.projectInstructions')}</span>
            </Card.Header>
            <Card.Body>
              <p className={styles.hint}>{t('agentHub:sections.projectInstructionsHint')}</p>
            </Card.Body>
          </Card>
        ) : null}
      </div>

      <Dialog
        open={previewOpen}
        titleId="agent-hub-preview-title"
        onClose={closePreviewDialog}
        closeOnEscape={!actionBusy}
        closeOnBackdrop={!actionBusy}
        initialFocusRef={previewFocusRef}
        className={styles.dialogSurface}
      >
        <div className={styles.dialogBody} data-testid="agent-hub-preview-dialog">
          <h2 id="agent-hub-preview-title" className={styles.drawerTitle}>
            {t('agentHub:preview.title')}
          </h2>
          <p className={styles.drawerSubtitle}>{t('agentHub:preview.desc')}</p>
          {deepLinkBridgePath ? (
            <p className={styles.hint} data-testid="agent-hub-preview-bridge-notice">
              {t('agentHub:preview.bridgeNotice', { path: deepLinkBridgePath })}
            </p>
          ) : null}
          <label className={styles.filterField}>
            <span>{t('agentHub:preview.projectId')}</span>
            <Input
              ref={previewFocusRef}
              value={previewProjectId}
              onChange={(event) => setPreviewProjectId(event.currentTarget.value)}
              placeholder={t('agentHub:preview.projectIdPlaceholder')}
              data-testid="agent-hub-preview-project-id"
            />
          </label>
          <div className={styles.dialogActions}>
            <Button
              variant="secondary"
              size="sm"
              loading={actionBusy}
              onClick={() => void runPreviewProject()}
              data-testid="agent-hub-run-preview"
            >
              {t('agentHub:preview.run')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              loading={actionBusy}
              disabled={writeBlocked}
              onClick={() => void runEnableProject()}
              data-testid="agent-hub-run-enable"
            >
              {t('agentHub:preview.enable')}
            </Button>
          </div>
          {preview ? (
            <div className={styles.previewResult} data-testid="agent-hub-preview-result">
              <p className={styles.hint}>
                {(preview.noCommitNotice as string) || t('agentHub:preview.noCommitDefault')}
              </p>
              {Array.isArray(preview.warnings) && preview.warnings.length > 0 ? (
                <ul className={styles.warningList}>
                  {(preview.warnings as string[]).map((warning) => (
                    <li key={warning}>{warning}</li>
                  ))}
                </ul>
              ) : null}
              <div className={styles.previewColumns}>
                <div>
                  <h3 className={styles.sectionTitle}>{t('agentHub:preview.checkouts')}</h3>
                  <pre className={styles.blockBody}>{JSON.stringify(previewCheckouts, null, 2)}</pre>
                </div>
                <div>
                  <h3 className={styles.sectionTitle}>{t('agentHub:preview.plannedActions')}</h3>
                  <pre className={styles.blockBody}>{JSON.stringify(previewActions, null, 2)}</pre>
                </div>
              </div>
            </div>
          ) : null}
        </div>
      </Dialog>

      <Dialog
        open={deleteEverywhereOpen}
        titleId="agent-hub-delete-everywhere-title"
        onClose={closeDeleteEverywhere}
        closeOnEscape={!actionBusy}
        closeOnBackdrop={!actionBusy}
        className={styles.dialogSurface}
      >
        <div className={styles.dialogBody} data-testid="agent-hub-delete-everywhere-dialog">
          <h2 id="agent-hub-delete-everywhere-title" className={styles.drawerTitle}>
            {t('agentHub:deleteEverywhere.title')}
          </h2>
          <p className={styles.drawerSubtitle}>{t('agentHub:deleteEverywhere.desc')}</p>
          <div className={styles.dialogActions}>
            <Button
              variant="secondary"
              size="sm"
              disabled={actionBusy}
              onClick={closeDeleteEverywhere}
              data-testid="agent-hub-delete-everywhere-cancel"
            >
              {t('common:action.cancel')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              loading={actionBusy}
              disabled={writeBlocked}
              onClick={() => void confirmDeleteEverywhere()}
              data-testid="agent-hub-delete-everywhere-confirm"
            >
              {t('agentHub:deleteEverywhere.confirm')}
            </Button>
          </div>
        </div>
      </Dialog>

      <AssetAdoptionDialog
        open={adoptionOpen}
        preview={adoptionPreview}
        busy={actionBusy}
        onClose={closeAdoptionDialog}
      />

      <Drawer
        open={conflictDrawerOpen}
        titleId="agent-hub-conflict-title"
        onClose={closeConflictDrawer}
        side="right"
        closeOnEscape={!actionBusy}
        closeOnBackdrop={!actionBusy}
        className={styles.drawerSurface}
      >
        <div className={styles.drawerBody} data-testid="agent-hub-conflict-drawer">
          <header className={styles.drawerHeader}>
            <h2 id="agent-hub-conflict-title" className={styles.drawerTitle}>
              {t('agentHub:conflict.title')}
            </h2>
            <p className={styles.drawerSubtitle}>
              {selectedAsset?.displayName || t('agentHub:conflict.noAsset')}
            </p>
          </header>
          {deepLinkConflictId ? (
            <p className={styles.hint}>
              {t('agentHub:conflict.deepLink', { id: deepLinkConflictId })}
            </p>
          ) : null}
          {selectedConflicts.length === 0 ? (
            <p className={styles.emptyInline} data-testid="conflicts-empty">
              {t('agentHub:conflict.empty')}
            </p>
          ) : (
            <ul className={styles.conflictList}>
              {selectedConflicts.map((conflict) => (
                <li key={conflict.id} className={styles.conflictItem} data-testid={`conflict-${conflict.id}`}>
                  <div className={styles.blockTitleRow}>
                    <span className={styles.blockId}>{conflict.id}</span>
                    {conflict.target ? (
                      <Pill tone="warn">{t(`agentHub:targets.${conflict.target}`)}</Pill>
                    ) : null}
                  </div>
                  <pre className={styles.blockBody}>{conflict.detailJson || '—'}</pre>
                  <div className={styles.blockActions}>
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={actionBusy || writeBlocked}
                      onClick={() =>
                        void resolveConflict({
                          conflictId: conflict.id,
                          resolution: 'keepHub',
                        })
                      }
                      data-testid={`conflict-keep-hub-${conflict.id}`}
                    >
                      {t('agentHub:conflict.keepHub')}
                    </Button>
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={actionBusy || writeBlocked}
                      onClick={() =>
                        void resolveConflict({
                          conflictId: conflict.id,
                          resolution: 'keepExternal',
                        })
                      }
                      data-testid={`conflict-keep-external-${conflict.id}`}
                    >
                      {t('agentHub:conflict.keepExternal')}
                    </Button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      </Drawer>

      <PluginComponentsDrawer
        open={pluginDrawerOpen}
        report={pluginReport}
        busy={actionBusy}
        error={actionError}
        onClose={closePluginDrawer}
      />

      <InstructionBlocksDrawer
        open={blocksDrawerOpen}
        asset={selectedAsset}
        busy={actionBusy}
        writeBlocked={writeBlocked}
        error={actionError}
        onClose={closeBlocksDrawer}
        onSaveDocument={(contentMarkdown) => {
          void updateInstruction({ contentMarkdown });
        }}
        onPromoteShared={(blockId, commonMarkdown) => {
          void updateInstructionBlock({
            blockId,
            mode: 'shared',
            commonMarkdown,
          });
        }}
        onPairAdapted={(blockIds, commonMarkdown) => {
          void pairInstructionVariants({ blockIds, commonMarkdown });
        }}
        onRevertTargetOnly={(blockId, sourceTarget, markdown) => {
          void updateInstructionBlock({
            blockId,
            mode: 'targetOnly',
            commonMarkdown: markdown,
            variants: { [sourceTarget]: markdown },
          });
        }}
        onUpdateBlock={(blockId, patch) => {
          void updateInstructionBlock({
            blockId,
            mode: patch.mode,
            commonMarkdown: patch.commonMarkdown,
            variants: patch.variants,
          });
        }}
      />

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

      <GitImportDrawer
        open={gitImportOpen}
        busy={actionBusy}
        error={actionError}
        inspectReport={gitInspectReport}
        selectedLaneDeviceId={gitSelectedLaneDeviceId}
        preview={gitPreview}
        selectedAssetIds={gitSelectedAssetIds}
        mappingDrafts={gitMappingDrafts}
        confirmOutcome={gitConfirmOutcome}
        lastMapping={gitLastMapping}
        onInspect={() => void runGitInspect()}
        onSelectLane={selectGitLane}
        onPreview={() => void runGitPreview()}
        onToggleAsset={toggleGitAsset}
        onMappingDraftChange={setGitMappingDraft}
        onConfirmMapping={(hub) => void runGitConfirmMapping(hub)}
        onConfirmImport={() => void runGitConfirmImport()}
        onClose={closeGitImportDrawer}
      />

      <PortableAssetDetailsDrawer
        open={portableDetailsOpen}
        item={portableSelectedItem}
        pluginReport={portablePluginSummary}
        busy={portableActionBusy}
        error={portableActionError}
        onClose={closePortableDetails}
        onRequestAction={(action) => {
          if (!portableSelectedItem) return;
          requestPortableAction(portableSelectedItem.inventoryItemId, action);
        }}
      />

      <PortableAssetActionDialog
        open={portableActionOpen}
        item={
          portableInventory.pendingAction
            ? portableInventory.snapshot?.items.find(
                (entry) => entry.inventoryItemId === portableInventory.pendingAction?.itemId,
              ) ?? portableSelectedItem
            : null
        }
        action={portableActionKind}
        inventorySnapshotHash={portableInventory.snapshot?.inventorySnapshotHash ?? null}
        plan={portableActionPlan}
        result={portableActionResult}
        busy={portableActionBusy}
        error={portableActionError}
        clientRequestId={portableActionClientRequestId}
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
 * 页面入口：注入 controller。
 */
export function AgentHub() {
  const controller = useAgentHubController();
  return <AgentHubView {...controller} />;
}

// 避免未使用类型告警（导出供测试）
export type { AgentHubAssetDetail, AgentHubStatus, AgentHubProjectPreview };
