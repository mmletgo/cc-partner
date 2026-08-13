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
  workbenchProjectNoteDecoder,
  workbenchProjectsDecoder,
  workbenchRemoveResultDecoder,
  workbenchRepairHookFailureDecoder,
  workbenchSaveTextResultDecoder,
  sessionSearchResultDecoder,
  workbenchSessionDecoder,
  workbenchSessionReplayDecoder,
  workbenchSessionsDecoder,
  workbenchWorktreeDecoder,
  workbenchWorktreesDecoder,
} from '@/lib/schemas/workbench';
import { agentRuntimeSnapshotDecoder } from '@/lib/schemas/agentRuntime';
import {
  agentLedgerPageDecoder,
  agentLedgerSummaryDecoder,
  clearAgentLedgerResultDecoder,
} from '@/lib/schemas/agentLedger';
import { lanFleetSnapshotDecoder } from '@/lib/schemas/lanFleet';
import type {
  ResumeClaudeSessionResult,
  SessionPreview,
  SessionSearchResult,
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
  WorkbenchRepairHookFailureDto,
  WorkbenchSqlitePreview,
  WorkbenchWorktree,
} from '@/lib/types';
import type { AgentRuntimeSnapshot } from '@/lib/types/agentRuntime';
import type {
  AgentLedgerListParams,
  AgentLedgerPage,
  AgentLedgerSummarizeParams,
  AgentLedgerSummary,
} from '@/lib/types/agentLedger';
import type { LanFleetSnapshot } from '@/lib/types/lanFleet';
import type { WorkbenchLaunchSummaryWire } from '@/lib/types';

/** Agent runtime Tauri live event 名（对齐 A1 emit_agent_runtime_changed）。 */
export const WORKBENCH_AGENT_RUNTIME_EVENT = 'workbench:agent-runtime' as const;

/** N1 runtime gap Tauri event 名（owner 切换/ring gap 时重 handshake）。 */
export const BACKEND_RUNTIME_GAP_EVENT = 'backend:runtime-gap' as const;

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

    /**
     * 拖拽重排项目列表顺序。
     *
     * Business Logic: 桌面侧栏拖拽后整表持久化；返回投影后的完整列表。
     * Code Logic: invoke reorder_workbench_projects + projects decoder。
     */
    reorder: (orderedIds: string[]) =>
      invokeDecoded(
        'reorder_workbench_projects',
        { orderedIds },
        workbenchProjectsDecoder,
      ),
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

    /**
     * Business Logic（为什么需要这个函数）:
     *   failedHook envelope 之后用户点「让 AI 修复」时调用：在该 worktree 终端启动可见
     *   Claude agent 修复钩子失败的根因（禁止 --no-verify / git push）。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded repair_worktree_hook_failure → WorkbenchRepairHookFailureDto（agent/terminal id）。
     *   V1 仅本机 worktree；远端项目返回可操作错误，由调用方降级处理。
     */
    repairHookFailure: (
      worktreeId: string,
      hookFailure: import('@/lib/types').WorkbenchHookFailure,
    ): Promise<WorkbenchRepairHookFailureDto> =>
      invokeDecoded(
        'repair_worktree_hook_failure',
        { worktreeId, hookFailure },
        workbenchRepairHookFailureDecoder,
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

    /**
     * Business Logic（为什么需要这个函数）:
     *   一键验证当前 live preview，只传 previewId。
     *
     * Code Logic（这个函数做什么）:
     *   invoke start_workbench_browser_verification。
     */
    startVerification: (
      previewId: string,
      requestId: string,
    ): Promise<import('@/lib/types').BrowserVerificationRun> =>
      invoke('start_workbench_browser_verification', {
        previewId,
        requestId,
        commands: null,
      }),

    /**
     * Business Logic（为什么需要这个函数）:
     *   轮询验证状态。
     *
     * Code Logic（这个函数做什么）:
     *   invoke get_workbench_browser_verification。
     */
    getVerification: (runId: string): Promise<import('@/lib/types').BrowserVerificationRun> =>
      invoke('get_workbench_browser_verification', { runId }),

    /**
     * Business Logic（为什么需要这个函数）:
     *   取消验证。
     *
     * Code Logic（这个函数做什么）:
     *   invoke cancel_workbench_browser_verification。
     */
    cancelVerification: (runId: string): Promise<import('@/lib/types').BrowserVerificationRun> =>
      invoke('cancel_workbench_browser_verification', { runId }),

    /**
     * Business Logic（为什么需要这个函数）:
     *   拉取截图 artifact。
     *
     * Code Logic（这个函数做什么）:
     *   invoke get_workbench_browser_verification_artifact。
     */
    getVerificationArtifact: (
      runId: string,
      artifactId: string,
    ): Promise<import('@/lib/types').BrowserVerificationArtifact> =>
      invoke('get_workbench_browser_verification_artifact', { runId, artifactId }),
  },

  /**
   * workspace layout / safe restore。
   *
   * Business Logic（为什么需要这个分组）:
   *   自动保存结构现场与启动 preflight/apply；layout 无 terminal 正文/命令字段。
   *
   * Code Logic（这个分组做什么）:
   *   invoke get/save/list/delete/preflight/apply 对应 Rust layout 命令。
   */
  layout: {
    /** 按 slot 读取 layout（默认 desktop:auto）。 */
    get: (slotKey?: string | null) =>
      invoke<import('@/pages/Workbench/workspaceLayout').WorkspaceLayout | null>(
        'get_workspace_layout',
        { slotKey: slotKey ?? null },
      ),

    /** CAS 保存 layout。 */
    save: (
      draft: import('@/pages/Workbench/workspaceLayout').WorkspaceLayoutDraft,
      expectedRevision: number | null,
    ) =>
      invoke<import('@/pages/Workbench/workspaceLayout').WorkspaceLayout>(
        'save_workspace_layout',
        { draft, expectedRevision },
      ),

    /** 列出命名 snapshot。 */
    listNamed: () =>
      invoke<import('@/pages/Workbench/workspaceLayout').WorkspaceLayout[]>(
        'list_named_workspace_layouts',
      ),

    /** 删除命名 snapshot。 */
    deleteNamed: (id: string) =>
      invoke<void>('delete_named_workspace_layout', { id }),

    /** side-effect-free preflight。 */
    preflight: (slotKey?: string | null, layoutId?: string | null) =>
      invoke<import('@/pages/Workbench/workspaceRestore').WorkspaceRestorePlan>(
        'preflight_workspace_restore_cmd',
        { slotKey: slotKey ?? null, layoutId: layoutId ?? null },
      ),

    /** 校验 revision 并执行 safeAttach 列表项；返回改写后的 actions（失败 attach→skip）。 */
    apply: (plan: import('@/pages/Workbench/workspaceRestore').WorkspaceRestorePlan) =>
      invoke<{
        restoreId: string;
        status: string;
        restoredCount: number;
        skippedCount: number;
        actions: import('@/pages/Workbench/workspaceRestore').WorkspaceRestoreAction[];
      }>('apply_workspace_restore_cmd', { plan }),
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

    /**
     * Business Logic（为什么需要这个函数）:
     *   桌面 Provider 在 listener 就绪后需要 baseline replay，用 lastSeq 做 stream cutover。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded replay_workbench_session → WorkbenchSessionReplay。
     */
    replay: (sessionId: string) =>
      invokeDecoded(
        'replay_workbench_session',
        { sessionId },
        workbenchSessionReplayDecoder,
      ),

    /**
     * Business Logic（为什么需要这个函数）:
     *   桌面 xterm 输入只等待 GUI 本机有界队列接纳，不能等待远端 RTT。
     *
     * Code Logic（这个函数做什么）:
     *   调用专用 enqueue invoke；实际发送与 ACK 由 Rust 常驻 WebSocket actor 异步处理。
     */
    enqueueInput: (sessionId: string, data: string) =>
      invoke<{ accepted: boolean; sessionId: string }>('enqueue_workbench_terminal_input', {
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

    /** 聚焦 terminal window；streamActive=false 仅 compare-and-clear 该窗口的远程正文流。 */
    focus: (sessionId: string, streamActive = true) =>
      invoke<{ ok: boolean; sessionId: string }>('focus_workbench_session', {
        sessionId,
        streamActive,
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

    /**
     * 按终端字符格坐标选中 tmux pane。
     *
     * Business Logic（为什么需要这个方法）:
     *   桌面用户在多 pane 终端内点击目标 pane 时，前端只能提供 (col, row)，
     *   由后端读取 tmux 真实 pane 几何完成命中并 select-pane；与相对 `.+` 循环不同，
     *   绝对坐标结果可重放。
     */
    selectPaneAt: (sessionId: string, col: number, row: number) =>
      invoke<{ ok: boolean; sessionId: string; paneId: string | null; changed: boolean }>(
        'select_workbench_pane_at',
        { sessionId, col, row },
      ),

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
    /**
     * Business Logic（为什么需要这个函数）:
     *   Command Palette 需要有界 session 搜索结果，并感知索引截断诊断；
     *   source 选择 Claude / Codex / OpenCode。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded search_claude_sessions → SessionSearchResult（items/truncated/diagnostics）。
     */
    search: (
      projectId: string,
      worktreeId: string | null,
      query: string,
      source: 'claude' | 'codex' | 'opencode' = 'claude',
    ): Promise<SessionSearchResult> =>
      invokeDecoded(
        'search_claude_sessions',
        {
          projectId,
          worktreeId,
          query,
          source,
        },
        sessionSearchResultDecoder,
      ),

    /** 取某 agent session 的最近 20 条对话 + 元信息，用于 preview 面板。 */
    preview: (
      projectId: string,
      worktreeId: string | null,
      sessionId: string,
      source: 'claude' | 'codex' | 'opencode' = 'claude',
    ) =>
      invoke<SessionPreview>('get_claude_session_preview', {
        projectId,
        worktreeId,
        sessionId,
        source,
      }),

    /** 新建 terminal window 并注入对应 agent resume 命令，返回新建 window 的 sessionId。 */
    resume: (
      projectId: string,
      worktreeId: string | null,
      sessionId: string,
      source: 'claude' | 'codex' | 'opencode' = 'claude',
    ) =>
      invoke<ResumeClaudeSessionResult>('resume_claude_session', {
        projectId,
        worktreeId,
        sessionId,
        source,
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

  /**
   * Agent runtime 投影（A1 snapshot + A2 handshake 消费）。
   *
   * Business Logic（为什么需要这个分组）:
   *   Desktop Gap 恢复与进入项目需要 owner 权威 active Agent baseline。
   *
   * Code Logic（这个分组做什么）:
   *   invokeDecoded get_agent_runtime_snapshot，可选 projectId 过滤。
   */
  agentRuntime: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   listener-first handshake 在注册后必须拉 snapshot 建立 asOfSequence baseline。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded `get_agent_runtime_snapshot` → AgentRuntimeSnapshot。
     */
    getSnapshot: (projectId?: string | null): Promise<AgentRuntimeSnapshot> =>
      invokeDecoded(
        'get_agent_runtime_snapshot',
        { projectId: projectId ?? null },
        agentRuntimeSnapshotDecoder,
      ),
  },

  /**
   * LAN Agent Fleet 只读聚合。
   *
   * Business Logic（为什么需要这个分组）:
   *   Rail/Fleet 需要一次拉取已保存 shortcut 的跨设备摘要。
   *
   * Code Logic（这个分组做什么）:
   *   invokeDecoded get_workbench_lan_fleet。
   */
  lanFleet: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   可见 reconcile / event invalidation 后刷新 Fleet display。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded `get_workbench_lan_fleet` → LanFleetSnapshot。
     */
    getSnapshot: (): Promise<LanFleetSnapshot> =>
      invokeDecoded('get_workbench_lan_fleet', undefined, lanFleetSnapshotDecoder),
  },

  /**
   * Agent Metadata Ledger（本机明细 / summary / 清除）。
   *
   * Business Logic（为什么需要这个分组）:
   *   drawer 与 Settings 一键清除需要 metadata-only 历史 API。
   *
   * Code Logic（这个分组做什么）:
   *   list / summarize / clear 三个 invoke。
   */
  agentLedger: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   本机 drawer 分页加载最近 metadata 历史。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded `list_agent_ledger`。
     */
    list: (req: AgentLedgerListParams = {}): Promise<AgentLedgerPage> =>
      invokeDecoded('list_agent_ledger', { req }, agentLedgerPageDecoder),

    /**
     * Business Logic（为什么需要这个函数）:
     *   本机时间窗聚合。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded `summarize_agent_ledger`。
     */
    summarize: (req: AgentLedgerSummarizeParams): Promise<AgentLedgerSummary> =>
      invokeDecoded('summarize_agent_ledger', { req }, agentLedgerSummaryDecoder),

    /**
     * Business Logic（为什么需要这个函数）:
     *   Settings 一键清除只删 ledger 行。
     *
     * Code Logic（这个函数做什么）:
     *   invokeDecoded `clear_agent_ledger` → 删除行数。
     */
    clear: (): Promise<number> =>
      invokeDecoded('clear_agent_ledger', undefined, clearAgentLedgerResultDecoder),
  },

  /**
   * 项目笔记（本机 SQLite，按 projectId 一份）。
   *
   * Business Logic（为什么需要这个分组）:
   *   右侧检查器「项目笔记」需要 get/save，不写仓库、不代理远端磁盘。
   *
   * Code Logic（这个分组做什么）:
   *   invoke get_workbench_project_note / save_workbench_project_note。
   */
  notes: {
    /** 读取项目笔记；无行返回空正文。 */
    get: (projectId: string) =>
      invokeDecoded('get_workbench_project_note', { projectId }, workbenchProjectNoteDecoder),

    /** 覆盖保存项目笔记。 */
    save: (projectId: string, content: string) =>
      invokeDecoded(
        'save_workbench_project_note',
        { projectId, content },
        workbenchProjectNoteDecoder,
      ),
  },
};
