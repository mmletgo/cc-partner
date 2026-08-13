import type {
  WorkbenchGitCommit,
  WorkbenchMergeStage,
  WorkbenchMergeStageId,
  WorkbenchSession,
  WorkbenchWorktree,
} from '@/lib/types';

export type WorktreeTone = 'neutral' | 'warning' | 'danger';
export {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  WORKTREE_BRANCH_PREFIXES,
  composeWorktreeBranchName,
  normalizeWorktreeBranchName,
} from '@/lib/workbenchWorktreeBranches';
export type { WorktreeBranchPrefix } from '@/lib/workbenchWorktreeBranches';

export interface WorkbenchGitGraphLane {
  hash: string;
  colorIndex: number;
}

export interface WorkbenchGitGraphRow {
  commit: WorkbenchGitCommit;
  lane: number;
  laneCount: number;
  /** 当前提交上方进入本行的泳道。 */
  activeLanes: WorkbenchGitGraphLane[];
  /** 当前提交处理完成后，从本行下方离开的泳道。 */
  outputLanes: WorkbenchGitGraphLane[];
  parentLanes: number[];
  colorIndex: number;
}

interface WorkbenchInitialTerminalSize {
  cols: number;
  rows: number;
}

interface CreateWorktreeWithTerminalWindowOptions {
  projectId: string;
  branchName: string;
  baseBranch?: string | null;
  initialSize?: WorkbenchInitialTerminalSize;
  createWorktree: (
    projectId: string,
    branchName: string,
    baseBranch?: string | null,
  ) => Promise<WorkbenchWorktree>;
  createSession: (
    projectId: string,
    initialSize: WorkbenchInitialTerminalSize | undefined,
    worktreeId: string,
  ) => Promise<WorkbenchSession>;
}

interface CreateWorktreeWithTerminalWindowResult {
  worktree: WorkbenchWorktree;
  session: WorkbenchSession | null;
  sessionError: unknown | null;
}

const GIT_GRAPH_COLOR_COUNT = 6;
export const WORKBENCH_MERGE_STAGE_IDS: WorkbenchMergeStageId[] = [
  'checkSource',
  'closeSessions',
  'mergeMain',
  'resolveConflicts',
  'cleanup',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   方案 C 中 worktree 是 terminal window 之上的管理层，切换 worktree 后只应看到该工作区的 window。
 *
 * Code Logic（这个函数做什么）:
 *   按 worktreeId 过滤 session；主 worktree 兼容旧 session 的 null worktreeId。
 */
export function sessionsForWorktree(
  sessions: WorkbenchSession[],
  worktreeId: string | null,
): WorkbenchSession[] {
  if (!worktreeId) {
    return sessions.filter((session) => session.worktreeId === null);
  }
  if (worktreeId.endsWith(':main')) {
    return sessions.filter((session) => session.worktreeId === worktreeId || session.worktreeId === null);
  }
  return sessions.filter((session) => session.worktreeId === worktreeId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户新建 worktree 后通常下一步就是进入该工作区终端，因此 Workbench 需要自动创建一个 terminal window 和默认 pane。
 *
 * Code Logic（这个函数做什么）:
 *   先调用 worktree 创建 API，随后用返回的 worktree.id 创建 terminal session；session 创建失败时保留 worktree 结果并返回错误。
 */
export async function createWorktreeWithTerminalWindow(
  options: CreateWorktreeWithTerminalWindowOptions,
): Promise<CreateWorktreeWithTerminalWindowResult> {
  const worktree = await options.createWorktree(
    options.projectId,
    options.branchName,
    options.baseBranch ?? null,
  );
  try {
    const session = await options.createSession(
      options.projectId,
      options.initialSize,
      worktree.id,
    );
    return { worktree, session, sessionError: null };
  } catch (error) {
    return { worktree, session: null, sessionError: error };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件树、终端 cwd 和 Prompt 优化都必须跟随 active worktree，而不是固定使用项目主路径。
 *
 * Code Logic（这个函数做什么）:
 *   active worktree 存在时返回 worktree.path；缺失时回退 projectPath。
 */
export function activeWorktreeRootPath(
  projectPath: string,
  activeWorktree: WorkbenchWorktree | null,
): string {
  return activeWorktree?.path ?? projectPath;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   worktree strip 需要用稳定 tone 让用户快速识别可合并、脏工作区和冲突状态。
 *
 * Code Logic（这个函数做什么）:
 *   conflict 映射 danger；dirty/ahead/behind 映射 warning；clean 映射 neutral。
 */
export function worktreeStatusTone(worktree: WorkbenchWorktree): WorktreeTone {
  if (worktree.status.conflicts > 0) return 'danger';
  if (!worktree.status.clean || worktree.status.ahead > 0 || worktree.status.behind > 0) {
    return 'warning';
  }
  return 'neutral';
}

/**
 * unknown 锁的最小形态（供 can* 纯 helper 使用，不耦合 controller 全字段）。
 *
 * Business Logic（为什么需要这个类型）:
 *   timeout/network 后 unknownMutationLock 是 worktree 级共享锁；UI 需按 kind 禁用 sibling 动作。
 *
 * Code Logic（字段说明）:
 *   kind 为锁定的 mutation 种类；其它字段可选，can* 只按 kind 判断。
 */
export type WorktreeUnknownMutationLockLike = {
  kind: 'commit' | 'push' | 'merge' | 'remove';
} | null;

/**
 * Business Logic（为什么需要这个函数）:
 *   unknown 锁住某种 mutation 后，只允许同 kind reconcile/retry，禁止 sibling Fresh 动作。
 *
 * Code Logic（这个函数做什么）:
 *   lock 为空时放行；非空时仅当 kind 匹配 actionKind 才放行。
 */
export function isWorktreeMutationKindAllowedByUnknownLock(
  actionKind: 'commit' | 'push' | 'merge' | 'remove',
  unknownMutationLock: WorktreeUnknownMutationLockLike = null,
): boolean {
  if (!unknownMutationLock) return true;
  return unknownMutationLock.kind === actionKind;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 状态来自轮询快照，用户在终端里改文件后快照可能暂时仍是 clean；Commit 点击应交给后端实时判断。
 *
 * Code Logic（这个函数做什么）:
 *   只检查是否有 active worktree、是否 busy，以及 unknown 锁是否允许 commit（同 kind 可 reconcile）。
 */
export function canCommitWorktree(
  activeWorktree: WorkbenchWorktree | null,
  worktreeBusy: string | null,
  unknownMutationLock: WorktreeUnknownMutationLockLike = null,
): boolean {
  return (
    activeWorktree !== null
    && worktreeBusy === null
    && isWorktreeMutationKindAllowedByUnknownLock('commit', unknownMutationLock)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史工具条需要判断当前 worktree 是否可以 push，避免本地未发布仓库显示可点击 Push。
 *
 * Code Logic（这个函数做什么）:
 *   有 active worktree、存在 branch、后端 canPush、非 busy，且 unknown 锁允许 push。
 */
export function canPushWorktree(
  activeWorktree: WorkbenchWorktree | null,
  worktreeBusy: string | null,
  unknownMutationLock: WorktreeUnknownMutationLockLike = null,
): boolean {
  return (
    Boolean(activeWorktree?.branch && activeWorktree.status.canPush)
    && worktreeBusy === null
    && isWorktreeMutationKindAllowedByUnknownLock('push', unknownMutationLock)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史工具条的 merge 适用于功能 worktree 合回主工作区，或主工作区 collect-merge。
 *
 * Code Logic（这个函数做什么）:
 *   active worktree 存在、非 busy，且 unknown 锁允许 merge；功能 worktree 始终可点，
 *   主工作区仅当 canCollectMerge 为 true；dirty 交给后端实时判断。
 */
export function canMergeWorktree(
  activeWorktree: WorkbenchWorktree | null,
  worktreeBusy: string | null,
  unknownMutationLock: WorktreeUnknownMutationLockLike = null,
): boolean {
  if (activeWorktree === null || worktreeBusy !== null) return false;
  if (!isWorktreeMutationKindAllowedByUnknownLock('merge', unknownMutationLock)) return false;
  if (!activeWorktree.isMain) return true;
  return activeWorktree.canCollectMerge === true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   一键合并会跨越多个后端阶段，UI 需要稳定顺序展示每一阶段，即使某些阶段尚未开始或被跳过。
 *
 * Code Logic（这个函数做什么）:
 *   按 canonical 阶段 id 补齐缺失阶段；已有阶段保留 status/message，未知阶段忽略。
 */
export function formatWorkbenchMergeStages(
  stages: WorkbenchMergeStage[],
): WorkbenchMergeStage[] {
  const byId = new Map(stages.map((stage) => [stage.id, stage]));
  return WORKBENCH_MERGE_STAGE_IDS.map((id) => {
    const stage = byId.get(id);
    if (stage) return stage;
    return { id, status: 'pending', message: '' };
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   一键合并成功后阶段条只是完成反馈，不应长期占用 Git 历史区域空间；失败状态则需要保留给用户排查。
 *
 * Code Logic（这个函数做什么）:
 *   复用 canonical 阶段顺序，只有 cleanup completed 且没有 failed/running 阶段时判定为可自动隐藏。
 */
export function shouldAutoDismissMergeStages(stages: WorkbenchMergeStage[]): boolean {
  const formatted = formatWorkbenchMergeStages(stages);
  return (
    formatted.some((stage) => stage.id === 'cleanup' && stage.status === 'completed') &&
    formatted.every((stage) => stage.status !== 'failed' && stage.status !== 'running')
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移除 worktree 是生命周期管理动作，不能对主工作区、busy 或 sibling unknown 锁开放。
 *
 * Code Logic（这个函数做什么）:
 *   active worktree 存在、非主、非 busy，且 unknown 锁允许 remove（同 kind 可 reconcile）。
 */
export function canRemoveWorktree(
  activeWorktree: WorkbenchWorktree | null,
  worktreeBusy: string | null,
  unknownMutationLock: WorktreeUnknownMutationLockLike = null,
): boolean {
  return (
    activeWorktree !== null
    && !activeWorktree.isMain
    && worktreeBusy === null
    && isWorktreeMutationKindAllowedByUnknownLock('remove', unknownMutationLock)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史工具条需要显示当前 worktree 的改动数，帮助用户决定是否提交。
 *
 * Code Logic（这个函数做什么）:
 *   读取 active worktree status.changed；没有 active worktree 时返回 0。
 */
export function worktreeChangeCount(activeWorktree: WorkbenchWorktree | null): number {
  return activeWorktree?.status.changed ?? 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史 tab 中每条提交需要紧凑时间标识，便于在窄侧栏内扫描最近提交。
 *
 * Code Logic（这个函数做什么）:
 *   1 分钟内显示 now，1 小时内显示 Xm，24 小时内显示 Xh，更早显示 YYYY-MM-DD。
 */
export function formatCommitRelativeTime(
  authoredAt: string,
  emptyValue: string,
  now = new Date(),
): string {
  const date = new Date(authoredAt);
  if (Number.isNaN(date.getTime())) return emptyValue;
  const diffMs = Math.max(0, now.getTime() - date.getTime());
  const diffMinutes = Math.floor(diffMs / 60_000);
  if (diffMinutes < 1) return 'now';
  if (diffMinutes < 60) return `${diffMinutes}m`;
  const diffHours = Math.floor(diffMinutes / 60);
  if (diffHours < 24) return `${diffHours}h`;
  return date.toISOString().slice(0, 10);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史 tab 需要区分空提交历史与加载失败，显示不同空态。
 *
 * Code Logic（这个函数做什么）:
 *   对任意包含 length 的数组式列表做非空判断，便于测试和 UI 复用。
 */
export function hasGitHistory(commits: Array<unknown>): boolean {
  return commits.length > 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史 tab 需要像 VS Code 一样根据提交 DAG 展示分支线和 merge 线。
 *
 * Code Logic（这个函数做什么）:
 *   按 git log 的拓扑顺序维护活跃 lane；第一个 parent 继承当前 lane，额外 parent 创建侧 lane。
 */
export function buildGitGraphRows(commits: WorkbenchGitCommit[]): WorkbenchGitGraphRow[] {
  const rows: WorkbenchGitGraphRow[] = [];
  let lanes: WorkbenchGitGraphLane[] = [];
  let nextColorIndex = 0;

  for (const commit of commits) {
    let lane = lanes.findIndex((entry) => entry.hash === commit.hash);
    const activeLanes = lanes.map((entry) => ({ ...entry }));
    if (lane < 0) {
      lane = lanes.length;
      lanes = [
        ...lanes,
        {
          hash: commit.hash,
          colorIndex: nextColorIndex % GIT_GRAPH_COLOR_COUNT,
        },
      ];
      nextColorIndex += 1;
    }

    const colorIndex = lanes[lane]?.colorIndex ?? 0;
    const nextLanes = lanes.map((entry) => ({ ...entry }));
    const [firstParent, ...extraParents] = commit.parentHashes;

    if (!firstParent) {
      nextLanes.splice(lane, 1);
    } else {
      const existingParentLane = nextLanes.findIndex((entry) => entry.hash === firstParent);
      if (existingParentLane >= 0 && existingParentLane !== lane) {
        nextLanes.splice(lane, 1);
      } else {
        nextLanes[lane] = { hash: firstParent, colorIndex };
      }
    }

    let insertAt = Math.min(lane + 1, nextLanes.length);
    for (const parentHash of extraParents) {
      const existingParentLane = nextLanes.findIndex((entry) => entry.hash === parentHash);
      if (existingParentLane < 0) {
        nextLanes.splice(insertAt, 0, {
          hash: parentHash,
          colorIndex: nextColorIndex % GIT_GRAPH_COLOR_COUNT,
        });
        nextColorIndex += 1;
        insertAt += 1;
      }
    }

    const parentLanes = commit.parentHashes
      .map((parentHash) => nextLanes.findIndex((entry) => entry.hash === parentHash))
      .filter((parentLane) => parentLane >= 0);
    const laneCount = Math.max(activeLanes.length, nextLanes.length, lane + 1, 1);

    rows.push({
      commit,
      lane,
      laneCount,
      activeLanes,
      outputLanes: nextLanes.map((entry) => ({ ...entry })),
      parentLanes,
      colorIndex,
    });
    lanes = nextLanes;
  }

  return rows;
}
