import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  formatRuntimeTimestamp,
  useMobileAutomationController,
  type MobileAutomationExecutionContext,
  type MobileAutomationPanelProps,
} from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';
import { MobileAutomationExperiments } from './MobileAutomationExperiments';
import { MobileAutomationCreateDialog } from './MobileAutomationCreateDialog';
import { MobileAutomationOutbox } from './MobileAutomationOutbox';
import { MobileAutomationTaskDetail } from './MobileAutomationTaskDetail';
import { MobileAutomationTaskList } from './MobileAutomationTaskList';

export type { MobileAutomationExecutionContext, MobileAutomationPanelProps };

/**
 * MobileAutomationPanel（移动端项目级自动化面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要在本机或远端项目中查看项目级 Orchestrator 任务、创建任务，并进入任务执行现场。
 *
 * Code Logic（这个组件做什么）:
 *   组合层：调用 useMobileAutomationController，渲染 shell chrome + runtime strip + 四个纯展示子视图。
 *   不直接调用 transport/API；公开类型仍从本文件 re-export 以稳定 MobileWorkbench 导入路径。
 */
export function MobileAutomationPanel(props: MobileAutomationPanelProps): ReactElement {
  const { shell, taskList, taskDetail, createDialog, outbox, experiments } =
    useMobileAutomationController(props);
  const { t } = useTranslation(['workbench', 'orchestrator']);
  const {
    titleId,
    runtimeTitleId,
    hasProject,
    loading,
    error,
    status,
    isListEmpty,
    runtimeDisplay,
    runtimeStatusLabel,
    showRuntimeCachedHint,
    onRefresh,
    onOpenCreateDialog,
  } = shell;

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <h1 id={titleId}>{t('workbench:mobile.automationPanel.title')}</h1>
        </div>
        <div className={styles.panelHeaderActions}>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={!hasProject || loading}
            onClick={onRefresh}
          >
            {t('workbench:refresh')}
          </button>
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={!hasProject || loading}
            onClick={onOpenCreateDialog}
          >
            {t('workbench:mobile.automationPanel.createOpen')}
          </button>
        </div>
      </div>

      {!hasProject ? (
        <p className={styles.panelState}>{t('workbench:mobile.automationPanel.noProject')}</p>
      ) : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}
      {status ? <p className={styles.panelState}>{status}</p> : null}

      {hasProject ? (
        <>
          <section className={styles.mobileAutomationGroup} aria-labelledby={runtimeTitleId}>
            <div className={styles.mobileAutomationGroupHeader}>
              <span id={runtimeTitleId}>
                {t('workbench:mobile.automationPanel.runtimeSnapshotTitle')}
              </span>
              <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                {runtimeDisplay.loading
                  ? t('workbench:mobile.automationPanel.runtimeSnapshotLoading')
                  : runtimeStatusLabel}
              </span>
            </div>
            <div className={styles.automationTaskBody}>
              {runtimeDisplay.loading && !runtimeDisplay.snapshot ? (
                <p className={styles.panelState}>
                  {t('workbench:mobile.automationPanel.runtimeSnapshotLoading')}
                </p>
              ) : null}
              {runtimeDisplay.error ? (
                <p className={styles.panelState}>{runtimeDisplay.error.message}</p>
              ) : null}
              {!runtimeDisplay.loading &&
              !runtimeDisplay.snapshot &&
              runtimeDisplay.remoteStatus &&
              runtimeDisplay.remoteStatus !== 'live' ? (
                <p className={styles.panelState}>{runtimeStatusLabel}</p>
              ) : null}
              {runtimeDisplay.snapshot ? (
                <>
                  <p className={styles.mobileListMeta}>
                    {t('workbench:mobile.automationPanel.runtimeGeneratedAt', {
                      time: formatRuntimeTimestamp(runtimeDisplay.snapshot.generatedAt),
                    })}
                  </p>
                  <p className={styles.mobileListMeta}>
                    {t('workbench:mobile.automationPanel.runtimeLatestTickAt', {
                      time: runtimeDisplay.snapshot.latestTickAt
                        ? formatRuntimeTimestamp(runtimeDisplay.snapshot.latestTickAt)
                        : t('workbench:mobile.automationPanel.runtimeLatestTickUnknown'),
                    })}
                  </p>
                  <p>
                    {t('workbench:mobile.automationPanel.runtimeSlots', {
                      used: runtimeDisplay.snapshot.slotsUsed,
                      max: runtimeDisplay.snapshot.maxConcurrentTasks,
                      running: runtimeDisplay.snapshot.runningTasks.length,
                      retrying: runtimeDisplay.snapshot.retryingTasks.length,
                    })}
                  </p>
                  {runtimeDisplay.snapshot.latestError ? (
                    <p>
                      {t('workbench:mobile.automationPanel.runtimeLatestError', {
                        error: runtimeDisplay.snapshot.latestError,
                      })}
                    </p>
                  ) : null}
                  {runtimeDisplay.snapshot.recentEvents.length > 0 ? (
                    <div className={styles.mobileList}>
                      <p className={styles.mobileListMeta}>
                        {t('workbench:mobile.automationPanel.runtimeRecentEvents')}
                      </p>
                      {runtimeDisplay.snapshot.recentEvents.map((event) => (
                        <p className={styles.mobileListMeta} key={event.id}>
                          {event.taskTitle}: {event.message}
                        </p>
                      ))}
                    </div>
                  ) : null}
                </>
              ) : null}
              {runtimeDisplay.cachedAt ? (
                <p className={styles.mobileListMeta}>
                  {t('workbench:mobile.automationPanel.runtimeLastUpdated', {
                    time: formatRuntimeTimestamp(runtimeDisplay.cachedAt),
                  })}
                </p>
              ) : null}
              {showRuntimeCachedHint ? (
                <p className={styles.mobileListMeta}>
                  {t('workbench:mobile.automationPanel.runtimeCachedHint')}
                </p>
              ) : null}
            </div>
          </section>

          {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
          {isListEmpty ? (
            <p className={styles.panelState}>{t('workbench:mobile.automationPanel.empty')}</p>
          ) : null}

          <div
            className={styles.mobileList}
            aria-label={t('workbench:mobile.automationPanel.listAriaLabel')}
          >
            <MobileAutomationTaskList {...taskList} />
            <MobileAutomationOutbox {...outbox} />
            <MobileAutomationExperiments {...experiments} />
          </div>
          <MobileAutomationTaskDetail {...taskDetail} />
          <MobileAutomationCreateDialog {...createDialog} />
        </>
      ) : null}
    </section>
  );
}
