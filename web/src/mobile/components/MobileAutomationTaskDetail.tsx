import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { orchestratorEvidenceKindTone } from '@/lib/orchestrator';
import {
  formatAutomationTimestamp,
  mobileAutomationEvidenceKindLabelKey,
  MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS,
  MOBILE_AUTOMATION_RUN_LABEL_KEYS,
  MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS,
  runtimeValue,
  type MobileAutomationTaskDetailProps,
} from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * MobileAutomationTaskDetail（移动端自动化任务详情）
 *
 * Business Logic（为什么需要这个组件）:
 *   选中真实任务后，用户需要在手机端查看 goal/runtime/blockedReason 与 evidence；
 *   A0 后无 review diff 产品面；本轨道不提供 Deliver/Rework。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：渲染 selectedTask 详情与 evidence；无 transport/API 调用。
 *   selectedTask 为空时返回 null。hooks 全部在 early return 前。
 */
export function MobileAutomationTaskDetail({
  selectedTask,
  detailTitleId,
  unknownLabel,
  evidenceItems,
  evidenceLoading,
  evidenceError,
  canOpenExecutionContext,
  onCloseDetails,
  onOpenExecutionContext,
}: MobileAutomationTaskDetailProps): ReactElement | null {
  const { t } = useTranslation(['workbench', 'orchestrator']);

  if (!selectedTask) {
    return null;
  }

  return (
    <aside className={styles.mobileAutomationDetail} aria-labelledby={detailTitleId}>
      <div className={styles.mobileListTitleRow}>
        <div className={styles.panelHeader}>
          <h2 id={detailTitleId}>{selectedTask.title}</h2>
        </div>
        <button type="button" className={styles.secondaryButton} onClick={onCloseDetails}>
          {t('workbench:mobile.automationPanel.closeDetails')}
        </button>
      </div>

      <div className={styles.mobileAutomationDetailBlock}>
        <span>{t('workbench:mobile.automationPanel.fields.goal')}</span>
        <p>{selectedTask.goal}</p>
      </div>
      <div className={styles.mobileAutomationDetailBlock}>
        <span>{t('workbench:mobile.automationPanel.fields.acceptanceCriteria')}</span>
        <p>{selectedTask.acceptanceCriteria}</p>
      </div>

      <dl className={styles.mobileAutomationDetailGrid}>
        <div>
          <dt>{t('workbench:mobile.automationPanel.workflowState')}</dt>
          <dd>{t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[selectedTask.workflowState])}</dd>
        </div>
        <div>
          <dt>{t('workbench:mobile.automationPanel.runStateLabel')}</dt>
          <dd>{t(MOBILE_AUTOMATION_RUN_LABEL_KEYS[selectedTask.runState])}</dd>
        </div>
        <div>
          <dt>{t('workbench:mobile.automationPanel.attemptPhaseLabel')}</dt>
          <dd>
            {selectedTask.attemptPhase
              ? t(MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS[selectedTask.attemptPhase])
              : unknownLabel}
          </dd>
        </div>
        <div>
          <dt>{t('workbench:mobile.automationPanel.runtimeMessageLabel')}</dt>
          <dd>{runtimeValue(selectedTask.lastRuntimeMessage, unknownLabel)}</dd>
        </div>
        <div>
          <dt>{t('workbench:mobile.automationPanel.claudeSession')}</dt>
          <dd>{runtimeValue(selectedTask.claudeSessionId, unknownLabel)}</dd>
        </div>
        <div>
          <dt>{t('workbench:mobile.automationPanel.transcript')}</dt>
          <dd>{runtimeValue(selectedTask.transcriptPath, unknownLabel)}</dd>
        </div>
      </dl>

      <div className={styles.mobileAutomationDetailBlock}>
        <span>{t('workbench:mobile.automationPanel.blockedReason')}</span>
        <p>{runtimeValue(selectedTask.blockedReason, unknownLabel)}</p>
      </div>

      <div className={styles.mobileBadgeRow}>
        <button
          type="button"
          className={styles.mobileTerminalPrimaryButton}
          disabled={!canOpenExecutionContext}
          onClick={onOpenExecutionContext}
        >
          {canOpenExecutionContext
            ? t('workbench:mobile.automationPanel.openExecutionContext')
            : t('workbench:mobile.automationPanel.executionContextUnavailable')}
        </button>
      </div>

      <section className={styles.mobileAutomationDetailBlock}>
        <div className={styles.mobileListTitleRow}>
          <span>{t('workbench:mobile.automationPanel.evidenceTitle')}</span>
          <span className={styles.mobileBadge}>
            {t('workbench:mobile.automationPanel.evidenceTimeline', {
              count: evidenceItems.length,
            })}
          </span>
        </div>
        {evidenceLoading ? (
          <p>{t('workbench:mobile.automationPanel.evidenceLoading')}</p>
        ) : null}
        {evidenceError ? <p role="alert">{evidenceError}</p> : null}
        {!evidenceLoading && !evidenceError && evidenceItems.length === 0 ? (
          <p>{t('workbench:mobile.automationPanel.evidenceEmpty')}</p>
        ) : null}
        {evidenceItems.length > 0 ? (
          <ul className={styles.mobileAutomationEvidenceList}>
            {evidenceItems.map((item) => (
              <li className={styles.mobileAutomationEvidenceItem} key={item.id}>
                <div className={styles.mobileListTitleRow}>
                  <strong className={styles.mobileListTitle}>{item.title}</strong>
                  <span className={styles.mobileBadge}>
                    {t(mobileAutomationEvidenceKindLabelKey(item.kind))}
                  </span>
                </div>
                <div className={styles.mobileBadgeRow}>
                  <span className={styles.mobileBadge}>
                    {formatAutomationTimestamp(item.createdAt)}
                  </span>
                  <span
                    className={`${styles.mobileBadge} ${
                      orchestratorEvidenceKindTone(item.kind) === 'success'
                        ? styles.mobileBadgeAccent
                        : ''
                    }`}
                  >
                    {item.summary || unknownLabel}
                  </span>
                </div>
                <pre>{item.content}</pre>
              </li>
            ))}
          </ul>
        ) : null}
      </section>
    </aside>
  );
}

export type { MobileAutomationTaskDetailProps };
