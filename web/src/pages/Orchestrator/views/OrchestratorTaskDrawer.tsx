/**
 * Orchestrator 任务详情/Evidence 抽屉视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户点击看板任务后需要在右侧查看详情、runtime 字段、最新 evidence 与显式业务动作。
 *
 * Code Logic（这个组件做什么）:
 *   渲染共享 Drawer 内的任务详情 Card + Evidence Card；动作按钮与 loading 态由 props 驱动；无 API import。
 */
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Drawer, Pill } from '@/components/primitives';
import {
  CheckIcon,
  FolderIcon,
  PlayIcon,
  StopIcon,
  SyncIcon,
  XIcon,
} from '@/lib/icons';
import {
  orchestratorAttemptLabel,
  orchestratorEvidenceKindLabel,
  orchestratorEvidenceKindTone,
  orchestratorStatusTone,
  orchestratorWorkflowStateTone,
} from '@/lib/orchestrator';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type { OrchestratorEvidence, OrchestratorTask } from '@/lib/types';
import {
  ATTEMPT_PHASE_LABEL_KEYS,
  evidenceSummaryLabelKey,
  evidenceSummaryTone,
  formatOptionalTaskTimestamp,
  formatTaskTimestamp,
  RUN_STATE_LABEL_KEYS,
  runStateTone,
  STATUS_LABEL_KEYS,
  taskRuntimeValue,
  WORKFLOW_STATE_LABEL_KEYS,
} from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   抽屉只渲染选中任务的详情与动作，状态与 API 副作用仍归 controller。
 *
 * Code Logic（这个类型做什么）:
 *   描述 selected task、capability flags、busy ids、evidence 与全部动作回调。
 */
export interface OrchestratorTaskDrawerProps {
  selectedTask: OrchestratorTask | null;
  selectedRenderableTask: OrchestratorRenderableTask | null;
  selectedTaskCanStart: boolean;
  selectedTaskCanComplete: boolean;
  selectedTaskCanRequestRework: boolean;
  selectedTaskCanDeliver: boolean;
  selectedTaskCanCancel: boolean;
  selectedTaskCanControlBlocked: boolean;
  selectedTaskCanOpenWorkbench: boolean;
  selectedTaskProgressMessage: string | null;
  selectedTaskTerminalLabel: string | null;
  startingTaskId: string | null;
  completingTaskId: string | null;
  reworkingTaskId: string | null;
  deliveringTaskId: string | null;
  retryingTaskId: string | null;
  cancelingTaskId: string | null;
  evidenceItems: OrchestratorEvidence[];
  evidenceLoading: boolean;
  evidenceError: string | null;
  latestVerifierEvidence: OrchestratorEvidence | null;
  latestRepairPromptEvidence: OrchestratorEvidence | null;
  developmentAttemptEvidenceItems: OrchestratorEvidence[];
  onClose: () => void;
  onStart: () => void;
  onCompleteAgentRun: () => void;
  onOpenWorkbench: () => void;
  onRetry: () => void;
  onRequestRework: () => void;
  onDeliver: () => void;
  onCancel: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   选中任务后需要右侧抽屉展示完整详情与 Evidence，关闭后回到纯看板。
 *
 * Code Logic（这个函数做什么）:
 *   selectedTask 为空返回 null；否则渲染 Drawer + 详情/Evidence 两栏内容与条件动作按钮。
 */
export function OrchestratorTaskDrawer(props: OrchestratorTaskDrawerProps): JSX.Element | null {
  const {
    selectedTask,
    selectedRenderableTask,
    selectedTaskCanStart,
    selectedTaskCanComplete,
    selectedTaskCanRequestRework,
    selectedTaskCanDeliver,
    selectedTaskCanCancel,
    selectedTaskCanControlBlocked,
    selectedTaskCanOpenWorkbench,
    selectedTaskProgressMessage,
    selectedTaskTerminalLabel,
    startingTaskId,
    completingTaskId,
    reworkingTaskId,
    deliveringTaskId,
    retryingTaskId,
    cancelingTaskId,
    evidenceItems,
    evidenceLoading,
    evidenceError,
    latestVerifierEvidence,
    latestRepairPromptEvidence,
    developmentAttemptEvidenceItems,
    onClose,
    onStart,
    onCompleteAgentRun,
    onOpenWorkbench,
    onRetry,
    onRequestRework,
    onDeliver,
    onCancel,
  } = props;
  const { t } = useTranslation(['orchestrator']);

  if (!selectedTask) return null;

  return (
    <Drawer
      open
      titleId="orchestrator-task-drawer-title"
      side="right"
      onClose={onClose}
      className={styles.taskDrawer}
    >
      <div className={styles.taskDrawerHeader}>
        <div className={styles.taskDrawerTitleGroup}>
          <span className={styles.label}>{t('orchestrator:detail.drawerLabel')}</span>
          <h2 id="orchestrator-task-drawer-title" className={styles.taskDrawerTitle}>
            {selectedTask.title}
          </h2>
        </div>
        <Button
          variant="icon"
          aria-label={t('orchestrator:detail.close')}
          icon={<XIcon />}
          onClick={onClose}
        />
      </div>
      <div className={styles.taskDrawerContent}>
        <div className={styles.detail}>
          <Card variant="outlined" padding="md">
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:detail.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:detail.subtitle')}</p>
              </div>
              <div className={styles.detailActions}>
                {selectedTaskCanStart ? (
                  <Button
                    variant="primary"
                    size="sm"
                    icon={<PlayIcon />}
                    loading={startingTaskId === selectedTask.id}
                    onClick={onStart}
                  >
                    {t('orchestrator:detail.start')}
                  </Button>
                ) : null}
                {selectedTaskCanComplete ? (
                  <Button
                    variant="primary"
                    size="sm"
                    icon={<CheckIcon />}
                    loading={completingTaskId === selectedTask.id}
                    onClick={onCompleteAgentRun}
                  >
                    {t('orchestrator:detail.completeAgentRun')}
                  </Button>
                ) : null}
                {selectedRenderableTask ? (
                  <Pill tone={selectedRenderableTask.origin === 'remote' ? 'accent' : 'neutral'} dot>
                    {selectedRenderableTask.origin === 'remote'
                      ? t('orchestrator:detail.remoteTask', {
                          deviceName:
                            selectedRenderableTask.deviceName ?? t('orchestrator:queue.unknownDevice'),
                        })
                      : t('orchestrator:detail.localTask')}
                  </Pill>
                ) : null}
                <Pill tone={orchestratorWorkflowStateTone(selectedTask.workflowState)} dot>
                  {t(WORKFLOW_STATE_LABEL_KEYS[selectedTask.workflowState])}
                </Pill>
                <Pill tone={runStateTone(selectedTask.runState)} dot>
                  {t(RUN_STATE_LABEL_KEYS[selectedTask.runState])}
                </Pill>
                <Pill tone={orchestratorStatusTone(selectedTask.status)} dot>
                  {t(STATUS_LABEL_KEYS[selectedTask.status])}
                </Pill>
              </div>
            </Card.Header>
            <Card.Body className={styles.detailBody}>
              <div className={styles.detailTitleRow}>
                <h3 className={styles.detailTitle}>{selectedTask.title}</h3>
              </div>
              {selectedTaskProgressMessage ? (
                <p className={styles.progressMessage}>{selectedTaskProgressMessage}</p>
              ) : null}
              <div className={styles.detailBlock}>
                <span className={styles.label}>{t('orchestrator:detail.goal')}</span>
                <p className={styles.detailText}>{selectedTask.goal}</p>
              </div>
              <div className={styles.detailBlock}>
                <span className={styles.label}>{t('orchestrator:detail.acceptanceCriteria')}</span>
                <p className={styles.detailText}>{selectedTask.acceptanceCriteria}</p>
              </div>
              <dl className={styles.metaGrid}>
                <div>
                  <dt>{t('orchestrator:detail.workflowState')}</dt>
                  <dd>{t(WORKFLOW_STATE_LABEL_KEYS[selectedTask.workflowState])}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.legacyStatus')}</dt>
                  <dd>{t(STATUS_LABEL_KEYS[selectedTask.status])}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.runState')}</dt>
                  <dd>{t(RUN_STATE_LABEL_KEYS[selectedTask.runState])}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.attemptPhase')}</dt>
                  <dd>
                    {selectedTask.attemptPhase
                      ? t(ATTEMPT_PHASE_LABEL_KEYS[selectedTask.attemptPhase])
                      : t('orchestrator:detail.unknown')}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.branch')}</dt>
                  <dd>{selectedTask.branchName ?? t('orchestrator:detail.unassigned')}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.attempt')}</dt>
                  <dd>{orchestratorAttemptLabel(selectedTask, t)}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.activeSession')}</dt>
                  <dd>
                    {selectedTaskTerminalLabel && selectedTaskCanOpenWorkbench ? (
                      <button
                        type="button"
                        className={styles.inlineLinkButton}
                        onClick={onOpenWorkbench}
                      >
                        {selectedTaskTerminalLabel}
                      </button>
                    ) : (
                      t('orchestrator:detail.unassigned')
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.runnerProvider')}</dt>
                  <dd>
                    {taskRuntimeValue(
                      selectedTask.runnerProvider,
                      t('orchestrator:detail.unassigned'),
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.claudeSession')}</dt>
                  <dd>
                    {taskRuntimeValue(
                      selectedTask.claudeSessionId,
                      t('orchestrator:detail.unassigned'),
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.transcript')}</dt>
                  <dd>
                    {taskRuntimeValue(
                      selectedTask.transcriptPath,
                      t('orchestrator:detail.unassigned'),
                    )}
                  </dd>
                </div>
                {selectedRenderableTask?.origin === 'remote' ? (
                  <div>
                    <dt>{t('orchestrator:detail.executionDevice')}</dt>
                    <dd>
                      {selectedRenderableTask.deviceName ?? t('orchestrator:queue.unknownDevice')}
                    </dd>
                  </div>
                ) : null}
                <div>
                  <dt>{t('orchestrator:detail.createdAt')}</dt>
                  <dd>{formatTaskTimestamp(selectedTask.createdAt)}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.updatedAt')}</dt>
                  <dd>{formatTaskTimestamp(selectedTask.updatedAt)}</dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.lastActivity')}</dt>
                  <dd>
                    {formatOptionalTaskTimestamp(
                      selectedTask.lastActivityAt,
                      t('orchestrator:detail.unknown'),
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.lastEvent')}</dt>
                  <dd>
                    {taskRuntimeValue(
                      selectedTask.lastRuntimeEvent,
                      t('orchestrator:detail.unknown'),
                    )}
                  </dd>
                </div>
                <div>
                  <dt>{t('orchestrator:detail.lastMessage')}</dt>
                  <dd>
                    {taskRuntimeValue(
                      selectedTask.lastRuntimeMessage,
                      t('orchestrator:detail.unknown'),
                    )}
                  </dd>
                </div>
              </dl>
              {selectedTask.status === 'blocked' ? (
                <div className={styles.blockedReason}>
                  <span className={styles.label}>{t('orchestrator:detail.blockedReason')}</span>
                  <p>{selectedTask.blockedReason ?? t('orchestrator:detail.noBlockedReason')}</p>
                </div>
              ) : null}
              {latestVerifierEvidence ? (
                <div className={styles.detailEvidenceSummary}>
                  <div className={styles.detailEvidenceHeader}>
                    <span className={styles.label}>
                      {t('orchestrator:detail.latestVerifierResult')}
                    </span>
                    <Pill tone={evidenceSummaryTone(latestVerifierEvidence.summary)}>
                      {t(evidenceSummaryLabelKey(latestVerifierEvidence.summary))}
                    </Pill>
                  </div>
                  <pre className={styles.detailEvidenceContent}>
                    {latestVerifierEvidence.content}
                  </pre>
                </div>
              ) : null}
              {latestRepairPromptEvidence ? (
                <div className={styles.detailEvidenceSummary}>
                  <div className={styles.detailEvidenceHeader}>
                    <span className={styles.label}>
                      {t('orchestrator:detail.latestRepairPrompt')}
                    </span>
                    <Pill tone={orchestratorEvidenceKindTone(latestRepairPromptEvidence.kind)}>
                      {orchestratorEvidenceKindLabel(latestRepairPromptEvidence.kind, t)}
                    </Pill>
                  </div>
                  <pre className={styles.detailEvidenceContent}>
                    {latestRepairPromptEvidence.content}
                  </pre>
                </div>
              ) : null}
              {developmentAttemptEvidenceItems.length > 0 ? (
                <div className={styles.attemptHistory}>
                  <span className={styles.label}>{t('orchestrator:detail.priorAttempts')}</span>
                  <ul className={styles.attemptHistoryList}>
                    {developmentAttemptEvidenceItems.map((item) => (
                      <li className={styles.attemptHistoryItem} key={item.id}>
                        <span>{item.title}</span>
                        <Pill tone={evidenceSummaryTone(item.summary)}>
                          {t(evidenceSummaryLabelKey(item.summary))}
                        </Pill>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : null}
              {selectedTaskCanOpenWorkbench ||
              selectedTaskCanControlBlocked ||
              selectedTaskCanRequestRework ||
              selectedTaskCanDeliver ||
              selectedTaskCanCancel ? (
                <div className={styles.blockedControls}>
                  {selectedTaskCanOpenWorkbench ? (
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<FolderIcon />}
                      onClick={onOpenWorkbench}
                    >
                      {t('orchestrator:detail.openWorkbench')}
                    </Button>
                  ) : null}
                  {selectedTaskCanControlBlocked ? (
                    <Button
                      variant="primary"
                      size="sm"
                      icon={<SyncIcon />}
                      loading={retryingTaskId === selectedTask.id}
                      onClick={onRetry}
                    >
                      {t('orchestrator:detail.retry')}
                    </Button>
                  ) : null}
                  {selectedTaskCanRequestRework ? (
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<SyncIcon />}
                      loading={reworkingTaskId === selectedTask.id}
                      onClick={onRequestRework}
                    >
                      {t('orchestrator:detail.requestRework')}
                    </Button>
                  ) : null}
                  {selectedTaskCanDeliver ? (
                    <Button
                      variant="primary"
                      size="sm"
                      icon={<CheckIcon />}
                      loading={deliveringTaskId === selectedTask.id}
                      onClick={onDeliver}
                    >
                      {t('orchestrator:detail.deliver')}
                    </Button>
                  ) : null}
                  {selectedTaskCanCancel ? (
                    <Button
                      variant="danger"
                      size="sm"
                      icon={<StopIcon />}
                      loading={cancelingTaskId === selectedTask.id}
                      onClick={onCancel}
                    >
                      {t('orchestrator:detail.cancel')}
                    </Button>
                  ) : null}
                </div>
              ) : null}
            </Card.Body>
          </Card>
        </div>

        <div className={styles.rightStack}>
          <Card variant="outlined" padding="md" className={styles.evidence}>
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:evidence.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:evidence.subtitle')}</p>
              </div>
              <Pill tone="neutral">{evidenceItems.length}</Pill>
            </Card.Header>
            <Card.Body className={styles.evidenceBody}>
              {evidenceLoading ? (
                <p className={styles.muted}>{t('orchestrator:evidence.loading')}</p>
              ) : null}
              {!evidenceLoading && evidenceError ? (
                <div className={styles.errorBox} role="alert">
                  {evidenceError}
                </div>
              ) : null}
              {!evidenceLoading && !evidenceError && evidenceItems.length === 0 ? (
                <div className={styles.empty}>
                  <h3 className={styles.emptyTitle}>{t('orchestrator:evidence.emptyTitle')}</h3>
                  <p className={styles.emptyBody}>{t('orchestrator:evidence.emptyBody')}</p>
                </div>
              ) : null}
              {!evidenceLoading && !evidenceError && evidenceItems.length > 0 ? (
                <ul className={styles.evidenceList}>
                  {evidenceItems.map((item) => (
                    <li className={styles.evidenceItem} key={item.id}>
                      <div className={styles.evidenceItemHeader}>
                        <div>
                          <h3 className={styles.evidenceTitle}>{item.title}</h3>
                          <p className={styles.evidenceMeta}>
                            {formatTaskTimestamp(item.createdAt)}
                          </p>
                        </div>
                        <div className={styles.evidencePills}>
                          <Pill tone={orchestratorEvidenceKindTone(item.kind)}>
                            {orchestratorEvidenceKindLabel(item.kind, t)}
                          </Pill>
                          <Pill tone={evidenceSummaryTone(item.summary)}>
                            {t(evidenceSummaryLabelKey(item.summary))}
                          </Pill>
                        </div>
                      </div>
                      <pre className={styles.evidenceContent}>{item.content}</pre>
                    </li>
                  ))}
                </ul>
              ) : null}
            </Card.Body>
          </Card>
        </div>
      </div>
    </Drawer>
  );
}
