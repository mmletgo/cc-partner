/**
 * Workbench Git 检查器叶子视图 —— Git 提交图 + commit/push/merge actions + merge stages。
 *
 * Business Logic（为什么需要这个组件）:
 *   Plan 2 Task 8 把 Workbench.tsx 内联的 inspector "history" tab 渲染（Git graph + actions + merge stages）
 *   抽到独立叶子组件。组件只接收 controller 派生的渲染数据与回调，不持有自己的状态，也不导入文件域。
 *
 * Code Logic（这个组件做什么）:
 *   - 内部封装 gitGraphColorStyle / gitGraphWidth / gitGraphX 与 GIT_GRAPH_* 常量（随 Git 检查器一起从页面迁出）；
 *   - 渲染刷新/commit/push/merge 按钮、merge stage panel 和带 lane 颜色的 commit graph SVG；
 *   - 暴露 WorkbenchGitInspectorProps 类型，所有数据均来自 useWorkbenchWorktreeGitController + Workbench.tsx 跨域共享。
 */
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { Button, Card, Pill } from '@/components/primitives';
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
import type { WorktreeBusyKind } from './controllers/useWorkbenchWorktreeGitController';

const GIT_GRAPH_LANE_WIDTH = 14;
const GIT_GRAPH_ROW_HEIGHT = 58;
const GIT_GRAPH_DOT_Y = 22;
const GIT_GRAPH_DOT_RADIUS = 4;

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
  return Math.max(24, laneCount * GIT_GRAPH_LANE_WIDTH + 10);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph 每个 lane 需要稳定 x 坐标，供点、竖线和 merge 曲线复用。
 *
 * Code Logic（这个函数做什么）:
 *   将 lane index 映射到 SVG 内部横坐标。
 */
function gitGraphX(lane: number): number {
  return 5 + lane * GIT_GRAPH_LANE_WIDTH;
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
  const {
    activeProjectId,
    activeWorktree,
    remoteWriteDisabled,
    gitCommits,
    gitHistoryLoading,
    gitHistoryError,
    worktreeBusy,
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
            disabled={!canCommitWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
            onClick={() => void handleCommitWorktree()}
          >
            {t('workbench:worktrees.commit')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<UploadIcon />}
            loading={worktreeBusy === 'push'}
            disabled={!canPushWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
            onClick={() => void handlePushWorktree()}
          >
            {t('workbench:worktrees.push')}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            icon={<SyncIcon />}
            loading={worktreeBusy === 'merge'}
            disabled={!canMergeWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
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

      {gitHistoryError ? <div className={styles.errorBox}>{gitHistoryError}</div> : null}

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
              return (
                <article key={row.commit.hash} className={styles.commitItem}>
                  <div className={styles.commitGraph} style={{ width: graphWidth }}>
                    <svg
                      className={styles.commitGraphSvg}
                      viewBox={`0 0 ${graphWidth} ${GIT_GRAPH_ROW_HEIGHT}`}
                      aria-hidden="true"
                    >
                      {row.activeLanes.map((lane, laneIndex) => {
                        const x = gitGraphX(laneIndex);
                        const isCommitLane = laneIndex === row.lane;
                        const continues = row.parentLanes.includes(laneIndex);
                        const y2 = isCommitLane && !continues ? GIT_GRAPH_DOT_Y : GIT_GRAPH_ROW_HEIGHT;
                        return (
                          <line
                            key={`${row.commit.hash}-${lane.hash}-${laneIndex}`}
                            className={styles.graphLine}
                            style={gitGraphColorStyle(lane.colorIndex)}
                            x1={x}
                            y1={0}
                            x2={x}
                            y2={y2}
                          />
                        );
                      })}
                      {row.parentLanes
                        .filter((parentLane) => parentLane !== row.lane)
                        .map((parentLane) => {
                          const fromX = gitGraphX(row.lane);
                          const toX = gitGraphX(parentLane);
                          return (
                            <path
                              key={`${row.commit.hash}-${parentLane}`}
                              className={styles.graphLine}
                              style={gitGraphColorStyle(row.colorIndex)}
                              d={`M ${fromX} ${GIT_GRAPH_DOT_Y} C ${fromX} 32 ${toX} 32 ${toX} ${GIT_GRAPH_ROW_HEIGHT}`}
                            />
                          );
                        })}
                      <circle
                        className={styles.graphDot}
                        style={gitGraphColorStyle(row.colorIndex)}
                        cx={gitGraphX(row.lane)}
                        cy={GIT_GRAPH_DOT_Y}
                        r={GIT_GRAPH_DOT_RADIUS}
                      />
                    </svg>
                  </div>
                  <div className={styles.commitContent}>
                    <div className={styles.commitHeader}>
                      <span className={styles.commitSummary}>
                        {row.commit.summary || emptyValue}
                      </span>
                      <span className={styles.commitTime}>
                        {formatCommitRelativeTime(row.commit.authoredAt, emptyValue)}
                      </span>
                    </div>
                    {row.commit.refs.length > 0 ? (
                      <div className={styles.refList}>
                        {row.commit.refs.map((ref) => (
                          <span
                            key={`${row.commit.hash}-${ref.fullName}`}
                            className={styles.refBadge}
                            data-kind={ref.kind}
                            title={ref.fullName}
                          >
                            {ref.kind === 'remote' ? <UploadIcon size={12} /> : null}
                            {ref.name}
                          </span>
                        ))}
                      </div>
                    ) : null}
                    <div className={styles.commitMeta}>
                      <span className={styles.commitHash}>{row.commit.shortHash}</span>
                      <span>{row.commit.authorName || row.commit.authorEmail || emptyValue}</span>
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
