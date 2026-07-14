/**
 * Orchestrator 任务详情抽屉视图
 *
 * Business Logic（为什么需要这个模块）:
 *   选中任务后需要右侧抽屉展示 Summary / Changes / Evidence 与 Deliver/Rework 等动作；
 *   视图必须 API-free，只消费 controller props。
 *
 * Code Logic（这个模块做什么）:
 *   渲染 Drawer + roving tabs + 条件动作按钮 + Rework Dialog；hooks 全部位于 early return 前。
 */
import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type JSX,
  type KeyboardEvent,
} from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Dialog, Drawer, Pill } from '@/components/primitives';
import {
  CheckIcon,
  FolderIcon,
  PlayIcon,
  StopIcon,
  SyncIcon,
  XIcon,
} from '@/lib/icons';
import { getRovingTabIndex, type RovingTabKey } from '@/lib/rovingTablist';
import type {
  OrchestratorEvidence,
  OrchestratorReviewDiff,
  OrchestratorReviewDiffLoadState,
  OrchestratorTask,
  ReviewDiffFile,
} from '@/lib/types';
import {
  orchestratorAttemptLabel,
  orchestratorEvidenceKindLabel,
  orchestratorEvidenceKindTone,
  orchestratorStatusTone,
  orchestratorWorkflowStateTone,
} from '@/lib/orchestrator';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
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

const DETAIL_TABS = ['summary', 'changes', 'evidence'] as const;
export type OrchestratorDetailTab = (typeof DETAIL_TABS)[number];

/**
 * Business Logic（为什么需要这个类型）:
 *   抽屉只消费 controller 派生数据与回调，禁止直接 import API。
 *
 * Code Logic（这个类型做什么）:
 *   描述 selected task、capability flags、busy ids、evidence、review diff 与全部动作回调。
 */
export interface OrchestratorTaskDrawerProps {
  selectedTask: OrchestratorTask | null;
  selectedRenderableTask: OrchestratorRenderableTask | null;
  selectedTaskCanStart: boolean;
  selectedTaskCanComplete: boolean;
  selectedTaskCanRequestRework: boolean;
  selectedTaskShowDeliver: boolean;
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
  detailTab: OrchestratorDetailTab;
  onDetailTabChange: (tab: OrchestratorDetailTab) => void;
  reviewDiffState: OrchestratorReviewDiffLoadState;
  reviewDiff: OrchestratorReviewDiff | null;
  reviewDiffError: string | null;
  selectedReviewFilePath: string | null;
  onSelectReviewFilePath: (path: string | null) => void;
  onRetryReviewDiff: () => void;
  reworkDialogOpen: boolean;
  reworkError: string | null;
  onOpenReworkDialog: () => void;
  onCloseReworkDialog: () => void;
  onSubmitRework: (reason: string) => void;
  onClose: () => void;
  onStart: () => void;
  onCompleteAgentRun: () => void;
  onOpenWorkbench: () => void;
  onRetry: () => void;
  onDeliver: () => void;
  onCancel: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   选中任务后需要右侧抽屉展示完整详情、变更与 Evidence，关闭后回到纯看板。
 *
 * Code Logic（这个函数做什么）:
 *   selectedTask 为空返回 null；否则渲染 Drawer + Summary/Changes/Evidence tabs 与 footer 动作。
 */
export function OrchestratorTaskDrawer(props: OrchestratorTaskDrawerProps): JSX.Element | null {
  const {
    selectedTask,
    selectedRenderableTask,
    selectedTaskCanStart,
    selectedTaskCanComplete,
    selectedTaskCanRequestRework,
    selectedTaskShowDeliver,
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
    detailTab,
    onDetailTabChange,
    reviewDiffState,
    reviewDiff,
    reviewDiffError,
    selectedReviewFilePath,
    onSelectReviewFilePath,
    onRetryReviewDiff,
    reworkDialogOpen,
    reworkError,
    onOpenReworkDialog,
    onCloseReworkDialog,
    onSubmitRework,
    onClose,
    onStart,
    onCompleteAgentRun,
    onOpenWorkbench,
    onRetry,
    onDeliver,
    onCancel,
  } = props;
  const { t } = useTranslation(['orchestrator']);
  const tablistId = useId();
  const reworkReasonRef = useRef<HTMLTextAreaElement | null>(null);
  const [reworkReason, setReworkReason] = useState('');
  const [reworkValidationError, setReworkValidationError] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   打开返工 Dialog 时应用默认意见草稿，失败后 reason 保留在本地 state。
   *
   * Code Logic（这个函数做什么）:
   *   reworkDialogOpen 从 false→true 时用 verifier 内容或默认文案填充 reason（仅当本地为空）。
   */
  useEffect(() => {
    if (!reworkDialogOpen) return;
    setReworkValidationError(null);
    setReworkReason((current) => {
      if (current.trim()) return current;
      return (
        latestVerifierEvidence?.content?.trim() ||
        t('orchestrator:detail.requestReworkDefaultReason')
      );
    });
  }, [latestVerifierEvidence, reworkDialogOpen, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   键盘用户在 tablist 内用方向键/Home/End 切换时，焦点与选中态必须同步。
   *
   * Code Logic（这个函数做什么）:
   *   识别 RovingTabKey；用 getRovingTabIndex 求下一索引；回调 onDetailTabChange 并 focus 目标。
   */
  const handleTabKeyDown = useCallback(
    (event: KeyboardEvent<HTMLButtonElement>) => {
      const key = event.key;
      if (key !== 'ArrowLeft' && key !== 'ArrowRight' && key !== 'Home' && key !== 'End') {
        return;
      }
      event.preventDefault();
      const currentIndex = DETAIL_TABS.indexOf(detailTab);
      const nextIndex = getRovingTabIndex(currentIndex, key as RovingTabKey, DETAIL_TABS.length);
      const nextTab = DETAIL_TABS[nextIndex];
      onDetailTabChange(nextTab);
      if (typeof window !== 'undefined') {
        window.requestAnimationFrame(() => {
          document.getElementById(`${tablistId}-tab-${nextTab}`)?.focus();
        });
      }
    },
    [detailTab, onDetailTabChange, tablistId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交返工前前端校验长度，避免空意见或超长文本打到后端。
   *
   * Code Logic（这个函数做什么）:
   *   trim 后要求 1–2000；通过则 onSubmitRework，失败写本地 validation。
   */
  const handleSubmitRework = useCallback(() => {
    const trimmed = reworkReason.trim();
    if (trimmed.length < 1 || trimmed.length > 2000) {
      setReworkValidationError(t('orchestrator:detail.reworkReasonRequired'));
      return;
    }
    setReworkValidationError(null);
    onSubmitRework(trimmed);
  }, [onSubmitRework, reworkReason, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭返工 Dialog 时清空本地草稿，避免下次误用旧意见。
   *
   * Code Logic（这个函数做什么）:
   *   清空 reason/validation 并调用 onCloseReworkDialog。
   */
  const handleCloseReworkDialog = useCallback(() => {
    setReworkReason('');
    setReworkValidationError(null);
    onCloseReworkDialog();
  }, [onCloseReworkDialog]);

  const selectedFile: ReviewDiffFile | null = useMemo(() => {
    if (!reviewDiff || !selectedReviewFilePath) return null;
    return reviewDiff.files.find((file) => file.path === selectedReviewFilePath) ?? null;
  }, [reviewDiff, selectedReviewFilePath]);

  if (!selectedTask) return null;

  const showFooterActions =
    selectedTaskCanOpenWorkbench ||
    selectedTaskCanControlBlocked ||
    selectedTaskCanRequestRework ||
    selectedTaskShowDeliver ||
    selectedTaskCanCancel ||
    selectedTaskCanStart ||
    selectedTaskCanComplete;

  const deliverDisabledTitle = !selectedTaskCanDeliver
    ? reviewDiffState === 'error'
      ? t('orchestrator:detail.deliverDisabledDiffError')
      : t('orchestrator:detail.deliverDisabledPendingDiff')
    : undefined;

  return (
    <>
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

        <div className={styles.taskDrawerTabs} role="tablist" aria-label={t('orchestrator:tabs.ariaLabel')}>
          {DETAIL_TABS.map((tab) => (
            <button
              key={tab}
              id={`${tablistId}-tab-${tab}`}
              type="button"
              className={styles.taskDrawerTab}
              data-active={detailTab === tab || undefined}
              role="tab"
              aria-selected={detailTab === tab}
              aria-controls={`${tablistId}-panel-${tab}`}
              tabIndex={detailTab === tab ? 0 : -1}
              onClick={() => onDetailTabChange(tab)}
              onKeyDown={handleTabKeyDown}
            >
              {t(`orchestrator:tabs.${tab}`)}
            </button>
          ))}
        </div>

        <div className={styles.taskDrawerContent}>
          <div
            id={`${tablistId}-panel-${detailTab}`}
            role="tabpanel"
            aria-labelledby={`${tablistId}-tab-${detailTab}`}
            className={styles.taskDrawerTabPanel}
          >
            {detailTab === 'summary' ? (
              <Card variant="outlined" padding="md">
                <Card.Header className={styles.cardHeader}>
                  <div>
                    <h2 className={styles.sectionTitle}>{t('orchestrator:detail.title')}</h2>
                    <p className={styles.sectionLead}>{t('orchestrator:detail.subtitle')}</p>
                  </div>
                  <div className={styles.detailActions}>
                    {selectedRenderableTask ? (
                      <Pill tone={selectedRenderableTask.origin === 'remote' ? 'accent' : 'neutral'} dot>
                        {selectedRenderableTask.origin === 'remote'
                          ? t('orchestrator:detail.remoteTask', {
                              deviceName:
                                selectedRenderableTask.deviceName ??
                                t('orchestrator:queue.unknownDevice'),
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
                  {reviewDiff && reviewDiff.files.length > 0 ? (
                    <p className={styles.muted}>
                      {t('orchestrator:review.baseHead', {
                        base: reviewDiff.baseRef,
                        head: reviewDiff.headRef,
                      })}{' '}
                      · {t('orchestrator:review.fileCount', {
                        count: reviewDiff.totalFiles,
                      })}
                      {reviewDiff.truncated
                        ? ` · ${t('orchestrator:review.truncatedFiles', {
                            total: reviewDiff.totalFiles,
                          })}`
                        : ''}
                    </p>
                  ) : null}
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
                </Card.Body>
              </Card>
            ) : null}

            {detailTab === 'changes' ? (
              <Card variant="outlined" padding="md">
                <Card.Header className={styles.cardHeader}>
                  <div>
                    <h2 className={styles.sectionTitle}>{t('orchestrator:tabs.changes')}</h2>
                    {reviewDiff ? (
                      <p className={styles.sectionLead}>
                        {t('orchestrator:review.baseHead', {
                          base: reviewDiff.baseRef,
                          head: reviewDiff.headRef,
                        })}
                      </p>
                    ) : (
                      <p className={styles.sectionLead}>{t('orchestrator:review.selectFile')}</p>
                    )}
                  </div>
                  {reviewDiff ? <Pill tone="neutral">{reviewDiff.totalFiles}</Pill> : null}
                </Card.Header>
                <Card.Body className={styles.evidenceBody}>
                  {reviewDiffState === 'loading' || reviewDiffState === 'idle' ? (
                    <p className={styles.muted}>{t('orchestrator:review.loading')}</p>
                  ) : null}
                  {reviewDiffState === 'error' ? (
                    <div className={styles.errorBox} role="alert">
                      <p>{reviewDiffError || t('orchestrator:review.errorTitle')}</p>
                      <Button variant="secondary" size="sm" onClick={onRetryReviewDiff}>
                        {t('orchestrator:review.retry')}
                      </Button>
                    </div>
                  ) : null}
                  {reviewDiffState === 'unsupported' ? (
                    <p className={styles.muted}>{t('orchestrator:review.unsupported')}</p>
                  ) : null}
                  {reviewDiffState === 'ready' && reviewDiff ? (
                    <>
                      {reviewDiff.truncated ? (
                        <p className={styles.muted}>
                          {t('orchestrator:review.truncatedFiles', {
                            total: reviewDiff.totalFiles,
                          })}
                        </p>
                      ) : null}
                      {reviewDiff.files.length === 0 ? (
                        <div className={styles.empty}>
                          <h3 className={styles.emptyTitle}>{t('orchestrator:review.emptyTitle')}</h3>
                          <p className={styles.emptyBody}>{t('orchestrator:review.emptyBody')}</p>
                        </div>
                      ) : (
                        <div className={styles.reviewDiffLayout}>
                          <ul className={styles.reviewFileList}>
                            {reviewDiff.files.map((file) => {
                              const selected = selectedReviewFilePath === file.path;
                              return (
                                <li key={file.path}>
                                  <button
                                    type="button"
                                    className={styles.reviewFileButton}
                                    data-active={selected || undefined}
                                    onClick={() => onSelectReviewFilePath(file.path)}
                                  >
                                    <span className={styles.reviewFilePath}>{file.path}</span>
                                    <span className={styles.reviewFileMeta}>
                                      {file.status}
                                      {' · '}
                                      {t('orchestrator:review.fileSummary', {
                                        additions: file.additions,
                                        deletions: file.deletions,
                                      })}
                                      {file.binary ? ` · ${t('orchestrator:review.binaryFile')}` : ''}
                                      {file.truncated
                                        ? ` · ${t('orchestrator:review.truncatedPatch')}`
                                        : ''}
                                    </span>
                                  </button>
                                </li>
                              );
                            })}
                          </ul>
                          <div className={styles.reviewPatchPane}>
                            {selectedFile ? (
                              selectedFile.binary ? (
                                <p className={styles.muted}>{t('orchestrator:review.binaryFile')}</p>
                              ) : selectedFile.patch ? (
                                <>
                                  {selectedFile.truncated ? (
                                    <p className={styles.muted}>
                                      {t('orchestrator:review.truncatedPatch')}
                                    </p>
                                  ) : null}
                                  <pre className={styles.reviewPatch}>{selectedFile.patch}</pre>
                                </>
                              ) : (
                                <p className={styles.muted}>{t('orchestrator:review.emptyBody')}</p>
                              )
                            ) : (
                              <p className={styles.muted}>{t('orchestrator:review.selectFile')}</p>
                            )}
                          </div>
                        </div>
                      )}
                    </>
                  ) : null}
                </Card.Body>
              </Card>
            ) : null}

            {detailTab === 'evidence' ? (
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
            ) : null}
          </div>
        </div>

        {showFooterActions ? (
          <div className={styles.taskDrawerFooter}>
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
            {selectedTaskCanOpenWorkbench ? (
              <Button variant="secondary" size="sm" icon={<FolderIcon />} onClick={onOpenWorkbench}>
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
                onClick={onOpenReworkDialog}
              >
                {t('orchestrator:detail.requestRework')}
              </Button>
            ) : null}
            {selectedTaskShowDeliver ? (
              <Button
                variant="primary"
                size="sm"
                icon={<CheckIcon />}
                loading={deliveringTaskId === selectedTask.id}
                disabled={!selectedTaskCanDeliver}
                title={deliverDisabledTitle}
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
      </Drawer>

      <Dialog
        open={reworkDialogOpen}
        titleId="orchestrator-rework-dialog-title"
        closeOnEscape={reworkingTaskId === null}
        closeOnBackdrop={reworkingTaskId === null}
        initialFocusRef={reworkReasonRef}
        onClose={handleCloseReworkDialog}
        className={styles.reworkDialog}
      >
        <div className={styles.reworkDialogHeader}>
          <h2 id="orchestrator-rework-dialog-title" className={styles.sectionTitle}>
            {t('orchestrator:detail.reworkDialogTitle')}
          </h2>
          <p className={styles.sectionLead}>{t('orchestrator:detail.reworkDialogLead')}</p>
        </div>
        <label className={styles.reworkReasonLabel} htmlFor="orchestrator-rework-reason">
          {t('orchestrator:detail.reworkReasonLabel')}
        </label>
        <textarea
          id="orchestrator-rework-reason"
          ref={reworkReasonRef}
          className={styles.reworkReasonInput}
          value={reworkReason}
          maxLength={2000}
          rows={6}
          placeholder={t('orchestrator:detail.reworkReasonPlaceholder')}
          onChange={(event) => {
            setReworkReason(event.target.value);
            if (reworkValidationError) setReworkValidationError(null);
          }}
        />
        {reworkValidationError || reworkError ? (
          <div className={styles.errorBox} role="alert">
            {reworkValidationError || reworkError}
          </div>
        ) : null}
        <div className={styles.reworkDialogActions}>
          <Button
            variant="secondary"
            size="sm"
            disabled={reworkingTaskId !== null}
            onClick={handleCloseReworkDialog}
          >
            {t('orchestrator:detail.reworkCancel')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            loading={reworkingTaskId !== null}
            onClick={handleSubmitRework}
          >
            {t('orchestrator:detail.reworkSubmit')}
          </Button>
        </div>
      </Dialog>
    </>
  );
}
