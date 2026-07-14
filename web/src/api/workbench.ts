/**
 * 工作台 API - 通过 Tauri invoke 调用 Rust 后端的本机项目、终端和文件树命令。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台页面需要统一管理项目文件夹、terminal window/pane 和右侧文件树交互。
 *   组件层不应直接拼 invoke 命令名，避免命令参数分散。
 *
 * Code Logic（这个模块做什么）:
 *   按 projects / remote / sessions / files 等业务分组封装 Rust workbench 命令；
 *   所有参数使用 camelCase，返回类型对齐 `src/lib/types.ts`。
 */

import { invoke, invokeDecoded } from './client';
import { nullableDecoder } from '@/lib/runtimeSchema';
import {
  workbenchFileNodesDecoder,
  workbenchLaunchSummaryDecoder,
  workbenchMergeResultDecoder,
  workbenchMutationEnvelopeDecoder,
  workbenchMutationOperationDecoder,
  workbenchOpenFileDecoder,
  workbenchPathInfoDecoder,
  workbenchProjectDecoder,
  workbenchProjectsDecoder,
  workbenchRemoveResultDecoder,
  workbenchSaveTextResultDecoder,
  workbenchSessionDecoder,
  workbenchSessionsDecoder,
  workbenchWorktreeDecoder,
  workbenchWorktreesDecoder,
} from '@/lib/schemas/workbench';
import type {
  ResumeClaudeSessionResult,
  SessionPreview,
  SessionSearchHit,
  WorkbenchFormatResult,
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchGitCommit,
  WorkbenchHtmlAsset,
  WorkbenchMergeResult,
  WorkbenchMutationEnvelope,
  WorkbenchMutationOperation,
  WorkbenchPathInfo,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
  WorkbenchRemoteRoot,
  WorkbenchSqlitePreview,
  WorkbenchWorktree,
} from '@/lib/types';
import type { WorkbenchLaunchSummaryWire } from '@/lib/types';

interface WorkbenchTerminalSize {
  cols: number;
  rows: number;
}

export type WorkbenchPaneSplitDirection = 'right' | 'down';

export const workbenchApi = {
  projects: {
    /** 列出工作台最近项目，后端按 lastOpenedAt 倒序返回。 */
    list: () =>
      invokeDecoded('list_workbench_projects', undefined, workbenchProjectsDecoder),

    /** 添加或重新打开一个项目文件夹，path 为本机或已挂载局域网目录。 */
    add: (path: string) =>
      invokeDecoded('add_workbench_project', { path }, workbenchProjectDecoder),

    /** 从最近项目列表移除项目记录，不删除磁盘文件。 */
    remove: (projectId: string) =>
      invoke<{ ok: boolean; projectId: string }>('remove_workbench_project', { projectId }),

    /** 更新最近打开时间，并返回最新项目 DTO。 */
    touch: (projectId: string) =>
      invokeDecoded('touch_workbench_project', { projectId }, workbenchProjectDecoder),
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   「继续工作」启动页需要有界只读摘要；section 失败彼此隔离，不得触发 mutation。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_workbench_launch_summary → WorkbenchLaunchSummaryWire。
   */
  getLaunchSummary: (): Promise<WorkbenchLaunchSummaryWire> =>
    invokeDecoded('get_workbench_launch_summary', undefined, workbenchLaunchSummaryDecoder),

  remote: {
    /** 列出指定局域网设备允许浏览的 Workbench 根目录。 */
    roots: (deviceId: string) =>
      invoke<WorkbenchRemoteRoot[]>('list_workbench_remote_roots', { deviceId }),

    /** 列出指定局域网设备远端路径下的一级目录项。 */
    listDir: (deviceId: string, path: string) =>
      invoke<WorkbenchRemoteDirectoryEntry[]>('list_workbench_remote_dir', { deviceId, path }),

    /** 获取指定局域网设备远端路径的信息。 */
    info: (deviceId: string, path: string) =>
      invoke<WorkbenchRemotePathInfo>('get_workbench_remote_path_info', { deviceId, path }),

    /**
     * Business Logic（为什么需要这个函数）:
     *   打开远端项目目录后写入最近项目列表，损坏 DTO 不得进入项目 rail。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded open_workbench_remote_project → WorkbenchProject。
     */
    openProject: (deviceId: string, path: string) =>
      invokeDecoded('open_workbench_remote_project', { deviceId, path }, workbenchProjectDecoder),
  },

  worktrees: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   worktree strip 与 Git 操作依赖完整 worktree 列表，损坏 status 不得写入页面状态。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded list_workbench_worktrees → WorkbenchWorktree[]。
     */
    list: (projectId: string) =>
      invokeDecoded('list_workbench_worktrees', { projectId }, workbenchWorktreesDecoder),

    /** 从项目创建一个新的 Git worktree 和分支。 */
    create: (projectId: string, branchName: string, baseBranch?: string | null) =>
      invokeDecoded(
        'create_workbench_worktree',
        {
          projectId,
          branchName,
          baseBranch: baseBranch ?? null,
        },
        workbenchWorktreeDecoder,
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   Commit 必须带稳定 clientOperationId，返回 typed envelope 供 timeout 后对账。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded commit_workbench_worktree → WorkbenchMutationEnvelope<WorkbenchWorktree>。
     */
    commit: (
      worktreeId: string,
      message: string | null | undefined,
      clientOperationId: string,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>> =>
      invokeDecoded(
        'commit_workbench_worktree',
        {
          worktreeId,
          message: message ?? null,
          clientOperationId,
        },
        workbenchMutationEnvelopeDecoder(workbenchWorktreeDecoder),
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   Push 必须带 clientOperationId 并返回 envelope，禁止 timeout 后盲重放。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded push_workbench_worktree → WorkbenchMutationEnvelope<WorkbenchWorktree>。
     */
    push: (
      worktreeId: string,
      clientOperationId: string,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>> =>
      invokeDecoded(
        'push_workbench_worktree',
        { worktreeId, clientOperationId },
        workbenchMutationEnvelopeDecoder(workbenchWorktreeDecoder),
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   Merge 返回 envelope 包装的阶段结果，uncertain transport 走 unknown。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded merge_workbench_worktree → WorkbenchMutationEnvelope<WorkbenchMergeResult>。
     */
    merge: (
      worktreeId: string,
      clientOperationId: string,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchMergeResult>> =>
      invokeDecoded(
        'merge_workbench_worktree',
        { worktreeId, clientOperationId },
        workbenchMutationEnvelopeDecoder(workbenchMergeResultDecoder),
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   Remove 带 force 与 clientOperationId，成功 value 为 {ok, worktreeId}。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded remove_workbench_worktree → WorkbenchMutationEnvelope<{ok,worktreeId}>。
     */
    remove: (
      worktreeId: string,
      force: boolean,
      clientOperationId: string,
    ): Promise<WorkbenchMutationEnvelope<{ ok: boolean; worktreeId: string }>> =>
      invokeDecoded(
        'remove_workbench_worktree',
        {
          worktreeId,
          force,
          clientOperationId,
        },
        workbenchMutationEnvelopeDecoder(workbenchRemoveResultDecoder),
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   unknown 后按 clientOperationId 查询 owning sidecar ledger 取得 intent/state。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded get_workbench_mutation_operation → WorkbenchMutationOperation | null。
     */
    getMutationOperation: (
      clientOperationId: string,
    ): Promise<WorkbenchMutationOperation | null> =>
      invokeDecoded(
        'get_workbench_mutation_operation',
        { clientOperationId },
        nullableDecoder(workbenchMutationOperationDecoder),
      ),
  },

  git: {
    /** 列出当前 worktree 最近 Git 提交历史。 */
    listCommits: (projectId: string, worktreeId?: string | null, limit = 30) =>
      invoke<WorkbenchGitCommit[]>('list_workbench_git_commits', {
        projectId,
        worktreeId: worktreeId ?? null,
        limit,
      }),
  },

  browser: {
    /** 自动发现当前项目/worktree 可预览的本机 dev server 候选。 */
    discover: (
      projectId: string,
      worktreeId?: string | null,
    ): Promise<WorkbenchBrowserDiscovery> =>
      invoke<WorkbenchBrowserDiscovery>('discover_workbench_browser_targets', {
        projectId,
        worktreeId: worktreeId ?? null,
      }),

    /** 为用户选择的 targetUrl 创建浏览器预览代理 session。 */
    createPreview: (
      projectId: string,
      worktreeId: string | null | undefined,
      targetUrl: string,
    ): Promise<WorkbenchBrowserPreview> =>
      invoke<WorkbenchBrowserPreview>('create_workbench_browser_preview', {
        projectId,
        worktreeId: worktreeId ?? null,
        targetUrl,
      }),
  },

  sessions: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   终端 tabs 依赖 session 列表；残缺 DTO 不得覆盖 active session。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded list_workbench_sessions → WorkbenchSession[]。
     */
    list: (projectId?: string) =>
      invokeDecoded(
        'list_workbench_sessions',
        { projectId: projectId ?? null },
        workbenchSessionsDecoder,
      ),

    /** 在指定项目下创建一个 terminal window。 */
    create: (projectId: string, initialSize?: WorkbenchTerminalSize, worktreeId?: string | null) =>
      invokeDecoded(
        'create_workbench_session',
        {
          projectId,
          worktreeId: worktreeId ?? null,
          initialCols: initialSize?.cols ?? null,
          initialRows: initialSize?.rows ?? null,
        },
        workbenchSessionDecoder,
      ),

    /** 向指定 terminal window 的 PTY attach 写入输入数据。 */
    writeInput: (sessionId: string, data: string) =>
      invoke<{ ok: boolean; sessionId: string }>('write_workbench_session_input', {
        sessionId,
        data,
      }),

    /** 调整终端 PTY 行列数。 */
    resize: (sessionId: string, cols: number, rows: number) =>
      invoke<{ ok: boolean; sessionId: string }>('resize_workbench_session', {
        sessionId,
        cols,
        rows,
      }),

    /** 聚焦 terminal window，并同步切换底层 tmux current window。 */
    focus: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('focus_workbench_session', {
        sessionId,
      }),

    /** 读取当前 worktree tmux current window 对应的 terminal session。 */
    focused: (projectId: string, worktreeId?: string | null) =>
      invoke<{ sessionId: string | null }>('get_focused_workbench_session', {
        projectId,
        worktreeId: worktreeId ?? null,
      }),

    /** 在当前 tmux window 内创建一个 pane。 */
    splitPane: (sessionId: string, direction: WorkbenchPaneSplitDirection) =>
      invoke<{ ok: boolean; sessionId: string; direction: WorkbenchPaneSplitDirection }>(
        'split_workbench_pane',
        {
          sessionId,
          direction,
        },
      ),

    /** 切换到当前 tmux window 的下一个 pane。 */
    switchPane: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('switch_workbench_pane', {
        sessionId,
      }),

    /** 确保当前 tmux active pane 以单 pane 视图显示。 */
    zoomPane: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('zoom_workbench_pane', {
        sessionId,
      }),

    /** 关闭当前 tmux pane；最后一个 pane 会关闭所属 terminal window。 */
    closePane: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string; closedWindow: boolean }>('close_workbench_pane', {
        sessionId,
      }),

    /** 关闭终端 tab，并释放后端 PTY 资源。 */
    close: (sessionId: string) =>
      invoke<{ ok: boolean; sessionId: string }>('close_workbench_session', {
        sessionId,
      }),

    /**
     * Business Logic（为什么需要这个函数）:
     *   重命名 terminal window 后 tab 标签依赖返回 session；残缺 name 不得覆盖当前 tab。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded rename_workbench_session → WorkbenchSession。
     */
    rename: (sessionId: string, name: string) =>
      invokeDecoded('rename_workbench_session', { sessionId, name }, workbenchSessionDecoder),
  },

  claudeSessions: {
    /** 搜索当前 worktree 范围的 Claude session；query 为空返回全部（倒序，最多 50 条）。 */
    search: (projectId: string, worktreeId: string | null, query: string) =>
      invoke<SessionSearchHit[]>('search_claude_sessions', {
        projectId,
        worktreeId,
        query,
      }),

    /** 取某 Claude session 的最近 20 条对话 + 元信息，用于 preview 面板。 */
    preview: (projectId: string, worktreeId: string | null, sessionId: string) =>
      invoke<SessionPreview>('get_claude_session_preview', {
        projectId,
        worktreeId,
        sessionId,
      }),

    /** 新建 terminal window 并注入 `claude --resume <sessionId>` 命令，返回新建 window 的 sessionId。 */
    resume: (projectId: string, worktreeId: string | null, sessionId: string) =>
      invoke<ResumeClaudeSessionResult>('resume_claude_session', {
        projectId,
        worktreeId,
        sessionId,
      }),
  },

  files: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   文件树展开依赖目录节点；残缺 path/kind 不得写入 childrenByPath。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded list_workbench_dir → WorkbenchFileNode[]；path 空表示项目根。
     */
    listDir: (projectId: string, path?: string, worktreeId?: string | null) =>
      invokeDecoded(
        'list_workbench_dir',
        {
          projectId,
          worktreeId: worktreeId ?? null,
          path: path ?? null,
        },
        workbenchFileNodesDecoder,
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   选中路径后刷新检查器元信息，错误 kind/nullability 不得污染文件树。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded get_workbench_path_info → WorkbenchPathInfo。
     */
    info: (projectId: string, path: string, worktreeId?: string | null) =>
      invokeDecoded(
        'get_workbench_path_info',
        {
          projectId,
          worktreeId: worktreeId ?? null,
          path,
        },
        workbenchPathInfoDecoder,
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   打开文件写入 tab 的能力与 baseHash 基线，损坏 payload 不得进入编辑器。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded open_workbench_file → WorkbenchOpenFile。
     */
    open: (projectId: string, path: string, worktreeId?: string | null) =>
      invokeDecoded(
        'open_workbench_file',
        {
          projectId,
          worktreeId: worktreeId ?? null,
          path,
        },
        workbenchOpenFileDecoder,
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   保存成功后的 baseHash/metadata 是乐观锁基线，损坏结果不得写入 tab。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded save_workbench_text_file → WorkbenchSaveTextResult。
     */
    saveText: (
      projectId: string,
      path: string,
      content: string,
      baseHash: string,
      worktreeId?: string | null,
    ) =>
      invokeDecoded(
        'save_workbench_text_file',
        {
          projectId,
          worktreeId: worktreeId ?? null,
          path,
          content,
          baseHash,
        },
        workbenchSaveTextResultDecoder,
      ),

    /** 格式化 JSON/TOML/YAML 内容，不直接保存文件。 */
    formatStructured: (kind: 'json' | 'toml' | 'yaml', content: string) =>
      invoke<WorkbenchFormatResult>('format_workbench_structured_content', { kind, content }),

    /** 重新预览 SQLite 文件的指定表。 */
    previewSqlite: (
      projectId: string,
      path: string,
      table?: string | null,
      limitRows = 100,
      worktreeId?: string | null,
    ) =>
      invoke<WorkbenchSqlitePreview>('preview_workbench_sqlite', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
        table: table ?? null,
        limitRows,
      }),

    /** 读取 HTML 预览中的项目内相对资源，返回可内联 data URL。 */
    previewHtmlAsset: (
      projectId: string,
      documentPath: string,
      assetPath: string,
      worktreeId?: string | null,
    ) =>
      invoke<WorkbenchHtmlAsset>('preview_workbench_html_asset', {
        projectId,
        worktreeId: worktreeId ?? null,
        documentPath,
        assetPath,
      }),

    /** 在父目录下创建空文件。 */
    createFile: (projectId: string, parentPath: string, name: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('create_workbench_file', {
        projectId,
        worktreeId: worktreeId ?? null,
        parentPath,
        name,
      }),

    /** 在父目录下创建文件夹。 */
    createDir: (projectId: string, parentPath: string, name: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('create_workbench_dir', {
        projectId,
        worktreeId: worktreeId ?? null,
        parentPath,
        name,
      }),

    /** 重命名项目内文件或文件夹。 */
    renamePath: (projectId: string, path: string, newName: string, worktreeId?: string | null) =>
      invoke<WorkbenchPathInfo>('rename_workbench_path', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
        newName,
      }),

    /** 删除项目内文件或文件夹。 */
    deletePath: (projectId: string, path: string, worktreeId?: string | null) =>
      invoke<{ ok: boolean; path: string }>('delete_workbench_path', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }),
  },
};
