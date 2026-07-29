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
import { InstructionBlocksDrawer } from './InstructionBlocksDrawer';
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
    conflictDrawerOpen,
    openConflictDrawer,
    closeConflictDrawer,
    blocksDrawerOpen,
    openBlocksDrawer,
    closeBlocksDrawer,
    adoptionOpen,
    adoptionPreview,
    openAdoptionPreview,
    closeAdoptionDialog,
    deleteEverywhereOpen,
    closeDeleteEverywhere,
    confirmDeleteEverywhere,
    deepLinkConflictId,
    reload,
    resolveConflict,
    updateInstructionBlock,
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

  if (loading && !status) {
    return (
      <div className={styles.page} data-testid="agent-hub-loading">
        <div className={styles.container}>
          <StatusMessage tone="info">{t('agentHub:loading')}</StatusMessage>
        </div>
      </div>
    );
  }

  if (error && !status) {
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
            <Button
              variant="secondary"
              size="sm"
              loading={refreshing}
              onClick={() => void reload()}
              data-testid="agent-hub-reload"
            >
              {t('common:action.refresh')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              onClick={openPreviewDialog}
              data-testid="agent-hub-open-preview"
            >
              {t('agentHub:actions.previewProject')}
            </Button>
          </div>
        </header>

        <StatusMessage tone="info" live="off" data-testid="agent-hub-lan-push-notice">
          {t('agentHub:lanPushGateC')}
        </StatusMessage>

        {upgradeRequired || writeBlocked ? (
          <StatusMessage tone="warn" data-testid="agent-hub-upgrade-required">
            {t('agentHub:upgradeRequired')}
          </StatusMessage>
        ) : null}

        {stale ? (
          <StatusMessage tone="warn" data-testid="agent-hub-stale">
            {t('agentHub:stale')}
          </StatusMessage>
        ) : null}

        {actionError ? (
          <StatusMessage tone="danger" data-testid="agent-hub-action-error">
            {actionError}
          </StatusMessage>
        ) : null}

        {status ? (
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

        <section className={styles.filters} data-testid="agent-hub-filters">
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
        </section>

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

      <InstructionBlocksDrawer
        open={blocksDrawerOpen}
        asset={selectedAsset}
        busy={actionBusy}
        writeBlocked={writeBlocked}
        error={actionError}
        onClose={closeBlocksDrawer}
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
