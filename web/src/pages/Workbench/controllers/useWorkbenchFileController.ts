/**
 * Workbench 文件工作区 controller —— 目录/文件树加载 + tab 生命周期 + dirty/save/format +
 * image/CSV/SQLite/HTML/Markdown 模式 + create/rename/delete/copy + project/worktree stale 守卫。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Workbench 文件域是最重的子域之一：它持有文件树根节点、按路径展开的 children cache、展开/选中状态、
 *   loading/error/notice、已打开文件 tabs、active tab、saving 标记、新建/重命名草稿；同时驱动多个
 *   带序号的异步请求序列（list_dir / get_path_info / open_file / save_text / format / preview_sqlite），
 *   每一条都必须做 project + worktree + per-path/per-tab seq 的 stale guard。把这些状态和请求序列集中到
 *   controller，让 Workbench.tsx 只负责调度和渲染，不再自管这些细粒度状态。
 *
 *   重要边界：
 *   - `workspaceView` 与 `automationConsoleOpen` 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
 *     仍归 Workbench.tsx 所有；controller 通过注入的 `requestWorkspaceView` / `requestHideAutomationConsole`
 *     回调表达“需要切到 files / terminal 视图”或“需要隐藏自动化控制台”的意图。
 *   - controller 只持有文件域元数据并调用 workbench files API；不持有终端字节内容或 worktree 列表。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 rootNodes / childrenByPath / expandedPaths / selectedPath / selectedInfo / fileLoadingPath /
 *     fileError / fileNotice / fileTabs / activeFileTabId / fileSaving / newEntryName / renameName 单一权威状态。
 *   - 维护 activeProjectIdRef / activeWorktreeIdRef / fileTabsRef / activeFileTabIdRef /
 *     openFileRequestSeqRef / saveRequestSeqRef / formatRequestSeqRef / sqlitePreviewRequestSeqRef /
 *     dirRequestSeqRef，让异步回调读取最新值做 stale guard。
 *   - 暴露 loadDir / loadPathInfo / handleSelectNode / handleToggleNode / handleOpenFile /
 *     handleActivateFileTab / handleCloseFileTab / handleReturnToTerminal / handleReturnToFiles /
 *     handleFileContentChange / handleFileModeChange / handleSaveFileTab / handleFormatFileTab /
 *     handleSelectSqliteTable / handleCreateEntry / handleRenamePath / handleDeletePath /
 *     handleCopySelectedPath / handleLoadHtmlAsset 操作函数。
 *   - 暴露 bridge：`resetForContext(projectId, worktreeId)` 清空文件域全部状态并使挂起请求 stale；
 *     `guardDirtyContextChange()` 检查 dirty tab 并按用户确认决定是否允许切换 context。
 *
 * 不复制邻接 controller 状态：project / session / worktree / terminal / application 状态仍归 Workbench.tsx
 * 或邻接 controller 所有。
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import { workbenchApi } from '@/api/workbench';
import type {
  WorkbenchFileNode,
  WorkbenchFileMode,
  WorkbenchHtmlAsset,
  WorkbenchPathInfo,
} from '@/lib/types';
import type { WorkbenchOpenFileTab } from '@/components/domain/WorkbenchFileWorkspace';
import { isSafeWorkbenchRelativePath } from '../workbenchDeepLink';
import {
  collectTabsForPath,
  dirtyTabNames,
  dropExpandedPathTree,
  dropPathTreeEntries,
  isLatestRequest,
  validateJsonText,
  validateTomlText,
  validateYamlText,
  workbenchDirRequestKey,
  workbenchDirRequestKeyMatchesPath,
} from '../workbenchFiles';
import type { WorkbenchFileWorkspaceView } from '../workbenchFiles';

/** controller 用到的 i18n 错误文案 key；调用方注入对应 t('workbench:errors.X')。 */
export type WorkbenchFileErrorKey =
  | 'files'
  | 'pathInfo'
  | 'openFile'
  | 'saveFile'
  | 'formatFile'
  | 'previewSqlite'
  | 'createPath'
  | 'renamePath'
  | 'deletePath'
  | 'copyPath';

/** controller 用到的 i18n 通用文案 key；调用方注入对应 t('workbench:X')。 */
export type WorkbenchFileMessageKey =
  | 'saved'
  | 'formatted'
  | 'pathCopied'
  | 'confirmCloseDirtyFile'
  | 'confirmDeleteDirtyFiles'
  | 'confirmDeletePath';

/**
 * controller 输入：窄 API + 回调，避免吞并 Projects / Worktrees / Terminal context。
 *
 * 字段说明：
 *   - activeProjectId / activeWorktreeId：从 Workbench 透传，仅用于读取。
 *   - remoteWriteDisabled：项目域 controller 决定的只读标记；影响 content/save/format/create/rename/delete 是否执行。
 *   - isCurrentProject / markRequestFailure / markRequestSuccess：项目域 controller 的窄 API，用于 stale guard
 *     与远端离线标记。
 *   - requestWorkspaceView：controller 需要切到 files/terminal 视图时调用；workspaceView 仍由页面持有。
 *   - requestHideAutomationConsole：打开/激活文件 tab 时需要隐藏自动化控制台；automationConsoleOpen 仍由页面持有。
 *   - displayErrorMessage / desktopUnavailableMessage：错误文案构造。
 *   - translateFileError / translateFileMessage：i18n 文案注入（错误 key / 通用提示 / 确认文案）。
 */
export interface UseWorkbenchFileControllerParams {
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  remoteWriteDisabled: boolean;
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  requestWorkspaceView: (view: WorkbenchFileWorkspaceView) => void;
  requestHideAutomationConsole: () => void;
  displayErrorMessage?: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  desktopUnavailableMessage: string;
  translateFileError?: (key: WorkbenchFileErrorKey) => string;
  translateFileMessage: (
    key: WorkbenchFileMessageKey,
    vars?: Record<string, unknown>,
  ) => string;
}

/**
 * controller 暴露给页面 deep link / 项目切换 / worktree 切换 / merge 等流程的窄接口。
 *
 * Business Logic: Workbench.tsx 在项目切换 / worktree 切换 / merge / remove 等流程里需要：
 *   - 重置文件域全部状态（包含 stale 请求失效）；
 *   - 在切换 active worktree / 关闭 worktree 前检查是否有未保存编辑并询问用户。
 * 把它们封装成 bridge 类型，避免页面散落调用更宽的 controller API。
 */
export interface WorkbenchFileBridge {
  resetForContext: (projectId: string | null, worktreeId: string | null) => void;
  guardDirtyContextChange: () => Promise<boolean>;
}

/**
 * controller 返回值：文件域权威状态 + 操作函数 + bridge 视图。
 */
export interface WorkbenchFileControllerResult extends WorkbenchFileBridge {
  // ---- 渲染数据 ----
  rootNodes: WorkbenchFileNode[];
  childrenByPath: Record<string, WorkbenchFileNode[]>;
  expandedPaths: Set<string>;
  selectedPath: string | null;
  selectedInfo: WorkbenchPathInfo | null;
  fileLoadingPath: string | null;
  fileError: string | null;
  fileNotice: string | null;
  fileTabs: WorkbenchOpenFileTab[];
  activeFileTabId: string | null;
  fileSaving: boolean;
  newEntryName: string;
  renameName: string;
  // ---- 派生 setters ----
  setNewEntryName: (next: string) => void;
  setRenameName: (next: string) => void;
  // ---- 文件树 / 选中 ----
  loadDir: (path: string) => Promise<void>;
  loadPathInfo: (path: string) => Promise<void>;
  handleToggleNode: (node: WorkbenchFileNode) => void;
  handleSelectNode: (node: WorkbenchFileNode) => void;
  refreshParentDir: (path: string) => Promise<void>;
  // ---- tab 生命周期 ----
  handleOpenFile: (node: WorkbenchFileNode) => Promise<void>;
  openFileByPath: (path: string) => Promise<boolean>;
  handleActivateFileTab: (id: string) => void;
  handleCloseFileTab: (id: string) => void;
  handleReturnToTerminal: () => void;
  handleReturnToFiles: () => void;
  // ---- 编辑 / 保存 / 格式化 ----
  handleFileContentChange: (id: string, value: string) => void;
  handleFileModeChange: (id: string, mode: WorkbenchFileMode) => void;
  handleSaveFileTab: (id: string) => Promise<void>;
  handleFormatFileTab: (id: string) => Promise<void>;
  handleSelectSqliteTable: (id: string, table: string) => Promise<void>;
  handleLoadHtmlAsset: (
    documentPath: string,
    assetPath: string,
  ) => Promise<WorkbenchHtmlAsset | null>;
  // ---- 路径操作 ----
  handleCreateEntry: (kind: 'file' | 'dir') => Promise<void>;
  handleRenamePath: () => Promise<void>;
  handleDeletePath: () => Promise<void>;
  handleCopySelectedPath: () => Promise<void>;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有文件域全部 state；
 *   2. 用 ref 跟踪 activeProjectId / activeWorktreeId / fileTabs / activeFileTabId / 各请求 seq，
 *      让异步回调读到最新值；
 *   3. 注册 activeFileTabId 同步 ref 的副作用（保持与原 Workbench.tsx 行为一致）；
 *   4. 暴露稳定的操作函数（useCallback + ref 输入）和 bridge 视图，便于 Workbench 在多处复用。
 */
export function useWorkbenchFileController(
  params: UseWorkbenchFileControllerParams,
): WorkbenchFileControllerResult {
  const {
    activeProjectId,
    activeWorktreeId,
    remoteWriteDisabled,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    requestWorkspaceView,
    requestHideAutomationConsole,
    displayErrorMessage: displayErrorMessageParam,
    desktopUnavailableMessage,
    translateFileError,
    translateFileMessage,
  } = params;

  const [rootNodes, setRootNodes] = useState<WorkbenchFileNode[]>([]);
  const [childrenByPath, setChildrenByPath] = useState<Record<string, WorkbenchFileNode[]>>({});
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [selectedInfo, setSelectedInfo] = useState<WorkbenchPathInfo | null>(null);
  const [fileLoadingPath, setFileLoadingPath] = useState<string | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [fileNotice, setFileNotice] = useState<string | null>(null);
  const [fileTabs, setFileTabs] = useState<WorkbenchOpenFileTab[]>([]);
  const [activeFileTabId, setActiveFileTabId] = useState<string | null>(null);
  const [fileSaving, setFileSaving] = useState<boolean>(false);
  const [newEntryName, setNewEntryName] = useState<string>('');
  const [renameName, setRenameName] = useState<string>('');

  // Business Logic: 异步加载回调返回时，active project / worktree 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const activeWorktreeIdRef = useRef<string | null>(activeWorktreeId);
  // Business Logic: 文件 tab 的异步 stale guard 依赖 fileTabsRef 读取最新内容；如果只等 React effect 同步 ref，
  // 编辑、保存或预览请求返回的窄窗口内可能读到旧 tab 状态。
  const fileTabsRef = useRef<WorkbenchOpenFileTab[]>([]);
  const activeFileTabIdRef = useRef<string | null>(null);
  // Business Logic: handleSaveFileTab / handleCreateEntry / handleRenamePath / handleDeletePath 的 selectedPath /
  // selectedInfo 依赖需要在异步回调里读取最新选中态；用 ref + 同步 effect 保持最新值，使操作函数依赖稳定。
  const selectedPathRef = useRef<string | null>(null);
  const selectedInfoRef = useRef<WorkbenchPathInfo | null>(null);
  const openFileRequestSeqRef = useRef<number>(0);
  const saveRequestSeqRef = useRef<Record<string, number>>({});
  const formatRequestSeqRef = useRef<Record<string, number>>({});
  const sqlitePreviewRequestSeqRef = useRef<Record<string, number>>({});
  const dirRequestSeqRef = useRef<Record<string, number>>({});

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    activeFileTabIdRef.current = activeFileTabId;
  }, [activeFileTabId]);

  useEffect(() => {
    selectedPathRef.current = selectedPath;
  }, [selectedPath]);

  useEffect(() => {
    selectedInfoRef.current = selectedInfo;
  }, [selectedInfo]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   与终端域 controller 的 displayErrorMessage 注入版保持一致；测试可替换，且和原 Workbench 实现保持一致。
   */
  const displayErrorMessage = useCallback(
    (error: unknown, fallback: string): string => {
      if (displayErrorMessageParam) {
        return displayErrorMessageParam(error, fallback, desktopUnavailableMessage);
      }
      const message =
        error instanceof Error
          ? error.message
          : typeof error === 'string'
            ? error
            : String(error);
      const normalized = message.toLowerCase();
      if (
        normalized.includes('invoke') ||
        normalized.includes('__tauri') ||
        normalized.includes("reading 'invoke'") ||
        normalized.includes('reading "invoke"')
      ) {
        return desktopUnavailableMessage;
      }
      return message && message !== 'undefined' && message !== 'null' ? message : fallback;
    },
    [displayErrorMessageParam, desktopUnavailableMessage],
  );

  const t = useCallback(
    (key: WorkbenchFileErrorKey): string => {
      if (translateFileError) return translateFileError(key);
      return `workbench:errors.${key}`;
    },
    [translateFileError],
  );

  const tm = translateFileMessage;

  /**
   * Business Logic（为什么需要这个 setter）:
   *   文件 tab 的异步 stale guard 依赖 fileTabsRef 读取最新内容；如果只等 React effect 同步 ref，
   *   编辑、保存或预览请求返回的窄窗口内可能读到旧 tab 状态。
   *
   * Code Logic（这个函数做什么）:
   *   基于 fileTabsRef.current 计算下一份 tabs，立即写入 ref，再调用 React setState；不把副作用放进
   *   React functional updater，避免 Strict Mode 下 updater 重放带来不一致。
   */
  const setFileTabsState = useCallback(
    (
      updater:
        | WorkbenchOpenFileTab[]
        | ((currentTabs: WorkbenchOpenFileTab[]) => WorkbenchOpenFileTab[]),
    ): WorkbenchOpenFileTab[] => {
      const currentTabs = fileTabsRef.current;
      const nextTabs = typeof updater === 'function' ? updater(currentTabs) : updater;
      fileTabsRef.current = nextTabs;
      setFileTabs(nextTabs);
      return nextTabs;
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件树展开和刷新会对同一路径发起多次异步请求，旧响应不能覆盖最新目录内容或错误状态。
   *
   * Code Logic（这个函数做什么）:
   *   按 project/worktree/path 生成请求 key 并递增序号；响应、错误和 loading 清理只在当前序号仍最新时回写。
   */
  const loadDir = useCallback(
    async (path: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      const requestKey = workbenchDirRequestKey(projectId, worktreeId, path);
      const requestSeq = (dirRequestSeqRef.current[requestKey] ?? 0) + 1;
      dirRequestSeqRef.current[requestKey] = requestSeq;
      try {
        setFileError(null);
        setFileLoadingPath(path);
        const nodes = await workbenchApi.files.listDir(projectId, path, worktreeId);
        if (
          !isCurrentProject(projectId) ||
          activeWorktreeIdRef.current !== worktreeId ||
          !isLatestRequest(dirRequestSeqRef.current[requestKey], requestSeq)
        ) {
          return;
        }
        if (path === '') {
          setRootNodes(nodes);
        } else {
          setChildrenByPath((current) => ({ ...current, [path]: nodes }));
        }
        markRequestSuccess(projectId);
      } catch (error) {
        if (
          !isCurrentProject(projectId) ||
          activeWorktreeIdRef.current !== worktreeId ||
          !isLatestRequest(dirRequestSeqRef.current[requestKey], requestSeq)
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('files')));
      } finally {
        if (
          isCurrentProject(projectId) &&
          activeWorktreeIdRef.current === worktreeId &&
          isLatestRequest(dirRequestSeqRef.current[requestKey], requestSeq)
        ) {
          setFileLoadingPath((current) => (current === path ? null : current));
        }
      }
    },
    [displayErrorMessage, isCurrentProject, markRequestFailure, markRequestSuccess, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   删除或重命名目录后，旧目录子树的异步加载响应不能再写回文件树。
   *
   * Code Logic（这个函数做什么）:
   *   遍历当前目录请求序号表，命中同 project/worktree/path 子树的 key 就递增序号，使旧响应 stale。
   */
  const invalidateDirRequestsForPath = useCallback((path: string): void => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    const worktreeId = activeWorktreeIdRef.current;
    for (const [requestKey, requestSeq] of Object.entries(dirRequestSeqRef.current)) {
      if (workbenchDirRequestKeyMatchesPath(requestKey, projectId, worktreeId, path)) {
        dirRequestSeqRef.current[requestKey] = requestSeq + 1;
      }
    }
  }, []);

  const loadPathInfo = useCallback(
    async (path: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        const info = await workbenchApi.files.info(projectId, path, worktreeId);
        if (!isCurrentProject(projectId) || activeWorktreeIdRef.current !== worktreeId) {
          return;
        }
        setSelectedInfo(info);
        setRenameName(info.name);
        markRequestSuccess(projectId);
      } catch (error) {
        if (!isCurrentProject(projectId) || activeWorktreeIdRef.current !== worktreeId) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('pathInfo')));
      }
    },
    [displayErrorMessage, isCurrentProject, markRequestFailure, markRequestSuccess, t],
  );

  const handleToggleNode = useCallback(
    (node: WorkbenchFileNode): void => {
      if (node.kind !== 'dir') return;
      setExpandedPaths((current) => {
        const next = new Set(current);
        if (next.has(node.path)) {
          next.delete(node.path);
        } else {
          next.add(node.path);
          if (!childrenByPath[node.path]) {
            void loadDir(node.path);
          }
        }
        return next;
      });
    },
    [childrenByPath, loadDir],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从右侧文件树点选文件时，需要在 Workbench 中打开文件工作区，同时保留终端会话上下文。
   *
   * Code Logic（这个函数做什么）:
   *   对当前 project/worktree 发起带序号的 open 文件请求；只有最后一次点击的响应允许激活 tab。
   *   已有 dirty tab 保留用户编辑内容、模式和原 opened.text 保存基线，只更新后端 metadata/preview。
   */
  const handleOpenFile = useCallback(
    async (node: WorkbenchFileNode): Promise<void> => {
      if (node.kind !== 'file') return;
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      const requestSeq = openFileRequestSeqRef.current + 1;
      openFileRequestSeqRef.current = requestSeq;

      try {
        setFileError(null);
        setFileNotice(null);
        const opened = await workbenchApi.files.open(projectId, node.path, worktreeId);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          openFileRequestSeqRef.current !== requestSeq
        ) {
          return;
        }

        const tabId = workbenchFileTabId(worktreeId, opened.metadata.path);
        const freshTab: WorkbenchOpenFileTab = {
          id: tabId,
          path: opened.metadata.path,
          name: opened.metadata.name,
          opened,
          content: opened.text?.content ?? '',
          dirty: false,
          mode: opened.capabilities.defaultMode,
        };

        setFileTabsState((currentTabs) => {
          const existingTab = currentTabs.find((tab) => tab.id === tabId);
          if (!existingTab) {
            return [...currentTabs, freshTab];
          }

          return currentTabs.map((tab) => {
            if (tab.id !== tabId) return tab;
            return mergeOpenedForReopenedTab(tab, freshTab);
          });
        });
        activeFileTabIdRef.current = tabId;
        setActiveFileTabId(tabId);
        requestHideAutomationConsole();
        requestWorkspaceView('files');
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          openFileRequestSeqRef.current !== requestSeq
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('openFile')));
      }
    },
    [
      displayErrorMessage,
      markRequestFailure,
      requestHideAutomationConsole,
      requestWorkspaceView,
      setFileTabsState,
      t,
    ],
  );

  const handleSelectNode = useCallback(
    (node: WorkbenchFileNode): void => {
      setSelectedPath(node.path);
      setSelectedInfo({
        name: node.name,
        path: node.path,
        kind: node.kind,
        size: node.size,
        modifiedAt: node.modifiedAt,
      });
      setRenameName(node.name);
      void loadPathInfo(node.path);
      if (node.kind === 'file') {
        void handleOpenFile(node);
      }
    },
    [handleOpenFile, loadPathInfo],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   WORKFLOW 向导与 files deep link 需要按相对路径打开文件，而不是要求用户先在树中点选节点。
   *
   * Code Logic（这个函数做什么）:
   *   拒绝绝对路径与目录穿越；构造最小 file node 后复用 handleOpenFile。
   */
  const openFileByPath = useCallback(
    async (path: string): Promise<boolean> => {
      const trimmed = path.trim();
      if (!isSafeWorkbenchRelativePath(trimmed)) {
        setFileError(t('openFile'));
        return false;
      }
      const name = basename(trimmed, trimmed);
      await handleOpenFile({
        name,
        path: trimmed,
        kind: 'file',
        size: null,
        modifiedAt: null,
        children: null,
      });
      return true;
    },
    [handleOpenFile, t],
  );

  const refreshParentDir = useCallback(
    async (path: string): Promise<void> => {
      const parent = parentPathOf(path);
      await loadDir(parent);
      if (parent === '') await loadDir('');
    },
    [loadDir],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在文件工作区点击 tab 时，需要切回文件视图并激活对应文件，而不是影响右侧检查器 tab。
   *
   * Code Logic（这个函数做什么）:
   *   只更新 activeFileTabId 并请求切到 files 视图、隐藏自动化控制台；具体 tab 内容由 WorkbenchFileWorkspace
   *   根据 id 渲染。
   */
  const handleActivateFileTab = useCallback(
    (id: string): void => {
      activeFileTabIdRef.current = id;
      setActiveFileTabId(id);
      requestHideAutomationConsole();
      requestWorkspaceView('files');
    },
    [requestHideAutomationConsole, requestWorkspaceView],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户关闭文件 tab 后，工作区需要选择相邻文件继续显示；dirty tab 不能在未确认时丢弃修改。
   *
   * Code Logic（这个函数做什么）:
   *   关闭前检查目标 tab 是否 dirty；用户确认后移除目标 tab，并在关闭 active tab 时选择相邻或剩余 tab。
   */
  const handleCloseFileTab = useCallback(
    (id: string): void => {
      const currentTabs = fileTabsRef.current;
      const targetTab = currentTabs.find((tab) => tab.id === id);
      if (!targetTab) return;
      if (
        targetTab.dirty &&
        !window.confirm(tm('confirmCloseDirtyFile', { names: dirtyTabNames([targetTab]).join(', ') }))
      ) {
        return;
      }
      const removedTabIds = new Set([id]);
      const nextTabs = currentTabs.filter((tab) => tab.id !== id);
      const nextActiveTabId = nextActiveFileTabIdAfterRemoval(
        currentTabs,
        removedTabIds,
        activeFileTabIdRef.current,
      );
      activeFileTabIdRef.current = nextActiveTabId;
      setFileTabsState(nextTabs);
      setActiveFileTabId(nextActiveTabId);
      if (!nextActiveTabId) requestWorkspaceView('terminal');
    },
    [requestWorkspaceView, setFileTabsState, tm],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件浏览/编辑完成后，用户需要回到原本常驻的终端工作区继续操作。
   *
   * Code Logic（这个函数做什么）:
   *   请求将中心工作区视图切回 terminal；终端 DOM 一直保持挂载，只是恢复可见和可输入。
   */
  const handleReturnToTerminal = useCallback((): void => {
    requestHideAutomationConsole();
    requestWorkspaceView('terminal');
  }, [requestHideAutomationConsole, requestWorkspaceView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从文件预览返回终端后，仍需要从终端工具栏一键回到已打开的文件工作区，形成对称导航。
   *
   * Code Logic（这个函数做什么）:
   *   优先恢复当前 active 文件 tab；如果 ref 丢失但仍有打开文件，则选择第一个 tab 并请求切到 files 视图。
   */
  const handleReturnToFiles = useCallback((): void => {
    const targetTabId = activeFileTabIdRef.current ?? fileTabsRef.current[0]?.id ?? null;
    if (!targetTabId) return;
    activeFileTabIdRef.current = targetTabId;
    setActiveFileTabId(targetTabId);
    requestHideAutomationConsole();
    requestWorkspaceView('files');
  }, [requestHideAutomationConsole, requestWorkspaceView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户编辑文件内容时需要标记未保存状态，避免保存按钮和 tab 脏标记失真。
   *
   * Code Logic（这个函数做什么）:
   *   remoteWriteDisabled 时静默拒绝（避免必然失败的远端写流程产生不一致中间态）；否则按 tab id 更新 content
   *   并设置 dirty=true，其他 tab 保持不变。
   */
  const handleFileContentChange = useCallback(
    (id: string, value: string): void => {
      if (remoteWriteDisabled) return;
      setFileTabsState((currentTabs) =>
        currentTabs.map((tab) => (tab.id === id ? { ...tab, content: value, dirty: true } : tab)),
      );
    },
    [remoteWriteDisabled, setFileTabsState],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Markdown 等文件支持多种查看/编辑模式，用户切换模式后应随 tab 保持。
   *
   * Code Logic（这个函数做什么）:
   *   按 tab id 写入新的 mode，不改变文件内容和保存状态。
   */
  const handleFileModeChange = useCallback(
    (id: string, mode: WorkbenchFileMode): void => {
      setFileTabsState((currentTabs) =>
        currentTabs.map((tab) => (tab.id === id ? { ...tab, mode } : tab)),
      );
    },
    [setFileTabsState],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   JSON/TOML/YAML 保存前必须先做前端语法校验，避免明显错误内容覆盖项目文件。
   *
   * Code Logic（这个函数做什么）:
   *   根据后端 detectedType 选择对应校验器；非结构化文本不做额外校验。
   */
  const validateStructuredFileTab = useCallback(
    (tab: WorkbenchOpenFileTab): string | null => {
      if (tab.opened.detectedType === 'json') {
        const result = validateJsonText(tab.content);
        return result.ok ? null : result.message;
      }
      if (tab.opened.detectedType === 'toml') {
        const result = validateTomlText(tab.content);
        return result.ok ? null : result.message;
      }
      if (tab.opened.detectedType === 'yaml') {
        const result = validateYamlText(tab.content);
        return result.ok ? null : result.message;
      }
      return null;
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户保存文件 tab 时，需要使用后端 baseHash 乐观锁写回当前 worktree，并刷新文件树元信息。
   *
   * Code Logic（这个函数做什么）:
   *   找到目标 tab、校验 JSON/TOML/YAML、捕获提交内容和请求序号后调用 saveText；响应仍最新时更新保存基线，
   *   若保存期间又有内存编辑则保留当前 content 和 dirty=true，否则清除 dirty，并刷新路径信息。
   */
  const handleSaveFileTab = useCallback(
    async (id: string): Promise<void> => {
      const tab = fileTabsRef.current.find((candidate) => candidate.id === id);
      if (!tab) return;
      if (remoteWriteDisabled) return;

      const baseHash = tab.opened.text?.baseHash;
      if (!baseHash) {
        setFileError(t('saveFile'));
        return;
      }

      const validationMessage = validateStructuredFileTab(tab);
      if (validationMessage) {
        setFileError(`${t('saveFile')}: ${validationMessage}`);
        return;
      }

      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      const submittedContent = tab.content;
      const requestSeq = (saveRequestSeqRef.current[id] ?? 0) + 1;
      saveRequestSeqRef.current[id] = requestSeq;

      try {
        setFileSaving(true);
        setFileError(null);
        setFileNotice(null);
        const saved = await workbenchApi.files.saveText(
          projectId,
          tab.path,
          submittedContent,
          baseHash,
          worktreeId,
        );
        const latestTab = fileTabsRef.current.find((candidate) => candidate.id === id);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          !isLatestRequest(saveRequestSeqRef.current[id], requestSeq) ||
          !latestTab
        ) {
          return;
        }

        setFileTabsState((currentTabs) =>
          currentTabs.map((currentTab) => {
            if (currentTab.id !== id) return currentTab;
            const contentChangedAfterSubmit = currentTab.content !== submittedContent;
            return {
              ...currentTab,
              path: saved.metadata.path,
              name: saved.metadata.name,
              dirty: contentChangedAfterSubmit,
              opened: {
                ...currentTab.opened,
                metadata: saved.metadata,
                text: currentTab.opened.text
                  ? {
                      ...currentTab.opened.text,
                      content: submittedContent,
                      baseHash: saved.baseHash,
                      baseModifiedAt: saved.baseModifiedAt,
                    }
                  : currentTab.opened.text,
              },
              content: contentChangedAfterSubmit ? currentTab.content : submittedContent,
            };
          }),
        );
        await refreshParentDir(tab.path);
        if (selectedPathRef.current === tab.path) {
          await loadPathInfo(tab.path);
        }
        setFileNotice(tm('saved'));
        setFileError(null);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          !isLatestRequest(saveRequestSeqRef.current[id], requestSeq)
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('saveFile')));
      } finally {
        if (
          activeProjectIdRef.current === projectId &&
          activeWorktreeIdRef.current === worktreeId &&
          isLatestRequest(saveRequestSeqRef.current[id], requestSeq)
        ) {
          setFileSaving(false);
        }
      }
    },
    [
      displayErrorMessage,
      loadPathInfo,
      refreshParentDir,
      remoteWriteDisabled,
      setFileTabsState,
      markRequestFailure,
      t,
      tm,
      validateStructuredFileTab,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要在保存前格式化 JSON/TOML/YAML，但格式化不应自动写盘。
   *
   * Code Logic（这个函数做什么）:
   *   捕获提交时的内容、project/worktree 和 tab 请求序号；响应回来后仍是最新请求且内容未变化时，
   *   才用后端格式化输出更新 tab 并标记 dirty。
   */
  const handleFormatFileTab = useCallback(
    async (id: string): Promise<void> => {
      const tab = fileTabsRef.current.find((candidate) => candidate.id === id);
      if (!tab) return;
      if (remoteWriteDisabled) return;
      const kind =
        tab.opened.detectedType === 'json' ||
        tab.opened.detectedType === 'toml' ||
        tab.opened.detectedType === 'yaml'
          ? tab.opened.detectedType
          : null;
      if (!kind) return;

      const validationMessage = validateStructuredFileTab(tab);
      if (validationMessage) {
        setFileError(`${t('formatFile')}: ${validationMessage}`);
        return;
      }
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      const submittedContent = tab.content;
      const requestSeq = (formatRequestSeqRef.current[id] ?? 0) + 1;
      formatRequestSeqRef.current[id] = requestSeq;

      try {
        setFileError(null);
        setFileNotice(null);
        const result = await workbenchApi.files.formatStructured(kind, submittedContent);
        const latestTab = fileTabsRef.current.find((candidate) => candidate.id === id);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          formatRequestSeqRef.current[id] !== requestSeq ||
          !latestTab ||
          latestTab.content !== submittedContent
        ) {
          return;
        }
        setFileTabsState((currentTabs) =>
          currentTabs.map((currentTab) =>
            currentTab.id === id && currentTab.content === submittedContent
              ? {
                  ...currentTab,
                  content: result.formatted,
                  dirty: true,
                }
              : currentTab,
          ),
        );
        setFileNotice(tm('formatted'));
      } catch (error) {
        const latestTab = fileTabsRef.current.find((candidate) => candidate.id === id);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          formatRequestSeqRef.current[id] !== requestSeq ||
          !latestTab ||
          latestTab.content !== submittedContent
        ) {
          return;
        }
        setFileError(displayErrorMessage(error, t('formatFile')));
      }
    },
    [
      displayErrorMessage,
      remoteWriteDisabled,
      setFileTabsState,
      t,
      tm,
      validateStructuredFileTab,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   SQLite 文件预览需要按用户选择的表重新加载行数据，而不是重新打开整个文件 tab。
   *
   * Code Logic（这个函数做什么）:
   *   为每个 tab 的表预览请求递增序号；响应仍属于当前 project/worktree 且是该 tab 最新请求时，
   *   才替换 tab.opened.sqlite。
   */
  const handleSelectSqliteTable = useCallback(
    async (id: string, table: string): Promise<void> => {
      const tab = fileTabsRef.current.find((candidate) => candidate.id === id);
      const projectId = activeProjectIdRef.current;
      if (!tab || !projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      const requestSeq = (sqlitePreviewRequestSeqRef.current[id] ?? 0) + 1;
      sqlitePreviewRequestSeqRef.current[id] = requestSeq;

      try {
        setFileError(null);
        const sqlite = await workbenchApi.files.previewSqlite(
          projectId,
          tab.path,
          table,
          100,
          worktreeId,
        );
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          sqlitePreviewRequestSeqRef.current[id] !== requestSeq ||
          !fileTabsRef.current.some((candidate) => candidate.id === id)
        ) {
          return;
        }
        setFileTabsState((currentTabs) =>
          currentTabs.map((currentTab) =>
            currentTab.id === id
              ? {
                  ...currentTab,
                  opened: {
                    ...currentTab.opened,
                    sqlite,
                  },
                }
              : currentTab,
          ),
        );
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId ||
          sqlitePreviewRequestSeqRef.current[id] !== requestSeq ||
          !fileTabsRef.current.some((candidate) => candidate.id === id)
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('previewSqlite')));
      }
    },
    [displayErrorMessage, markRequestFailure, setFileTabsState, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   HTML 预览组件只知道当前文件路径，项目/worktree 上下文由页面层持有，因此资源读取必须经页面层转接。
   *
   * Code Logic（这个函数做什么）:
   *   捕获当前 projectId/worktreeId，调用 workbench files API 获取 data URL；失败返回 null 让预览移除该资源引用。
   */
  const handleLoadHtmlAsset = useCallback(
    async (documentPath: string, assetPath: string): Promise<WorkbenchHtmlAsset | null> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return null;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        return await workbenchApi.files.previewHtmlAsset(
          projectId,
          documentPath,
          assetPath,
          worktreeId,
        );
      } catch {
        return null;
      }
    },
    [],
  );

  const handleCreateEntry = useCallback(
    async (kind: 'file' | 'dir'): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || !newEntryName.trim()) return;
      if (remoteWriteDisabled) return;
      const worktreeId = activeWorktreeIdRef.current;
      const parentPath = selectedParentPathFromInfo(selectedInfoRef.current);
      try {
        setFileError(null);
        setFileNotice(null);
        const created =
          kind === 'file'
            ? await workbenchApi.files.createFile(projectId, parentPath, newEntryName.trim(), worktreeId)
            : await workbenchApi.files.createDir(projectId, parentPath, newEntryName.trim(), worktreeId);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setNewEntryName('');
        setSelectedPath(created.path);
        setSelectedInfo(created);
        setRenameName(created.name);
        if (parentPath) {
          setExpandedPaths((current) => new Set(current).add(parentPath));
        }
        await loadDir(parentPath);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('createPath')));
      }
    },
    [displayErrorMessage, loadDir, markRequestFailure, newEntryName, remoteWriteDisabled, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件树重命名成功后，用户已经打开的文件 tab 应继续指向新路径，且不能丢失未保存编辑。
   *
   * Code Logic（这个函数做什么）:
   *   调用后端 rename 后按原路径映射所有受影响 tab 的 path/id/metadata；activeFileTabId 同步改名后的 id，
   *   content、dirty、mode 和保存基线保持不变，并让此前发出的旧路径 open 响应失效。
   */
  const handleRenamePath = useCallback(
    async (): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      const currentSelectedInfo = selectedInfoRef.current;
      if (!projectId || !currentSelectedInfo || !renameName.trim()) return;
      if (remoteWriteDisabled) return;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        setFileError(null);
        setFileNotice(null);
        const originalPath = currentSelectedInfo.path;
        const renamed = await workbenchApi.files.renamePath(
          projectId,
          originalPath,
          renameName.trim(),
          worktreeId,
        );
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        openFileRequestSeqRef.current += 1;
        if (currentSelectedInfo.kind === 'dir') {
          invalidateDirRequestsForPath(originalPath);
          invalidateDirRequestsForPath(renamed.path);
          setChildrenByPath((current) =>
            dropPathTreeEntries(dropPathTreeEntries(current, originalPath), renamed.path),
          );
          setExpandedPaths((current) =>
            dropExpandedPathTree(dropExpandedPathTree(current, originalPath), renamed.path),
          );
        }
        const renamedTabIds = new Map<string, string>();
        const nextTabs = fileTabsRef.current.map((tab) => {
          const nextPath = renamedPathForTab(tab.path, originalPath, renamed.path);
          if (!nextPath) return tab;

          const nextId = workbenchFileTabId(worktreeId, nextPath);
          const nextName =
            tab.path === originalPath ? renamed.name : basename(nextPath, tab.name);
          renamedTabIds.set(tab.id, nextId);
          return {
            ...tab,
            id: nextId,
            path: nextPath,
            name: nextName,
            opened: {
              ...tab.opened,
              metadata: {
                ...tab.opened.metadata,
                ...(tab.path === originalPath ? renamed : {}),
                path: nextPath,
                name: nextName,
              },
            },
          };
        });
        setFileTabsState(nextTabs);
        const nextActiveFileTabId = activeFileTabIdRef.current
          ? (renamedTabIds.get(activeFileTabIdRef.current) ?? activeFileTabIdRef.current)
          : null;
        activeFileTabIdRef.current = nextActiveFileTabId;
        setActiveFileTabId(nextActiveFileTabId);
        setSelectedPath(renamed.path);
        setSelectedInfo(renamed);
        setRenameName(renamed.name);
        await refreshParentDir(originalPath);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('renamePath')));
      }
    },
    [
      displayErrorMessage,
      invalidateDirRequestsForPath,
      markRequestFailure,
      refreshParentDir,
      remoteWriteDisabled,
      renameName,
      setFileTabsState,
      t,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件树删除路径成功后，被删除文件或目录下的已打开 tab 不能继续指向不存在的路径；
   *   如果这些 tab 有未保存编辑，必须先让用户确认放弃。
   *
   * Code Logic（这个函数做什么）:
   *   删除前用当前 tabs 收集受影响路径并提示 dirty 文件；确认后调用后端 delete，成功后关闭命中 tab，
   *   active tab 被删除时按相邻/剩余 tab 重新选择，并让此前发出的旧路径 open 响应失效。
   */
  const handleDeletePath = useCallback(
    async (): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      const currentSelectedInfo = selectedInfoRef.current;
      if (!projectId || !currentSelectedInfo) return;
      if (remoteWriteDisabled) return;
      const affectedTabs = collectTabsForPath(
        fileTabsRef.current,
        currentSelectedInfo.path,
        currentSelectedInfo.kind,
      );
      const affectedDirtyNames = dirtyTabNames(affectedTabs);
      if (
        affectedDirtyNames.length > 0 &&
        !window.confirm(tm('confirmDeleteDirtyFiles', { names: affectedDirtyNames.join(', ') }))
      ) {
        return;
      }
      if (!window.confirm(tm('confirmDeletePath', { name: currentSelectedInfo.name }))) return;
      const worktreeId = activeWorktreeIdRef.current;
      const path = currentSelectedInfo.path;
      try {
        setFileError(null);
        setFileNotice(null);
        await workbenchApi.files.deletePath(projectId, path, worktreeId);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        openFileRequestSeqRef.current += 1;
        if (currentSelectedInfo.kind === 'dir') {
          invalidateDirRequestsForPath(path);
          setChildrenByPath((current) => dropPathTreeEntries(current, path));
          setExpandedPaths((current) => dropExpandedPathTree(current, path));
        }
        const removedTabIds = new Set(
          collectTabsForPath(fileTabsRef.current, path, currentSelectedInfo.kind).map(
            (tab) => tab.id,
          ),
        );
        const nextActiveTabId = nextActiveFileTabIdAfterRemoval(
          fileTabsRef.current,
          removedTabIds,
          activeFileTabIdRef.current,
        );
        const nextTabs = fileTabsRef.current.filter((tab) => !removedTabIds.has(tab.id));
        activeFileTabIdRef.current = nextActiveTabId;
        setFileTabsState(nextTabs);
        setActiveFileTabId(nextActiveTabId);
        if (!nextActiveTabId) requestWorkspaceView('terminal');
        setSelectedPath(null);
        setSelectedInfo(null);
        setRenameName('');
        await refreshParentDir(path);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        markRequestFailure(projectId, error);
        setFileError(displayErrorMessage(error, t('deletePath')));
      }
    },
    [
      displayErrorMessage,
      invalidateDirRequestsForPath,
      markRequestFailure,
      refreshParentDir,
      remoteWriteDisabled,
      requestWorkspaceView,
      setFileTabsState,
      t,
      tm,
    ],
  );

  const handleCopySelectedPath = useCallback(
    async (): Promise<void> => {
      const currentSelectedInfo = selectedInfoRef.current;
      if (!currentSelectedInfo) return;
      try {
        const value = currentSelectedInfo.path || '.';
        await navigator.clipboard.writeText(value);
        setFileError(null);
        setFileNotice(tm('pathCopied'));
      } catch (error) {
        setFileError(displayErrorMessage(error, t('copyPath')));
      }
    },
    [displayErrorMessage, t, tm],
  );

  /**
   * Business Logic（为什么需要 selectedPathRef / selectedInfoRef）:
   *   handleSaveFileTab 的 selectedPath 依赖、handleCreateEntry 的 selectedInfo 依赖、handleRenamePath /
   *   handleDeletePath 的 selectedInfo 依赖都需要在异步回调里读取最新选中态。若直接读取 React state，
   *   useCallback 依赖会每次渲染变化；用 ref + 同步副作用保持最新值，使操作函数依赖稳定。
   *
   * Code Logic: ref 与 useEffect 在 hook 顶部声明（与 active*IdRef 同位置），避免 react-hooks/immutability
   *   规则把“在 hook 中段声明 ref + 紧随其后的 effect 写回”判为不可变违规。操作函数读 ref.current。
   */

  /**
   * Business Logic（为什么需要这个 bridge）:
   *   项目切换 / worktree 切换 / merge / remove 等流程必须把文件域全部状态重置回初始值，并使挂起的
   *   open/save/format/sqlite/dir 请求 stale，避免旧响应在新 context 下激活 tab 或写回文件树。
   *
   * Code Logic（这个函数做什么）:
   *   递增 open/seq，清空 save/format/sqlite/dir seq 表，清空 activeFileTabId ref，再清空所有文件域 state。
   *   传入 projectId/worktreeId 仅作为 bridge 契约语义；实际清空与当前 active ref 无关，保证 reset 后旧响应
   *   无论回到哪个 context 都被丢弃。
   */
  const resetForContext = useCallback(
    // eslint-disable-next-line @typescript-eslint/no-unused-vars -- bridge 契约保留参数语义。
    (_projectId: string | null, _worktreeId: string | null): void => {
      openFileRequestSeqRef.current += 1;
      saveRequestSeqRef.current = {};
      formatRequestSeqRef.current = {};
      sqlitePreviewRequestSeqRef.current = {};
      dirRequestSeqRef.current = {};
      activeFileTabIdRef.current = null;
      setRootNodes([]);
      setChildrenByPath({});
      setExpandedPaths(new Set());
      setSelectedPath(null);
      setSelectedInfo(null);
      setFileTabsState([]);
      setActiveFileTabId(null);
      setFileSaving(false);
      setFileError(null);
      setFileNotice(null);
    },
    [setFileTabsState],
  );

  /**
   * Business Logic（为什么需要这个 bridge）:
   *   worktree 切换 / merge / remove 等会破坏文件编辑上下文的流程在执行前必须检查是否有未保存编辑；
   *   如果有且用户取消，应中止切换；如果用户确认，应继续切换（切换流程随后会通过 resetForContext 清空状态）。
   *
   * Code Logic（这个函数做什么）:
   *   收集当前所有 dirty tab；无 dirty 直接返回 true；有 dirty 时弹出统一确认文案，用户确认返回 true、
   *   取消返回 false（保留 dirty tab 不变）。
   */
  const guardDirtyContextChange = useCallback(async (): Promise<boolean> => {
    const dirtyTabs = dirtyTabNames(fileTabsRef.current);
    if (dirtyTabs.length === 0) return true;
    return window.confirm(
      tm('confirmCloseDirtyFile', { names: dirtyTabs.join(', ') }),
    );
  }, [tm]);

  return {
    // 渲染数据
    rootNodes,
    childrenByPath,
    expandedPaths,
    selectedPath,
    selectedInfo,
    fileLoadingPath,
    fileError,
    fileNotice,
    fileTabs,
    activeFileTabId,
    fileSaving,
    newEntryName,
    renameName,
    // 派生 setters
    setNewEntryName,
    setRenameName,
    // 文件树 / 选中
    loadDir,
    loadPathInfo,
    handleToggleNode,
    handleSelectNode,
    refreshParentDir,
    // tab 生命周期
    handleOpenFile,
    openFileByPath,
    handleActivateFileTab,
    handleCloseFileTab,
    handleReturnToTerminal,
    handleReturnToFiles,
    // 编辑 / 保存 / 格式化
    handleFileContentChange,
    handleFileModeChange,
    handleSaveFileTab,
    handleFormatFileTab,
    handleSelectSqliteTable,
    handleLoadHtmlAsset,
    // 路径操作
    handleCreateEntry,
    handleRenamePath,
    handleDeletePath,
    handleCopySelectedPath,
    // bridge 视图
    resetForContext,
    guardDirtyContextChange,
  };
}

/* ---------------------------------------------------------------------------
 * 文件域 helper —— 从 Workbench.tsx 迁移而来，行为与原实现完全一致。
 *
 * 这些 helper 与文件 tab 的路径/id 映射相关，仅被 controller 内部使用，所以放在本文件而不是 workbenchFiles.ts
 * （后者只放与后端/纯函数相关的工具）。
 * ------------------------------------------------------------------------- */

/**
 * Business Logic（为什么需要这个函数）:
 *   文件操作默认作用在当前选中文件夹；若选中的是文件，则作用在它的父目录。
 *
 * Code Logic（这个函数做什么）:
 *   从相对路径中取最后一个 `/` 之前的部分；根级文件返回空字符串。
 */
function parentPathOf(path: string): string {
  const index = path.lastIndexOf('/');
  return index >= 0 ? path.slice(0, index) : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   重命名时已有 tab 的 name 需要按新路径重新计算 basename；原 tab name 可能是用户自定义的旧名。
 *
 * Code Logic（这个函数做什么）:
 *   取相对路径最后一段；空路径返回 rootLabel。
 */
function basename(path: string, rootLabel: string): string {
  if (!path) return rootLabel;
  const parts = path.split('/').filter(Boolean);
  return parts[parts.length - 1] ?? rootLabel;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件工作区 tab id 需要同时区分 main worktree 与功能 worktree，避免同一路径跨 worktree 冲突。
 *
 * Code Logic（这个函数做什么）:
 *   按当前 worktreeId 和文件相对路径生成稳定 tab id；主工作区使用 main 前缀。
 */
function workbenchFileTabId(worktreeId: string | null, path: string): string {
  return `${worktreeId ?? 'main'}:${path}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户重命名文件或目录后，已打开 tab 需要继续指向新的相对路径并保留未保存编辑。
 *
 * Code Logic（这个函数做什么）:
 *   命中原路径时返回新路径；命中原目录后代时拼接新目录路径和原后缀；不相关路径返回 null。
 */
function renamedPathForTab(
  path: string,
  originalPath: string,
  renamedPath: string,
): string | null {
  if (path === originalPath) return renamedPath;
  if (!originalPath || !path.startsWith(`${originalPath}/`)) return null;
  return `${renamedPath}${path.slice(originalPath.length)}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   dirty tab 重新打开时可以刷新 preview/metadata，但不能刷新保存基线，否则会绕过后端 optimistic lock。
 *
 * Code Logic（这个函数做什么）:
 *   基于后端最新 opened payload 更新 metadata/preview；当 tab 已 dirty 时保留原 opened.text 作为 baseHash、
 *   baseModifiedAt 和打开时 content 的来源。
 */
function mergeOpenedForReopenedTab(
  existingTab: WorkbenchOpenFileTab,
  freshTab: WorkbenchOpenFileTab,
): WorkbenchOpenFileTab {
  if (!existingTab.dirty) return freshTab;

  return {
    ...existingTab,
    path: freshTab.path,
    name: freshTab.name,
    opened: {
      ...freshTab.opened,
      text: existingTab.opened.text,
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   关闭或删除 tab 后需要选择合理的相邻 tab，避免 activeFileTabId 指向不存在的文件。
 *
 * Code Logic（这个函数做什么）:
 *   如果当前 active tab 未被移除则保持不变；否则优先选择原 active 前一个邻居，再退到最后一个剩余 tab。
 */
function nextActiveFileTabIdAfterRemoval(
  currentTabs: WorkbenchOpenFileTab[],
  removedTabIds: Set<string>,
  activeTabId: string | null,
): string | null {
  const remainingTabs = currentTabs.filter((tab) => !removedTabIds.has(tab.id));
  if (remainingTabs.length === 0) return null;
  if (activeTabId && !removedTabIds.has(activeTabId)) return activeTabId;

  const activeIndex = activeTabId
    ? currentTabs.findIndex((tab) => tab.id === activeTabId)
    : -1;
  const fallbackIndex = activeIndex >= 0 ? Math.max(0, activeIndex - 1) : 0;
  return remainingTabs[Math.min(fallbackIndex, remainingTabs.length - 1)]?.id ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件操作默认作用在当前选中文件夹；若选中的是文件，则作用在它的父目录。controller 内部为 selectedInfo ref
 *   单独提供一个可测的纯函数版本。
 *
 * Code Logic（这个函数做什么）:
 *   selectedInfo 为 dir 时返回其路径；为 file 时返回 parentPathOf(path)；无选中返回空字符串。
 */
function selectedParentPathFromInfo(info: WorkbenchPathInfo | null): string {
  if (!info) return '';
  return info.kind === 'dir' ? info.path : parentPathOf(info.path);
}
