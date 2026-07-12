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
 *   - 维护 activeProjectIdRef / activeWorktreeIdRef / mergeProgressWorktreeIdRef / mergeStageDismissTimerRef，
 *     让异步回调读取最新值做 stale guard。
 *   - 暴露 loadWorktrees / loadGitHistory / handleOpenCreateWorktree / handleCancelCreateWorktree /
 *     handleCreateWorktree / handleCommitWorktree / handlePushWorktree / handleMergeWorktree /
 *     handleRemoveWorktree / clearMergeStagePanel 操作函数。
 *   - 注册 workbench:merge-progress 事件订阅（按当前 project/worktree 过滤）。
 *
 * 不复制邻接 controller 状态：project / session / file / application / prompt optimizer 状态仍归
 * Workbench.tsx 各自所有。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { workbenchApi } from '@/api/workbench';
import { isLatestRequest } from '../workbenchFiles';
import type {
  WorkbenchGitCommit,
  WorkbenchMergeProgressEvent,
  WorkbenchMergeStage,
  WorkbenchMergeStageId,
  WorkbenchWorktree,
} from '@/lib/types';
import {
  DEFAULT_WORKTREE_BRANCH_PREFIX,
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
  | 'gitHistory';

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
 *   - confirmAction：merge/remove 前的用户确认（页面注入 window.confirm，便于测试）。
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
  inspectorTab: 'files' | 'history';
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  refreshProjectSessionStats: (projectId: string) => void;
  terminalBridge: WorkbenchTerminalBridge;
  displayErrorMessage: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage: string;
  translateError: (key: WorkbenchWorktreeGitErrorKey) => string;
  translateWorktreeMessage: (
    key: 'mergeConfirm' | 'removeConfirm' | 'checkSourceMessage',
    vars?: Record<string, unknown>,
  ) => string;
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
  worktreeError: string | null;
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
  loadWorktrees: (projectId: string) => Promise<void>;
  loadGitHistory: () => Promise<void>;
  handleOpenCreateWorktree: () => void;
  handleCancelCreateWorktree: () => void;
  handleCreateWorktree: () => Promise<void>;
  handleCommitWorktree: () => Promise<void>;
  handlePushWorktree: () => Promise<void>;
  handleMergeWorktree: () => Promise<void>;
  handleRemoveWorktree: () => Promise<void>;
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

  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [worktreeBusy, setWorktreeBusy] = useState<WorktreeBusyKind | null>(null);
  const [worktreeError, setWorktreeError] = useState<string | null>(null);
  const [createWorktreeOpen, setCreateWorktreeOpen] = useState<boolean>(false);
  const [createWorktreeBranchPrefix, setCreateWorktreeBranchPrefix] =
    useState<WorktreeBranchPrefix>(DEFAULT_WORKTREE_BRANCH_PREFIX);
  const [createWorktreeBranchSuffixDraft, setCreateWorktreeBranchSuffixDraft] =
    useState<string>('');
  const [gitCommits, setGitCommits] = useState<WorkbenchGitCommit[]>([]);
  const [gitHistoryLoading, setGitHistoryLoading] = useState<boolean>(false);
  const [gitHistoryError, setGitHistoryError] = useState<string | null>(null);
  const [mergeProgressWorktreeId, setMergeProgressWorktreeId] = useState<string | null>(null);
  const [mergeStages, setMergeStages] = useState<WorkbenchMergeStage[]>([]);

  // Business Logic: 异步加载回调返回时，active project / worktree 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const activeWorktreeIdRef = useRef<string | null>(activeWorktreeId);
  // Business Logic: 用户可能连续发起 merge 或切换项目，旧追踪不能干扰新一轮进度。
  const mergeProgressWorktreeIdRef = useRef<string | null>(null);
  // Business Logic: 成功 merge 后阶段条延迟隐藏；用 ref 持有 timer 以便取消。
  const mergeStageDismissTimerRef = useRef<number | null>(null);
  // Business Logic: 同一 project 的 worktree list 与同一 project/worktree 的 git history 可能并发；
  // 用单调 request seq 丢弃过期响应，避免 create/remove/merge 后被慢速 list 回写旧状态。
  const worktreeListRequestSeqRef = useRef<Record<string, number>>({});
  const gitHistoryRequestSeqRef = useRef<Record<string, number>>({});

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    mergeProgressWorktreeIdRef.current = mergeProgressWorktreeId;
  }, [mergeProgressWorktreeId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可能连续发起 merge 或切换项目，旧的自动隐藏计时器不能误清新一轮进度。
   *
   * Code Logic（这个函数做什么）:
   *   如果存在 merge 阶段条隐藏计时器，则取消并清空 ref。
   */
  const clearMergeStageDismissTimer = useCallback(() => {
    if (mergeStageDismissTimerRef.current === null) return;
    window.clearTimeout(mergeStageDismissTimerRef.current);
    mergeStageDismissTimerRef.current = null;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   项目切换或成功完成后的阶段条应释放 Git 历史区域空间。
   *
   * Code Logic（这个函数做什么）:
   *   取消隐藏计时器，清空当前追踪 worktree 与阶段列表。
   */
  const clearMergeStagePanel = useCallback(() => {
    clearMergeStageDismissTimer();
    mergeProgressWorktreeIdRef.current = null;
    setMergeProgressWorktreeId(null);
    setMergeStages([]);
  }, [clearMergeStageDismissTimer]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   成功 merge 后用户只需要短暂看到完成反馈，不应长期保留状态条占位。
   *
   * Code Logic（这个函数做什么）:
   *   为指定 worktree 安排延迟隐藏；触发时若已经开始追踪别的 worktree，则不清理新状态。
   */
  const scheduleMergeStagePanelDismiss = useCallback(
    (worktreeId: string) => {
      clearMergeStageDismissTimer();
      mergeStageDismissTimerRef.current = window.setTimeout(() => {
        mergeStageDismissTimerRef.current = null;
        if (mergeProgressWorktreeIdRef.current !== worktreeId) return;
        mergeProgressWorktreeIdRef.current = null;
        setMergeProgressWorktreeId(null);
        setMergeStages([]);
      }, MERGE_STAGE_AUTO_DISMISS_MS);
    },
    [clearMergeStageDismissTimer],
  );

  // Business Logic: 与原 Workbench.tsx 行为一致——组件卸载时取消尚未触发的隐藏计时器。
  useEffect(() => clearMergeStageDismissTimer, [clearMergeStageDismissTimer]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   切换项目或刷新时需要重新拉取当前项目的所有 worktree，并按返回结果校正 active worktree。
   *
   * Code Logic（这个函数做什么）:
   *   1. 调用 worktrees.list(projectId)，并通过 isCurrentProject 做 stale guard；
   *   2. 成功时更新 worktrees、保留仍存在的 active worktree（否则回退到第一个），markRequestSuccess；
   *   3. 失败时 markRequestFailure + 展示 worktreeError。
   */
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

  const loadWorktrees = useCallback(
    async (projectId: string): Promise<void> => {
      const requestSeq = (worktreeListRequestSeqRef.current[projectId] ?? 0) + 1;
      worktreeListRequestSeqRef.current[projectId] = requestSeq;
      try {
        setWorktreeError(null);
        const list = await workbenchApi.worktrees.list(projectId);
        if (
          !isCurrentProject(projectId) ||
          !isLatestRequest(worktreeListRequestSeqRef.current[projectId], requestSeq)
        ) {
          return;
        }
        markRequestSuccess(projectId);
        setWorktrees(list);
        // Business Logic: 保留仍存在的 active worktree；否则回退到第一个。读取 ref 拿到最新 active id，
        // 与原 Workbench.tsx 的 functional setState 行为等价。
        const currentActive = activeWorktreeIdRef.current;
        if (currentActive && list.some((worktree) => worktree.id === currentActive)) {
          return;
        }
        setActiveWorktreeId(list[0]?.id ?? null);
      } catch (error) {
        if (
          !isCurrentProject(projectId) ||
          !isLatestRequest(worktreeListRequestSeqRef.current[projectId], requestSeq)
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setWorktreeError(
          displayErrorMessage(error, translateError('worktrees'), desktopUnavailableMessage),
        );
      }
    },
    [isCurrentProject, markRequestSuccess, desktopUnavailableMessage, markRequestFailure, translateError, displayErrorMessage, setActiveWorktreeId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Git 历史 tab 与 commit/push/merge 完成后需要刷新当前 worktree 的提交历史；多 worktree 场景下旧响应不能覆盖。
   *
   * Code Logic（这个函数做什么）:
   *   1. 无 active project 时清空 commits/error/loading 并返回；
   *   2. 调用 git.listCommits(projectId, worktreeId, 30)，project/worktree 切换时丢弃响应；
   *   3. 成功时 setGitCommits + markRequestSuccess；失败时 markRequestFailure + 清空 + 展示 gitHistoryError。
   */
  const loadGitHistory = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      setGitCommits([]);
      setGitHistoryError(null);
      setGitHistoryLoading(false);
      return;
    }
    const worktreeId = activeWorktreeIdRef.current;
    const requestKey = `${projectId}::${worktreeId ?? '__none__'}`;
    const requestSeq = (gitHistoryRequestSeqRef.current[requestKey] ?? 0) + 1;
    gitHistoryRequestSeqRef.current[requestKey] = requestSeq;
    try {
      setGitHistoryLoading(true);
      setGitHistoryError(null);
      const commits = await workbenchApi.git.listCommits(projectId, worktreeId, 30);
      if (
        !isCurrentProject(projectId) ||
        activeWorktreeIdRef.current !== worktreeId ||
        !isLatestRequest(gitHistoryRequestSeqRef.current[requestKey], requestSeq)
      ) {
        return;
      }
      setGitCommits(commits);
      markRequestSuccess(projectId);
    } catch (error) {
      if (
        !isCurrentProject(projectId) ||
        activeWorktreeIdRef.current !== worktreeId ||
        !isLatestRequest(gitHistoryRequestSeqRef.current[requestKey], requestSeq)
      ) {
        return;
      }
      markRequestFailure(projectId, error);
      setGitCommits([]);
      setGitHistoryError(
        displayErrorMessage(error, translateError('gitHistory'), desktopUnavailableMessage),
      );
    } finally {
      if (
        isCurrentProject(projectId) &&
        activeWorktreeIdRef.current === worktreeId &&
        isLatestRequest(gitHistoryRequestSeqRef.current[requestKey], requestSeq)
      ) {
        setGitHistoryLoading(false);
      }
    }
  }, [isCurrentProject, markRequestSuccess, desktopUnavailableMessage, markRequestFailure, translateError, displayErrorMessage]);

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
   *   用户填入分支后缀并提交；先创建 worktree，再通过 terminalBridge.createSessionForWorktree 创建并注册
   *   关联终端 session（不 focus），然后刷新 worktree 列表、切到新 worktree，最后再 focus 该 session。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验 active project、remoteWriteDisabled、composeWorktreeBranchName 非空；
   *   2. setWorktreeBusy('create')，调用 worktrees.create；
   *   3. project 切换则丢弃；否则 terminalBridge.createSessionForWorktree（注册 session，bridge 此时
   *      因新 worktree 尚未 active 而不 focus）；
   *   4. loadWorktrees、setActiveWorktreeId(created.id)，随后显式 focusSession(sessionId)（此时 active
   *      已正确，焦点不会错落在旧 worktree 上下文）；
   *   5. 成功时关闭表单、清空 draft；失败时 markRequestFailure + worktreeError；
   *   6. finally 清空 worktreeBusy。
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
    try {
      setWorktreeBusy('create');
      setWorktreeError(null);
      const created = await workbenchApi.worktrees.create(projectId, branchName, null);
      if (activeProjectIdRef.current !== projectId) return;
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
      if (activeProjectIdRef.current !== projectId) return;
      // Business Logic: create 成功后作废旧 list，防止慢速 worktree list 覆盖新建结果。
      invalidateWorktreeListRequests(projectId);
      await loadWorktrees(projectId);
      if (activeProjectIdRef.current !== projectId) return;
      setActiveWorktreeId(created.id);
      // Business Logic: worktree 已激活为 active 后，再把刚创建的 session 设为焦点。若期间 project 已切走
      // 则跳过（focus 会落到错误上下文）。
      if (createdSessionId && activeProjectIdRef.current === projectId) {
        void terminalBridge.focusSession(createdSessionId);
      }
      setCreateWorktreeOpen(false);
      setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setCreateWorktreeBranchSuffixDraft('');
    } catch (error) {
      if (activeProjectIdRef.current !== projectId) return;
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('createWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    desktopUnavailableMessage,
    displayErrorMessage,
    invalidateWorktreeListRequests,
    loadWorktrees,
    markRequestFailure,
    remoteWriteDisabled,
    setActiveWorktreeId,
    terminalBridge,
    translateError,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交当前 worktree 的全部改动；提交后刷新 worktree 状态与 Git 历史（如当前在 history tab）。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验 activeWorktree、remoteWriteDisabled；
   *   2. setWorktreeBusy('commit')，调用 worktrees.commit(worktreeId, null)；
   *   3. 成功时 loadWorktrees + 按需 loadGitHistory；
   *   4. 失败时 markRequestFailure + loadWorktrees + 按需 loadGitHistory + worktreeError；
   *   5. finally 清空 worktreeBusy。
   */
  const handleCommitWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    try {
      setWorktreeBusy('commit');
      setWorktreeError(null);
      await workbenchApi.worktrees.commit(worktreeId, null);
      invalidateWorktreeListRequests(projectId);
      invalidateGitHistoryRequests(projectId, worktreeId);
      await loadWorktrees(projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      markRequestFailure(projectId, error);
      await loadWorktrees(projectId);
      if (inspectorTab === 'history') await loadGitHistory();
      setWorktreeError(
        displayErrorMessage(error, translateError('commitWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    remoteWriteDisabled,
    translateError,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户推送当前 worktree 分支到 upstream；推送后刷新 worktree 状态与 Git 历史（如当前在 history tab）。
   *
   * Code Logic（这个函数做什么）:
   *   与 handleCommitWorktree 类似，区别是不在失败路径强制 loadGitHistory（与原 Workbench.tsx 行为一致）。
   */
  const handlePushWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    try {
      setWorktreeBusy('push');
      setWorktreeError(null);
      await workbenchApi.worktrees.push(worktreeId);
      invalidateWorktreeListRequests(projectId);
      invalidateGitHistoryRequests(projectId, worktreeId);
      await loadWorktrees(projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('pushWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    remoteWriteDisabled,
    translateError,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在功能 worktree 上一键合回主工作区；merge 会经过多个后端阶段，UI 需要稳定展示进度。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验非主 worktree、remoteWriteDisabled、用户确认；
   *   2. clearMergeStageDismissTimer、setWorktreeBusy('merge')、初始化 checkSource running 阶段；
   *   3. 调用 worktrees.merge，成功时把最终阶段写入、按需 auto-dismiss、loadWorktrees + loadSessions(bridge)、
   *      clearBuffersForWorktree(bridge)、refreshProjectSessionStats、按需 loadGitHistory；
   *   4. 失败时 markRequestFailure、把当前 running 阶段标 failed、loadWorktrees + loadSessions(bridge)、worktreeError；
   *   5. finally 清空 worktreeBusy。
   *
   *   注意：session 刷新与 buffer 清理通过 terminalBridge 显式完成，绝不直接 mutate terminal state。
   */
  const handleMergeWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    // Business Logic: 仅功能 worktree（非 main）允许 merge；主工作区没有合并目标。
    const current = worktrees.find((worktree) => worktree.id === worktreeId);
    if (!current || current.isMain) return;
    if (!confirmAction(translateWorktreeMessage('mergeConfirm', { name: current.name }))) {
      return;
    }
    try {
      clearMergeStageDismissTimer();
      setWorktreeBusy('merge');
      setWorktreeError(null);
      setMergeProgressWorktreeId(worktreeId);
      mergeProgressWorktreeIdRef.current = worktreeId;
      setMergeStages(
        formatWorkbenchMergeStages([
          {
            id: INITIAL_MERGE_STAGE_ID,
            status: 'running',
            message: translateWorktreeMessage('checkSourceMessage'),
          },
        ]),
      );
      const result = await workbenchApi.worktrees.merge(worktreeId);
      const finalStages = formatWorkbenchMergeStages(result.stages);
      setMergeStages(finalStages);
      if (shouldAutoDismissMergeStages(finalStages)) {
        scheduleMergeStagePanelDismiss(worktreeId);
      }
      invalidateWorktreeListRequests(projectId);
      invalidateGitHistoryRequests(projectId, worktreeId);
      await loadWorktrees(projectId);
      await terminalBridge.loadSessions(projectId);
      terminalBridge.clearBuffersForWorktree(worktreeId);
      void refreshProjectSessionStats(projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      markRequestFailure(projectId, error);
      const message = displayErrorMessage(
        error,
        translateError('mergeWorktree'),
        desktopUnavailableMessage,
      );
      clearMergeStageDismissTimer();
      setMergeStages((currentStages) => {
        const formatted = formatWorkbenchMergeStages(currentStages);
        if (formatted.some((stage) => stage.status === 'failed')) return formatted;
        const failedStage = formatted.find((stage) => stage.status === 'running') ?? formatted[0];
        return formatted.map((stage) =>
          stage.id === failedStage?.id ? { ...stage, status: 'failed', message } : stage,
        );
      });
      await loadWorktrees(projectId);
      await terminalBridge.loadSessions(projectId);
      setWorktreeError(message);
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    clearMergeStageDismissTimer,
    desktopUnavailableMessage,
    displayErrorMessage,
    inspectorTab,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    loadGitHistory,
    loadWorktrees,
    markRequestFailure,
    confirmAction,
    refreshProjectSessionStats,
    remoteWriteDisabled,
    scheduleMergeStagePanelDismiss,
    terminalBridge,
    translateError,
    translateWorktreeMessage,
    worktrees,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户移除非主 worktree；移除前需用户确认，移除后切到剩余 worktree 并刷新列表。
   *
   * Code Logic（这个函数做什么）:
   *   1. 校验非主 worktree、remoteWriteDisabled、用户确认；
   *   2. setWorktreeBusy('remove')，调用 worktrees.remove(worktreeId)；
   *   3. 若 active 仍是被移除的 worktree，则切到剩余的第一个；
   *   4. loadWorktrees；失败时 markRequestFailure + worktreeError；
   *   5. finally 清空 worktreeBusy。
   *
   *   注意：remove 不直接清理 terminal session/buffer；后端会在 remove_workbench_worktree 时关闭关联
   *   session，随后页面通过 terminal-status 事件或下一次 loadSessions 同步状态。这与抽取前的原 Workbench.tsx
   *   （eae5bef 行 2244–2275）行为完全一致——原 remove 也只 setActiveWorktreeId + loadWorktrees，不做 buffer/
   *   session 清理（merge 才做）。Codex 二次评审 Finding 3 已确认这不是回归。
   */
  const handleRemoveWorktree = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    if (remoteWriteDisabled) return;
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    const current = worktrees.find((worktree) => worktree.id === worktreeId);
    if (!current || current.isMain) return;
    if (!confirmAction(translateWorktreeMessage('removeConfirm', { name: current.name }))) {
      return;
    }
    try {
      setWorktreeBusy('remove');
      setWorktreeError(null);
      await workbenchApi.worktrees.remove(worktreeId, false);
      if (activeWorktreeIdRef.current === worktreeId) {
        const next = worktrees.find((worktree) => worktree.id !== worktreeId);
        setActiveWorktreeId(next?.id ?? null);
      }
      invalidateWorktreeListRequests(projectId);
      invalidateGitHistoryRequests(projectId, worktreeId);
      await loadWorktrees(projectId);
    } catch (error) {
      markRequestFailure(projectId, error);
      setWorktreeError(
        displayErrorMessage(error, translateError('removeWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    desktopUnavailableMessage,
    displayErrorMessage,
    invalidateGitHistoryRequests,
    invalidateWorktreeListRequests,
    loadWorktrees,
    markRequestFailure,
    confirmAction,
    remoteWriteDisabled,
    setActiveWorktreeId,
    translateError,
    translateWorktreeMessage,
    worktrees,
  ]);

  // Business Logic: 与原 Workbench.tsx 行为一致——监听后端 workbench:merge-progress 事件，按当前 project
  // 过滤；同一时刻只追踪一个 worktree（首个事件吸附 worktreeId，之后不同 worktreeId 的事件忽略）；
  // 同一 stage.id 的最新状态覆盖旧状态。
  // 非 Tauri 环境（普通浏览器调试）跳过 listen 注册，避免底层 invoke 报错。
  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const mergeUnlisten = listen<WorkbenchMergeProgressEvent>(
      'workbench:merge-progress',
      (event) => {
        const payload = event.payload;
        const currentProjectId = activeProjectIdRef.current;
        if (!currentProjectId || payload.projectId !== currentProjectId) return;
        const trackedWorktreeId = mergeProgressWorktreeIdRef.current;
        if (trackedWorktreeId && trackedWorktreeId !== payload.worktreeId) return;
        if (!trackedWorktreeId) {
          mergeProgressWorktreeIdRef.current = payload.worktreeId;
          setMergeProgressWorktreeId(payload.worktreeId);
        }
        setMergeStages((current) =>
          formatWorkbenchMergeStages([
            ...current.filter((stage) => stage.id !== payload.stage.id),
            payload.stage,
          ]),
        );
      },
    );
    return () => {
      void mergeUnlisten.then((fn) => fn());
    };
  }, [canListenToTauriEvents]);

  return {
    worktrees,
    worktreeBusy,
    worktreeError,
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
    clearMergeStagePanel,
  };
}
