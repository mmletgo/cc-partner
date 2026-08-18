/**
 * Orchestrator 看板视图
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要按 workflow 泳道查看独立任务与串行任务块，并在 Backlog/Todo 用 + 打开创建弹窗。
 *
 * Code Logic（这个组件做什么）:
 *   渲染固定泳道、Lane +、独立任务卡与块卡片；通过 props 接收 groups/selection/drag/append/reorder；
 *   不 import API 模块。
 */
import type { DragEvent, JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Pill } from '@/components/primitives';
import { ChevronDownIcon, ChevronUpIcon, PlusIcon } from '@/lib/icons';
import { orchestratorStatusTone, orchestratorWorkflowStateTone } from '@/lib/orchestrator';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type { OrchestratorTask, OrchestratorWorkflowState } from '@/lib/types';
import {
  canAppendToBlock,
  canCreateInLane,
  canMoveBoardBlockToWorkflowState,
  canMoveRenderableTaskToWorkflowState,
  canReorderBlock,
  ORCHESTRATOR_BOARD_LANES,
  type OrchestratorBoardGroups,
  type OrchestratorBoardItem,
} from '../orchestratorBoard';
import {
  ATTEMPT_PHASE_LABEL_KEYS,
  RUN_STATE_LABEL_KEYS,
  runStateTone,
  STATUS_LABEL_KEYS,
  WORKFLOW_STATE_LABEL_KEYS,
} from '../orchestratorViewHelpers';
import styles from '../Orchestrator.module.css';

export type OrchestratorCreateMode = 'task' | 'taskBlock';

/**
 * Business Logic（为什么需要这个类型）:
 *   看板只消费 controller 下发的派生数据与 handler，不持有本地业务状态。
 *
 * Code Logic（这个类型做什么）:
 *   描述泳道 groups、选中、移动中项与创建/追加/重排/拖拽回调。
 */
export interface OrchestratorBoardProps {
  groups: OrchestratorBoardGroups;
  selectedTask: OrchestratorTask | null;
  movingTaskId: string | null;
  movingBlockId: string | null;
  canCreateTaskBlock: boolean;
  onSelectTask: (taskId: string) => void;
  onOpenLaneCreate: (lane: OrchestratorWorkflowState, mode: OrchestratorCreateMode) => void;
  onOpenAppend: (blockId: string) => void;
  onReorderBlock: (blockId: string, orderedTaskIds: string[]) => void;
  onTaskDragStart: (event: DragEvent<HTMLButtonElement>, item: OrchestratorRenderableTask) => void;
  onBlockDragStart: (
    event: DragEvent<HTMLElement>,
    blockId: string,
    members: OrchestratorRenderableTask[],
  ) => void;
  onTaskDragEnd: () => void;
  onLaneDragOver: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => void;
  onLaneDrop: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   独立任务与块成员行共用同一套标题/来源/状态展示，避免两套卡片漂移。
 *
 * Code Logic（这个函数做什么）:
 *   渲染任务标题、来源与可选 run/status/attempt pills。
 */
function TaskCardBody(props: { item: OrchestratorRenderableTask }): JSX.Element {
  const { item } = props;
  const { task } = item;
  const { t } = useTranslation(['orchestrator']);
  const showRunState = task.runState !== 'idle';
  const showStatus = task.status === 'blocked' || task.status === 'aborted';
  return (
    <>
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
            <Pill tone={runStateTone(task.runState)}>{t(RUN_STATE_LABEL_KEYS[task.runState])}</Pill>
          ) : null}
          {showStatus ? (
            <Pill tone={orchestratorStatusTone(task.status)}>{t(STATUS_LABEL_KEYS[task.status])}</Pill>
          ) : null}
          {task.attemptPhase ? (
            <Pill tone="neutral">{t(ATTEMPT_PHASE_LABEL_KEYS[task.attemptPhase])}</Pill>
          ) : null}
        </span>
      ) : null}
    </>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   块卡片要把成员留在同一张卡里，并按规则提供追加与上移/下移。
 *
 * Code Logic（这个函数做什么）:
 *   渲染块标题、成员行、可选重排按钮与末尾追加。
 */
function BlockCard(props: {
  item: Extract<OrchestratorBoardItem, { kind: 'block' }>;
  selectedTaskId: string | null;
  moving: boolean;
  onSelectTask: (taskId: string) => void;
  onOpenAppend: (blockId: string) => void;
  onReorderBlock: (blockId: string, orderedTaskIds: string[]) => void;
  onBlockDragStart: (
    event: DragEvent<HTMLElement>,
    blockId: string,
    members: OrchestratorRenderableTask[],
  ) => void;
  onTaskDragEnd: () => void;
}): JSX.Element {
  const {
    item,
    selectedTaskId,
    moving,
    onSelectTask,
    onOpenAppend,
    onReorderBlock,
    onBlockDragStart,
    onTaskDragEnd,
  } = props;
  const { t } = useTranslation(['orchestrator']);
  const canReorder = canReorderBlock(item.members);
  const canAppend = canAppendToBlock(item.members);
  const draggable =
    !moving &&
    ORCHESTRATOR_BOARD_LANES.some((targetLane) =>
      canMoveBoardBlockToWorkflowState(item.members, targetLane),
    );

  /**
   * Business Logic（为什么需要这个函数）:
   *   上移/下移只交换相邻成员，提交完整置换给后端。
   *
   * Code Logic（这个函数做什么）:
   *   按 delta 交换 id 数组后调用 onReorderBlock。
   */
  const moveMember = (index: number, delta: number): void => {
    const next = item.members.map((member) => member.task.id);
    const target = index + delta;
    if (target < 0 || target >= next.length) return;
    const currentId = next[index];
    const targetId = next[target];
    if (!currentId || !targetId) return;
    next[index] = targetId;
    next[target] = currentId;
    onReorderBlock(item.blockId, next);
  };

  return (
    <article
      className={`${styles.blockCard} ${moving ? styles.blockCardMoving : ''}`}
      draggable={draggable}
      aria-label={t('orchestrator:blocks.blockAria', { title: item.title })}
      onDragStart={(event) => onBlockDragStart(event, item.blockId, item.members)}
      onDragEnd={onTaskDragEnd}
    >
      <div className={styles.blockHeader}>
        <span className={styles.blockTitle}>{item.title}</span>
        <Pill tone="neutral">{t('orchestrator:blocks.members', { count: item.members.length })}</Pill>
      </div>
      <div className={styles.blockMembers}>
        {item.members.map((member, index) => {
          const active = selectedTaskId === member.task.id;
          return (
            <div className={styles.blockMemberRow} key={member.task.id}>
              {canReorder ? (
                <div className={styles.blockReorder}>
                  <Button
                    variant="icon"
                    size="sm"
                    aria-label={t('orchestrator:blocks.moveUp')}
                    icon={<ChevronUpIcon />}
                    disabled={index === 0}
                    onClick={() => moveMember(index, -1)}
                  />
                  <Button
                    variant="icon"
                    size="sm"
                    aria-label={t('orchestrator:blocks.moveDown')}
                    icon={<ChevronDownIcon />}
                    disabled={index === item.members.length - 1}
                    onClick={() => moveMember(index, 1)}
                  />
                </div>
              ) : null}
              <button
                className={`${styles.task} ${active ? styles.taskActive : ''}`}
                type="button"
                aria-pressed={active}
                aria-label={t('orchestrator:queue.taskAria', { title: member.task.title })}
                onClick={() => onSelectTask(member.task.id)}
              >
                <TaskCardBody item={member} />
              </button>
            </div>
          );
        })}
      </div>
      {canAppend ? (
        <div className={styles.blockFooter}>
          <Button variant="ghost" size="sm" icon={<PlusIcon />} onClick={() => onOpenAppend(item.blockId)}>
            {t('orchestrator:blocks.append')}
          </Button>
        </div>
      ) : null}
    </article>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板必须始终渲染全部 workflow 泳道，即使用户当前没有任务。
 *
 * Code Logic（这个函数做什么）:
 *   映射泳道、Lane +、独立任务卡与块卡片；全部文案走 i18n。
 */
export function OrchestratorBoard(props: OrchestratorBoardProps): JSX.Element {
  const {
    groups,
    selectedTask,
    movingTaskId,
    movingBlockId,
    canCreateTaskBlock,
    onSelectTask,
    onOpenLaneCreate,
    onOpenAppend,
    onReorderBlock,
    onTaskDragStart,
    onBlockDragStart,
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
            <div className={styles.laneHeaderMeta}>
              <Pill tone={orchestratorWorkflowStateTone(lane)}>{groups[lane].length}</Pill>
              {canCreateInLane(lane) ? (
                <div className={styles.laneAdds}>
                  <Button
                    variant="icon"
                    size="sm"
                    aria-label={t('orchestrator:blocks.addTask')}
                    icon={<PlusIcon />}
                    onClick={() => onOpenLaneCreate(lane, 'task')}
                  />
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={t('orchestrator:blocks.addBlock')}
                    disabled={!canCreateTaskBlock}
                    onClick={() => onOpenLaneCreate(lane, 'taskBlock')}
                  >
                    {t('orchestrator:create.modeBlock')}
                  </Button>
                </div>
              ) : null}
            </div>
          </div>
          <div className={styles.laneTaskList}>
            {groups[lane].map((card) => {
              if (card.kind === 'block') {
                return (
                  <BlockCard
                    item={card}
                    key={card.blockId}
                    selectedTaskId={selectedTask?.id ?? null}
                    moving={movingBlockId === card.blockId}
                    onSelectTask={onSelectTask}
                    onOpenAppend={onOpenAppend}
                    onReorderBlock={onReorderBlock}
                    onBlockDragStart={onBlockDragStart}
                    onTaskDragEnd={onTaskDragEnd}
                  />
                );
              }
              const { item } = card;
              const { task } = item;
              const active = selectedTask?.id === task.id;
              const moving = movingTaskId === task.id;
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
                  <TaskCardBody item={item} />
                </button>
              );
            })}
          </div>
        </section>
      ))}
    </div>
  );
}
