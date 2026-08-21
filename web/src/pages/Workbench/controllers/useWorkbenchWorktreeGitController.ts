/**
 * Workbench worktree/Git 域 controller —— worktree 生命周期 + 创建表单/busy/error + Git 提交刷新 + merge 阶段。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Workbench 的 worktree/Git 域负责 worktree 的 load/select/create/remove/commit/push/merge 全生命周期，
 *   以及创建表单的 busy/error、Git 提交历史刷新、一键合并的阶段进度（merge-progress 事件）。这些状态和
 *   effect 原先散落在 Workbench.tsx 多处 state/useCallback/useEffect；本 controller 把它们集中持有，让
 *   Workbench.tsx 只负责调度与渲染。
 *
 *   重要边界：
 *   - worktree 创建走 terminalBridge.createSessionForWorktree（controller 不直接调用 sessions.create）。
 *   - remove/merge 后的 terminal buffer/session 清理走显式 bridge（terminalBridge.loadSessions /
 *     clearBuffersForWorktree），绝不直接 mutate terminal state。
 *   - controller 不复制 project / file / application / prompt optimizer 状态；这些仍归 Workbench.tsx
 *     或邻接 controller 所有。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 worktrees / worktreeBusy / worktreeError / createWorktreeOpen /
 *     createWorktreeBranchPrefix / createWorktreeBranchSuffixDraft / gitCommits / gitHistoryLoading /
 *     gitHistoryError / mergeProgressWorktreeId / mergeStages 单一权威状态；activeWorktreeId 由页面持有
 *     （终端域 controller / 文件 effect / deep link effect 都需要读取），controller 接收为输入并透传 setter。
 *   - 维护 activeProjectIdRef / activeWorktreeIdRef / 按项目 merge 快照与 operation ref，
 *     让普通 mutation 继续做 stale guard，同时不因切换工作区丢弃后台 merge 的阶段与终态。
 *   - 暴露 loadWorktrees / loadGitHistory / handleOpenCreateWorktree / handleCancelCreateWorktree /
 *     handleCreateWorktree / handleCommitWorktree / handlePushWorktree / handleMergeWorktree /
 *     handleRemoveWorktree / clearMergeStagePanel 操作函数。
 *   - loadGitHistory 在拉提交前 best-effort 对账 worktree 列表（复用 list → sync_git_worktrees），
 *     让外部已清理的 worktree 从导航入口消失；对账失败不挡历史刷新。
 *   - 注册 workbench:merge-progress 事件订阅（按 payload project/worktree 写入对应快照）。
 *
 * 不复制邻接 controller 状态：project / session / file / application / prompt optimizer 状态仍归
 * Workbench.tsx 各自所有。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { workbenchApi, type WorktreeGitApiScope } from '@/api/workbench';

/**
 * 缺省 API scope：模块级稳定引用，避免 `params.api ?? {...}` 内联对象导致引用随 render
 * 变化（进而让依赖 api.worktrees/api.git 的 useCallback 反复失效）。生产 build 走真实实现，
 * 测试侧由 Params.api 注入 fake。
 */
const DEFAULT_WORKTREE_GIT_API: WorktreeGitApiScope = {
  worktrees: workbenchApi.worktrees,
  git: workbenchApi.git,
};
import {
  createOperationKey,
  isCurrentOperation,
  nextOperationSequence,
  type WorkbenchOperationKey,
} from '@/lib/asyncState/operationContext';
import {
  isMutationFailedHook,
  isMutationSucceeded,
  isMutationUnknown,
} from '@/lib/asyncState/mutationOutcome';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
  type WorkbenchMutationReconcileResult,
} from '@/lib/workbenchMutationReconciliation';
import { isLatestRequest } from '../workbenchFiles';
import type {
  MutationIntent,
  MutationKind,
  WorkbenchGitCommit,
  WorkbenchMergeProgressEvent,
  WorkbenchMergeStage,
  WorkbenchMergeStageId,
  WorkbenchMutationEnvelope,
  WorkbenchHookFailure,
  WorkbenchWorktree,
} from '@/lib/types';
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  canMergeWorktree,
  composeWorktreeBranchName,
  formatWorkbenchMergeStages,
  shouldAutoDismissMergeStages,
} from '../workbenchWorktrees';
import type { WorktreeBranchPrefix } from '../workbenchWorktrees';
import type { WorkbenchTerminalBridge } from './useWorkbenchTerminalController';

/**
 * controller 用的 worktree 操作 busy 标记；与原 Workbench.tsx 内部使用的 string 标记保持一致。
 */
export type WorktreeBusyKind = 'create' | 'commit' | 'push' | 'merge' | 'remove';

/**
 * unknown 后保留的稳定 operation 锁。
 *
 * Business Logic（为什么需要这个类型）:
 *   timeout/network 后禁止 mint 新 clientOperationId 盲重放；必须 same-id 重试/对账。
 *
 * Code Logic（字段说明）:
 *   kind 为 mutation 种类；projectId/worktreeId 限定上下文；clientOperationId 复用。
 */
export type WorktreeUnknownMutationLock = {
  kind: Exclude<MutationKind, never>;
  projectId: string;
  worktreeId: string;
  clientOperationId: string;
};

/**
 * failedHook envelope 之后保留的修复上下文。
 *
 * Business Logic（为什么需要这个类型）:
 *   commit/push 因 pre-commit/pre-push 钩子失败时，前端展示钩子原始输出与「让 AI 修复」按钮；
 *   用户点击后启动 Claude agent 在 worktree 终端运行；agent 不直接 commit/push（禁止 --no-verify / git push），
 *   用户在终端观察后手动点「重试 commit/push」（fresh clientOperationId）。
 *
 * Code Logic（字段说明）:
 *   kind 限定可修复的种类（commit|push）；hookFailure 透传 envelope 载荷供 prompt 与重试用；
 *   clientOperationId 是原失败动作的 id（修复不消耗它，重试由新 id 走 ledger Fresh 路径）；
 *   repair 成功后保留 sessionId 直到用户点重试或开始新 commit/push。
 */
export type WorkbenchHookRepair = {
  kind: 'commit' | 'push';
  hookFailure: WorkbenchHookFailure;
  clientOperationId: string;
  /** 修复启动返回的新终端 session id（前端聚焦该终端）。 */
  terminalSessionId?: string;
};

/** 单个项目正在展示的一键合并阶段快照。 */
type WorkbenchMergeProgressSnapshot = {
  worktreeId: string;
  stages: WorkbenchMergeStage[];
};

/** 不受当前选中 worktree 影响的后台 merge 身份。 */
type WorkbenchPendingMergeOperation = {
  projectId: string;
  worktreeId: string;
  clientOperationId: string;
};

/**
 * 一键合并自动隐藏阶段条的延迟；与原 Workbench.tsx 内部常量保持一致。
 */
const MERGE_STAGE_AUTO_DISMISS_MS = 2500;
const INITIAL_MERGE_STAGE_ID: WorkbenchMergeStageId = 'checkSource';

/** controller 用到的 i18n 错误文案 key；调用方注入对应 t('workbench:errors.X')。 */
export type WorkbenchWorktreeGitErrorKey =
  | 'worktrees'
  | 'createWorktree'
  | 'commitWorktree'
  | 'pushWorktree'
  | 'mergeWorktree'
  | 'removeWorktree'
  | 'gitHistory'
  | 'mutationUnknown';

/**
 * controller 输入：窄 API + 回调 + 外部 bridge，避免吞并 Projects / Sessions / Terminal buffer context。
 *
 * 字段说明：
 *   - activeProjectId / activeWorktreeId：从 Workbench 透传，仅用于读取；activeWorktreeId 同时也是 controller
 *     持有的权威 state（通过 setActiveWorktreeId 暴露），页面把它绑到 chip 点击与 deep link effect。
 *   - remoteWriteDisabled：项目域 controller 决定的只读标记；影响 create/commit/push/merge/remove 是否执行。
 *   - inspectorTab：当前右侧检查器 tab；'history' 时 commit/push/merge 完成后需刷新 Git 历史。
 *   - isCurrentProject / markRequestFailure / markRequestSuccess：项目域 controller 的窄 API，用于 stale guard
 *     与远端离线标记。
 *   - refreshProjectSessionStats：worktree 创建/合并完成后刷新项目 session 统计。
 *   - terminalBridge：终端域 controller 暴露的 bridge，worktree 创建/合并/移除后通过它联动 session/buffer。
 *     create 走 terminalBridge.createSessionForWorktree（内部完成终端尺寸估算、session 创建、focus、reset、统计刷新）。
 *   - displayErrorMessage / desktopUnavailableMessage：错误文案构造。
 *   - translateError / translateWorktreeMessage：i18n 文案注入（错误 key / worktree 提示 / merge 阶段消息）。
 *   - canListenToTauriEvents：判断是否注册 Tauri event listener（与终端域 controller 行为一致）。
 *   - merge clearBuffers 不再需要 sessions 输入：terminalBridge.clearBuffersForWorktree 内部读取终端域 sessions。
 */
export interface UseWorkbenchWorktreeGitControllerParams {
  activeProjectId: string | null;
  /** 当前 active worktree id；由页面持有（终端域 controller / 文件 effect 也读取同一值）。 */
  activeWorktreeId: string | null;
  /** 页面持有的 activeWorktreeId setter；controller 在 loadWorktrees / create / remove 后调用它切换。 */
  setActiveWorktreeId: (next: string | null) => void;
  remoteWriteDisabled: boolean;
  inspectorTab: 'files' | 'history' | 'notes';
  /**
   * worktree/Git 域 API scope；用于让 controller 接受窄 API 注入，避免直接依赖
   * `workbenchApi.worktrees` / `workbenchApi.git`。测试和生产均可注入。缺省回落
   * `workbenchApi.worktrees` / `workbenchApi.git`。
   */
  api?: WorktreeGitApiScope;
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  refreshProjectSessionStats: (projectId: string) => void;
  terminalBridge: WorkbenchTerminalBridge;
  displayErrorMessage: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage: string;
  translateError: (key: WorkbenchWorktreeGitErrorKey) => string;
  translateWorktreeMessage: (
    key: 'mergeConfirm' | 'mergeCollectConfirm' | 'checkSourceMessage',
    vars?: Record<string, unknown>,
  ) => string;
  /** merge 前的用户确认；remove 已迁出本 controller 到 UI 层 Dialog。 */
  confirmAction: (message: string) => boolean;
  canListenToTauriEvents: () => boolean;
}

/**
 * controller 返回值：worktree/Git 域权威状态 + 操作函数 + bridge 视图。
 */
export interface WorkbenchWorktreeGitControllerResult {
  // ---- 渲染数据 ----
  worktrees: WorkbenchWorktree[];
  worktreeBusy: WorktreeBusyKind | null;
  /**
   * unknown 后仍持有的 stable clientOperationId 锁；非 null 时禁止 mint 新 id。
   */
  unknownMutationLock: WorktreeUnknownMutationLock | null;
  worktreeError: string | null;
  /**
   * failedHook 之后的修复上下文；null 表示无待修复项或用户已开始新一次 commit/push。
   */
  hookRepair: WorkbenchHookRepair | null;
  createWorktreeOpen: boolean;
  createWorktreeBranchPrefix: WorktreeBranchPrefix;
  createWorktreeBranchSuffixDraft: string;
  gitCommits: WorkbenchGitCommit[];
  gitHistoryLoading: boolean;
  gitHistoryError: string | null;
  mergeProgressWorktreeId: string | null;
  mergeStages: WorkbenchMergeStage[];
  // ---- 派生 setters / actions ----
  setWorktrees: (next: WorkbenchWorktree[]) => void;
  setCreateWorktreeOpen: (next: boolean) => void;
  setCreateWorktreeBranchPrefix: (next: WorktreeBranchPrefix) => void;
  setCreateWorktreeBranchSuffixDraft: (next: string) => void;
  setGitCommits: (next: WorkbenchGitCommit[]) => void;
  setGitHistoryError: (next: string | null) => void;
  /** 返回对账后的列表；stale/失败时 null（调用方可忽略返回值）。 */
  loadWorktrees: (projectId: string) => Promise<WorkbenchWorktree[] | null>;
  loadGitHistory: () => Promise<void>;
  handleOpenCreateWorktree: () => void;
  handleCancelCreateWorktree: () => void;
  handleCreateWorktree: () => Promise<void>;
  handleCommitWorktree: () => Promise<void>;
  handlePushWorktree: () => Promise<void>;
  handleMergeWorktree: () => Promise<void>;
  handleRemoveWorktree: (worktreeId: string) => Promise<void>;
  /**
   * failedHook 之后用户点「让 AI 修复」时调用：在该 worktree 终端启动 Claude agent 修复钩子根因。
   * 失败/未设置 hookRepair 时 no-op。
   */
  handleRepairHookFailure: () => Promise<void>;
  /**
   * failedHook 面板上的「忽略 / Dismiss」按钮：纯本地动作，清空 hookRepair，不触发任何 IPC。
   * 未设置 hookRepair 时 no-op。user 已决定不修也不重试，主动放弃当前失败上下文。
   */
  handleDismissHookFailure: () => Promise<void>;
  /**
   * 修复完成后用户点「重试 commit/push」：按 hookRepair.kind 复用对应 handler 走 fresh clientOperationId。
   * 未设置 hookRepair 时 no-op。
   */
  handleRetryAfterRepair: () => Promise<void>;
  /** 立即清空 merge 阶段条（取消隐藏计时器、清空追踪 worktree 与阶段列表）。 */
  clearMergeStagePanel: () => void;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有 worktrees / activeWorktreeId / worktreeBusy / worktreeError / createWorktreeOpen /
 *      createWorktreeBranchPrefix / createWorktreeBranchSuffixDraft / gitCommits / gitHistoryLoading /
 *      gitHistoryError / mergeProgressWorktreeId / mergeStages state；
 *   2. 用 ref 跟踪 activeProjectId / activeWorktreeId / mergeProgressWorktreeId，让异步回调读到最新值；
 *   3. 注册 workbench:merge-progress 事件订阅（按当前 project/worktree 过滤）；
 *   4. 暴露稳定的操作函数（useCallback + ref/bridge 输入），便于 Workbench 在多处复用。
 */
export function useWorkbenchWorktreeGitController(
  params: UseWorkbenchWorktreeGitControllerParams,
): WorkbenchWorktreeGitControllerResult {
  const {
    activeProjectId,
    activeWorktreeId,
    setActiveWorktreeId,
    remoteWriteDisabled,
    inspectorTab,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    refreshProjectSessionStats,
    terminalBridge,
    displayErrorMessage,
    desktopUnavailableMessage,
    translateError,
    translateWorktreeMessage,
    confirmAction,
    canListenToTauriEvents,
  } = params;

  // Business Logic: 让 controller 接受窄 API 注入；缺省回落真实 workbenchApi（fallback 不去掉
  // workbenchApi import，保证线上行为完全等价）。fallback 用 module 级稳定常量，避免内联对象
  // 引起 useCallback dep 抖动。
  const api = params.api ?? DEFAULT_WORKTREE_GIT_API;

  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [worktreeBusy, setWorktreeBusy] = useState<WorktreeBusyKind | null>(null);
  const [unknownMutationLock, setUnknownMutationLock] =
    useState<WorktreeUnknownMutationLock | null>(null);
  const [worktreeError, setWorktreeError] = useState<string | null>(null);
  // failedHook 修复上下文：commit/push 钩子失败时设置；用户点重试/开始新 commit/push 时清空。
  const [hookRepair, setHookRepair] = useState<WorkbenchHookRepair | null>(null);
  const [createWorktreeOpen, setCreateWorktreeOpen] = useState<boolean>(false);
  const [createWorktreeBranchPrefix, setCreateWorktreeBranchPrefix] =
    useState<WorktreeBranchPrefix>(DEFAULT_WORKTREE_BRANCH_PREFIX);
  const [createWorktreeBranchSuffixDraft, setCreateWorktreeBranchSuffixDraft] =
    useState<string>('');
  const [gitCommits, setGitCommits] = useState<WorkbenchGitCommit[]>([]);
  const [gitHistoryLoading, setGitHistoryLoading] = useState<boolean>(false);
  const [gitHistoryError, setGitHistoryError] = useState<string | null>(null);
  // Business Logic: merge 在后端独立运行，切换 project/worktree 不能丢掉其阶段；按项目缓存后只投影
  // 当前项目，既能在返回时恢复进度，也不会把另一个项目的 Claude 状态串到当前 Inspector。
  const [mergeProgressByProject, setMergeProgressByProject] = useState<
    Record<string, WorkbenchMergeProgressSnapshot>
  >({});

  // Business Logic: 异步加载回调返回时，active project / worktree 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const activeWorktreeIdRef = useRef<string | null>(activeWorktreeId);
  // Business Logic: 事件可能在 React commit 前连续到达；同步 ref 让每个项目都以最新阶段做覆盖合并。
  const mergeProgressByProjectRef = useRef<Record<string, WorkbenchMergeProgressSnapshot>>({});
  // Business Logic: project A 的 merge 可在用户查看 project B 时继续；每个项目单独持有后台 operation 身份。
  const pendingMergeOperationsRef = useRef<Record<string, WorkbenchPendingMergeOperation>>({});
  // Business Logic: 多项目可各自完成 merge；自动隐藏计时器必须按项目隔离，不能互相取消。
  const mergeStageDismissTimerRef = useRef<Record<string, number>>({});
  // Business Logic: 同一 project 的 worktree list 与同一 project/worktree 的 git history 可能并发；
  // 用单调 request seq 丢弃过期响应，避免 create/remove/merge 后被慢速 list 回写旧状态。
  const worktreeListRequestSeqRef = useRef<Record<string, number>>({});
  const gitHistoryRequestSeqRef = useRef<Record<string, number>>({});
  // Business Logic: Git mutation 的 success/catch/finally 必须绑定发起时的 project/worktree/sequence，
  // 防止旧异步结果污染用户已切换到的新上下文。
  const mutationSequenceRef = useRef(0);

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  const activeMergeProgress = activeProjectId ? mergeProgressByProject[activeProjectId] : undefined;
  const mergeProgressWorktreeId = activeMergeProgress?.worktreeId ?? null;
  const mergeStages = activeMergeProgress?.stages ?? [];

  // Business Logic: 用户切换 project/worktree 后，挂起 mutation 的 UI busy/error/unknown 锁不得粘在新上下文。
  // Code Logic: 递增 sequence 使普通 mutation 的旧 settlement 过期，并清空 error/unknown lock；merge 使用独立
  // operation 身份，因此同项目切换 worktree 时保持 busy，切到其他项目时按目标项目是否有 merge 恢复 busy。
  // hookRepair 同样属于「上一失败上下文」的产物（clientOperationId + 失败 stage 都绑定旧 worktree）；
  // 不清会让「让 AI 修复 / 重试 commit-push」按钮在用户已切到其他工作台项目时仍渲染并指向 stale context。
  useEffect(() => {
    mutationSequenceRef.current = nextOperationSequence(mutationSequenceRef.current);
    setWorktreeBusy(
      activeProjectId && pendingMergeOperationsRef.current[activeProjectId] ? 'merge' : null,
    );
    setWorktreeError(null);
    setUnknownMutationLock(null);
    setHookRepair(null);
  }, [activeProjectId, activeWorktreeId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可能连续发起 merge 或切换项目，旧的自动隐藏计时器不能误清新一轮进度。
   *
   * Code Logic（这个函数做什么）:
   *   如果存在 merge 阶段条隐藏计时器，则取消并清空 ref。
   */
  const clearMergeStageDismissTimer = useCallback((projectId?: string) => {
    if (projectId) {
      const timer = mergeStageDismissTimerRef.current[projectId];
      if (timer === undefined) return;
      window.clearTimeout(timer);
      delete mergeStageDismissTimerRef.current[projectId];
      return;
    }
    for (const timer of Object.values(mergeStageDismissTimerRef.current)) {
      window.clearTimeout(timer);
    }
    mergeStageDismissTimerRef.current = {};
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   merge-progress 可能来自当前项目、后台项目或尚未返回的 command；所有来源必须写同一份项目快照。
   *
   * Code Logic（这个函数做什么）:
   *   同步更新 ref 与 React state，并返回新快照供调用方判断是否进入终态。
   */
  const updateProjectMergeProgress = useCallback(
    (
      projectId: string,
      updater: (
        current: WorkbenchMergeProgressSnapshot | undefined,
      ) => WorkbenchMergeProgressSnapshot | undefined,
    ): WorkbenchMergeProgressSnapshot | undefined => {
      const nextSnapshot = updater(mergeProgressByProjectRef.current[projectId]);
      const nextByProject = { ...mergeProgressByProjectRef.current };
      if (nextSnapshot) {
        nextByProject[projectId] = nextSnapshot;
      } else {
        delete nextByProject[projectId];
      }
      mergeProgressByProjectRef.current = nextByProject;
      setMergeProgressByProject(nextByProject);
      return nextSnapshot;
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   项目切换或成功完成后的阶段条应释放 Git 历史区域空间。
   *
   * Code Logic（这个函数做什么）:
   *   取消隐藏计时器，清空当前追踪 worktree 与阶段列表。
   */
  const clearMergeStagePanel = useCallback(() => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    clearMergeStageDismissTimer(projectId);
    updateProjectMergeProgress(projectId, () => undefined);
  }, [clearMergeStageDismissTimer, updateProjectMergeProgress]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   成功 merge 后用户只需要短暂看到完成反馈，不应长期保留状态条占位。
   *
   * Code Logic（这个函数做什么）:
   *   为指定 worktree 安排延迟隐藏；触发时若已经开始追踪别的 worktree，则不清理新状态。
   */
  const scheduleMergeStagePanelDismiss = useCallback(
    (projectId: string, worktreeId: string) => {
      clearMergeStageDismissTimer(projectId);
      mergeStageDismissTimerRef.current[projectId] = window.setTimeout(() => {
        delete mergeStageDismissTimerRef.current[projectId];
        updateProjectMergeProgress(projectId, (current) =>
          current?.worktreeId === worktreeId ? undefined : current,
        );
      }, MERGE_STAGE_AUTO_DISMISS_MS);
    },
    [clearMergeStageDismissTimer, updateProjectMergeProgress],
  );

  // Business Logic: 与原 Workbench.tsx 行为一致——组件卸载时取消尚未触发的隐藏计时器。
  useEffect(() => clearMergeStageDismissTimer, [clearMergeStageDismissTimer]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   create/remove/merge/commit 等 mutation 成功后，旧 worktree list 响应不能覆盖新列表。
   *
   * Code Logic（这个函数做什么）:
   *   递增指定 project 的 worktree list request seq。
   */
  const invalidateWorktreeListRequests = useCallback((projectId: string): void => {
    const current = worktreeListRequestSeqRef.current[projectId] ?? 0;
    worktreeListRequestSeqRef.current[projectId] = current + 1;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   commit/push/merge 或切换 worktree 后，旧 git history 响应不能覆盖新历史。
   *
   * Code Logic（这个函数做什么）:
   *   递增指定 project+worktree 键的 git history request seq；worktreeId 为空时用 `__none__`。
   */
  const invalidateGitHistoryRequests = useCallback(
    (projectId: string, worktreeId: string | null): void => {
      const key = `${projectId}::${worktreeId ?? '__none__'}`;
      const current = gitHistoryRequestSeqRef.current[key] ?? 0;
      gitHistoryRequestSeqRef.current[key] = current + 1;
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   切换项目或刷新时需要重新拉取当前项目的所有 worktree，并按返回结果校正 active worktree。
   *   调用方（尤其 loadGitHistory）需要知道对账后的列表，以便在 React 尚未把 activeWorktreeId
   *   同步进 ref 之前，用正确的 worktree 拉提交历史；不得在此同步改写 activeWorktreeIdRef，
   *   否则会让进行中的 mutation settlement（绑定发起时 worktreeId）误判为 stale。
   *
   * Code Logic（这个函数做什么）:
   *   1. 调用 worktrees.list(projectId)，并通过 isCurrentProject 做 stale guard；
   *   2. 成功时更新 worktrees、保留仍存在的 active worktree（否则 setActiveWorktreeId 回退到第一个），
   *      markRequestSuccess，返回 list；
   *   3. 失败/stale 时返回 null；失败时 markRequestFailure + 展示 worktreeError。
   */
  const loadWorktrees = useCallback(
    async (projectId: string): Promise<WorkbenchWorktree[] | null> => {
      const requestSeq = (worktreeListRequestSeqRef.current[projectId] ?? 0) + 1;
      worktreeListRequestSeqRef.current[projectId] = requestSeq;
      try {
        setWorktreeError(null);
        const list = await api.worktrees.list(projectId);
        if (
          !isCurrentProject(projectId) ||
          !isLatestRequest(worktreeListRequestSeqRef.current[projectId], requestSeq)
        ) {
          return null;
        }
        markRequestSuccess(projectId);
        setWorktrees(list);
        // Business Logic: 保留仍存在的 active worktree；否则回退到第一个。读取 ref 拿到最新 active id，
        // 与原 Workbench.tsx 的 functional setState 行为等价。
        // 只通过 setter 通知页面；不在此写 activeWorktreeIdRef，避免打断 mutation settlement。
        const currentActive = activeWorktreeIdRef.current;
        if (!(currentActive && list.some((worktree) => worktree.id === currentActive))) {
          setActiveWorktreeId(list[0]?.id ?? null);
        }
        return list;
      } catch (error) {
        if (
          !isCurrentProject(projectId) ||
          !isLatestRequest(worktreeListRequestSeqRef.current[projectId], requestSeq)
        ) {
          return null;
        }
        markRequestFailure(projectId, error);
        setWorktreeError(
          displayErrorMessage(error, translateError('worktrees'), desktopUnavailableMessage),
        );
        return null;
      }
    },
    [isCurrentProject, markRequestSuccess, desktopUnavailableMessage, markRequestFailure, translateError, displayErrorMessage, setActiveWorktreeId, api.worktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Git 历史 tab 与 commit/push/merge 完成后需要刷新当前 worktree 的提交历史；多 worktree 场景下旧响应不能覆盖。
   *   外部清理（rm -rf / git worktree prune）后的孤儿导航入口也应在刷新历史时消失，因此每次刷新先对账 worktree。
   *
   * Code Logic（这个函数做什么）:
   *   1. 无 active project 时清空 commits/error/loading 并返回；
   *   2. best-effort 调用 loadWorktrees（后端 list 内 sync_git_worktrees 会 prune 磁盘已不存在的非主 worktree）；
   *      对账失败只走 worktree 既有 error 路径，不中止历史刷新；
   *   3. 若对账返回 list：active 仍在则用之，否则用 list[0]（与 loadWorktrees 回退一致）；list 为 null 时回退到 ref；
   *   4. 再 git.listCommits(projectId, worktreeId, 30)；project/worktree 切换时丢弃响应。
   */
  const loadGitHistory = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      setGitCommits([]);
      setGitHistoryError(null);
      setGitHistoryLoading(false);
      return;
    }

    // Best-effort worktree reconcile: keep navigation honest without blocking history refresh.
    // loadWorktrees swallows its own errors into worktreeError / markRequestFailure and returns null.
    const reconciled = await loadWorktrees(projectId);
    if (!isCurrentProject(projectId) || activeProjectIdRef.current !== projectId) {
      return;
    }

    // Prefer still-present active from reconcile result; if pruned, fall back to list[0] for this fetch.
    // When reconcile failed (null), keep the current ref so history still attempts the active worktree.
    let worktreeId = activeWorktreeIdRef.current;
    if (reconciled) {
      if (!(worktreeId && reconciled.some((worktree) => worktree.id === worktreeId))) {
        worktreeId = reconciled[0]?.id ?? null;
      }
    }

    const requestKey = `${projectId}::${worktreeId ?? '__none__'}`;
    const requestSeq = (gitHistoryRequestSeqRef.current[requestKey] ?? 0) + 1;
    gitHistoryRequestSeqRef.current[requestKey] = requestSeq;

    // Accept response when active ref still matches the requested worktree, or when we
    // intentionally fell back after prune (ref still points at a missing id, request used list[0]).
    const isHistoryRequestCurrent = (): boolean => {
      if (!isCurrentProject(projectId)) return false;
      if (!isLatestRequest(gitHistoryRequestSeqRef.current[requestKey], requestSeq)) return false;
      const current = activeWorktreeIdRef.current;
      if (current === worktreeId) return true;
      if (
        reconciled
        && worktreeId === (reconciled[0]?.id ?? null)
        && current !== null
        && !reconciled.some((worktree) => worktree.id === current)
      ) {
        return true;
      }
      return false;
    };

    try {
      setGitHistoryLoading(true);
      setGitHistoryError(null);
      const commits = await api.git.listCommits(projectId, worktreeId, 30);
      if (!isHistoryRequestCurrent()) {
        return;
      }
      setGitCommits(commits);
      markRequestSuccess(projectId);
    } catch (error) {
      if (!isHistoryRequestCurrent()) {
        return;
      }
      markRequestFailure(projectId, error);
      setGitCommits([]);
      setGitHistoryError(
        displayErrorMessage(error, translateError('gitHistory'), desktopUnavailableMessage),
      );
    } finally {
      if (isHistoryRequestCurrent()) {
        setGitHistoryLoading(false);
      }
    }
  }, [isCurrentProject, markRequestSuccess, desktopUnavailableMessage, markRequestFailure, translateError, displayErrorMessage, loadWorktrees, api.git]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击“新建 worktree”展开表单；remoteWriteDisabled 或 busy 时静默拒绝，避免必然失败的远端写。
   *
   * Code Logic（这个函数做什么）:
   *   清空 worktreeError 与创建表单 draft，打开表单。
   */
  const handleOpenCreateWorktree = useCallback((): void => {
    if (!activeProjectIdRef.current || worktreeBusy !== null || remoteWriteDisabled) return;
    setWorktreeError(null);
    setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
    setCreateWorktreeBranchSuffixDraft('');
    setCreateWorktreeOpen(true);
  }, [remoteWriteDisabled, worktreeBusy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消创建 worktree；正在 create 时禁止取消，避免半态。
   *
   * Code Logic（这个函数做什么）:
   *   非创建中时关闭表单并重置 draft。
   */
  const handleCancelCreateWorktree = useCallback((): void => {
    if (worktreeBusy === 'create') return;
    setCreateWorktreeOpen(false);
    setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
    setCreateWorktreeBranchSuffixDraft('');
  }, [worktreeBusy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   发起 mutation 时绑定 project/worktree + 单调 sequence，供 settlement 守卫。
   *
   * Code Logic（这个函数做什么）:
   *   递增 mutationSequenceRef 并返回 WorkbenchOperationKey；worktreeId 可为 null（create 项目级）。
   */
  const beginMutationOperation = useCallback(
    (projectId: string, worktreeId: string | null): WorkbenchOperationKey => {
      const sequence = nextOperationSequence(mutationSequenceRef.current);
      mutationSequenceRef.current = sequence;
      return createOperationKey(projectId, worktreeId, sequence);
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   success/catch/finally 每个写入点都必须确认仍处于发起操作时的上下文。
   *
   * Code Logic（这个函数做什么）:
   *   用当前 active project/worktree + 最新 sequence 与 settled key 比较。
   */
  const isSettledCurrent = useCallback((settled: WorkbenchOperationKey): boolean => {
    const current = createOperationKey(
      activeProjectIdRef.current ?? '',
      activeWorktreeIdRef.current,
      mutationSequenceRef.current,
    );
    return isCurrentOperation(current, settled);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   unknown 后禁止 mint 新 id；同 kind+context 复用 lock 中的 clientOperationId。
   *
   * Code Logic（这个函数做什么）:
   *   lock 匹配 kind/project/worktree 时返回既有 id；否则 mint UUID。
   */
  const resolveClientOperationId = useCallback(
    (
      kind: WorktreeUnknownMutationLock['kind'],
      projectId: string,
      worktreeId: string,
      lock: WorktreeUnknownMutationLock | null,
    ): string => {
      if (
        lock
        && lock.kind === kind
        && lock.projectId === projectId
        && lock.worktreeId === worktreeId
      ) {
        return lock.clientOperationId;
      }
      return crypto.randomUUID();
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   unknown 是 worktree 级共享锁；仅同 kind reconcile/终态可清锁，sibling 成功/失败绝不能丢弃原锁。
   *
   * Code Logic（这个函数做什么）:
   *   仅当当前 lock.kind 等于给定 kind 时置 null，否则保留原锁。
   */
  const clearUnknownMutationLockForKind = useCallback(
    (kind: WorktreeUnknownMutationLock['kind']): void => {
      setUnknownMutationLock((prev) => (prev && prev.kind === kind ? null : prev));
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   非 null unknownMutationLock 期间禁止 sibling kind 发起 Fresh claim。
   *
   * Code Logic（这个函数做什么）:
   *   lock 为空或 kind 匹配时返回 true；否则 false。
   */
  const isMutationKindAllowedUnderUnknownLock = useCallback(
    (kind: WorktreeUnknownMutationLock['kind']): boolean => {
      if (!unknownMutationLock) return true;
      return unknownMutationLock.kind === kind;
    },
    [unknownMutationLock],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   envelope.unknown 后禁止盲重放；ledger 终态优先，否则 authority 矩阵对账。
   *
   * Code Logic（这个函数做什么）:
   *   查询 getMutationOperation；刷新 worktrees；merge 填充 source 存在性 + mainContainsSourceHead
   *   （main listCommits）；remove 填充 identity；commit/push 缺 head 权威时依赖 ledger 终态。
   */
  const reconcileUnknownMutation = useCallback(
    async (
      clientOperationId: string,
      projectId: string,
      worktreeId: string,
      settled: WorkbenchOperationKey,
    ): Promise<WorkbenchMutationReconcileResult> => {
      const ledger = await api.worktrees
        .getMutationOperation(clientOperationId)
        .catch(() => null);
      if (!isSettledCurrent(settled)) return 'unknown';

      // ledger 终态可直接确认，不必先刷新 authority（仍刷新列表以便 UI 一致）。
      invalidateWorktreeListRequests(projectId);
      invalidateGitHistoryRequests(projectId, worktreeId);
      await loadWorktrees(projectId);
      if (!isSettledCurrent(settled)) return 'unknown';

      const intent: MutationIntent | null = ledger?.intent ?? null;
      if (!intent) {
        if (ledger?.state === 'succeeded') return 'confirmedSucceeded';
        if (ledger?.state === 'failed') return 'confirmedFailed';
        return 'unknown';
      }

      if (ledger?.state === 'succeeded' || ledger?.state === 'failed') {
        return reconcileWorkbenchMutation(intent, ledger, {});
      }

      if (intent.kind === 'merge' || intent.kind === 'collectMerge' || intent.kind === 'remove') {
        try {
          const latest = await api.worktrees.list(projectId);
          if (!isSettledCurrent(settled)) return 'unknown';
          let mainCommitHashes: string[] | undefined;
          if (intent.kind === 'merge' || intent.kind === 'collectMerge') {
            const main = latest.find((item) => item.isMain) ?? null;
            if (main) {
              try {
                const commits = await api.git.listCommits(projectId, main.id, 100);
                if (!isSettledCurrent(settled)) return 'unknown';
                mainCommitHashes = commits.map((commit) => commit.hash);
              } catch {
                mainCommitHashes = undefined;
              }
            }
          }
          const authority = buildMergeRemoveAuthority(intent, latest, { mainCommitHashes });
          return reconcileWorkbenchMutation(intent, ledger, authority);
        } catch {
          return 'unknown';
        }
      }

      // commit/push：前端无 head 权威字段 → 保持 unknown（除非 ledger 终态，已在上方处理）。
      return reconcileWorkbenchMutation(intent, ledger, {});
    },
    [
      invalidateGitHistoryRequests,
      invalidateWorktreeListRequests,
      isSettledCurrent,
      loadWorktrees,
      api.worktrees,
      api.git,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户填入分支后缀并提交；先创建 worktree，再通过 terminalBridge.createSessionForWorktree 创建并注册
   *   关联终端 session（不 focus），然后刷新 worktree 列表、切到新 worktree，最后再 focus 该 session。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验 active project、remoteWriteDisabled、composeWorktreeBranchName 非空；
   *   2. beginMutationOperation 绑定 project + 当前 active worktree + sequence；
   *   3. setWorktreeBusy('create')，调用 worktrees.create；
   *   4. 每个 success/catch/finally 写入点 isSettledCurrent 守卫（含 setActiveWorktreeId / focusSession）；
   *   5. 同 project 内 worktree 切换也会 bump sequence，旧 create 不得偷换 active。
   *
   *   注意：session 创建交给 terminalBridge.createSessionForWorktree，controller 不直接调用 sessions.create，
   *   避免与终端域 session 状态管理重复。
   */
  const handleCreateWorktree = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    if (remoteWriteDisabled) return;
    const branchName = composeWorktreeBranchName(
      createWorktreeBranchPrefix,
      createWorktreeBranchSuffixDraft,
    );
    if (!branchName) return;
    // create 绑定发起时的 project + active worktree（可 null）；同 project 内 worktree 切换会使 settled 过期。
    const settled = beginMutationOperation(projectId, activeWorktreeIdRef.current);
    try {
      setWorktreeBusy('create');
      setWorktreeError(null);
      const created = await api.worktrees.create(projectId, branchName, null);
      if (!isSettledCurrent(settled)) return;
      // Business Logic: session 创建/注册通过终端域 bridge 完成；bridge 只在目标 worktree 已是 active 时
      // 才 focus。此时新 worktree 还未成为 active（setActiveWorktreeId 在下方），故 bridge 内不会 focus；
      // 我们拿到 sessionId 后，在 setActiveWorktreeId(created.id) 之后再显式 focusSession，保证焦点落在
      // 正确的 worktree 上下文（Codex 二次评审 Finding 4：修复 session 创建竞态）。
      let createdSessionId: string | null = null;
      try {
        createdSessionId = await terminalBridge.createSessionForWorktree(created.id);
      } catch {
        // bridge 内部已处理错误；这里吞掉以避免中断 worktree 列表刷新。
      }
      if (!isSettledCurrent(settled)) return;
      // Business Logic: create 成功后作废旧 list，防止慢速 worktree list 覆盖新建结果。
      invalidateWorktreeListRequests(projectId);
      await loadWorktrees(projectId);
      if (!isSettledCurrent(settled)) return;
      // 先关表单/清 busy（仍 current），再切 active：切 active 会 bump sequence 使 settled 过期。
      setCreateWorktreeOpen(false);
      setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setCreateWorktreeBranchSuffixDraft('');
      setWorktreeBusy(null);
      setActiveWorktreeId(created.id);
      // Business Logic: worktree 切到新建后，再把刚创建的 session 设为焦点。
      // focus 本身按 sessionId 操作；若用户已手动切走，后续 focus 仍可能短暂落到新 session，可接受。
      if (createdSessionId) {
        void terminalBridge.focusSession(createdSessionId);
      }
      return;
    } catch (error) {
      if (!isSettledCurrent(settled)) return;
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('createWorktree'), desktopUnavailableMessage),
      );
    } finally {
      if (isSettledCurrent(settled)) {
        setWorktreeBusy(null);
      }
    }
  }, [
    beginMutationOperation,
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    desktopUnavailableMessage,
    displayErrorMessage,
    invalidateWorktreeListRequests,
    isSettledCurrent,
    loadWorktrees,
    markRequestFailure,
    remoteWriteDisabled,
    setActiveWorktreeId,
    terminalBridge,
    translateError,
    api.worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交当前 worktree 的全部改动；提交后刷新 worktree 状态与 Git 历史（如当前在 history tab）。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验 activeWorktree、remoteWriteDisabled；mint clientOperationId + operation key；
   *   2. setWorktreeBusy('commit')，调用 worktrees.commit(..., clientOperationId)；
   *   3. succeeded 且 current → 刷新列表/历史；unknown 且 current → 对账、不盲重放；
   *   4. catch/finally 仅在 isSettledCurrent 时写 error/busy。
   */
  const handleCommitWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    // unknown 共享锁：sibling kind 禁止 Fresh claim / 清锁。
    if (!isMutationKindAllowedUnderUnknownLock('commit')) return;
    const settled = beginMutationOperation(projectId, worktreeId);
    const clientOperationId = resolveClientOperationId(
      'commit',
      projectId,
      worktreeId,
      unknownMutationLock,
    );
    try {
      setWorktreeBusy('commit');
      setWorktreeError(null);
      // 若仍持有 unknown 锁：只对账，不 mint 新 id、不二次执行。
      if (
        unknownMutationLock
        && unknownMutationLock.kind === 'commit'
        && unknownMutationLock.clientOperationId === clientOperationId
      ) {
        const confirmed = await reconcileUnknownMutation(
          clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('commit');
          setWorktreeError(null);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('commit');
          setWorktreeError(translateError('commitWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'commit',
            projectId,
            worktreeId,
            clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
        return;
      }

      const envelope: WorkbenchMutationEnvelope<WorkbenchWorktree> =
        await api.worktrees.commit(worktreeId, null, clientOperationId);
      if (!isSettledCurrent(settled)) return;

      if (isMutationSucceeded(envelope)) {
        clearUnknownMutationLockForKind('commit');
        // 成功 commit 清空待修复上下文。
        setHookRepair(null);
        invalidateWorktreeListRequests(projectId);
        invalidateGitHistoryRequests(projectId, worktreeId);
        await loadWorktrees(projectId);
        if (!isSettledCurrent(settled)) return;
        if (inspectorTab === 'history') await loadGitHistory();
        return;
      }

      if (isMutationFailedHook(envelope)) {
        // pre-commit 钩子失败：保留结构化失败 + 原 id 供前端展示「让 AI 修复」按钮；worktreeError 不覆盖。
        clearUnknownMutationLockForKind('commit');
        setHookRepair({
          kind: 'commit',
          hookFailure: envelope.hookFailure,
          clientOperationId: envelope.clientOperationId,
        });
        setWorktreeError(null);
        return;
      }

      if (isMutationUnknown(envelope)) {
        // loadWorktrees 会清 worktreeError，故 unknown 文案在对账结束后再写。
        const confirmed = await reconcileUnknownMutation(
          envelope.clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('commit');
          setWorktreeError(null);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('commit');
          setWorktreeError(translateError('commitWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'commit',
            projectId,
            worktreeId,
            clientOperationId: envelope.clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
      }
    } catch (error) {
      if (!isSettledCurrent(settled)) return;
      clearUnknownMutationLockForKind('commit');
      markRequestFailure(projectId, error);
      await loadWorktrees(projectId);
      if (!isSettledCurrent(settled)) return;
      if (inspectorTab === 'history') await loadGitHistory();
      if (!isSettledCurrent(settled)) return;
      setWorktreeError(
        displayErrorMessage(error, translateError('commitWorktree'), desktopUnavailableMessage),
      );
    } finally {
      if (isSettledCurrent(settled)) {
        setWorktreeBusy(null);
      }
    }
  }, [
    beginMutationOperation,
    clearUnknownMutationLockForKind,
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    isMutationKindAllowedUnderUnknownLock,
    isSettledCurrent,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    reconcileUnknownMutation,
    remoteWriteDisabled,
    resolveClientOperationId,
    translateError,
    unknownMutationLock,
    api.worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   failedHook 之后用户点「让 AI 修复」时调用：在该 worktree 终端启动 Claude agent 修复钩子根因。
   *   修复不消耗原 failedHook 的 clientOperationId（保留供前端「重试」入口），agent 完成后用户手动重试。
   *
   * Code Logic（这个函数做什么）:
   *   校验 active project/worktree + 待修复上下文；调 workbenchApi.worktrees.repairHookFailure；
   *   成功后聚焦新 terminal session（terminalBridge.focusSession）、保留 hookRepair + 写入 terminalSessionId；
   *   失败保留 hookRepair 并经 markRequestFailure 上报（让用户可重试 repair）。
   */
  const handleRepairHookFailure = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    if (!hookRepair) return;
    const settled = beginMutationOperation(projectId, worktreeId);
    try {
      setWorktreeBusy('commit');
      setWorktreeError(null);
      const repair = await api.worktrees.repairHookFailure(
        worktreeId,
        hookRepair.hookFailure,
      );
      if (!isSettledCurrent(settled)) return;
      void terminalBridge.focusSession(repair.terminalSessionId);
      setHookRepair({ ...hookRepair, terminalSessionId: repair.terminalSessionId });
    } catch (error) {
      if (!isSettledCurrent(settled)) return;
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('commitWorktree'), desktopUnavailableMessage),
      );
    } finally {
      if (isSettledCurrent(settled)) {
        setWorktreeBusy(null);
      }
    }
  }, [
    activeWorktreeIdRef,
    activeProjectIdRef,
    hookRepair,
    remoteWriteDisabled,
    terminalBridge,
    beginMutationOperation,
    isSettledCurrent,
    markRequestFailure,
    displayErrorMessage,
    translateError,
    desktopUnavailableMessage,
    setWorktreeBusy,
    setWorktreeError,
    api.worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户推送当前 worktree 分支到 upstream；推送后刷新 worktree 状态与 Git 历史（如当前在 history tab）。
   *
   * Code Logic（这个函数做什么）:
   *   与 handleCommitWorktree 相同 envelope/context 守卫；失败路径不强制 loadGitHistory。
   */
  const handlePushWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    if (!isMutationKindAllowedUnderUnknownLock('push')) return;
    const settled = beginMutationOperation(projectId, worktreeId);
    const clientOperationId = resolveClientOperationId(
      'push',
      projectId,
      worktreeId,
      unknownMutationLock,
    );
    try {
      setWorktreeBusy('push');
      setWorktreeError(null);
      if (
        unknownMutationLock
        && unknownMutationLock.kind === 'push'
        && unknownMutationLock.clientOperationId === clientOperationId
      ) {
        const confirmed = await reconcileUnknownMutation(
          clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('push');
          setWorktreeError(null);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('push');
          setWorktreeError(translateError('pushWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'push',
            projectId,
            worktreeId,
            clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
        return;
      }

      const envelope = await api.worktrees.push(worktreeId, clientOperationId);
      if (!isSettledCurrent(settled)) return;

      if (isMutationSucceeded(envelope)) {
        clearUnknownMutationLockForKind('push');
        // 成功 push 清空待修复上下文。
        setHookRepair(null);
        invalidateWorktreeListRequests(projectId);
        invalidateGitHistoryRequests(projectId, worktreeId);
        await loadWorktrees(projectId);
        if (!isSettledCurrent(settled)) return;
        if (inspectorTab === 'history') await loadGitHistory();
        return;
      }

      if (isMutationFailedHook(envelope)) {
        // pre-push 钩子失败：保留结构化失败 + 原 id 供前端展示「让 AI 修复」按钮；worktreeError 不覆盖。
        clearUnknownMutationLockForKind('push');
        setHookRepair({
          kind: 'push',
          hookFailure: envelope.hookFailure,
          clientOperationId: envelope.clientOperationId,
        });
        setWorktreeError(null);
        return;
      }

      if (isMutationUnknown(envelope)) {
        const confirmed = await reconcileUnknownMutation(
          envelope.clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('push');
          setWorktreeError(null);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('push');
          setWorktreeError(translateError('pushWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'push',
            projectId,
            worktreeId,
            clientOperationId: envelope.clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
      }
    } catch (error) {
      if (!isSettledCurrent(settled)) return;
      clearUnknownMutationLockForKind('push');
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('pushWorktree'), desktopUnavailableMessage),
      );
    } finally {
      if (isSettledCurrent(settled)) {
        setWorktreeBusy(null);
      }
    }
  }, [
    beginMutationOperation,
    clearUnknownMutationLockForKind,
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    isMutationKindAllowedUnderUnknownLock,
    isSettledCurrent,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    reconcileUnknownMutation,
    remoteWriteDisabled,
    resolveClientOperationId,
    translateError,
    unknownMutationLock,
    api.worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   修复上下文面板给用户三个出口：「让 AI 修复 / 重试 / 忽略」。前两个已有 handler；
   *   「忽略」是用户已决定不修也不重试的纯本地动作——清空 hookRepair + 不发起任何 IPC。
   *   避免用户卡在 stale failedHook 面板、又必须强行 commit/push 才能脱离。
   *
   * Code Logic（这个函数做什么）:
   *   hookRepair 已为 null 时 no-op；否则 setHookRepair(null)。不调 workbenchApi、不动 busy/error。
   */
  const handleDismissHookFailure = useCallback(async (): Promise<void> => {
    if (!hookRepair) return;
    setHookRepair(null);
  }, [hookRepair]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   修复完成后用户点「重试 commit/push」：清空 hookRepair 并复用对应 handler 走 fresh clientOperationId 路径。
   *
   * Code Logic（这个函数做什么）:
   *   按 hookRepair.kind 调 handleCommitWorktree / handlePushWorktree；handler 内部 resolveClientOperationId
   *   因 hookRepair 已清空而 mint fresh id（除非持有 unknownMutationLock，否则走 ledger Fresh 路径）。
   */
  const handleRetryAfterRepair = useCallback(async (): Promise<void> => {
    if (!hookRepair) return;
    const kind = hookRepair.kind;
    setHookRepair(null);
    if (kind === 'commit') {
      await handleCommitWorktree();
    } else {
      await handlePushWorktree();
    }
  }, [hookRepair, handleCommitWorktree, handlePushWorktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   功能 worktree 一键合回主工作区，或主工作区把可收集分支合回 home；merge 会经过多个后端阶段。
   *
   * Code Logic（这个函数做什么）:
   *   1. 用 canMergeWorktree 校验（主工作区需 canCollectMerge）、remoteWriteDisabled、用户确认；
   *   2. mint operation key + clientOperationId；初始化 merge 阶段；
   *   3. envelope succeeded 且 current → 写阶段/刷新 sessions/buffers；
   *   4. unknown 且 current → 对账、不盲重放；catch/finally 均 isSettledCurrent 守卫。
   */
  const handleMergeWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    if (!isMutationKindAllowedUnderUnknownLock('merge')) return;
    const current = worktrees.find((worktree) => worktree.id === worktreeId);
    if (!current || !canMergeWorktree(current, worktreeBusy, unknownMutationLock)) return;
    const confirmed = current.isMain
      ? translateWorktreeMessage('mergeCollectConfirm', {
          home: current.homeBranch ?? 'main',
          names: current.collectibleBranches.join(', '),
          count: current.collectibleBranches.length,
        })
      : translateWorktreeMessage('mergeConfirm', { name: current.name });
    if (!confirmAction(confirmed)) {
      return;
    }
    beginMutationOperation(projectId, worktreeId);
    const clientOperationId = resolveClientOperationId(
      'merge',
      projectId,
      worktreeId,
      unknownMutationLock,
    );
    const pendingMerge: WorkbenchPendingMergeOperation = {
      projectId,
      worktreeId,
      clientOperationId,
    };
    try {
      clearMergeStageDismissTimer(projectId);
      pendingMergeOperationsRef.current[projectId] = pendingMerge;
      setWorktreeBusy('merge');
      setWorktreeError(null);
      updateProjectMergeProgress(projectId, () => ({
        worktreeId,
        stages: formatWorkbenchMergeStages([
          {
            id: INITIAL_MERGE_STAGE_ID,
            status: 'running',
            message: translateWorktreeMessage('checkSourceMessage'),
          },
        ]),
      }));

      if (
        unknownMutationLock
        && unknownMutationLock.kind === 'merge'
        && unknownMutationLock.clientOperationId === clientOperationId
      ) {
        const confirmed = await reconcileUnknownMutation(
          clientOperationId,
          projectId,
          worktreeId,
          createOperationKey(
            activeProjectIdRef.current ?? '',
            activeWorktreeIdRef.current,
            mutationSequenceRef.current,
          ),
        );
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('merge');
          setWorktreeError(null);
          await terminalBridge.loadSessions(projectId);
          terminalBridge.clearBuffersForWorktree(worktreeId);
          void refreshProjectSessionStats(projectId);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('merge');
          setWorktreeError(translateError('mergeWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'merge',
            projectId,
            worktreeId,
            clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
        return;
      }

      const envelope = await api.worktrees.merge(worktreeId, clientOperationId);

      if (isMutationSucceeded(envelope)) {
        clearUnknownMutationLockForKind('merge');
        const finalStages = formatWorkbenchMergeStages(envelope.value.stages);
        updateProjectMergeProgress(projectId, () => ({ worktreeId, stages: finalStages }));
        if (shouldAutoDismissMergeStages(finalStages)) {
          scheduleMergeStagePanelDismiss(projectId, worktreeId);
        }
        invalidateWorktreeListRequests(projectId);
        invalidateGitHistoryRequests(projectId, worktreeId);
        if (activeProjectIdRef.current === projectId) {
          await loadWorktrees(projectId);
          await terminalBridge.loadSessions(projectId);
          terminalBridge.clearBuffersForWorktree(worktreeId);
          void refreshProjectSessionStats(projectId);
        }
        if (activeProjectIdRef.current === projectId && inspectorTab === 'history') {
          await loadGitHistory();
        }
        return;
      }

      if (isMutationUnknown(envelope)) {
        const confirmed = await reconcileUnknownMutation(
          envelope.clientOperationId,
          projectId,
          worktreeId,
          createOperationKey(
            activeProjectIdRef.current ?? '',
            activeWorktreeIdRef.current,
            mutationSequenceRef.current,
          ),
        );
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('merge');
          setWorktreeError(null);
          await terminalBridge.loadSessions(projectId);
          terminalBridge.clearBuffersForWorktree(worktreeId);
          void refreshProjectSessionStats(projectId);
          if (inspectorTab === 'history') await loadGitHistory();
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('merge');
          setWorktreeError(translateError('mergeWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'merge',
            projectId,
            worktreeId,
            clientOperationId: envelope.clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
      }
    } catch (error) {
      clearUnknownMutationLockForKind('merge');
      if (activeProjectIdRef.current === projectId) {
        markRequestFailure(projectId, error);
      }
      const message = displayErrorMessage(
        error,
        translateError('mergeWorktree'),
        desktopUnavailableMessage,
      );
      clearMergeStageDismissTimer(projectId);
      updateProjectMergeProgress(projectId, (currentProgress) => {
        const formatted = formatWorkbenchMergeStages(currentProgress?.stages ?? []);
        if (formatted.some((stage) => stage.status === 'failed')) {
          return { worktreeId, stages: formatted };
        }
        const failedStage = formatted.find((stage) => stage.status === 'running') ?? formatted[0];
        return {
          worktreeId,
          stages: formatted.map((stage) =>
            stage.id === failedStage?.id ? { ...stage, status: 'failed', message } : stage,
          ),
        };
      });
      if (activeProjectIdRef.current === projectId) {
        await loadWorktrees(projectId);
        await terminalBridge.loadSessions(projectId);
        setWorktreeError(message);
      }
    } finally {
      const currentPending = pendingMergeOperationsRef.current[projectId];
      if (currentPending?.clientOperationId === clientOperationId) {
        delete pendingMergeOperationsRef.current[projectId];
        if (activeProjectIdRef.current === projectId) setWorktreeBusy(null);
      }
    }
  }, [
    beginMutationOperation,
    clearUnknownMutationLockForKind,
    clearMergeStageDismissTimer,
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    isMutationKindAllowedUnderUnknownLock,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    confirmAction,
    reconcileUnknownMutation,
    refreshProjectSessionStats,
    remoteWriteDisabled,
    resolveClientOperationId,
    scheduleMergeStagePanelDismiss,
    terminalBridge,
    translateError,
    translateWorktreeMessage,
    unknownMutationLock,
    updateProjectMergeProgress,
    worktreeBusy,
    worktrees,
    api.worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户移除非主 worktree；由调用方在 chip 的 x 按钮触发，按入参 worktreeId 精确删除目标 worktree，
   *   移除后切到剩余 worktree 并刷新列表。
   *
   * Code Logic（这个函数做什么）:
   *   mint/reuse operation key + clientOperationId；envelope succeeded 且 current 才切 active/刷新；
   *   unknown 对账并保留 same-id 锁；catch/finally 均 isSettledCurrent 守卫。
   *   用户确认由调用方通过共享 Dialog 原语完成，本函数不再持有同步 confirm 阻塞。
   *
   *   后端会关闭该 worktree 下属 terminal window；成功后本函数经 terminalBridge
   *   loadSessions + clearBuffersForWorktree 同步前端 tab/buffer，不得直接 mutate terminal state。
   */
  const handleRemoveWorktree = useCallback(async (worktreeId: string): Promise<void> => {
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    if (!isMutationKindAllowedUnderUnknownLock('remove')) return;
    const current = worktrees.find((worktree) => worktree.id === worktreeId);
    if (!current || current.isMain) return;
    const settled = beginMutationOperation(projectId, worktreeId);
    const clientOperationId = resolveClientOperationId(
      'remove',
      projectId,
      worktreeId,
      unknownMutationLock,
    );
    try {
      setWorktreeBusy('remove');
      setWorktreeError(null);

      if (
        unknownMutationLock
        && unknownMutationLock.kind === 'remove'
        && unknownMutationLock.clientOperationId === clientOperationId
      ) {
        const confirmed = await reconcileUnknownMutation(
          clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('remove');
          setWorktreeError(null);
          setWorktreeBusy(null);
          if (activeWorktreeIdRef.current === worktreeId) {
            const next = worktrees.find((worktree) => worktree.id !== worktreeId);
            setActiveWorktreeId(next?.id ?? null);
          }
          await terminalBridge.loadSessions(projectId);
          terminalBridge.clearBuffersForWorktree(worktreeId);
          void refreshProjectSessionStats(projectId);
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('remove');
          setWorktreeError(translateError('removeWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'remove',
            projectId,
            worktreeId,
            clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
        return;
      }

      const envelope = await api.worktrees.remove(
        worktreeId,
        false,
        clientOperationId,
      );
      if (!isSettledCurrent(settled)) return;

      if (isMutationSucceeded(envelope)) {
        clearUnknownMutationLockForKind('remove');
        // 先清 busy，再切 active worktree：切 active 会使 settled.worktreeId 不再匹配 current。
        setWorktreeBusy(null);
        if (activeWorktreeIdRef.current === worktreeId) {
          const next = worktrees.find((worktree) => worktree.id !== worktreeId);
          setActiveWorktreeId(next?.id ?? null);
        }
        invalidateWorktreeListRequests(projectId);
        invalidateGitHistoryRequests(projectId, worktreeId);
        await loadWorktrees(projectId);
        await terminalBridge.loadSessions(projectId);
        terminalBridge.clearBuffersForWorktree(worktreeId);
        void refreshProjectSessionStats(projectId);
        return;
      }

      if (isMutationUnknown(envelope)) {
        const confirmed = await reconcileUnknownMutation(
          envelope.clientOperationId,
          projectId,
          worktreeId,
          settled,
        );
        if (!isSettledCurrent(settled)) return;
        if (confirmed === 'confirmedSucceeded') {
          clearUnknownMutationLockForKind('remove');
          setWorktreeError(null);
          setWorktreeBusy(null);
          if (activeWorktreeIdRef.current === worktreeId) {
            const next = worktrees.find((worktree) => worktree.id !== worktreeId);
            setActiveWorktreeId(next?.id ?? null);
          }
          await terminalBridge.loadSessions(projectId);
          terminalBridge.clearBuffersForWorktree(worktreeId);
          void refreshProjectSessionStats(projectId);
        } else if (confirmed === 'confirmedFailed') {
          clearUnknownMutationLockForKind('remove');
          setWorktreeError(translateError('removeWorktree'));
        } else {
          setUnknownMutationLock({
            kind: 'remove',
            projectId,
            worktreeId,
            clientOperationId: envelope.clientOperationId,
          });
          setWorktreeError(translateError('mutationUnknown'));
        }
      }
    } catch (error) {
      if (!isSettledCurrent(settled)) return;
      clearUnknownMutationLockForKind('remove');
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('removeWorktree'), desktopUnavailableMessage),
      );
    } finally {
      if (isSettledCurrent(settled)) {
        setWorktreeBusy(null);
      }
    }
  }, [
    beginMutationOperation,
    clearUnknownMutationLockForKind,
    desktopUnavailableMessage,
    displayErrorMessage,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    isMutationKindAllowedUnderUnknownLock,
    isSettledCurrent,
    loadWorktrees,
    markRequestFailure,
    confirmAction,
    reconcileUnknownMutation,
    refreshProjectSessionStats,
    remoteWriteDisabled,
    resolveClientOperationId,
    setActiveWorktreeId,
    terminalBridge,
    translateError,
    translateWorktreeMessage,
    unknownMutationLock,
    worktrees,
    api.worktrees,
  ]);

  // Business Logic: merge 在后台运行时用户可切换 project/worktree；事件必须按 payload.projectId 写入缓存，
  // 不能只接收当前项目，否则切换期间的完成事件会永久丢失。每个项目同一时刻只追踪一个 worktree，
  // 同一 stage.id 的最新状态覆盖旧状态。
  // 非 Tauri 环境（普通浏览器调试）跳过 listen 注册，避免底层 invoke 报错。
  //
  // 后端 `merge_workbench_worktree` 命令的 envelope 走 succeeded 分支时会主动 schedule dismiss；
  // 但当 envelope 因 timeout/network 走 unknown / reconcile 路径、或 catch 分支时，cleanup 阶段
  // 的 completed 事件仍可能由后端单独推过来。如果只信任 succeeded 路径，UI 阶段条会永远挂着等用户手动忽略。
  // 这里在 listener 里追加一次兜底：cleanup 推到 completed 且快照满足自动隐藏条件时也 schedule dismiss；
  // scheduleMergeStagePanelDismiss 内部 clearMergeStageDismissTimer 重入安全，与 succeeded 路径不冲突。
  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const mergeUnlisten = listen<WorkbenchMergeProgressEvent>(
      'workbench:merge-progress',
      (event) => {
        const payload = event.payload;
        const nextSnapshot = updateProjectMergeProgress(payload.projectId, (current) => {
          if (current && current.worktreeId !== payload.worktreeId) return current;
          return {
            worktreeId: payload.worktreeId,
            stages: formatWorkbenchMergeStages([
              ...(current?.stages ?? []).filter((stage) => stage.id !== payload.stage.id),
              payload.stage,
            ]),
          };
        });
        if (
          payload.stage.id === 'cleanup'
          && payload.stage.status === 'completed'
          && nextSnapshot
          && shouldAutoDismissMergeStages(nextSnapshot.stages)
        ) {
          scheduleMergeStagePanelDismiss(payload.projectId, payload.worktreeId);
        }
      },
    );
    return () => {
      void mergeUnlisten.then((fn) => fn());
    };
  }, [
    canListenToTauriEvents,
    scheduleMergeStagePanelDismiss,
    updateProjectMergeProgress,
  ]);

  return {
    worktrees,
    worktreeBusy,
    unknownMutationLock,
    worktreeError,
    hookRepair,
    createWorktreeOpen,
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    gitCommits,
    gitHistoryLoading,
    gitHistoryError,
    mergeProgressWorktreeId,
    mergeStages,
    setWorktrees,
    setCreateWorktreeOpen,
    setCreateWorktreeBranchPrefix,
    setCreateWorktreeBranchSuffixDraft,
    setGitCommits,
    setGitHistoryError,
    loadWorktrees,
    loadGitHistory,
    handleOpenCreateWorktree,
    handleCancelCreateWorktree,
    handleCreateWorktree,
    handleCommitWorktree,
    handlePushWorktree,
    handleMergeWorktree,
    handleRemoveWorktree,
    handleRepairHookFailure,
    handleDismissHookFailure,
    handleRetryAfterRepair,
    clearMergeStagePanel,
  };
}
