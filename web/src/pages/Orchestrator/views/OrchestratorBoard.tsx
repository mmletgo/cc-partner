/**
 * Orchestrator 看板视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要按 workflow 泳道查看项目任务，并在非活跃本机/远端任务上拖拽到相邻泳道。
 *
 * Code Logic（这个组件做什么）:
 *   渲染固定 ORCHESTRATOR_BOARD_LANES 泳道与任务卡；通过 props 接收 groups/selection/drag handlers；
 *   不 import API 模块。
 */
import type { DragEvent, JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Pill } from '@/components/primitives';
import { orchestratorStatusTone, orchestratorWorkflowStateTone } from '@/lib/orchestrator';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type { OrchestratorTask, OrchestratorWorkflowState } from '@/lib/types';
import {
  canMoveRenderableTaskToWorkflowState,
  ORCHESTRATOR_BOARD_LANES,
  type OrchestratorBoardGroups,
} from '../orchestratorBoard';
import {
  ATTEMPT_PHASE_LABEL_KEYS,
  RUN_STATE_LABEL_KEYS,
  runStateTone,
  STATUS_LABEL_KEYS,
  WORKFLOW_STATE_LABEL_KEYS,
} from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   看板只消费 controller 下发的派生数据与拖拽 handler，不持有本地业务状态。
 *
 * Code Logic（这个类型做什么）:
 *   描述泳道 groups、当前选中、移动中任务与拖拽/选择回调。
 */
export interface OrchestratorBoardProps {
  groups: OrchestratorBoardGroups;
  selectedTask: OrchestratorTask | null;
  movingTaskId: string | null;
  onSelectTask: (taskId: string) => void;
  onTaskDragStart: (event: DragEvent<HTMLButtonElement>, item: OrchestratorRenderableTask) => void;
  onTaskDragEnd: () => void;
  onLaneDragOver: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => void;
  onLaneDrop: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板必须始终渲染全部 workflow 泳道，即使用户当前没有任务。
 *
 * Code Logic（这个函数做什么）:
 *   映射泳道与任务卡，绑定拖拽与点击选中；全部文案走 i18n。
 */
export function OrchestratorBoard(props: OrchestratorBoardProps): JSX.Element {
  const {
    groups,
    selectedTask,
    movingTaskId,
    onSelectTask,
    onTaskDragStart,
    onTaskDragEnd,
    onLaneDragOver,
    onLaneDrop,
  } = props;
  const { t } = useTranslation(['orchestrator']);

  return (
    <div className={styles.board} aria-label={t('orchestrator:workflow.boardAria')}>
      {ORCHESTRATOR_BOARD_LANES.map((lane) => (
        <section
          className={styles.lane}
          key={lane}
          onDragOver={(event) => onLaneDragOver(event, lane)}
          onDrop={(event) => onLaneDrop(event, lane)}
        >
          <div className={styles.laneHeader}>
            <span>{t(WORKFLOW_STATE_LABEL_KEYS[lane])}</span>
            <Pill tone={orchestratorWorkflowStateTone(lane)}>{groups[lane].length}</Pill>
          </div>
          <div className={styles.laneTaskList}>
            {groups[lane].map((item) => {
              const { task } = item;
              const active = selectedTask?.id === task.id;
              const moving = movingTaskId === task.id;
              const showRunState = task.runState !== 'idle';
              const showStatus = task.status === 'blocked' || task.status === 'aborted';
              const draggable =
                !moving &&
                ORCHESTRATOR_BOARD_LANES.some((targetLane) =>
                  canMoveRenderableTaskToWorkflowState(item, targetLane),
                );
              return (
                <button
                  className={`${styles.task} ${active ? styles.taskActive : ''} ${
                    moving ? styles.taskMoving : ''
                  }`}
                  type="button"
                  aria-pressed={active}
                  aria-label={t('orchestrator:queue.taskAria', { title: task.title })}
                  draggable={draggable}
                  key={task.id}
                  disabled={moving}
                  onDragStart={(event) => onTaskDragStart(event, item)}
                  onDragEnd={onTaskDragEnd}
                  onClick={() => onSelectTask(task.id)}
                >
                  <span className={styles.taskTitle}>{task.title}</span>
                  <span className={styles.taskMeta}>
                    {t('orchestrator:queue.priority', { priority: task.priority })}
                    {' · '}
                    {item.origin === 'remote'
                      ? t('orchestrator:queue.remoteTask', {
                          deviceName: item.deviceName ?? t('orchestrator:queue.unknownDevice'),
                        })
                      : t('orchestrator:queue.localTask')}
                  </span>
                  {showRunState || showStatus || task.attemptPhase ? (
                    <span className={styles.taskPills}>
                      {showRunState ? (
                        <Pill tone={runStateTone(task.runState)}>
                          {t(RUN_STATE_LABEL_KEYS[task.runState])}
                        </Pill>
                      ) : null}
                      {showStatus ? (
                        <Pill tone={orchestratorStatusTone(task.status)}>
                          {t(STATUS_LABEL_KEYS[task.status])}
                        </Pill>
                      ) : null}
                      {task.attemptPhase ? (
                        <Pill tone="neutral">{t(ATTEMPT_PHASE_LABEL_KEYS[task.attemptPhase])}</Pill>
                      ) : null}
                    </span>
                  ) : null}
                </button>
              );
            })}
          </div>
        </section>
      ))}
    </div>
  );
}
