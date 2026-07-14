import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS,
  MOBILE_AUTOMATION_RUN_LABEL_KEYS,
  MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS,
  runtimeValue,
  type MobileAutomationTaskListProps,
} from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * MobileAutomationTaskList（移动端自动化任务分组列表）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端自动化面板需要按 workflow 泳道展示真实任务摘要，点击后展开详情，而不引入拖拽看板。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：按 visibleWorkflowStates 渲染 groupedTasks 行；调用 onSelectTaskView 选中任务。
 *   不导入 transport/API。
 */
export function MobileAutomationTaskList({
  visibleWorkflowStates,
  groupedTasks,
  selectedTaskId,
  unknownLabel,
  onSelectTaskView,
}: MobileAutomationTaskListProps): ReactElement {
  const { t } = useTranslation(['workbench', 'orchestrator']);

  return (
    <>
      {visibleWorkflowStates.map((workflowState) => (
        <section className={styles.mobileAutomationGroup} key={workflowState}>
          <div className={styles.mobileAutomationGroupHeader}>
            <span>{t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[workflowState])}</span>
            <span className={styles.mobileBadge}>
              {t('workbench:mobile.automationPanel.taskCount', {
                count: groupedTasks[workflowState].length,
              })}
            </span>
          </div>
          <div className={styles.mobileList}>
            {groupedTasks[workflowState].map((task) => {
              const view = task.view;
              const taskDto = task.task;
              const selected = selectedTaskId === taskDto.id;
              const originLabel =
                task.origin === 'remote'
                  ? t('workbench:mobile.automationPanel.origin.remote', {
                      deviceName:
                        task.deviceName ??
                        t('workbench:mobile.automationPanel.origin.unknownDevice'),
                    })
                  : t('workbench:mobile.automationPanel.origin.local');
              return (
                <button
                  type="button"
                  key={taskDto.id}
                  className={`${styles.mobileListItem} ${
                    selected ? styles.mobileListItemActive : ''
                  }`}
                  aria-pressed={selected}
                  onClick={() => {
                    onSelectTaskView(view);
                  }}
                >
                  <div className={styles.mobileListTitleRow}>
                    <strong className={styles.mobileListTitle}>{taskDto.title}</strong>
                    <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                      {originLabel}
                    </span>
                  </div>
                  <div className={styles.automationTaskBody}>
                    <p>{taskDto.goal}</p>
                    <p>
                      {t('workbench:mobile.automationPanel.runtimeMessage', {
                        value: runtimeValue(taskDto.lastRuntimeMessage, unknownLabel),
                      })}
                    </p>
                    <p>
                      {t('workbench:mobile.automationPanel.runtimeRefs', {
                        claudeSessionId: runtimeValue(taskDto.claudeSessionId, unknownLabel),
                        transcriptPath: runtimeValue(taskDto.transcriptPath, unknownLabel),
                      })}
                    </p>
                  </div>
                  <div className={styles.mobileBadgeRow}>
                    <span className={styles.mobileBadge}>
                      {t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[taskDto.workflowState])}
                    </span>
                    <span className={styles.mobileBadge}>
                      {t(MOBILE_AUTOMATION_RUN_LABEL_KEYS[taskDto.runState])}
                    </span>
                    <span className={styles.mobileBadge}>
                      {taskDto.attemptPhase
                        ? t(MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS[taskDto.attemptPhase])
                        : unknownLabel}
                    </span>
                    <span className={styles.mobileListMeta}>
                      {t('workbench:mobile.automationPanel.attempt', {
                        attempt: taskDto.attempt,
                      })}
                    </span>
                  </div>
                </button>
              );
            })}
          </div>
        </section>
      ))}
    </>
  );
}

export type { MobileAutomationTaskListProps };
