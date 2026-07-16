/**
 * Orchestrator 页面 - 自动化任务编排入口
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在当前 Workbench 项目下管理项目级自动化任务队列，包括本机任务、远端任务和离线远端待发送项。
 *   自动化配置统一由 Settings 自动化 tab 管理，本面板只展示任务执行与证据。
 *   当前前端只提供任务、验证证据与 blocked 控制入口，并可把任务定位回对应 Workbench 上下文。
 *
 * Code Logic（这个组件做什么）:
 *   - 调用 useOrchestratorController 获取状态与 handler
 *   - 组合 shell（header / error / queue 卡 / runtime snapshot 条）与 Board/Outbox/Drawer/Create 视图
 *   - 不直接调用 orchestratorApi；hooks 全部位于渲染分支之前
 */
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
import { PlusIcon, RefreshIcon, SettingsIcon, SyncIcon } from '@/lib/icons';
import type {
  OrchestratorRuntimeEvent,
  OrchestratorRuntimeTaskSummary,
} from '@/lib/types';
import {
  useOrchestratorController,
  type OrchestratorPanelProps,
} from './controllers/useOrchestratorController';
import {
  ATTEMPT_PHASE_LABEL_KEYS,
  formatOptionalTaskTimestamp,
  formatTaskTimestamp,
  RUN_STATE_LABEL_KEYS,
  WORKFLOW_STATE_LABEL_KEYS,
} from './orchestratorViewHelpers';
import { OrchestratorBoard } from './views/OrchestratorBoard';
import { OrchestratorCreateDialog } from './views/OrchestratorCreateDialog';
import { OrchestratorExperimentPanel } from './views/OrchestratorExperimentPanel';
import { OrchestratorOutbox } from './views/OrchestratorOutbox';
import { OrchestratorTaskDrawer } from './views/OrchestratorTaskDrawer';
import { WorkflowWizardDialog } from './views/WorkflowWizardDialog';
import styles from './Orchestrator.module.css';

export type { OrchestratorPanelProps };

/**
 * Orchestrator 可嵌入面板组件
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench 需要把自动化看板作为终端、文件预览同级的工作区视图，同时保留页面壳复用能力。
 *
 * Code Logic（这个函数做什么）:
 *   调用 controller，渲染 shell + Board/Outbox/Drawer/Create 视图；embedded=true 时省略页面级 header。
 */
export function OrchestratorPanel(props: OrchestratorPanelProps): JSX.Element {
  const c = useOrchestratorController(props);
  const { t } = useTranslation(['orchestrator', 'nav', 'common']);

  return (
    <div className={c.embedded ? styles.embedded : styles.page}>
      {!c.embedded ? (
        <header className={styles.header}>
          <div className={styles.headerText}>
            <span className={styles.eyebrow}>{t('nav:orchestrator')}</span>
            <h1 className={styles.title}>{t('orchestrator:title')}</h1>
            <p className={styles.subtitle}>{t('orchestrator:subtitle')}</p>
          </div>
          <div className={styles.projectStatus}>
            <Pill tone={c.activeProject ? 'success' : 'warn'} dot>
              {c.activeProject ? c.activeProject.name : t('orchestrator:noProject')}
            </Pill>
          </div>
        </header>
      ) : null}

      {c.error ? (
        <div className={styles.error} role="alert">
          {c.error}
        </div>
      ) : null}

      <div className={styles.grid}>
        <Card variant="outlined" padding="md" className={styles.queue}>
          <Card.Header className={styles.cardHeader}>
            <div>
              <h2 className={styles.sectionTitle}>{t('orchestrator:queue.title')}</h2>
              <p className={styles.sectionLead}>{t('orchestrator:queue.subtitle')}</p>
            </div>
            <div className={styles.queueActions}>
              <Pill tone="neutral">{c.tasks.length + c.pendingRemoteItems.length}</Pill>
              <Button
                variant="secondary"
                size="sm"
                icon={<SyncIcon />}
                disabled={!c.activeProjectId}
                loading={c.refreshingProjectId === c.activeProjectId}
                onClick={() => {
                  void c.handleRefreshProject();
                }}
              >
                {t('orchestrator:detail.refresh')}
              </Button>
              <Button
                variant="primary"
                size="sm"
                icon={<PlusIcon />}
                disabled={!c.activeProjectId}
                onClick={c.handleOpenCreateDialog}
              >
                {t('orchestrator:create.open')}
              </Button>
            </div>
          </Card.Header>
          <Card.Body className={styles.queueBody}>
            {c.activeProject ? (
              <div className={styles.snapshotBar}>
                <div className={styles.snapshotHeader}>
                  <span className={styles.label}>{t('orchestrator:snapshot.title')}</span>
                  <div className={styles.snapshotActions}>
                    {c.runtimeSnapshotLoading ? (
                      <Pill tone="accent">{t('orchestrator:snapshot.loading')}</Pill>
                    ) : null}
                    {c.runtimeRemoteStatus === 'offline' && c.runtimeCachedAt ? (
                      <Pill tone="warn">
                        {t('orchestrator:snapshot.remoteOfflineCachedBadge', {
                          time: formatTaskTimestamp(c.runtimeCachedAt),
                        })}
                      </Pill>
                    ) : null}
                    <Button
                      variant="ghost"
                      size="sm"
                      icon={<RefreshIcon />}
                      disabled={c.runtimeSnapshotLoading}
                      onClick={c.handleRefreshRuntimeSnapshot}
                    >
                      {t('orchestrator:snapshot.refresh')}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      icon={<SettingsIcon />}
                      onClick={c.handleOpenAutomationSettings}
                    >
                      {t('orchestrator:snapshot.settings')}
                    </Button>
                  </div>
                </div>
                {c.showRuntimeSnapshotContent && c.runtimeSnapshot ? (
                  <>
                    {c.runtimeRemoteStatus === 'offline' && c.runtimeCachedAt ? (
                      <p className={styles.snapshotMuted} role="status">
                        {t('orchestrator:snapshot.remoteOfflineCached', {
                          time: formatTaskTimestamp(c.runtimeCachedAt),
                        })}
                      </p>
                    ) : null}
                    <div className={styles.snapshotItems}>
                      <Pill tone={c.runtimeSnapshot.schedulerEnabled ? 'success' : 'warn'} dot>
                        {c.runtimeSnapshot.schedulerEnabled
                          ? t('orchestrator:snapshot.schedulerEnabled')
                          : t('orchestrator:snapshot.schedulerDisabled')}
                      </Pill>
                      <Pill tone={c.runtimeSnapshot.workflowValid ? 'success' : 'danger'} dot>
                        {c.runtimeSnapshot.workflowValid
                          ? t('orchestrator:snapshot.workflowValid')
                          : t('orchestrator:snapshot.workflowInvalid')}
                      </Pill>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={c.handleOpenWorkflowWizard}
                        data-testid="open-workflow-wizard"
                      >
                        {t('orchestrator:workflowWizard.open')}
                      </Button>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.slotsUsed', {
                          used: c.runtimeSnapshot.slotsUsed,
                          max: c.runtimeSnapshot.maxConcurrentTasks,
                        })}
                      </span>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.slotsAvailable', {
                          available: c.runtimeSnapshot.slotsAvailable,
                        })}
                      </span>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.runningCount', {
                          count: c.runtimeSnapshot.runningTasks.length,
                        })}
                      </span>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.retryingCount', {
                          count: c.runtimeSnapshot.retryingTasks.length,
                        })}
                      </span>
                    </div>
                    <div className={styles.snapshotMetaGrid}>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.generatedAt', {
                          time: formatTaskTimestamp(c.runtimeSnapshot.generatedAt),
                        })}
                      </span>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.latestTickAt', {
                          time: formatOptionalTaskTimestamp(
                            c.runtimeSnapshot.latestTickAt,
                            t('orchestrator:snapshot.latestTickUnknown'),
                          ),
                        })}
                      </span>
                      <span className={styles.snapshotMetric}>
                        {t('orchestrator:snapshot.lastDispatchedCount', {
                          count: c.runtimeSnapshot.lastDispatchedCount,
                        })}
                      </span>
                    </div>
                    {c.runtimeSnapshot.runningTasks.length > 0 ? (
                      <section className={styles.snapshotSection}>
                        <h3 className={styles.snapshotSectionTitle}>
                          {t('orchestrator:snapshot.runningTasks')}
                        </h3>
                        <ul className={styles.snapshotList}>
                          {c.runtimeSnapshot.runningTasks.map(
                            (task: OrchestratorRuntimeTaskSummary) => (
                              <li className={styles.snapshotListItem} key={task.taskId}>
                                <span className={styles.snapshotItemTitle}>{task.title}</span>
                                <span className={styles.snapshotItemMeta}>
                                  {t(RUN_STATE_LABEL_KEYS[task.runState])}
                                  {task.attemptPhase
                                    ? ` · ${t(ATTEMPT_PHASE_LABEL_KEYS[task.attemptPhase])}`
                                    : ''}
                                  {task.lastRuntimeMessage ? ` · ${task.lastRuntimeMessage}` : ''}
                                </span>
                              </li>
                            ),
                          )}
                        </ul>
                      </section>
                    ) : null}
                    {c.runtimeSnapshot.retryingTasks.length > 0 ? (
                      <section className={styles.snapshotSection}>
                        <h3 className={styles.snapshotSectionTitle}>
                          {t('orchestrator:snapshot.retryingTasks')}
                        </h3>
                        <ul className={styles.snapshotList}>
                          {c.runtimeSnapshot.retryingTasks.map(
                            (task: OrchestratorRuntimeTaskSummary) => (
                              <li className={styles.snapshotListItem} key={task.taskId}>
                                <span className={styles.snapshotItemTitle}>{task.title}</span>
                                <span className={styles.snapshotItemMeta}>
                                  {t(WORKFLOW_STATE_LABEL_KEYS[task.workflowState])}
                                  {' · '}
                                  {t(RUN_STATE_LABEL_KEYS[task.runState])}
                                </span>
                              </li>
                            ),
                          )}
                        </ul>
                      </section>
                    ) : null}
                    {c.runtimeSnapshot.recentEvents.length > 0 ? (
                      <section className={styles.snapshotSection}>
                        <h3 className={styles.snapshotSectionTitle}>
                          {t('orchestrator:snapshot.recentEvents')}
                        </h3>
                        <ul className={styles.snapshotList}>
                          {c.runtimeSnapshot.recentEvents.map((event: OrchestratorRuntimeEvent) => (
                            <li className={styles.snapshotListItem} key={event.id}>
                              <span className={styles.snapshotItemTitle}>{event.taskTitle}</span>
                              <span className={styles.snapshotItemMeta}>
                                {event.kind} · {event.message} ·{' '}
                                {formatTaskTimestamp(event.createdAt)}
                              </span>
                            </li>
                          ))}
                        </ul>
                      </section>
                    ) : null}
                    {c.runtimeSnapshot.workflowError ? (
                      <p className={styles.snapshotWarning}>
                        {t('orchestrator:snapshot.workflowError', {
                          error: c.runtimeSnapshot.workflowError,
                        })}
                      </p>
                    ) : null}
                    {c.runtimeSnapshot.latestError ? (
                      <p className={styles.snapshotWarning}>
                        {t('orchestrator:snapshot.latestError', {
                          error: c.runtimeSnapshot.latestError,
                        })}
                      </p>
                    ) : null}
                  </>
                ) : null}
                {!c.showRuntimeSnapshotContent && !c.runtimeSnapshotLoading ? (
                  <p className={styles.snapshotMuted} role="status">
                    {c.runtimeRemoteStatus === 'unsupported'
                      ? t('orchestrator:snapshot.remoteUnsupported')
                      : c.runtimeRemoteStatus === 'offline'
                        ? t('orchestrator:snapshot.remoteOffline')
                        : t('orchestrator:snapshot.remoteUnavailable')}
                  </p>
                ) : null}
                {c.runtimeSnapshotErrorMessage ? (
                  <p className={styles.snapshotWarning} role="status">
                    {c.runtimeSnapshotErrorMessage}
                  </p>
                ) : null}
              </div>
            ) : null}
            {c.loading ? <p className={styles.muted}>{t('common:loading')}</p> : null}
            {!c.loading ? (
              <OrchestratorBoard
                groups={c.groups}
                selectedTask={c.selectedTask}
                movingTaskId={c.movingTaskId}
                onSelectTask={(taskId) => c.setSelectedTaskId(taskId)}
                onTaskDragStart={c.handleTaskDragStart}
                onTaskDragEnd={c.handleTaskDragEnd}
                onLaneDragOver={c.handleLaneDragOver}
                onLaneDrop={(event, targetState) => {
                  void c.handleLaneDrop(event, targetState);
                }}
              />
            ) : null}
            {!c.loading ? (
              <OrchestratorOutbox
                pendingRemoteItems={c.pendingRemoteItems}
                focusedOutboxId={c.focusedOutboxId}
                outboxActionId={c.outboxActionId}
                onRetry={(outboxId) => {
                  void c.handleRetryRemoteOutbox(outboxId);
                }}
                onDiscard={(outboxId) => {
                  void c.handleDiscardRemoteOutbox(outboxId);
                }}
              />
            ) : null}
            {!c.loading ? (
              <section className={styles.group} aria-label={t('orchestrator:experiments.title')}>
                <div className={styles.groupHeader}>
                  <span>{t('orchestrator:experiments.title')}</span>
                  <Pill tone="neutral">{c.experiments.length}</Pill>
                </div>
                {c.experimentsLoading ? (
                  <p className={styles.muted}>{t('common:loading')}</p>
                ) : null}
                {!c.experimentsLoading && c.experiments.length === 0 ? (
                  <p className={styles.muted}>{t('orchestrator:experiments.empty')}</p>
                ) : null}
                {c.experiments.map((experiment) => (
                  <OrchestratorExperimentPanel
                    key={experiment.id}
                    experiment={experiment}
                    onApproveRecommended={(experimentId, winnerTaskId) => {
                      void c.handleApproveExperimentWinner(experimentId, winnerTaskId);
                    }}
                    onSelectCandidate={(experimentId, taskId) => {
                      void c.handleApproveExperimentWinner(experimentId, taskId);
                    }}
                    onCancel={(experimentId) => {
                      void c.handleCancelExperiment(experimentId);
                    }}
                  />
                ))}
              </section>
            ) : null}
          </Card.Body>
        </Card>

        <OrchestratorTaskDrawer
          selectedTask={c.selectedTask}
          selectedRenderableTask={c.selectedRenderableTask}
          selectedTaskCanStart={c.selectedTaskCanStart}
          selectedTaskCanComplete={c.selectedTaskCanComplete}
          selectedTaskCanRequestRework={c.selectedTaskCanRequestRework}
          selectedTaskShowDeliver={c.selectedTaskShowDeliver}
          selectedTaskCanDeliver={c.selectedTaskCanDeliver}
          selectedTaskCanCancel={c.selectedTaskCanCancel}
          selectedTaskCanControlBlocked={c.selectedTaskCanControlBlocked}
          selectedTaskCanOpenWorkbench={c.selectedTaskCanOpenWorkbench}
          selectedTaskProgressMessage={c.selectedTaskProgressMessage}
          selectedTaskTerminalLabel={c.selectedTaskTerminalLabel}
          startingTaskId={c.startingTaskId}
          completingTaskId={c.completingTaskId}
          reworkingTaskId={c.reworkingTaskId}
          deliveringTaskId={c.deliveringTaskId}
          retryingTaskId={c.retryingTaskId}
          cancelingTaskId={c.cancelingTaskId}
          evidenceItems={c.evidenceItems}
          evidenceLoading={c.evidenceLoading}
          evidenceError={c.evidenceError}
          latestVerifierEvidence={c.latestVerifierEvidence}
          latestRepairPromptEvidence={c.latestRepairPromptEvidence}
          developmentAttemptEvidenceItems={c.developmentAttemptEvidenceItems}
          detailTab={c.detailTab}
          onDetailTabChange={c.setDetailTab}
          onSelectReviewFilePath={c.setSelectedReviewFilePath}
          reworkDialogOpen={c.reworkDialogOpen}
          reworkError={c.reworkError}
          onOpenReworkDialog={c.handleOpenReworkDialog}
          onCloseReworkDialog={c.handleCloseReworkDialog}
          onSubmitRework={(reason) => {
            void c.handleSubmitRework(reason);
          }}
          onClose={c.handleCloseTaskDrawer}
          onStart={() => {
            void c.handleStartSelectedTask();
          }}
          onCompleteAgentRun={() => {
            void c.handleCompleteAgentRun();
          }}
          onOpenWorkbench={c.handleOpenWorkbench}
          onRetry={() => {
            void c.handleRetryTask();
          }}
          onDeliver={() => {
            void c.handleDeliverReviewedTask();
          }}
          onCancel={() => {
            void c.handleCancelTask();
          }}
        />
      </div>
      <OrchestratorCreateDialog
        open={c.createDialogOpen}
        form={c.form}
        completionPrompt={c.completionPrompt}
        completingPrompt={c.completingPrompt}
        creatingAction={c.creatingAction}
        canCreate={c.canCreate}
        canCompletePrompt={c.canCompletePrompt}
        completionPromptRef={c.completionPromptRef}
        creatingExperiment={c.creatingExperiment}
        onClose={c.handleCloseCreateDialog}
        onCompletionPromptChange={c.setCompletionPrompt}
        onUpdateFormField={c.updateFormField}
        onCompleteWithAi={() => {
          void c.handleCompleteTaskPrompt();
        }}
        onCreateFormSubmit={c.handleCreateFormSubmit}
        onCreateAction={(createAction) => {
          void c.handleCreateTaskAction(createAction);
        }}
        onCreateExperiment={() => {
          void c.handleCreateExperiment();
        }}
      />
      <WorkflowWizardDialog
        open={c.workflowWizardOpen}
        loadState={c.workflowLoadState}
        documentStatus={c.workflowDocumentStatus}
        draft={c.workflowDraft}
        expectedHash={c.workflowExpectedHash}
        diagnostics={c.workflowDiagnostics}
        preview={c.workflowPreview}
        loadError={c.workflowLoadError}
        saveError={c.workflowSaveError}
        conflict={c.workflowConflict}
        busy={c.workflowBusy}
        focusedDiagnosticLine={c.workflowFocusedDiagnosticLine}
        draftTextareaRef={c.workflowDraftTextareaRef}
        onClose={c.handleCloseWorkflowWizard}
        onDraftChange={c.handleWorkflowDraftChange}
        onCreateFromTemplate={c.handleCreateWorkflowFromTemplate}
        onValidate={() => {
          void c.handleValidateWorkflowDocument();
        }}
        onSave={() => {
          void c.handleSaveWorkflowDocument();
        }}
        onReload={() => {
          void c.handleReloadWorkflowDocument();
        }}
        onOpenFile={c.handleOpenWorkflowFile}
        onFocusDiagnostic={c.handleFocusWorkflowDiagnostic}
      />
    </div>
  );
}

/**
 * Orchestrator 页面组件
 *
 * Business Logic（为什么需要这个函数）:
 *   旧页面入口仍可作为独立渲染边界保留，便于内部复用和未来路由调整。
 *
 * Code Logic（这个函数做什么）:
 *   渲染非嵌入模式 OrchestratorPanel，保留完整页面级 header shell。
 */
export function Orchestrator(): JSX.Element {
  return <OrchestratorPanel />;
}
