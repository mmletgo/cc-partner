/**
 * Workbench Git 检查器叶子视图 —— Git 提交图 + commit/push/merge actions + merge stages。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspector "history" tab 渲染（Git graph + actions + merge stages）
 *   抽到独立叶子组件。组件只接收 controller 派生的渲染数据与回调，不持有自己的状态，也不导入文件域。
 *
 * Code Logic（这个组件做什么）:
 *   - 内部封装 gitGraphColorStyle / gitGraphWidth / gitGraphX 与 GIT_GRAPH_* 常量（随 Git 检查器一起从页面迁出）；
 *   - 参考 VS Code Source Control Graph 的紧凑泳道语法，渲染连续 lane、HEAD/merge 节点和内联 ref badge；
 *   - 渲染刷新/commit/push/merge 按钮、merge stage panel 和带 lane 颜色的 commit graph SVG；
 *   - 暴露 WorkbenchGitInspectorProps 类型，所有数据均来自 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享。
 */
import * as React from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill, StatusMessage } from '@/components/primitives';
import { EditIcon, SyncIcon, UploadIcon } from '@/lib/icons';
import type {
  WorkbenchGitCommit,
  WorkbenchMergeStage,
  WorkbenchMergeStageId,
  WorkbenchWorktree,
} from '@/lib/types';
import styles from './Workbench.module.css';
import {
  buildGitGraphRows,
  canCommitWorktree,
  canMergeWorktree,
  canPushWorktree,
  formatCommitRelativeTime,
  formatWorkbenchMergeStages,
  hasGitHistory,
  worktreeChangeCount,
  worktreeStatusTone,
} from './workbenchWorktrees';
import type { WorkbenchGitGraphRow } from './workbenchWorktrees';
import type {
  WorkbenchHookRepair,
  WorktreeBusyKind,
  WorktreeUnknownMutationLock,
} from './controllers/useWorkbenchWorktreeGitController';

const GIT_GRAPH_LANE_WIDTH = 12;
const GIT_GRAPH_ROW_HEIGHT = 40;
const GIT_GRAPH_NODE_Y = GIT_GRAPH_ROW_HEIGHT / 2;
const GIT_GRAPH_DOT_RADIUS = 4;
const GIT_GRAPH_HEAD_RADIUS = 6;
const GIT_GRAPH_MERGE_RADIUS = 5;

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph 需要多条稳定颜色 lane，但具体颜色由 design token 控制。
 *
 * Code Logic（这个函数做什么）:
 *   将 graph helper 的 colorIndex 映射到 CSS custom property。
 */
function gitGraphColorStyle(colorIndex: number): CSSProperties {
  return {
    '--git-graph-color': `var(--git-graph-${colorIndex % 6})`,
  } as CSSProperties;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph SVG 需要按 lane 数动态扩展宽度，避免 merge 线被裁切。
 *
 * Code Logic（这个函数做什么）:
 *   根据 laneCount 计算紧凑 graph 宽度。
 */
function gitGraphWidth(laneCount: number): number {
  return GIT_GRAPH_LANE_WIDTH * (Math.max(laneCount, 1) + 1);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph 每个 lane 需要稳定 x 坐标，供点、竖线和 merge 曲线复用。
 *
 * Code Logic（这个函数做什么）:
 *   将 lane index 映射到 SVG 内部横坐标。
 */
function gitGraphX(lane: number): number {
  return GIT_GRAPH_LANE_WIDTH * (lane + 1);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   泳道在分支收束后会横向移动；若直接画斜线，紧凑行高下会出现尖锐折角，难以追踪。
 *
 * Code Logic（这个函数做什么）:
 *   生成从本行顶部泳道到下方泳道的平滑三次贝塞尔路径；同 lane 时退化为竖线。
 */
function gitGraphLanePath(fromLane: number, toLane: number): string {
  const fromX = gitGraphX(fromLane);
  const toX = gitGraphX(toLane);
  if (fromLane === toLane) {
    return `M ${fromX} 0 V ${GIT_GRAPH_ROW_HEIGHT}`;
  }
  return `M ${fromX} 0 C ${fromX} ${GIT_GRAPH_NODE_Y} ${toX} ${GIT_GRAPH_NODE_Y} ${toX} ${GIT_GRAPH_ROW_HEIGHT}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge commit 的多个 parent 需要从提交节点清晰分叉到各自的输出泳道。
 *
 * Code Logic（这个函数做什么）:
 *   生成从提交节点到目标 parent lane 底部的平滑路径；同 lane 时保持竖直。
 */
function gitGraphParentPath(fromLane: number, toLane: number): string {
  const fromX = gitGraphX(fromLane);
  const toX = gitGraphX(toLane);
  if (fromLane === toLane) {
    return `M ${fromX} ${GIT_GRAPH_NODE_Y} V ${GIT_GRAPH_ROW_HEIGHT}`;
  }
  return `M ${fromX} ${GIT_GRAPH_NODE_Y} C ${fromX} ${GIT_GRAPH_NODE_Y + 6} ${toX} ${GIT_GRAPH_NODE_Y + 6} ${toX} ${GIT_GRAPH_ROW_HEIGHT}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pre-commit/pre-push 钩子输出可能很大，前端展示按 stdout+stderr 拼接并保留换行；
 *   单端空时只展示另一端；两端都空时给一个占位（前端 i18n）。
 *
 * Code Logic（这个函数做什么）:
 *   把 { stdout, stderr } 合并为多行字符串；空 fallback 由调用方 i18n 替换。
 */
function formatHookRepairOutput(hookFailure: {
  stdout: string;
  stderr: string;
}): string {
  const stdout = hookFailure.stdout.trim();
  const stderr = hookFailure.stderr.trim();
  if (stdout && stderr) {
    return `${stdout}\n${stderr}`;
  }
  return stdout || stderr;
}

/**
 * Git 检查器叶子组件的输入 props。
 *
 * Business Logic: 所有数据均由 useWorkbenchWorktreeGitController 派生（除 activeWorktree / activeProjectId /
 * remoteWriteDisabled 由 Workbench.tsx 跨域共享/路由 context 透传）；组件本身不持有状态、不导入文件域。
 */
export interface WorkbenchGitInspectorProps {
  activeProjectId: string | null;
  activeWorktree: WorkbenchWorktree | null;
  remoteWriteDisabled: boolean;
  gitCommits: WorkbenchGitCommit[];
  gitHistoryLoading: boolean;
  gitHistoryError: string | null;
  worktreeBusy: WorktreeBusyKind | null;
  /** unknown 共享锁；can* 禁用 sibling mutation。 */
  unknownMutationLock: WorktreeUnknownMutationLock | null;
  hookRepair: WorkbenchHookRepair | null;
  handleRepairHookFailure: () => Promise<void>;
  /**
   * 修复面板上的「忽略」按钮：纯本地动作，清空 hookRepair，不发起任何 IPC。
   * 与「让 AI 修复 / 重试 commit-push」并列，给用户第三个出口，避免 stale failedHook 面板卡住 UI。
   */
  handleDismissHookFailure: () => Promise<void>;
  handleRetryAfterRepair: () => Promise<void>;
  mergeStages: WorkbenchMergeStage[];
  loadGitHistory: () => Promise<void>;
  handleCommitWorktree: () => Promise<void>;
  handlePushWorktree: () => Promise<void>;
  handleMergeWorktree: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 检查器的 "history" tab 需要一个独立的叶子视图，把 Git 提交图、commit/push/merge actions 和
 *   merge stage panel 集中渲染。该组件由 WorkbenchInspector 在 history tab 时挂载；接收 controller 派生的 props。
 *
 * Code Logic（这个组件做什么）:
 *   渲染刷新/commit/push/merge 按钮 + merge stage panel + commit graph SVG；不持有状态、不调用 workbenchApi。
 */
export function WorkbenchGitInspector(props: WorkbenchGitInspectorProps) {
  const { t } = useTranslation(['workbench']);
  const [hookOutputExpanded, setHookOutputExpanded] = React.useState(false);
  const {
    activeProjectId,
    activeWorktree,
    remoteWriteDisabled,
    gitCommits,
    gitHistoryLoading,
    gitHistoryError,
    worktreeBusy,
    unknownMutationLock,
    hookRepair,
    handleRepairHookFailure,
    handleDismissHookFailure,
    handleRetryAfterRepair,
    mergeStages,
    loadGitHistory,
    handleCommitWorktree,
    handlePushWorktree,
    handleMergeWorktree,
  } = props;

  const emptyValue = t('workbench:emptyValue');
  const gitGraphRows: WorkbenchGitGraphRow[] = buildGitGraphRows(gitCommits);
  const renderedMergeStages =
    mergeStages.length > 0 ? formatWorkbenchMergeStages(mergeStages) : [];
  const activeWorktreeTone = activeWorktree ? worktreeStatusTone(activeWorktree) : 'neutral';
  const activeWorktreePillTone = activeWorktreeTone === 'warning' ? 'warn' : activeWorktreeTone;
  const activeWorktreeChangedCount = worktreeChangeCount(activeWorktree);
  const activeWorktreeStatusLabel = activeWorktree
    ? activeWorktree.status.conflicts > 0
      ? t('workbench:worktrees.status.conflict', { count: activeWorktree.status.conflicts })
      : activeWorktree.status.clean
        ? t('workbench:worktrees.status.clean')
        : t('workbench:worktrees.status.dirty', { count: activeWorktree.status.changed })
    : emptyValue;

  /**
   * Business Logic（为什么需要这个函数）:
   *   merge stage 面板的 label 文案按 stage id 选择；保留为函数以匹配原 Workbench.tsx 行为。
   */
  const mergeStageLabel = (stageId: WorkbenchMergeStageId): string => {
    switch (stageId) {
      case 'checkSource':
        return t('workbench:mergeStages.labels.checkSource');
      case 'closeSessions':
        return t('workbench:mergeStages.labels.closeSessions');
      case 'mergeMain':
        return t('workbench:mergeStages.labels.mergeMain');
      case 'resolveConflicts':
        return t('workbench:mergeStages.labels.resolveConflicts');
      case 'cleanup':
        return t('workbench:mergeStages.labels.cleanup');
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   stage message 缺失时按 status 显示通用 fallback 文案，避免空白行。
   */
  const mergeStageFallbackMessage = (stage: WorkbenchMergeStage): string => {
    switch (stage.status) {
      case 'pending':
        return t('workbench:mergeStages.status.pending');
      case 'running':
        return t('workbench:mergeStages.status.running');
      case 'completed':
        return t('workbench:mergeStages.status.completed');
      case 'failed':
        return t('workbench:mergeStages.status.failed');
      case 'skipped':
        return t('workbench:mergeStages.status.skipped');
    }
  };

  return (
    <Card className={styles.historyCard} padding="sm">
      <div className={styles.cardTitleRow}>
        <h3 className={styles.cardTitle}>{t('workbench:gitHistoryTitle')}</h3>
        <Button
          variant="icon"
          icon={<SyncIcon />}
          title={t('workbench:refreshGitHistory')}
          aria-label={t('workbench:refreshGitHistory')}
          disabled={!activeProjectId || gitHistoryLoading}
          onClick={() => void loadGitHistory()}
        />
      </div>

      <div className={styles.gitActionBar}>
        <div className={styles.gitActionStatus}>
          <Pill tone={activeWorktreePillTone} dot>
            {activeWorktreeStatusLabel}
          </Pill>
          <span className={styles.gitActionBranch}>
            {activeWorktree?.branch ?? activeWorktree?.name ?? emptyValue}
          </span>
        </div>
        <div className={styles.gitActionButtons}>
          <Button
            size="sm"
            variant={activeWorktreeChangedCount > 0 ? 'primary' : 'secondary'}
            icon={<EditIcon />}
            loading={worktreeBusy === 'commit'}
            disabled={
              !canCommitWorktree(activeWorktree, worktreeBusy, unknownMutationLock)
              || remoteWriteDisabled
            }
            onClick={() => void handleCommitWorktree()}
          >
            {t('workbench:worktrees.commit')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<UploadIcon />}
            loading={worktreeBusy === 'push'}
            disabled={
              !canPushWorktree(activeWorktree, worktreeBusy, unknownMutationLock)
              || remoteWriteDisabled
            }
            onClick={() => void handlePushWorktree()}
          >
            {t('workbench:worktrees.push')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<SyncIcon />}
            loading={worktreeBusy === 'merge'}
            disabled={
              !canMergeWorktree(activeWorktree, worktreeBusy, unknownMutationLock)
              || remoteWriteDisabled
            }
            onClick={() => void handleMergeWorktree()}
          >
            {t('workbench:worktrees.merge')}
          </Button>
        </div>
      </div>

      {renderedMergeStages.length > 0 ? (
        <div className={styles.mergeStagePanel} role="status" aria-live="polite">
          {renderedMergeStages.map((stage) => (
            <div
              key={stage.id}
              className={styles.mergeStageItem}
              data-status={stage.status}
            >
              <span className={styles.mergeStageDot} aria-hidden="true" />
              <div className={styles.mergeStageCopy}>
                <span className={styles.mergeStageLabel}>
                  {mergeStageLabel(stage.id)}
                </span>
                <span className={styles.mergeStageMessage}>
                  {stage.message || mergeStageFallbackMessage(stage)}
                </span>
              </div>
            </div>
          ))}
        </div>
      ) : null}

      {hookRepair ? (
        <div
          className={styles.hookRepairPanel}
          role={hookRepair.kind === 'push' ? 'alert' : 'status'}
          aria-live={hookRepair.kind === 'push' ? 'assertive' : 'polite'}
          data-testid="workbench-hook-repair-panel"
        >
          <div className={styles.hookRepairHeader}>
            <span className={styles.hookRepairTitle}>
              {hookRepair.kind === 'commit'
                ? t('workbench:worktrees.hookRepair.titleCommit')
                : t('workbench:worktrees.hookRepair.titlePush')}
            </span>
            {typeof hookRepair.hookFailure.exitCode === 'number' ? (
              <span className={styles.hookRepairMeta}>
                {t('workbench:worktrees.hookRepair.exitCode', {
                  code: hookRepair.hookFailure.exitCode,
                })}
              </span>
            ) : null}
          </div>
          {hookRepair.terminalSessionId ? (
            <span className={styles.hookRepairHint}>
              {t('workbench:worktrees.hookRepair.terminalHint')}
            </span>
          ) : null}
          <button
            type="button"
            className={styles.hookRepairOutputToggle}
            aria-expanded={hookOutputExpanded}
            onClick={() => setHookOutputExpanded((v) => !v)}
          >
            {hookOutputExpanded
              ? t('workbench:worktrees.hookRepair.hideOutput')
              : t('workbench:worktrees.hookRepair.showOutput')}
          </button>
          {hookOutputExpanded ? (
            <pre className={styles.hookRepairOutput}>
              {formatHookRepairOutput(hookRepair.hookFailure)}
            </pre>
          ) : null}
          <div className={styles.hookRepairActions}>
            {hookRepair.terminalSessionId ? (
              <Button
                size="sm"
                variant="primary"
                loading={worktreeBusy === 'commit' && !hookRepair.terminalSessionId}
                onClick={() => void handleRetryAfterRepair()}
              >
                {hookRepair.kind === 'commit'
                  ? t('workbench:worktrees.hookRepair.retryCommit')
                  : t('workbench:worktrees.hookRepair.retryPush')}
              </Button>
            ) : (
              <Button
                size="sm"
                variant="primary"
                loading={worktreeBusy === 'commit'}
                disabled={remoteWriteDisabled}
                onClick={() => void handleRepairHookFailure()}
              >
                {worktreeBusy === 'commit'
                  ? t('workbench:worktrees.hookRepair.runButtonBusy')
                  : t('workbench:worktrees.hookRepair.runButton')}
              </Button>
            )}
            {/*
              「忽略」出口：与「让 AI 修复 / 重试」并列。纯本地动作（清空 hookRepair），
              不发起任何 IPC；用户已决定不修也不重试时，主动放弃当前失败上下文。
              worktreeBusy 不影响 dismiss（dismiss 不调任何 workbenchApi）。
            */}
            <Button
              size="sm"
              variant="secondary"
              onClick={() => void handleDismissHookFailure()}
              data-testid="workbench-hook-repair-dismiss"
            >
              {t('workbench:worktrees.hookRepair.dismissButton')}
            </Button>
          </div>
        </div>
      ) : null}

      {gitHistoryError ? (
        <StatusMessage tone="danger" className={styles.errorBox}>
          {gitHistoryError}
        </StatusMessage>
      ) : null}

      <div className={styles.historyPanel}>
        {!activeProjectId ? (
          <div className={styles.treeEmpty}>{t('workbench:gitHistoryNoProject')}</div>
        ) : gitHistoryLoading ? (
          <div className={styles.treeEmpty}>{t('workbench:gitHistoryLoading')}</div>
        ) : !hasGitHistory(gitCommits) ? (
          <div className={styles.treeEmpty}>{t('workbench:gitHistoryEmpty')}</div>
        ) : (
          <div className={styles.commitList}>
            {gitGraphRows.map((row) => {
              const graphWidth = gitGraphWidth(row.laneCount);
              const author = row.commit.authorName || row.commit.authorEmail || emptyValue;
              const relativeTime = formatCommitRelativeTime(row.commit.authoredAt, emptyValue);
              const isHead = row.commit.refs.some(
                (ref) => ref.isHead || ref.kind === 'head',
              );
              const isMerge = row.commit.parentHashes.length > 1;
              return (
                <article
                  key={row.commit.hash}
                  className={styles.commitItem}
                  data-head={isHead ? 'true' : undefined}
                  data-merge={isMerge ? 'true' : undefined}
                  data-testid="git-history-row"
                  aria-label={`${row.commit.summary || emptyValue}, ${author}, ${row.commit.shortHash}, ${relativeTime}`}
                  title={`${row.commit.summary || emptyValue}\n${author} · ${row.commit.shortHash} · ${relativeTime}`}
                >
                  <div className={styles.commitGraph} style={{ width: graphWidth }}>
                    <svg
                      className={styles.commitGraphSvg}
                      viewBox={`0 0 ${graphWidth} ${GIT_GRAPH_ROW_HEIGHT}`}
                      aria-hidden="true"
                    >
                      {row.activeLanes.map((lane, inputLane) => {
                        if (inputLane === row.lane) return null;
                        const outputLane = row.outputLanes.findIndex(
                          (candidate) => candidate.hash === lane.hash,
                        );
                        if (outputLane < 0) return null;
                        return (
                          <path
                            key={`${row.commit.hash}-${lane.hash}-${inputLane}`}
                            className={styles.graphLine}
                            style={gitGraphColorStyle(lane.colorIndex)}
                            d={gitGraphLanePath(inputLane, outputLane)}
                          />
                        );
                      })}
                      {row.activeLanes[row.lane] ? (
                        <path
                          className={styles.graphLine}
                          style={gitGraphColorStyle(row.colorIndex)}
                          d={`M ${gitGraphX(row.lane)} 0 V ${GIT_GRAPH_NODE_Y}`}
                        />
                      ) : null}
                      {row.parentLanes.map((parentLane, parentIndex) => {
                        const parentColor = row.outputLanes[parentLane]?.colorIndex
                          ?? row.colorIndex;
                        return (
                          <path
                            key={`${row.commit.hash}-${parentLane}-${parentIndex}`}
                            className={styles.graphLine}
                            style={gitGraphColorStyle(parentColor)}
                            d={gitGraphParentPath(row.lane, parentLane)}
                          />
                        );
                      })}
                      {isHead || isMerge ? (
                        <>
                          <circle
                            className={styles.graphNodeRing}
                            style={gitGraphColorStyle(row.colorIndex)}
                            cx={gitGraphX(row.lane)}
                            cy={GIT_GRAPH_NODE_Y}
                            r={isHead ? GIT_GRAPH_HEAD_RADIUS : GIT_GRAPH_MERGE_RADIUS}
                          />
                          <circle
                            className={styles.graphNodeCore}
                            style={gitGraphColorStyle(row.colorIndex)}
                            cx={gitGraphX(row.lane)}
                            cy={GIT_GRAPH_NODE_Y}
                            r={isHead ? 2.25 : 1.75}
                          />
                        </>
                      ) : (
                        <circle
                          className={styles.graphDot}
                          style={gitGraphColorStyle(row.colorIndex)}
                          cx={gitGraphX(row.lane)}
                          cy={GIT_GRAPH_NODE_Y}
                          r={GIT_GRAPH_DOT_RADIUS}
                        />
                      )}
                    </svg>
                  </div>
                  <div className={styles.commitContent}>
                    <div className={styles.commitPrimary}>
                      <span className={styles.commitSummary}>
                        {row.commit.summary || emptyValue}
                      </span>
                      {row.commit.refs.length > 0 ? (
                        <div className={styles.refList}>
                          {row.commit.refs.map((ref) => (
                            <span
                              key={`${row.commit.hash}-${ref.fullName}`}
                              className={styles.refBadge}
                              data-kind={ref.kind}
                              data-head={ref.isHead ? 'true' : undefined}
                              title={ref.fullName}
                            >
                              {ref.kind === 'remote' ? <UploadIcon size={11} /> : null}
                              {ref.name}
                            </span>
                          ))}
                        </div>
                      ) : null}
                    </div>
                    <div className={styles.commitMeta}>
                      <span className={styles.commitAuthor}>{author}</span>
                      <span className={styles.commitHash}>{row.commit.shortHash}</span>
                      <span className={styles.commitTime}>{relativeTime}</span>
                    </div>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>
    </Card>
  );
}
