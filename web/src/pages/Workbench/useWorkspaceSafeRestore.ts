/**
 * Workspace safe restore 窄 bridge hook（非第 8 个业务 controller）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 启动时 preflight+apply 与 selection autosave 需要挂在现有页面上，
 *   但不得再增页面级业务 controller；抽出 hook 控制 Workbench.tsx 行数。
 *
 * Code Logic（这个模块做什么）:
 *   封装 autosave coordinator、启动 restore、snapshot 对话框状态。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { workbenchApi } from '@/api/workbench';
import {
  WorkspaceLayoutAutosaveCoordinator,
  type WorkspaceLayout,
  type WorkspaceView,
} from './workspaceLayout';
import {
  applyWorkspaceRestorePlan,
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
  dirtyEditor: boolean;
  activeProjectIdRef: React.MutableRefObject<string | null>;
  activeWorktreeIdRef: React.MutableRefObject<string | null>;
  selectProjectFromDeepLink: (projectId: string) => Promise<boolean>;
  setActiveWorktreeId: (id: string | null) => void;
  focusSession: (sessionId: string) => Promise<boolean> | Promise<void> | void;
  setWorkspaceView: (view: WorkbenchFileWorkspaceView) => void;
  setInspectorTab: (tab: WorkbenchInspectorTab) => void;
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
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 需要零配置 restore/autosave，但不增加第 8 controller。
 *
 * Code Logic（这个函数做什么）:
 *   挂载 autosave；projects 就绪后 preflight+apply+UI selection；snapshot CRUD 状态。
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
    dirtyEditor,
    activeProjectIdRef,
    activeWorktreeIdRef,
    selectProjectFromDeepLink,
    setActiveWorktreeId,
    focusSession,
    setWorkspaceView,
    setInspectorTab,
  } = params;

  const [restoreSummary, setRestoreSummary] = useState<WorkspaceRestoreSummary | null>(null);
  const [snapshotOpen, setSnapshotOpen] = useState(false);
  const [namedSnapshots, setNamedSnapshots] = useState<WorkspaceLayout[]>([]);
  const restoreRanRef = useRef(false);
  const layoutAutosaveRef = useRef<WorkspaceLayoutAutosaveCoordinator | null>(null);
  const selectionRef = useRef({
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
  });
  selectionRef.current = {
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
  };

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
          inspectorTab: s.inspectorTab === 'history' ? 'history' : 'files',
          browserTargetUrl: null,
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
  }, [activeProjectId, activeWorktreeId, activeSessionId, workspaceView, inspectorTab]);

  useEffect(() => {
    if (restoreRanRef.current || projectsLoading || projectsLength === 0) return;
    restoreRanRef.current = true;
    const previous = {
      projectId: selectionRef.current.activeProjectId,
      worktreeId: selectionRef.current.activeWorktreeId,
      sessionId: selectionRef.current.activeSessionId,
      workspaceView: selectionRef.current.workspaceView as WorkspaceView,
      inspectorTab:
        selectionRef.current.inspectorTab === 'history'
          ? ('history' as const)
          : ('files' as const),
      browserTargetUrl: null,
      dirtyEditor,
    };
    void (async () => {
      try {
        const plan = await workbenchApi.layout.preflight();
        await workbenchApi.layout.apply(plan).catch(() => undefined);
        const summary = await applyWorkspaceRestorePlan({
          previous,
          preflight: async () => plan,
          bridge: {
            selectProject: async (projectId) => {
              await selectProjectFromDeepLink(projectId);
            },
            selectWorktree: async (worktreeId) => {
              setActiveWorktreeId(worktreeId);
            },
            focusSession: async (sessionId) => {
              await focusSession(sessionId);
            },
            safeAttachSession: async (sessionId) => {
              await focusSession(sessionId);
            },
            setWorkspaceView: (view) => {
              setWorkspaceView(view as WorkbenchFileWorkspaceView);
            },
            setInspectorTab: (tab) => {
              if (tab === 'history' || tab === 'files') {
                setInspectorTab(tab);
              }
            },
            restoreBrowserTarget: async (url) => {
              if (!activeProjectIdRef.current) return;
              await workbenchApi.browser.createPreview(
                activeProjectIdRef.current,
                activeWorktreeIdRef.current,
                url,
              );
              setWorkspaceView('browser');
            },
            applySelectionSnapshot: async (snapshot) => {
              if (snapshot.projectId) await selectProjectFromDeepLink(snapshot.projectId);
              setActiveWorktreeId(snapshot.worktreeId);
              if (snapshot.sessionId) await focusSession(snapshot.sessionId);
              setWorkspaceView(snapshot.workspaceView as WorkbenchFileWorkspaceView);
            },
          },
        });
        if (summary && !summary.silent) setRestoreSummary(summary);
      } catch {
        // 无 layout：静默
      }
    })();
  }, [
    projectsLoading,
    projectsLength,
    dirtyEditor,
    selectProjectFromDeepLink,
    setActiveWorktreeId,
    focusSession,
    setWorkspaceView,
    setInspectorTab,
    activeProjectIdRef,
    activeWorktreeIdRef,
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
          inspectorTab: inspectorTab === 'history' ? 'history' : 'files',
          browserTargetUrl: null,
        },
        null,
      );
      setNamedSnapshots(await workbenchApi.layout.listNamed());
    },
    [activeProjectId, activeWorktreeId, activeSessionId, workspaceView, inspectorTab],
  );

  const applyNamedSnapshot = useCallback(async (layoutId: string) => {
    const plan = await workbenchApi.layout.preflight(null, layoutId);
    await workbenchApi.layout.apply(plan);
    setSnapshotOpen(false);
  }, []);

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
  };
}
