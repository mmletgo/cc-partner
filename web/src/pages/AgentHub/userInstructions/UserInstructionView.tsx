import { useState, type JSX } from 'react';
import { Button, StatusMessage } from '@/components/primitives';
import { allHubTargets } from '@/lib/agentCatalog';
import type { TFunction } from 'i18next';
import { CrossAgentSyncDialog } from './CrossAgentSyncDialog';
import { UserInstructionDangerZone } from './UserInstructionDangerZone';
import { UserInstructionEditor } from './UserInstructionEditor';
import { UserInstructionPreviewDialog } from './UserInstructionPreviewDialog';
import { UserInstructionSetupDialog } from './UserInstructionSetupDialog';
import { UserInstructionTargetCard } from './UserInstructionTargetCard';
import {
  getUserInstructionSummaryPresentation,
} from './userInstructionPresentation';
import type { UseUserInstructionManagerResult } from './useUserInstructionManager';
import styles from '../AgentHub.module.css';

export interface UserInstructionViewProps {
  t: TFunction<['agentHub', 'common']>;
  manager: UseUserInstructionManagerResult;
}

/**
 * Business Logic（为什么需要）:
 *   Agent Hub 默认入口必须直接回答用户级指令的来源、路径、管理状态与下一步。
 *
 * Code Logic（做什么）:
 *   组合专用 editor/target cards/setup/preview/danger views；所有 mutation 委托 controller。
 */
export function UserInstructionView(props: UserInstructionViewProps): JSX.Element | null {
  const { t, manager } = props;
  const {
    workspace,
    loading,
    refreshing,
    error,
    actionError,
    actionBusy,
    draft,
    dirty,
    activePane,
    setActivePane,
    updateDraftContent,
    resetDraft,
    setupOpen,
    openSetup,
    closeSetup,
    setTargetSelection,
    promoteTargetExtensionToCommon,
    previewOpen,
    plan,
    closePreview,
    previewDraft,
    applyPlan,
    applyResult,
    dismissApplyResult,
    runTargetIntent,
    openPath,
    copyPath,
    refresh,
    canPreview,
    canonicalContentTruncated,
    deleteDialogOpen,
    deleteConfirmation,
    setDeleteConfirmation,
    openDeleteDialog,
    closeDeleteDialog,
    previewDeleteAsset,
  } = manager;
  const [crossAgentOpen, setCrossAgentOpen] = useState(false);

  if (loading && !workspace) {
    return <StatusMessage tone="info">{t('agentHub:userInstructions.loading')}</StatusMessage>;
  }
  if (error && !workspace) {
    return (
      <StatusMessage
        tone="danger"
        action={<Button size="sm" onClick={() => void refresh()}>{t('common:action.retry')}</Button>}
      >
        {error}
      </StatusMessage>
    );
  }
  if (!workspace) return null;

  const summary = getUserInstructionSummaryPresentation(workspace);
  const hasWritableTarget = workspace.targets.some(
    (target) => target.capability.write === 'supported',
  );
  const canDeleteAsset = workspace.targets.some((target) =>
    target.availableActions.includes('deleteAsset'),
  );
  const applyHasFailure = Boolean(
    applyResult?.targets.some((target) =>
      ['stalePreview', 'blocked', 'conflict', 'failed'].includes(target.status),
    ),
  );

  return (
    <div className={styles.userWorkspace} data-testid="user-instruction-workspace">
      <section className={styles.userHero} aria-labelledby="user-instruction-title">
        <div className={styles.userHeroCopy}>
          <h2 id="user-instruction-title" className={styles.title}>
            {t('agentHub:userInstructions.title')}
          </h2>
          <p className={styles.subtitle}>{t('agentHub:userInstructions.subtitle')}</p>
          <p className={styles.userSummary} data-testid="user-instruction-summary">
            {t(`agentHub:userInstructions.summary.${summary.key}`, {
              managed: summary.managedCount,
              actions: summary.actionCount,
            })}
          </p>
          <p className={styles.userRefreshTime}>
            {t('agentHub:userInstructions.refreshedAt', {
              time: new Date(workspace.refreshedAt).toLocaleString(),
            })}
          </p>
        </div>
        <div className={styles.userHeroActions}>
          <Button
            variant="secondary"
            size="sm"
            loading={refreshing}
            onClick={() => void refresh()}
            data-testid="user-instruction-refresh"
          >
            {t('common:action.refresh')}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={!draft.commonContent.trim() && Object.values(draft.targetExtensions).every((v) => !v?.trim())}
            onClick={() => setCrossAgentOpen(true)}
            data-testid="user-instruction-cross-agent-sync"
          >
            {t('agentHub:crossAgent.openButton')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={!hasWritableTarget}
            onClick={() => openSetup()}
            data-testid="user-instruction-primary-action"
          >
            {workspace.canonical
              ? workspace.setupState === 'configured'
                ? t('agentHub:userInstructions.actions.manageTargets')
                : t('agentHub:userInstructions.actions.startManaging')
              : t('agentHub:userInstructions.actions.create')}
          </Button>
        </div>
      </section>

      {!hasWritableTarget ? (
        <StatusMessage tone="info" live="off" data-testid="user-instruction-scan-only">
          {t('agentHub:userInstructions.scanOnlyNotice')}
        </StatusMessage>
      ) : null}
      {canonicalContentTruncated ? (
        <StatusMessage tone="warn">{t('agentHub:userInstructions.editor.contentTruncated')}</StatusMessage>
      ) : null}
      {actionError ? <StatusMessage tone="danger">{actionError}</StatusMessage> : null}
      {error ? <StatusMessage tone="warn">{error}</StatusMessage> : null}

      {applyResult ? (
        <StatusMessage
          tone={applyHasFailure ? 'warn' : 'success'}
          action={<Button size="sm" variant="ghost" onClick={dismissApplyResult}>{t('common:action.confirm')}</Button>}
          data-testid="user-instruction-apply-result"
        >
          <span>
            {applyHasFailure
              ? t('agentHub:userInstructions.result.partial')
              : t('agentHub:userInstructions.result.success')}
          </span>
          <ul className={styles.userResultList}>
            {applyResult.targets.map((target) => (
              <li key={`${target.target}-${target.path}`}>
                {t(`agentHub:targets.${target.target}`)} ·{' '}
                {t(`agentHub:userInstructions.result.status.${target.status}`)} · {target.path}
              </li>
            ))}
          </ul>
        </StatusMessage>
      ) : null}

      <UserInstructionEditor
        t={t}
        draft={draft}
        activePane={activePane}
        dirty={dirty}
        busy={actionBusy}
        contentTruncated={canonicalContentTruncated}
        writeAvailable={hasWritableTarget}
        onPaneChange={setActivePane}
        onContentChange={updateDraftContent}
        onReset={resetDraft}
        onPreview={() => {
          if (workspace.setupState === 'configured') void previewDraft();
          else openSetup();
        }}
      />

      <section className={styles.userTargetsSection} aria-labelledby="user-instruction-targets-title">
        <div className={styles.userSectionHeading}>
          <div>
            <h2 id="user-instruction-targets-title" className={styles.userSectionTitle}>
              {t('agentHub:userInstructions.targetsTitle')}
            </h2>
            <p className={styles.userSectionDescription}>
              {t('agentHub:userInstructions.targetsDescription')}
            </p>
          </div>
        </div>
        <div className={styles.userTargetGrid}>
          {workspace.targets.map((target) => (
            <UserInstructionTargetCard
              key={target.target}
              t={t}
              target={target}
              busy={actionBusy}
              onIntent={(item, intent) => void runTargetIntent(item, intent)}
              onOpenPath={(path) => void openPath(path)}
              onCopyPath={(path) => void copyPath(path)}
            />
          ))}
        </div>
      </section>

      {workspace.canonical && canDeleteAsset ? (
        <UserInstructionDangerZone
          t={t}
          displayName={workspace.canonical.displayName}
          open={deleteDialogOpen}
          confirmation={deleteConfirmation}
          busy={actionBusy}
          onOpen={openDeleteDialog}
          onClose={closeDeleteDialog}
          onConfirmationChange={setDeleteConfirmation}
          onPreviewDelete={() => void previewDeleteAsset()}
        />
      ) : null}

      <UserInstructionSetupDialog
        t={t}
        open={setupOpen}
        busy={actionBusy}
        targets={workspace.targets}
        draft={draft}
        canPreview={canPreview}
        error={actionError}
        onClose={closeSetup}
        onSelectionChange={setTargetSelection}
        onPromoteToCommon={promoteTargetExtensionToCommon}
        onPreview={() => void previewDraft()}
      />
      <UserInstructionPreviewDialog
        t={t}
        open={previewOpen}
        busy={actionBusy}
        plan={plan}
        error={actionError}
        onClose={closePreview}
        onApply={() => void applyPlan()}
      />
      <CrossAgentSyncDialog
        t={t}
        open={crossAgentOpen}
        sourceMarkdown={[
          draft.commonContent,
          ...allHubTargets().map((target) => draft.targetExtensions[target]),
        ]
          .filter((part) => Boolean(part?.trim()))
          .join('\n\n')}
        onClose={() => setCrossAgentOpen(false)}
      />
    </div>
  );
}
