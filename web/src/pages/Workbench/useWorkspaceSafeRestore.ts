/**
 * Workspace safe restore 窄 bridge hook（非第 8 个业务 controller）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 启动时 preflight+apply 与 selection autosave 需要挂在现有页面上，
 *   但不得再增页面级业务 controller；抽出 hook 控制 Workbench.tsx 行数。
 *
 * Code Logic（这个模块做什么）:
 *   封装 autosave coordinator、启动 restore、snapshot 对话框状态；
 *   apply 窗口内通过 suppressContextResetRef 禁止 project/worktree effect 清掉 selection。
 *
 *   初始 restore 路径会通过 `forceTerminalWorkspaceView` 强制把 `workspaceView` 写为
 *   `'terminal'`，即使 plan 中保存的是 files / browser；命名 snapshot apply 不传该选项，
 *   默认尊重快照中的 `workspaceView`。这是「打开项目默认进终端」的产品诉求：
 *   首次会话内首次打开项目总进终端,之后用户在会话内的手动选择不被回滚,
 *   命名 snapshot 是用户显式 apply,应当原样还原。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { workbenchApi } from '@/api/workbench';
import {
  WorkspaceLayoutAutosaveCoordinator,
  type InspectorTab,
  type WorkspaceLayout,
  type WorkspaceView,
} from './workspaceLayout';
import {
  applyWorkspaceRestorePlan,
  type WorkspaceRestorePlan,
  type WorkspaceRestoreSummary,
} from './workspaceRestore';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';

export interface UseWorkspaceSafeRestoreParams {
  projectsLoading: boolean;
  projectsLength: number;
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  activeSessionId: string | null;
  workspaceView: WorkbenchFileWorkspaceView;
  inspectorTab: WorkbenchInspectorTab;
  /** 当前浏览器预览 target（loopback URL）；无预览时 null。 */
  browserTargetUrl: string | null;
  dirtyEditor: boolean;
  activeProjectIdRef: React.MutableRefObject<string | null>;
  activeWorktreeIdRef: React.MutableRefObject<string | null>;
  selectProjectFromDeepLink: (projectId: string) => Promise<boolean>;
  setActiveWorktreeId: (id: string | null) => void;
  focusSession: (sessionId: string) => Promise<boolean> | Promise<void> | void;
  setWorkspaceView: (view: WorkbenchFileWorkspaceView) => void;
  setInspectorTab: (tab: WorkbenchInspectorTab) => void;
  setBrowserTargetUrl: (url: string | null) => void;
}

export interface UseWorkspaceSafeRestoreResult {
  restoreSummary: WorkspaceRestoreSummary | null;
  dismissRestoreNotice: () => void;
  snapshotOpen: boolean;
  setSnapshotOpen: (open: boolean) => void;
  namedSnapshots: WorkspaceLayout[];
  openSnapshotDialog: () => void;
  saveNamedSnapshot: (name: string) => Promise<void>;
  applyNamedSnapshot: (layoutId: string) => Promise<void>;
  deleteNamedSnapshot: (layoutId: string) => Promise<void>;
  /**
   * apply 进行中为 true：Workbench project/worktree effect 不得清 worktree / 强制 terminal，
   * 避免与 restore 顺序竞态。
   */
  suppressContextResetRef: React.MutableRefObject<boolean>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI inspector 仅 files|history，layout 模型含 git|automation，持久化不得折叠丢枚举。
 *
 * Code Logic（这个函数做什么）:
 *   WorkbenchInspectorTab → 完整 InspectorTab（1:1 映射 files/history）。
 */
function toLayoutInspectorTab(tab: WorkbenchInspectorTab): InspectorTab {
  return tab;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   恢复 layout 中的完整 inspector 枚举到当前 UI 能力。
 *
 * Code Logic（这个函数做什么）:
 *   history|git → history；files|automation → files。
 */
function fromLayoutInspectorTab(tab: InspectorTab): WorkbenchInspectorTab {
  if (tab === 'history' || tab === 'git') return 'history';
  return 'files';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   server apply 可能把 failed safeAttach 改写为 skip；UI 必须用 post-apply actions。
 *
 * Code Logic（这个函数做什么）:
 *   用 apply 返回的 actions/status 覆盖 plan，保留 layout 解析字段。
 */
function mergeAppliedPlan(
  plan: WorkspaceRestorePlan,
  applied: {
    restoreId: string;
    status: WorkspaceRestorePlan['status'] | string;
    restoredCount: number;
    skippedCount: number;
    actions?: WorkspaceRestorePlan['actions'];
  },
): WorkspaceRestorePlan {
  if (!applied.actions || applied.actions.length === 0) {
    return plan;
  }
  const status = applied.status as WorkspaceRestorePlan['status'];
  return {
    ...plan,
    restoreId: applied.restoreId || plan.restoreId,
    status:
      status === 'complete' ||
      status === 'partial' ||
      status === 'offline' ||
      status === 'empty'
        ? status
        : plan.status,
    actions: applied.actions,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 需要零配置 restore/autosave，但不增加第 8 controller。
 *
 * Code Logic（这个函数做什么）:
 *   挂载 autosave；projects 就绪后 preflight+server apply+UI selection；snapshot CRUD 状态。
 */
export function useWorkspaceSafeRestore(
  params: UseWorkspaceSafeRestoreParams,
): UseWorkspaceSafeRestoreResult {
  const {
    projectsLoading,
    projectsLength,
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
    browserTargetUrl,
    dirtyEditor,
    activeProjectIdRef,
    activeWorktreeIdRef,
    selectProjectFromDeepLink,
    setActiveWorktreeId,
    focusSession,
    setWorkspaceView,
    setInspectorTab,
    setBrowserTargetUrl,
  } = params;

  const [restoreSummary, setRestoreSummary] = useState<WorkspaceRestoreSummary | null>(null);
  const [snapshotOpen, setSnapshotOpen] = useState(false);
  const [namedSnapshots, setNamedSnapshots] = useState<WorkspaceLayout[]>([]);
  const restoreRanRef = useRef(false);
  const suppressContextResetRef = useRef(false);
  const layoutAutosaveRef = useRef<WorkspaceLayoutAutosaveCoordinator | null>(null);
  const selectionRef = useRef({
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
    browserTargetUrl,
  });

  // selection 仅供 autosave/restore 异步回调读取，不得在 render 中写 ref
  useEffect(() => {
    selectionRef.current = {
      activeProjectId,
      activeWorktreeId,
      activeSessionId,
      workspaceView,
      inspectorTab,
      browserTargetUrl,
    };
  }, [
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
    browserTargetUrl,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   启动 restore 与命名 snapshot 共用同一套 UI bridge，避免 named 只 apply 不切界面。
   *
   * Code Logic（这个函数做什么）:
   *   返回绑定当前 controller 的 WorkspaceRestoreBridge 实现。
   *   当 `forceTerminalWorkspaceView=true` 时,bridge 内的 `setWorkspaceView` 会无视
   *   入参,固定写入 `'terminal'`——用于初始 restore 路径（打开项目默认进终端）。
   *   命名 snapshot apply 不传该选项,保持尊重快照中的 `workspaceView`。
   */
  const buildBridge = useCallback(
    (options: { forceTerminalWorkspaceView?: boolean } = {}) => ({
      selectProject: async (projectId: string) => {
        await selectProjectFromDeepLink(projectId);
      },
      selectWorktree: async (worktreeId: string) => {
        setActiveWorktreeId(worktreeId);
      },
      focusSession: async (sessionId: string) => {
        await focusSession(sessionId);
      },
      safeAttachSession: async (sessionId: string) => {
        // server apply 已完成真实 attach；此处只做 UI focus，不创建 shell
        await focusSession(sessionId);
      },
      setWorkspaceView: (view: WorkspaceView) => {
        const target: WorkbenchFileWorkspaceView = options.forceTerminalWorkspaceView
          ? 'terminal'
          : (view as WorkbenchFileWorkspaceView);
        setWorkspaceView(target);
      },
      setInspectorTab: (tab: InspectorTab) => {
        setInspectorTab(fromLayoutInspectorTab(tab));
      },
      restoreBrowserTarget: async (url: string) => {
        if (!activeProjectIdRef.current) return;
        await workbenchApi.browser.createPreview(
          activeProjectIdRef.current,
          activeWorktreeIdRef.current,
          url,
        );
        setBrowserTargetUrl(url);
        // browser 目标由用户显式 apply（命名 snapshot 或 restore browserTarget action），
        // 必须保留 browser view；此处不走 forceTerminalWorkspaceView 强制逻辑。
        setWorkspaceView('browser');
      },
      applySelectionSnapshot: async (snapshot: {
        projectId: string | null;
        worktreeId: string | null;
        sessionId: string | null;
        workspaceView: WorkspaceView;
        browserTargetUrl: string | null;
      }) => {
        if (snapshot.projectId) await selectProjectFromDeepLink(snapshot.projectId);
        setActiveWorktreeId(snapshot.worktreeId);
        if (snapshot.sessionId) await focusSession(snapshot.sessionId);
        // 回滚 previous 时同样保留原始 view,不应用 force-terminal 强制,
        // 避免异常路径误把 UI 锁在 terminal。
        setWorkspaceView(snapshot.workspaceView as WorkbenchFileWorkspaceView);
        setBrowserTargetUrl(snapshot.browserTargetUrl);
      },
    }),
    [
      selectProjectFromDeepLink,
      setActiveWorktreeId,
      focusSession,
      setWorkspaceView,
      setInspectorTab,
      setBrowserTargetUrl,
      activeProjectIdRef,
      activeWorktreeIdRef,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   统一 preflight → server apply → UI apply，失败时不静默继续不安全路径。
   *
   * Code Logic（这个函数做什么）:
   *   suppress context reset → apply server → merge post-apply actions → UI bridge；
   *   apply 失败则跳过 UI 选择并返回 partial summary。
   *   `forceTerminalWorkspaceView=true` 时把选项注入 bridge,实现「首次打开项目强制
   *   terminal」;默认 false 供命名 snapshot apply 保留快照 view。
   */
  const runRestoreWithUi = useCallback(
    async (options: {
      previous: {
        projectId: string | null;
        worktreeId: string | null;
        sessionId: string | null;
        workspaceView: WorkspaceView;
        inspectorTab: InspectorTab;
        browserTargetUrl: string | null;
        dirtyEditor: boolean;
      };
      loadPlan: () => Promise<WorkspaceRestorePlan>;
      /**
       * 初始 restore 路径传 true:bridge 内 setWorkspaceView 强制写入 `'terminal'`,
       * 与 plan 中保存的 `workspaceView` 解耦。命名 snapshot apply 不传,
       * 默认尊重快照中的 `workspaceView`。
       */
      forceTerminalWorkspaceView?: boolean;
    }): Promise<WorkspaceRestoreSummary | null> => {
      suppressContextResetRef.current = true;
      const bridgeOptions = { forceTerminalWorkspaceView: options.forceTerminalWorkspaceView };
      try {
        const plan = await options.loadPlan();
        let appliedPlan = plan;
        try {
          const applied = await workbenchApi.layout.apply(plan);
          appliedPlan = mergeAppliedPlan(plan, applied);
        } catch {
          // apply 失败：不得继续 UI selection 触发 list-restore 误路径；回滚 previous
          await buildBridge(bridgeOptions).applySelectionSnapshot(options.previous);
          return {
            restoreId: plan.restoreId,
            status: 'partial',
            restoredCount: 0,
            skippedCount: plan.actions.length,
            reasons: ['applyFailed'],
            silent: false,
            dirtyEditorPreserved: options.previous.dirtyEditor,
          };
        }
        return await applyWorkspaceRestorePlan({
          previous: options.previous,
          preflight: async () => appliedPlan,
          bridge: buildBridge(bridgeOptions),
        });
      } finally {
        // 让 deferEffect 中的 project/worktree effect 在 suppress 窗口内完成
        await new Promise<void>((resolve) => {
          window.setTimeout(() => {
            suppressContextResetRef.current = false;
            resolve();
          }, 50);
        });
      }
    },
    [buildBridge],
  );

  useEffect(() => {
    const coordinator = new WorkspaceLayoutAutosaveCoordinator({
      save: (draft, expectedRevision) => workbenchApi.layout.save(draft, expectedRevision),
      get: (slotKey) => workbenchApi.layout.get(slotKey),
      select: () => {
        const s = selectionRef.current;
        return {
          projectId: s.activeProjectId,
          activeWorktreeId: s.activeWorktreeId,
          activeSessionId: s.activeSessionId,
          workspaceView: s.workspaceView as WorkspaceView,
          // 完整 enum：不把非 history 折叠丢字段（UI 当前为 files|history，1:1 写入 layout）
          inspectorTab: toLayoutInspectorTab(s.inspectorTab),
          browserTargetUrl: s.browserTargetUrl,
        };
      },
    });
    layoutAutosaveRef.current = coordinator;
    void coordinator.hydrateRevision();
    return () => {
      coordinator.dispose();
      layoutAutosaveRef.current = null;
    };
  }, []);

  useEffect(() => {
    layoutAutosaveRef.current?.notifySelectionChanged();
  }, [
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
    browserTargetUrl,
  ]);

  useEffect(() => {
    if (restoreRanRef.current || projectsLoading || projectsLength === 0) return;
    restoreRanRef.current = true;
    const previous = {
      projectId: selectionRef.current.activeProjectId,
      worktreeId: selectionRef.current.activeWorktreeId,
      sessionId: selectionRef.current.activeSessionId,
      workspaceView: selectionRef.current.workspaceView as WorkspaceView,
      inspectorTab: toLayoutInspectorTab(selectionRef.current.inspectorTab),
      browserTargetUrl: selectionRef.current.browserTargetUrl,
      dirtyEditor,
    };
    void (async () => {
      try {
        const summary = await runRestoreWithUi({
          previous,
          loadPlan: () => workbenchApi.layout.preflight(),
          // 首次打开本会话的项目强制进 terminal,即使 plan 中保存的是 files / browser。
          // 该 effect 在 mount 后 projectsLength > 0 时只跑一次 (restoreRanRef),
          // 因此「只在首次」语义由 restoreRanRef 守门,这里显式传 true 与产品诉求一一对应。
          forceTerminalWorkspaceView: true,
        });
        if (summary && !summary.silent) setRestoreSummary(summary);
      } catch {
        // 无 layout 或 preflight 空：静默
      }
    })();
  }, [
    projectsLoading,
    projectsLength,
    dirtyEditor,
    runRestoreWithUi,
  ]);

  const dismissRestoreNotice = useCallback(() => setRestoreSummary(null), []);

  const openSnapshotDialog = useCallback(() => {
    void workbenchApi.layout.listNamed().then(setNamedSnapshots);
    setSnapshotOpen(true);
  }, []);

  const saveNamedSnapshot = useCallback(
    async (name: string) => {
      if (!activeProjectId) return;
      const slotKey = `named:${crypto.randomUUID()}`;
      await workbenchApi.layout.save(
        {
          slotKey,
          kind: 'named',
          name,
          projectId: activeProjectId,
          activeWorktreeId,
          activeSessionId,
          workspaceView: workspaceView as WorkspaceView,
          inspectorTab: toLayoutInspectorTab(inspectorTab),
          browserTargetUrl,
        },
        null,
      );
      setNamedSnapshots(await workbenchApi.layout.listNamed());
    },
    [
      activeProjectId,
      activeWorktreeId,
      activeSessionId,
      workspaceView,
      inspectorTab,
      browserTargetUrl,
    ],
  );

  const applyNamedSnapshot = useCallback(
    async (layoutId: string) => {
      const previous = {
        projectId: selectionRef.current.activeProjectId,
        worktreeId: selectionRef.current.activeWorktreeId,
        sessionId: selectionRef.current.activeSessionId,
        workspaceView: selectionRef.current.workspaceView as WorkspaceView,
        inspectorTab: toLayoutInspectorTab(selectionRef.current.inspectorTab),
        browserTargetUrl: selectionRef.current.browserTargetUrl,
        dirtyEditor: false,
      };
      try {
        // 命名 snapshot 是用户显式 apply 的工作现场,必须保留快照中的 workspaceView
        // (不传 forceTerminalWorkspaceView = 默认 false)。
        const summary = await runRestoreWithUi({
          previous,
          loadPlan: () => workbenchApi.layout.preflight(null, layoutId),
        });
        if (summary && !summary.silent) setRestoreSummary(summary);
      } finally {
        setSnapshotOpen(false);
      }
    },
    [runRestoreWithUi],
  );

  const deleteNamedSnapshot = useCallback(async (layoutId: string) => {
    await workbenchApi.layout.deleteNamed(layoutId);
    setNamedSnapshots(await workbenchApi.layout.listNamed());
  }, []);

  return {
    restoreSummary,
    dismissRestoreNotice,
    snapshotOpen,
    setSnapshotOpen,
    namedSnapshots,
    openSnapshotDialog,
    saveNamedSnapshot,
    applyNamedSnapshot,
    deleteNamedSnapshot,
    suppressContextResetRef,
  };
}
