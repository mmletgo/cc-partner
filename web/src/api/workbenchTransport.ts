/**
 * Workbench Transport 抽象。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端继续通过 Tauri invoke 使用 Workbench；移动端 `/mobile` 运行在普通浏览器中，只能通过同源 HTTP。
 *   页面层需要一套稳定接口切换底层通道，避免把 Tauri/HTTP 分支散落到业务组件。
 *
 * Code Logic（这个模块做什么）:
 *   定义 WorkbenchTransport 接口，并提供映射到现有 `workbenchApi` 的 Tauri adapter。
 *   HTTP adapter 在 `workbenchHttp.ts` 中实现同一接口。
 */

import { workbenchApi } from './workbench';
import type { WorkbenchPaneSplitDirection } from './workbench';
import { promptOptimizerApi } from './promptOptimizer';
import type {
  WorkbenchFileNode,
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchGitCommit,
  WorkbenchMergeResult,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchSessionReplay,
  WorkbenchWorktree,
  PromptOptimizerFillLanguage,
  AgentRuntimeSnapshot,
} from '@/lib/types';
import type { LanFleetSnapshot } from '@/lib/types/lanFleet';

/**
 * Workbench 终端初始尺寸。
 *
 * Business Logic（为什么需要这个接口）:
 *   创建 terminal window 前，前端会测量当前 xterm viewport，避免后端用默认列宽启动导致 TUI 首屏错位。
 *
 * Code Logic（字段说明）:
 *   cols/rows 分别表示终端列数和行数；缺省时由后端使用默认值。
 */
export interface WorkbenchTerminalSize {
  cols: number;
  rows: number;
}

/**
 * Workbench 底层通信接口。
 *
 * Business Logic（为什么需要这个接口）:
 *   `/workbench` 桌面页和 `/mobile` 浏览器页需要共享项目、worktree、终端、文件和 Git 操作语义。
 *
 * Code Logic（字段说明）:
 *   每个分组对应 Workbench 的一类后端能力；方法参数保持 camelCase，并返回 `src/lib/types.ts` 中的 DTO。
 */
export interface WorkbenchTransport {
  projects: {
    list: () => Promise<WorkbenchProject[]>;
    open: (path: string) => Promise<WorkbenchProject>;
  };
  worktrees: {
    list: (projectId: string) => Promise<WorkbenchWorktree[]>;
    create: (
      projectId: string,
      branchName: string,
      baseBranch?: string | null,
    ) => Promise<WorkbenchWorktree>;
    commit: (worktreeId: string, message?: string | null) => Promise<WorkbenchWorktree>;
    push: (worktreeId: string) => Promise<WorkbenchWorktree>;
    merge: (worktreeId: string) => Promise<WorkbenchMergeResult>;
    remove: (worktreeId: string, force?: boolean) => Promise<{ ok: boolean; worktreeId: string }>;
  };
  sessions: {
    list: (projectId?: string | null) => Promise<WorkbenchSession[]>;
    create: (
      projectId: string,
      initialSize?: WorkbenchTerminalSize,
      worktreeId?: string | null,
    ) => Promise<WorkbenchSession>;
    resize: (
      sessionId: string,
      cols: number,
      rows: number,
    ) => Promise<{ ok: boolean; sessionId: string }>;
    replay: (sessionId: string) => Promise<WorkbenchSessionReplay>;
    focus: (
      sessionId: string,
      streamActive?: boolean,
    ) => Promise<{ ok: boolean; sessionId: string }>;
    focused: (
      projectId: string,
      worktreeId?: string | null,
    ) => Promise<{ sessionId: string | null }>;
    splitPane: (
      sessionId: string,
      direction: WorkbenchPaneSplitDirection,
    ) => Promise<{ ok: boolean; sessionId: string; direction: WorkbenchPaneSplitDirection }>;
    switchPane: (sessionId: string) => Promise<{ ok: boolean; sessionId: string }>;
    zoomPane: (sessionId: string) => Promise<{ ok: boolean; sessionId: string }>;
    closePane: (
      sessionId: string,
    ) => Promise<{ ok: boolean; sessionId: string; closedWindow: boolean }>;
    close: (sessionId: string) => Promise<{ ok: boolean; sessionId: string }>;
  };
  files: {
    listDir: (
      projectId: string,
      path?: string | null,
      worktreeId?: string | null,
    ) => Promise<WorkbenchFileNode[]>;
    info: (
      projectId: string,
      path: string,
      worktreeId?: string | null,
    ) => Promise<WorkbenchPathInfo>;
    open: (
      projectId: string,
      path: string,
      worktreeId?: string | null,
    ) => Promise<WorkbenchOpenFile>;
    saveText: (
      projectId: string,
      path: string,
      content: string,
      baseHash: string,
      worktreeId?: string | null,
    ) => Promise<WorkbenchSaveTextResult>;
  };
  git: {
    listCommits: (
      projectId: string,
      worktreeId?: string | null,
      limit?: number,
    ) => Promise<WorkbenchGitCommit[]>;
  };
  browser: {
    discover: (
      projectId: string,
      worktreeId?: string | null,
    ) => Promise<WorkbenchBrowserDiscovery>;
    createPreview: (
      projectId: string,
      worktreeId: string | null | undefined,
      targetUrl: string,
    ) => Promise<WorkbenchBrowserPreview>;
    startVerification?: (
      previewId: string,
      requestId: string,
    ) => Promise<import('@/lib/types').BrowserVerificationRun>;
    getVerification?: (
      runId: string,
    ) => Promise<import('@/lib/types').BrowserVerificationRun>;
    cancelVerification?: (
      runId: string,
    ) => Promise<import('@/lib/types').BrowserVerificationRun>;
    getVerificationArtifact?: (
      runId: string,
      artifactId: string,
    ) => Promise<import('@/lib/types').BrowserVerificationArtifact>;
  };
  prompt: {
    streamToTerminal: (
      prompt: string,
      options: {
        workingDirectory?: string | null;
        targetLanguage: PromptOptimizerFillLanguage;
        sessionId: string;
      },
    ) => Promise<{ ok: boolean; sessionId: string }>;
  };
  /**
   * Agent runtime 投影（可选：旧 transport 实现可省略）。
   *
   * Business Logic（为什么需要这个分组）:
   *   Desktop/Mobile 共享 snapshot 拉取语义，供 A2 handshake 使用。
   *
   * Code Logic（字段说明）:
   *   getSnapshot 可选 projectId 过滤 active sessions。
   */
  agentRuntime?: {
    getSnapshot: (projectId?: string | null) => Promise<AgentRuntimeSnapshot>;
  };
  /**
   * LAN Agent Fleet（可选：旧 transport 可省略）。
   *
   * Business Logic（为什么需要这个分组）:
   *   Desktop/Mobile 共享控制设备聚合 snapshot。
   *
   * Code Logic（字段说明）:
   *   getSnapshot 无参数，返回全局 LanFleetSnapshot。
   */
  lanFleet?: {
    getSnapshot: () => Promise<LanFleetSnapshot>;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端已经通过 Tauri 事件实时缓存终端输出，不需要额外 HTTP replay；但 transport 接口必须保持一致。
 *
 * Code Logic（这个函数做什么）:
 *   按请求的 sessionId 返回空 replay DTO，表示没有需要从 HTTP 通道补齐的历史输出。
 */
function createEmptyDesktopReplay(sessionId: string): WorkbenchSessionReplay {
  return {
    sessionId,
    buffer: '',
    truncated: false,
    lastSeq: 0,
    // 空 desktop transport 无 owner 权威，不得冒充 live stream。
  };
}

/**
 * Tauri Workbench Transport。
 *
 * Business Logic（为什么需要这个常量）:
 *   桌面端仍应使用现有 invoke adapter，不能因为移动端 HTTP 支持改变原有 Workbench 通信路径。
 *
 * Code Logic（这个常量做什么）:
 *   将 WorkbenchTransport 方法逐一映射到 `workbenchApi`；projects.open 复用桌面 add/open 项目语义。
 */
export const tauriWorkbenchTransport: WorkbenchTransport = {
  projects: {
    list: () => workbenchApi.projects.list(),
    open: (path) => workbenchApi.projects.add(path),
  },
  worktrees: {
    list: (projectId) => workbenchApi.worktrees.list(projectId),
    create: (projectId, branchName, baseBranch) =>
      workbenchApi.worktrees.create(projectId, branchName, baseBranch),
    // Business Logic: transport 旧签名仍返回权威 value；envelope unknown 暂抛错，完整对账由 desktop controller / T6-T7 处理。
    // Code Logic: 生成临时 clientOperationId 调用 workbenchApi；succeeded 解包 value，unknown 抛中文错误。
    commit: async (worktreeId, message) => {
      const envelope = await workbenchApi.worktrees.commit(
        worktreeId,
        message,
        crypto.randomUUID(),
      );
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    push: async (worktreeId) => {
      const envelope = await workbenchApi.worktrees.push(worktreeId, crypto.randomUUID());
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    merge: async (worktreeId) => {
      const envelope = await workbenchApi.worktrees.merge(worktreeId, crypto.randomUUID());
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    remove: async (worktreeId, force) => {
      const envelope = await workbenchApi.worktrees.remove(
        worktreeId,
        force ?? false,
        crypto.randomUUID(),
      );
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
  },
  sessions: {
    list: (projectId) => workbenchApi.sessions.list(projectId ?? undefined),
    create: (projectId, initialSize, worktreeId) =>
      workbenchApi.sessions.create(projectId, initialSize, worktreeId),
    resize: (sessionId, cols, rows) => workbenchApi.sessions.resize(sessionId, cols, rows),
    replay: async (sessionId) => createEmptyDesktopReplay(sessionId),
    focus: (sessionId, streamActive) => workbenchApi.sessions.focus(sessionId, streamActive),
    focused: (projectId, worktreeId) => workbenchApi.sessions.focused(projectId, worktreeId),
    splitPane: (sessionId, direction) => workbenchApi.sessions.splitPane(sessionId, direction),
    switchPane: (sessionId) => workbenchApi.sessions.switchPane(sessionId),
    zoomPane: (sessionId) => workbenchApi.sessions.zoomPane(sessionId),
    closePane: (sessionId) => workbenchApi.sessions.closePane(sessionId),
    close: (sessionId) => workbenchApi.sessions.close(sessionId),
  },
  files: {
    listDir: (projectId, path, worktreeId) =>
      workbenchApi.files.listDir(projectId, path ?? undefined, worktreeId),
    info: (projectId, path, worktreeId) => workbenchApi.files.info(projectId, path, worktreeId),
    open: (projectId, path, worktreeId) => workbenchApi.files.open(projectId, path, worktreeId),
    saveText: (projectId, path, content, baseHash, worktreeId) =>
      workbenchApi.files.saveText(projectId, path, content, baseHash, worktreeId),
  },
  git: {
    listCommits: (projectId, worktreeId, limit) =>
      workbenchApi.git.listCommits(projectId, worktreeId, limit),
  },
  agentRuntime: {
    getSnapshot: (projectId) => workbenchApi.agentRuntime.getSnapshot(projectId),
  },
  lanFleet: {
    getSnapshot: () => workbenchApi.lanFleet.getSnapshot(),
  },
  browser: {
    discover: (projectId, worktreeId) =>
      workbenchApi.browser.discover(projectId, worktreeId ?? null),
    createPreview: (projectId, worktreeId, targetUrl) =>
      workbenchApi.browser.createPreview(projectId, worktreeId ?? null, targetUrl),
    startVerification: (previewId, requestId) =>
      workbenchApi.browser.startVerification(previewId, requestId),
    getVerification: (runId) => workbenchApi.browser.getVerification(runId),
    cancelVerification: (runId) => workbenchApi.browser.cancelVerification(runId),
    getVerificationArtifact: (runId, artifactId) =>
      workbenchApi.browser.getVerificationArtifact(runId, artifactId),
  },
  prompt: {
    streamToTerminal: (prompt, options) =>
      promptOptimizerApi.streamToTerminal(prompt, {
        workingDirectory: options.workingDirectory,
        targetLanguage: options.targetLanguage,
        sessionId: options.sessionId,
      }),
  },
};
