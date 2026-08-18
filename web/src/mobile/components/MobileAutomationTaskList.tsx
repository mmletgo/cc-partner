import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  canAppendToBlock,
  canCreateInLane,
  canReorderBlock,
} from '@/pages/Orchestrator/orchestratorBoard';
import type { OrchestratorBoardItem } from '@/pages/Orchestrator/orchestratorBoard';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import {
  MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS,
  MOBILE_AUTOMATION_RUN_LABEL_KEYS,
  MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS,
  runtimeValue,
  type MobileAutomationTaskListProps,
} from '../controllers/useMobileAutomationController';
import styles from '../MobileWorkbench.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   块成员与独立任务行共用同一套摘要，避免两套列表漂移。
 *
 * Code Logic（这个函数做什么）:
 *   渲染标题、来源、goal/runtime 与状态徽章。
 */
function MobileTaskRow(props: {
  task: OrchestratorRenderableTask;
  selected: boolean;
  unknownLabel: string;
  onSelect: () => void;
}): ReactElement {
  const { task, selected, unknownLabel, onSelect } = props;
  const { t } = useTranslation(['workbench', 'orchestrator']);
  const view = task.view;
  const taskDto = task.task;
  const originLabel =
    task.origin === 'remote'
      ? t('workbench:mobile.automationPanel.origin.remote', {
          deviceName: task.deviceName ?? t('workbench:mobile.automationPanel.origin.unknownDevice'),
        })
      : t('workbench:mobile.automationPanel.origin.local');
  return (
    <button
      type="button"
      className={`${styles.mobileListItem} ${selected ? styles.mobileListItemActive : ''}`}
      aria-pressed={selected}
      onClick={onSelect}
    >
      <div className={styles.mobileListTitleRow}>
        <strong className={styles.mobileListTitle}>{taskDto.title}</strong>
        <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>{originLabel}</span>
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
        <span className={styles.mobileBadge}>{t(MOBILE_AUTOMATION_RUN_LABEL_KEYS[taskDto.runState])}</span>
        <span className={styles.mobileBadge}>
          {taskDto.attemptPhase
            ? t(MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS[taskDto.attemptPhase])
            : unknownLabel}
        </span>
        <span className={styles.mobileListMeta}>
          {t('workbench:mobile.automationPanel.attempt', { attempt: taskDto.attempt })}
        </span>
      </div>
      <span className={styles.mobileListMeta} hidden>
        {view.origin}
      </span>
    </button>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端把块渲染为可展开组，并在组内提供追加与上移/下移。
 *
 * Code Logic（这个函数做什么）:
 *   展开时列出成员；canReorder 时显示上移/下移；canAppend 时显示末尾添加。
 */
function MobileBlockGroup(props: {
  item: Extract<OrchestratorBoardItem, { kind: 'block' }>;
  expanded: boolean;
  selectedTaskId: string | null;
  unknownLabel: string;
  onToggle: () => void;
  onSelectTaskView: MobileAutomationTaskListProps['onSelectTaskView'];
  onOpenAppend: (blockId: string) => void;
  onReorderBlock: (blockId: string, orderedTaskIds: string[]) => void;
}): ReactElement {
  const { item, expanded, selectedTaskId, unknownLabel, onToggle, onSelectTaskView, onOpenAppend, onReorderBlock } =
    props;
  const { t } = useTranslation(['workbench']);
  const canReorder = canReorderBlock(item.members);
  const canAppend = canAppendToBlock(item.members);

  /**
   * Business Logic（为什么需要这个函数）:
   *   上移/下移只交换相邻成员。
   *
   * Code Logic（这个函数做什么）:
   *   交换 id 后提交完整置换。
   */
  const moveMember = (index: number, delta: number): void => {
    const next = item.members.map((member) => member.task.id);
    const target = index + delta;
    const currentId = next[index];
    const targetId = next[target];
    if (!currentId || !targetId) return;
    next[index] = targetId;
    next[target] = currentId;
    onReorderBlock(item.blockId, next);
  };

  return (
    <div>
      <button type="button" className={styles.mobileListItem} onClick={onToggle}>
        <div className={styles.mobileListTitleRow}>
          <strong className={styles.mobileListTitle}>{item.title}</strong>
          <span className={styles.mobileBadge}>
            {t('orchestrator:blocks.members', { count: item.members.length })}
          </span>
        </div>
        <span className={styles.mobileListMeta}>
          {expanded
            ? t('workbench:mobile.automationPanel.collapseBlock')
            : t('workbench:mobile.automationPanel.expandBlock')}
        </span>
      </button>
      {expanded ? (
        <div className={styles.mobileList}>
          {item.members.map((member, index) => (
            <div key={member.task.id}>
              {canReorder ? (
                <div className={styles.mobileBadgeRow}>
                  <button
                    type="button"
                    className={styles.secondaryButton}
                    disabled={index === 0}
                    onClick={() => moveMember(index, -1)}
                  >
                    {t('workbench:mobile.automationPanel.moveUp')}
                  </button>
                  <button
                    type="button"
                    className={styles.secondaryButton}
                    disabled={index === item.members.length - 1}
                    onClick={() => moveMember(index, 1)}
                  >
                    {t('workbench:mobile.automationPanel.moveDown')}
                  </button>
                </div>
              ) : null}
              <MobileTaskRow
                task={member}
                selected={selectedTaskId === member.task.id}
                unknownLabel={unknownLabel}
                onSelect={() => onSelectTaskView(member.view)}
              />
            </div>
          ))}
          {canAppend ? (
            <button
              type="button"
              className={styles.secondaryButton}
              onClick={() => onOpenAppend(item.blockId)}
            >
              {t('workbench:mobile.automationPanel.appendTitle')}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * MobileAutomationTaskList（移动端自动化任务分组列表）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端自动化面板需要按 workflow 泳道展示真实任务与任务块，点击后展开详情，而不引入拖拽看板。
 *
 * Code Logic（这个组件做什么）:
 *   纯展示：按 visibleWorkflowStates 渲染 grouped board items；Backlog/Todo 头提供 +。
 *   不导入 transport/API。
 */
export function MobileAutomationTaskList({
  visibleWorkflowStates,
  groupedTasks,
  selectedTaskId,
  unknownLabel,
  expandedBlockIds,
  onSelectTaskView,
  onToggleBlock,
  onOpenLaneCreate,
  canCreateTaskBlock,
  onOpenAppend,
  onReorderBlock,
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
            {canCreateInLane(workflowState) ? (
              <>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  onClick={() => onOpenLaneCreate(workflowState, 'task')}
                >
                  {t('workbench:mobile.automationPanel.addTask')}
                </button>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  disabled={!canCreateTaskBlock}
                  onClick={() => onOpenLaneCreate(workflowState, 'taskBlock')}
                >
                  {t('workbench:mobile.automationPanel.addBlock')}
                </button>
              </>
            ) : null}
          </div>
          <div className={styles.mobileList}>
            {groupedTasks[workflowState].map((card) => {
              if (card.kind === 'block') {
                return (
                  <MobileBlockGroup
                    item={card}
                    key={card.blockId}
                    expanded={expandedBlockIds.includes(card.blockId)}
                    selectedTaskId={selectedTaskId}
                    unknownLabel={unknownLabel}
                    onToggle={() => onToggleBlock(card.blockId)}
                    onSelectTaskView={onSelectTaskView}
                    onOpenAppend={onOpenAppend}
                    onReorderBlock={onReorderBlock}
                  />
                );
              }
              return (
                <MobileTaskRow
                  key={card.item.task.id}
                  task={card.item}
                  selected={selectedTaskId === card.item.task.id}
                  unknownLabel={unknownLabel}
                  onSelect={() => onSelectTaskView(card.item.view)}
                />
              );
            })}
          </div>
        </section>
      ))}
    </>
  );
}

export type { MobileAutomationTaskListProps };
