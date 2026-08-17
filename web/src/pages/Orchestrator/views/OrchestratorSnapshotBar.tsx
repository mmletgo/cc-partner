/**
 * Orchestrator 运行时状态条
 *
 * Business Logic（为什么需要这个组件）:
 *   工作区要把调度器/工作流/槽位放在看板上方的一行摘要里，详细运行列表默认收起，避免把泳道挤出首屏。
 *
 * Code Logic（这个组件做什么）:
 *   渲染紧凑状态条、可展开的运行摘要，以及始终可见的错误/远端不可用提示；不 import API。
 */
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import { RefreshIcon, SettingsIcon } from '@/lib/icons';
import type {
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRuntimeEvent,
  OrchestratorRuntimeSnapshot,
  OrchestratorRuntimeTaskSummary,
} from '@/lib/types';
import {
  ATTEMPT_PHASE_LABEL_KEYS,
  formatOptionalTaskTimestamp,
  formatTaskTimestamp,
  RUN_STATE_LABEL_KEYS,
  WORKFLOW_STATE_LABEL_KEYS,
} from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   状态条只展示 controller 已经算好的 snapshot 与远端态，不自己发请求。
 *
 * Code Logic（这个类型做什么）:
 *   描述 snapshot、远端四态、加载/错误与刷新/设置/WORKFLOW 回调。
 */
export interface OrchestratorSnapshotBarProps {
  snapshot: OrchestratorRuntimeSnapshot | null;
  remoteStatus: OrchestratorRemoteRuntimeStatus | null;
  cachedAt: string | null;
  loading: boolean;
  errorMessage: string | null;
  showContent: boolean;
  onRefresh: () => void;
  onOpenSettings: () => void;
  onOpenWorkflowWizard: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板需要一行就能判断调度是否可用；运行中任务与事件只在展开后占用高度。
 *
 * Code Logic（这个函数做什么）:
 *   始终渲染操作与关键 Pill；有快照时再渲染指标、details 与错误。
 */
export function OrchestratorSnapshotBar(props: OrchestratorSnapshotBarProps): JSX.Element {
  const {
    snapshot,
    remoteStatus,
    cachedAt,
    loading,
    errorMessage,
    showContent,
    onRefresh,
    onOpenSettings,
    onOpenWorkflowWizard,
  } = props;
  const { t } = useTranslation(['orchestrator']);
  const runtimeSnapshot = snapshot;
  const hasDetailLists = Boolean(
    runtimeSnapshot &&
      (runtimeSnapshot.runningTasks.length > 0 ||
        runtimeSnapshot.retryingTasks.length > 0 ||
        runtimeSnapshot.recentEvents.length > 0),
  );

  return (
    <div className={styles.snapshotBar}>
      <div className={styles.snapshotStrip}>
        <div className={styles.snapshotPrimary}>
          <span className={styles.label}>{t('orchestrator:snapshot.title')}</span>
          {loading ? <Pill tone="accent">{t('orchestrator:snapshot.loading')}</Pill> : null}
          {remoteStatus === 'offline' && cachedAt ? (
            <Pill tone="warn">
              {t('orchestrator:snapshot.remoteOfflineCachedBadge', {
                time: formatTaskTimestamp(cachedAt),
              })}
            </Pill>
          ) : null}
          {showContent && runtimeSnapshot ? (
            <>
              <Pill tone={runtimeSnapshot.schedulerEnabled ? 'success' : 'warn'} dot>
                {runtimeSnapshot.schedulerEnabled
                  ? t('orchestrator:snapshot.schedulerEnabled')
                  : t('orchestrator:snapshot.schedulerDisabled')}
              </Pill>
              <Pill tone={runtimeSnapshot.workflowValid ? 'success' : 'danger'} dot>
                {runtimeSnapshot.workflowValid
                  ? t('orchestrator:snapshot.workflowValid')
                  : t('orchestrator:snapshot.workflowInvalid')}
              </Pill>
              <span className={styles.snapshotMetric}>
                {t('orchestrator:snapshot.slotsUsed', {
                  used: runtimeSnapshot.slotsUsed,
                  max: runtimeSnapshot.maxConcurrentTasks,
                })}
              </span>
              <span className={styles.snapshotMetric}>
                {t('orchestrator:snapshot.runningCount', {
                  count: runtimeSnapshot.runningTasks.length,
                })}
              </span>
              {runtimeSnapshot.retryingTasks.length > 0 ? (
                <span className={styles.snapshotMetric}>
                  {t('orchestrator:snapshot.retryingCount', {
                    count: runtimeSnapshot.retryingTasks.length,
                  })}
                </span>
              ) : null}
            </>
          ) : null}
        </div>
        <div className={styles.snapshotActions}>
          <Button
            variant="ghost"
            size="sm"
            onClick={onOpenWorkflowWizard}
            data-testid="open-workflow-wizard"
          >
            {t('orchestrator:workflowWizard.open')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            icon={<SettingsIcon />}
            onClick={onOpenSettings}
          >
            {t('orchestrator:snapshot.settings')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            icon={<RefreshIcon />}
            disabled={loading}
            onClick={onRefresh}
          >
            {t('orchestrator:snapshot.refresh')}
          </Button>
        </div>
      </div>
      {showContent && runtimeSnapshot ? (
        <>
          {remoteStatus === 'offline' && cachedAt ? (
            <p className={styles.snapshotMuted} role="status">
              {t('orchestrator:snapshot.remoteOfflineCached', {
                time: formatTaskTimestamp(cachedAt),
              })}
            </p>
          ) : null}
          <details className={styles.snapshotDetails}>
            <summary className={styles.snapshotDetailsSummary}>
              {t('orchestrator:snapshot.details')}
              {hasDetailLists
                ? ` · ${t('orchestrator:snapshot.runningCount', {
                    count: runtimeSnapshot.runningTasks.length,
                  })}`
                : ''}
            </summary>
            <div className={styles.snapshotDetailsBody}>
              <div className={styles.snapshotMetaGrid}>
                <span className={styles.snapshotMetric}>
                  {t('orchestrator:snapshot.generatedAt', {
                    time: formatTaskTimestamp(runtimeSnapshot.generatedAt),
                  })}
                </span>
                <span className={styles.snapshotMetric}>
                  {t('orchestrator:snapshot.latestTickAt', {
                    time: formatOptionalTaskTimestamp(
                      runtimeSnapshot.latestTickAt,
                      t('orchestrator:snapshot.latestTickUnknown'),
                    ),
                  })}
                </span>
                <span className={styles.snapshotMetric}>
                  {t('orchestrator:snapshot.slotsAvailable', {
                    available: runtimeSnapshot.slotsAvailable,
                  })}
                </span>
                <span className={styles.snapshotMetric}>
                  {t('orchestrator:snapshot.lastDispatchedCount', {
                    count: runtimeSnapshot.lastDispatchedCount,
                  })}
                </span>
              </div>
              {runtimeSnapshot.runningTasks.length > 0 ? (
                <section className={styles.snapshotSection}>
                  <h3 className={styles.snapshotSectionTitle}>
                    {t('orchestrator:snapshot.runningTasks')}
                  </h3>
                  <ul className={styles.snapshotList}>
                    {runtimeSnapshot.runningTasks.map((task: OrchestratorRuntimeTaskSummary) => (
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
                    ))}
                  </ul>
                </section>
              ) : null}
              {runtimeSnapshot.retryingTasks.length > 0 ? (
                <section className={styles.snapshotSection}>
                  <h3 className={styles.snapshotSectionTitle}>
                    {t('orchestrator:snapshot.retryingTasks')}
                  </h3>
                  <ul className={styles.snapshotList}>
                    {runtimeSnapshot.retryingTasks.map((task: OrchestratorRuntimeTaskSummary) => (
                      <li className={styles.snapshotListItem} key={task.taskId}>
                        <span className={styles.snapshotItemTitle}>{task.title}</span>
                        <span className={styles.snapshotItemMeta}>
                          {t(WORKFLOW_STATE_LABEL_KEYS[task.workflowState])}
                          {' · '}
                          {t(RUN_STATE_LABEL_KEYS[task.runState])}
                        </span>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}
              {runtimeSnapshot.recentEvents.length > 0 ? (
                <section className={styles.snapshotSection}>
                  <h3 className={styles.snapshotSectionTitle}>
                    {t('orchestrator:snapshot.recentEvents')}
                  </h3>
                  <ul className={styles.snapshotList}>
                    {runtimeSnapshot.recentEvents.map((event: OrchestratorRuntimeEvent) => (
                      <li className={styles.snapshotListItem} key={event.id}>
                        <span className={styles.snapshotItemTitle}>{event.taskTitle}</span>
                        <span className={styles.snapshotItemMeta}>
                          {event.kind} · {event.message} · {formatTaskTimestamp(event.createdAt)}
                        </span>
                      </li>
                    ))}
                  </ul>
                </section>
              ) : null}
            </div>
          </details>
          {runtimeSnapshot.workflowError ? (
            <p className={styles.snapshotWarning}>
              {t('orchestrator:snapshot.workflowError', {
                error: runtimeSnapshot.workflowError,
              })}
            </p>
          ) : null}
          {runtimeSnapshot.latestError ? (
            <p className={styles.snapshotWarning}>
              {t('orchestrator:snapshot.latestError', {
                error: runtimeSnapshot.latestError,
              })}
            </p>
          ) : null}
        </>
      ) : null}
      {!showContent && !loading ? (
        <p className={styles.snapshotMuted} role="status">
          {remoteStatus === 'unsupported'
            ? t('orchestrator:snapshot.remoteUnsupported')
            : remoteStatus === 'offline'
              ? t('orchestrator:snapshot.remoteOffline')
              : t('orchestrator:snapshot.remoteUnavailable')}
        </p>
      ) : null}
      {errorMessage ? (
        <p className={styles.snapshotWarning} role="status">
          {errorMessage}
        </p>
      ) : null}
    </div>
  );
}
