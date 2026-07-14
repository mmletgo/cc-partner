/**
 * Workbench 项目域 controller —— 远端离线状态机 + 跨项目请求序列守卫 + 项目级 deep link 选择
 * + 「继续工作」启动摘要（有项目未选中时）。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Workbench 在加载 worktrees / sessions / files / git history 时，会发起多个针对当前 active project 的
 *   异步请求。后端可能返回“远端设备不在线”业务错误，此时页面必须进入只读态阻止用户继续点击必然失败的
 *   远端写操作；远端恢复后下一次成功读请求要清除只读态。同时项目切换瞬间旧响应不能再回写——必须以最新
 *   active project 为准。
 *   另外当 projects 已有但未选中 active 时，需要拉取有界 launch summary；零项目与已选中项目不得请求。
 *   该状态并入本 controller，避免引入第八个页面 controller。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 `remoteOfflineProjectId` 单一权威状态；对外暴露 `remoteProjectOffline` / `remoteWriteDisabled`。
 *   - 维护 `activeProjectIdRef`，在异步回调里读取最新 active project id；暴露 `isCurrentProject(id)`。
 *   - `markRequestFailure(id, err)`：仅当 id 仍是当前 active 远端项目且错误文案匹配离线时记录 offline。
 *   - `markRequestSuccess(id)`：仅当 id 等于当前记录的 offline projectId 时清除（避免误清其他项目）。
 *   - `selectProjectFromDeepLink(id)`：从 projects 中找到目标项目并触发 selectProject；返回是否命中。
 *   - 在 activeProjectId 变化时通过 queueMicrotask 重置 offline state，保持与原 Workbench 重置顺序一致。
 *   - 持有 `launchSummary` 五 section 资源态；仅 `projects.length > 0 && !activeProjectId` 时 fetch，
 *     可见时 ≤15s 轮询；unmount/上下文切换用 sequence + AbortController 语义丢弃；整次失败 mark stale。
 *
 * 不复制邻接 controller 状态：worktree / session / file / application 状态仍归 Workbench.tsx 各自所有。
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import { workbenchApi } from '@/api/workbench';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import {
  isRemoteWorkbenchOfflineError,
  isRemoteWorkbenchProjectOffline,
} from '@/lib/workbenchRemoteProjects';
import type { WorkbenchProject } from '@/lib/types';
import {
  createInitialLaunchSummaryState,
  markLaunchSummaryStaleOnFailure,
  reduceWorkbenchLaunchResults,
  type WorkbenchLaunchSummaryState,
} from '../workbenchLaunchState';

/** launch summary 可见轮询上限（ms），≤15s。 */
export const WORKBENCH_LAUNCH_SUMMARY_POLL_MS = 15_000;

/**
 * controller 输入：窄 API + 回调，避免吞并 Projects context。
 *
 * 字段说明：
 *   - activeProject / activeProjectId / projects：从 WorkbenchProjectsContext 透传，仅用于读取。
 *   - selectProject：项目切换回调（来自 WorkbenchProjectsContext），用于 deep link 触发项目切换。
 */
export interface UseWorkbenchProjectControllerParams {
  activeProject: WorkbenchProject | null;
  activeProjectId: string | null;
  projects: WorkbenchProject[];
  selectProject: (project: WorkbenchProject) => Promise<WorkbenchProject>;
}

/**
 * controller 返回值：项目域权威状态 + 操作函数。
 *
 * 字段语义：
 *   - remoteProjectOffline：当前 active 远端项目是否离线（local 项目永远 false）。
 *   - remoteWriteDisabled：与 remoteProjectOffline 同值；保留独立字段是因为后续 controller 可能在此叠加
 *     其他“只读原因”，Workbench 现有代码也独立使用 remoteWriteDisabled 概念。
 *   - isCurrentProject(id)：异步闭包里判断 projectId 是否仍是当前 active 项目，用于 stale guard。
 *   - markRequestFailure(id, error)：远端读请求失败时调用，按需把当前项目置为离线。
 *   - markRequestSuccess(id)：远端读请求成功时调用，按需清除离线状态。
 *   - selectProjectFromDeepLink(id)：从 deep link 选项目，返回是否命中；命中且未激活时触发 selectProject。
 *   - launchSummary：五 section 独立资源态 + generatedAt；仅 launch 模式有意义。
 *   - refreshLaunchSummary：手动刷新启动摘要（失败保留 stale）。
 */
export interface WorkbenchProjectControllerResult {
  remoteProjectOffline: boolean;
  remoteWriteDisabled: boolean;
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  selectProjectFromDeepLink: (projectId: string) => Promise<boolean>;
  launchSummary: WorkbenchLaunchSummaryState;
  refreshLaunchSummary: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   invoke 失败时需要人类可读 message 写入 error/stale 态。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 提取 Error.message / string，否则 fallback。
 */
function launchErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error) return error;
  return fallback;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有 remoteOfflineProjectId state；
 *   2. 用 ref 跟踪 activeProjectId，让异步回调读到最新值；
 *   3. 注册 activeProjectId 变化的 queueMicrotask 重置副作用（与 Workbench 原重置顺序一致）；
 *   4. 在有项目未选中时拉取/轮询 launch summary，失败 mark stale；
 *   5. 暴露稳定的操作函数（useCallback + ref 输入），便于 Workbench 在多个 load 函数里复用。
 */
export function useWorkbenchProjectController(
  params: UseWorkbenchProjectControllerParams,
): WorkbenchProjectControllerResult {
  const { activeProject, activeProjectId, projects, selectProject } = params;

  const [remoteOfflineProjectId, setRemoteOfflineProjectId] = useState<string | null>(null);
  const [launchSummary, setLaunchSummary] = useState<WorkbenchLaunchSummaryState>(
    createInitialLaunchSummaryState,
  );

  // Business Logic: 异步加载回调返回时，active project 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  // Business Logic: 与原 Workbench.tsx 行为保持一致——active project 变化后用 queueMicrotask 异步清空离线
  // 状态，避免在新项目首个 effect 阶段就立刻把上一项目的 offline 标记抹掉。
  // Code Logic: 不直接同步清空是为了兼容原 effect 顺序；测试和 characterization 都依赖这个时序。
  useEffect(() => {
    queueMicrotask(() => {
      setRemoteOfflineProjectId(null);
    });
  }, [activeProjectId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   远端项目一旦发现设备离线，页面需要进入只读/不可写状态，避免用户继续点击必然失败的远端写操作。
   *
   * Code Logic（这个函数做什么）:
   *   只在当前 active project 仍是同一个 remote project 且错误包含后端离线文案时，记录离线 projectId。
   */
  const markRequestFailure = useCallback(
    (projectId: string, error: unknown) => {
      if (activeProjectIdRef.current !== projectId) return;
      if (activeProject?.id !== projectId || activeProject.kind !== 'remote') return;
      if (!isRemoteWorkbenchOfflineError(error)) return;
      setRemoteOfflineProjectId(projectId);
    },
    [activeProject],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   远端设备恢复在线并有请求成功后，应恢复当前项目的可写操作。
   *
   * Code Logic（这个函数做什么）:
   *   仅当成功请求对应当前记录的离线 projectId 时清空状态，避免误清其他项目的离线提示。
   */
  const markRequestSuccess = useCallback((projectId: string) => {
    setRemoteOfflineProjectId((current) => (current === projectId ? null : current));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench 多个 load 函数在 await 之后需要判断“响应回来时当前项目是否仍是同一项目”，
   *   暴露一个窄函数比让消费方读 ref 更不容易被滥用。
   *
   * Code Logic（这个函数做什么）:
   *   比较 projectId 与 ref 中最新的 activeProjectId。
   */
  const isCurrentProject = useCallback((projectId: string): boolean => {
    return activeProjectIdRef.current === projectId;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Orchestrator 通过 deep link 把用户带回任务关联的项目；Workbench 需要在项目未激活时自动切换。
   *
   * Code Logic（这个函数做什么）:
   *   若目标项目已在 projects 列表中，则触发 selectProject（当前已激活则跳过实际调用），并返回 true；
   *   否则返回 false。返回 Promise 以便调用方按需等待切换完成。
   */
  const selectProjectFromDeepLink = useCallback(
    async (projectId: string): Promise<boolean> => {
      if (activeProjectIdRef.current === projectId) return true;
      const targetProject = projects.find((project) => project.id === projectId);
      if (!targetProject) return false;
      await selectProject(targetProject);
      return true;
    },
    [projects, selectProject],
  );

  // ---- launch summary -------------------------------------------------------

  const projectsCount = projects.length;
  const launchFetchEnabled = projectsCount > 0 && !activeProjectId;

  // 用 sequence 代替 AbortController 取消 IPC；signal 仍用于文档可见语义与测试断言入口。
  const launchFetchSeqRef = useRef(0);
  const launchAbortRef = useRef<AbortController | null>(null);
  const launchFetchEnabledRef = useRef(launchFetchEnabled);
  useEffect(() => {
    launchFetchEnabledRef.current = launchFetchEnabled;
  }, [launchFetchEnabled]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   启动摘要只在「有项目未选中」时拉取；成功按 section 归约，整次失败 mark stale 保留缓存。
   *
   * Code Logic（这个函数做什么）:
   *   递增 sequence；调用 workbenchApi.getLaunchSummary；过期/abort 丢弃结果。
   */
  const fetchLaunchSummary = useCallback(async (): Promise<void> => {
    if (!launchFetchEnabledRef.current) return;

    const seq = launchFetchSeqRef.current + 1;
    launchFetchSeqRef.current = seq;
    const abort = new AbortController();
    launchAbortRef.current?.abort();
    launchAbortRef.current = abort;

    try {
      const wire = await workbenchApi.getLaunchSummary();
      if (abort.signal.aborted || launchFetchSeqRef.current !== seq) return;
      if (!launchFetchEnabledRef.current) return;
      setLaunchSummary((previous) => reduceWorkbenchLaunchResults(previous, wire));
    } catch (error) {
      if (abort.signal.aborted || launchFetchSeqRef.current !== seq) return;
      if (!launchFetchEnabledRef.current) return;
      const message = launchErrorMessage(error, 'launch summary failed');
      setLaunchSummary((previous) => markLaunchSummaryStaleOnFailure(previous, message));
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户手动刷新启动摘要，与轮询共享同一失败/stale 语义。
   *
   * Code Logic（这个函数做什么）:
   *   委托 fetchLaunchSummary。
   */
  const refreshLaunchSummary = useCallback(async (): Promise<void> => {
    await fetchLaunchSummary();
  }, [fetchLaunchSummary]);

  // 进入 launch 模式：重置 loading 并立即拉取；离开时 abort + 作废 sequence。
  useEffect(() => {
    if (!launchFetchEnabled) {
      launchFetchSeqRef.current += 1;
      launchAbortRef.current?.abort();
      launchAbortRef.current = null;
      return;
    }

    // 进入 continue-working 模式时重置 section 状态并立即拉取
    // eslint-disable-next-line react-hooks/set-state-in-effect -- intentional mode-enter bootstrap
    setLaunchSummary(createInitialLaunchSummaryState());
    void fetchLaunchSummary();

    return () => {
      launchFetchSeqRef.current += 1;
      launchAbortRef.current?.abort();
      launchAbortRef.current = null;
    };
  }, [launchFetchEnabled, fetchLaunchSummary]);

  // 可见时 ≤15s 轮询；hidden 暂停；不立即再跑（进入模式 effect 已跑）。
  useVisibilityPolling(fetchLaunchSummary, {
    intervalMs: WORKBENCH_LAUNCH_SUMMARY_POLL_MS,
    enabled: launchFetchEnabled,
    runImmediately: false,
    refreshOnVisible: true,
  });

  const remoteProjectOffline = isRemoteWorkbenchProjectOffline(
    activeProject,
    remoteOfflineProjectId,
  );
  const remoteWriteDisabled = remoteProjectOffline;

  return {
    remoteProjectOffline,
    remoteWriteDisabled,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    selectProjectFromDeepLink,
    launchSummary,
    refreshLaunchSummary,
  };
}
