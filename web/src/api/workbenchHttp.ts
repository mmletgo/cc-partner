/**
 * Workbench HTTP API。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 是普通浏览器 SPA，无法调用 Tauri invoke，需要通过同源 HTTP 访问桌面端暴露的 Workbench routes。
 *
 * Code Logic（这个模块做什么）:
 *   提供 JSON GET/POST helper，并实现与 `WorkbenchTransport` 相同形状的 HTTP adapter。
 */

import type {
  OrchestratorEvidence,
  OrchestratorRuntimeSnapshot,
  OrchestratorTask,
  OrchestratorTaskPromptCompletion,
  OrchestratorTaskView,
  WorkbenchBrowserDiscovery,
  WorkbenchBrowserPreview,
  WorkbenchFileNode,
  WorkbenchGitCommit,
  WorkbenchMergeResult,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchSessionReplay,
  WorkbenchWorktree,
} from '@/lib/types';
import type { WorkbenchPaneSplitDirection } from './workbench';
import type { OrchestratorCreateAction } from './orchestrator';
import {
  OrchestratorRuntimeTransportError,
  toOrchestratorRuntimeTransportError,
} from './orchestratorRuntimeTransportError';
import type { WorkbenchTransport } from './workbenchTransport';

export type HttpCreateOrchestratorTaskAction = OrchestratorCreateAction;

export interface HttpCreateOrchestratorTaskRequest {
  projectId: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  priority?: number;
  createAction?: HttpCreateOrchestratorTaskAction;
  source?: string;
  externalId?: string;
  externalIdentifier?: string;
  externalUrl?: string;
  externalState?: string;
  externalLabels?: string[];
}

export interface HttpCompleteOrchestratorTaskPromptRequest {
  projectId?: string | null;
  prompt: string;
  workingDirectory?: string | null;
}

interface HttpOrchestratorTaskListResponse {
  tasks: OrchestratorTask[];
}

interface HttpOrchestratorTaskViewListResponse {
  views: OrchestratorTaskView[];
}

interface HttpOrchestratorEvidenceListResponse {
  evidence: OrchestratorEvidence[];
}

const MOBILE_WORKBENCH_API_PREFIX = '/api/mobile/workbench';

/**
 * Business Logic（为什么需要这个函数）:
 *   HTTP Workbench route 失败时，移动端页面需要展示后端返回的可读错误，而不是笼统的 fetch status。
 *
 * Code Logic（这个函数做什么）:
 *   读取响应文本，优先解析 JSON 中的 error/message 字段；否则回退到文本、statusText 或 HTTP 状态码。
 */
async function readHttpErrorMessage(response: Response): Promise<string> {
  const fallback = response.statusText || `HTTP ${response.status}`;
  const text = await response.text().catch(() => '');
  const trimmed = text.trim();
  if (!trimmed) return fallback;

  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (parsed && typeof parsed === 'object') {
      const record = parsed as Record<string, unknown>;
      const message = record.error ?? record.message;
      if (typeof message === 'string' && message.trim()) return message;
    }
  } catch {
    return trimmed;
  }

  return trimmed;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTTP helper 需要统一处理非 2xx 状态和 JSON 反序列化，避免每个 API 方法重复样板代码。
 *
 * Code Logic（这个函数做什么）:
 *   检查 Response.ok；失败时抛出带可读消息的 Error，成功时按泛型 T 解析 JSON。
 */
async function parseJsonResponse<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const message = await readHttpErrorMessage(response);
    // HTTP 非 2xx 是协议/业务失败：用 protocol，禁止 hook 因 message 含“连接”误判 offline。
    throw new OrchestratorRuntimeTransportError(message, 'protocol');
  }
  return (await response.json()) as T;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTTP routes 大多通过 POST 接收 camelCase JSON 请求体，移动端需要一个统一入口。
 *
 * Code Logic（这个函数做什么）:
 *   以 application/json 发送 body，解析成功 JSON；非 2xx 时抛出后端 error/message。
 */
export async function postJson<T>(path: string, body: unknown): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });
  } catch (reason) {
    // fetch 本身失败（断网/DNS/CORS 等）是传输层 network，不靠文案匹配。
    throw toOrchestratorRuntimeTransportError(reason, 'network');
  }
  return parseJsonResponse<T>(response);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   少量 Workbench/mobile routes 使用 GET，移动端需要与 POST helper 一致的错误处理。
 *
 * Code Logic（这个函数做什么）:
 *   发起同源 GET 请求，成功时解析 JSON，失败时读取 JSON error/message 或文本后抛 Error。
 */
export async function getJson<T>(path: string): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method: 'GET',
      headers: {
        Accept: 'application/json',
      },
    });
  } catch (reason) {
    throw toOrchestratorRuntimeTransportError(reason, 'network');
  }
  return parseJsonResponse<T>(response);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端创建 Orchestrator 任务可能因为网络波动重试，后端需要稳定非空 clientRequestId 来做幂等去重。
 *
 * Code Logic（这个函数做什么）:
 *   优先使用 crypto.randomUUID；不可用时用时间戳和随机数生成 fallback 字符串，保证请求体仍包含非空 id。
 */
export function createHttpOrchestratorClientRequestId(): string {
  const randomUUID = globalThis.crypto?.randomUUID;
  if (typeof randomUUID === 'function') {
    return randomUUID.call(globalThis.crypto);
  }
  const timePart = Date.now().toString(36);
  const randomPart = Math.random().toString(36).slice(2);
  return `${timePart}-${randomPart}`;
}

/**
 * HTTP Orchestrator Transport。
 *
 * Business Logic（为什么需要这个常量）:
 *   手机端 `/mobile` 需要通过同源 HTTP 操作当前本机项目的 Orchestrator 项目级任务，而不能调用桌面 Tauri invoke。
 *
 * Code Logic（这个常量做什么）:
 *   将任务 list/create/action/evidence 映射到 `/api/orchestrator/tasks/...` routes；create/createView 显式携带
 *   createAction、tracker 预留字段和非空 clientRequestId。
 */
export const httpOrchestratorTransport = {
  tasks: {
    list: async (projectId: string): Promise<OrchestratorTask[]> => {
      const response = await postJson<HttpOrchestratorTaskListResponse>(
        '/api/orchestrator/tasks/list',
        { projectId },
      );
      return response.tasks;
    },
    listViews: async (projectId: string): Promise<OrchestratorTaskView[]> => {
      const response = await postJson<HttpOrchestratorTaskViewListResponse>(
        '/api/orchestrator/task-views/list',
        { projectId },
      );
      return response.views;
    },
    create: (request: HttpCreateOrchestratorTaskRequest): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/create', {
        projectId: request.projectId,
        title: request.title,
        goal: request.goal,
        acceptanceCriteria: request.acceptanceCriteria,
        priority: request.priority ?? 0,
        createAction: request.createAction ?? 'backlog',
        source: request.source,
        externalId: request.externalId,
        externalIdentifier: request.externalIdentifier,
        externalUrl: request.externalUrl,
        externalState: request.externalState,
        externalLabels: request.externalLabels,
        clientRequestId: createHttpOrchestratorClientRequestId(),
      }),
    createView: (request: HttpCreateOrchestratorTaskRequest): Promise<OrchestratorTaskView> =>
      postJson<OrchestratorTaskView>('/api/orchestrator/task-views/create', {
        projectId: request.projectId,
        title: request.title,
        goal: request.goal,
        acceptanceCriteria: request.acceptanceCriteria,
        priority: request.priority ?? 0,
        createAction: request.createAction ?? 'backlog',
        source: request.source,
        externalId: request.externalId,
        externalIdentifier: request.externalIdentifier,
        externalUrl: request.externalUrl,
        externalState: request.externalState,
        externalLabels: request.externalLabels,
        clientRequestId: createHttpOrchestratorClientRequestId(),
      }),
    completePrompt: (
      request: HttpCompleteOrchestratorTaskPromptRequest,
    ): Promise<OrchestratorTaskPromptCompletion> =>
      postJson<OrchestratorTaskPromptCompletion>('/api/orchestrator/tasks/complete-prompt', {
        projectId: request.projectId?.trim() || null,
        prompt: request.prompt.trim(),
        workingDirectory: request.workingDirectory?.trim() || null,
      }),
    queue: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/queue', { taskId }),
    retry: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/retry', { taskId }),
    abort: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/abort', { taskId }),
    listEvidence: async (projectId: string, taskId: string): Promise<OrchestratorEvidence[]> => {
      const response = await postJson<HttpOrchestratorEvidenceListResponse>(
        '/api/orchestrator/tasks/evidence',
        { projectId, taskId },
      );
      return response.evidence;
    },
  },
  /**
   * Business Logic（为什么需要这个方法）:
   *   手机自动化面板需要拉取本机/远端 shortcut 的 runtime snapshot，且不能直连 owning device P2P base URL。
   *
   * Code Logic（这个函数做什么）:
   *   POST `/api/mobile/orchestrator/runtime-snapshot`，body `{projectId}`，返回 camelCase snapshot DTO。
   */
  getRuntimeSnapshot: (projectId: string): Promise<OrchestratorRuntimeSnapshot> =>
    postJson<OrchestratorRuntimeSnapshot>('/api/mobile/orchestrator/runtime-snapshot', {
      projectId,
    }),
} as const;

/**
 * HTTP Workbench Transport。
 *
 * Business Logic（为什么需要这个常量）:
 *   手机浏览器访问 `/mobile` 时必须用同源 HTTP 操作桌面端 Workbench 能力。
 *
 * Code Logic（这个常量做什么）:
 *   将 WorkbenchTransport 分组方法映射到 `/api/workbench/...` routes，保持与桌面 Tauri adapter 相同的业务语义。
 */
export const httpWorkbenchTransport: WorkbenchTransport = {
  projects: {
    list: () => getJson<WorkbenchProject[]>(`${MOBILE_WORKBENCH_API_PREFIX}/projects/list`),
    open: (path) => postJson<WorkbenchProject>(`${MOBILE_WORKBENCH_API_PREFIX}/projects/open`, { path }),
  },
  worktrees: {
    list: (projectId) =>
      postJson<WorkbenchWorktree[]>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/list`, { projectId }),
    create: (projectId, branchName, baseBranch) =>
      postJson<WorkbenchWorktree>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/create`, {
        projectId,
        branchName,
        baseBranch: baseBranch ?? null,
      }),
    commit: (worktreeId, message) =>
      postJson<WorkbenchWorktree>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/commit`, {
        worktreeId,
        message: message ?? null,
      }),
    push: (worktreeId) =>
      postJson<WorkbenchWorktree>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/push`, {
        worktreeId,
      }),
    merge: (worktreeId) =>
      postJson<WorkbenchMergeResult>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/merge`, {
        worktreeId,
      }),
    remove: (worktreeId, force = false) =>
      postJson<{ ok: boolean; worktreeId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/remove`, {
        worktreeId,
        force,
      }),
  },
  sessions: {
    list: (projectId) =>
      postJson<WorkbenchSession[]>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/list`, {
        projectId: projectId ?? null,
      }),
    create: (projectId, initialSize, worktreeId) =>
      postJson<WorkbenchSession>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/create`, {
        projectId,
        worktreeId: worktreeId ?? null,
        initialCols: initialSize?.cols ?? null,
        initialRows: initialSize?.rows ?? null,
      }),
    writeInput: (sessionId, data) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/write`, {
        sessionId,
        data,
      }),
    resize: (sessionId, cols, rows) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/resize`, {
        sessionId,
        cols,
        rows,
      }),
    replay: (sessionId) =>
      postJson<WorkbenchSessionReplay>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/replay`, { sessionId }),
    focus: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/focus`, {
        sessionId,
      }),
    focused: (projectId, worktreeId) =>
      postJson<{ sessionId: string | null }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/focused`, {
        projectId,
        worktreeId: worktreeId ?? null,
      }),
    splitPane: (sessionId, direction) =>
      postJson<{ ok: boolean; sessionId: string; direction: WorkbenchPaneSplitDirection }>(
        `${MOBILE_WORKBENCH_API_PREFIX}/sessions/split-pane`,
        {
          sessionId,
          direction,
        },
      ),
    switchPane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/switch-pane`, {
        sessionId,
      }),
    zoomPane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/zoom-pane`, {
        sessionId,
      }),
    closePane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string; closedWindow: boolean }>(
        `${MOBILE_WORKBENCH_API_PREFIX}/sessions/close-pane`,
        { sessionId },
      ),
    close: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/close`, {
        sessionId,
      }),
  },
  files: {
    listDir: (projectId, path, worktreeId) =>
      postJson<WorkbenchFileNode[]>(`${MOBILE_WORKBENCH_API_PREFIX}/files/list-dir`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path: path ?? null,
      }),
    info: (projectId, path, worktreeId) =>
      postJson<WorkbenchPathInfo>(`${MOBILE_WORKBENCH_API_PREFIX}/files/info`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }),
    open: (projectId, path, worktreeId) =>
      postJson<WorkbenchOpenFile>(`${MOBILE_WORKBENCH_API_PREFIX}/files/open`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }),
    saveText: (projectId, path, content, baseHash, worktreeId) =>
      postJson<WorkbenchSaveTextResult>(`${MOBILE_WORKBENCH_API_PREFIX}/files/save-text`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
        content,
        baseHash,
      }),
  },
  git: {
    listCommits: (projectId, worktreeId, limit = 30) =>
      postJson<WorkbenchGitCommit[]>(`${MOBILE_WORKBENCH_API_PREFIX}/git/commits`, {
        projectId,
        worktreeId: worktreeId ?? null,
        limit,
      }),
  },
  browser: {
    discover: (projectId, worktreeId) =>
      postJson<WorkbenchBrowserDiscovery>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/discover`, {
        projectId,
        worktreeId: worktreeId ?? null,
      }),
    createPreview: (projectId, worktreeId, targetUrl) =>
      postJson<WorkbenchBrowserPreview>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/preview`, {
        projectId,
        worktreeId: worktreeId ?? null,
        targetUrl,
      }),
  },
  prompt: {
    streamToTerminal: (prompt, options) =>
      postJson<{ ok: boolean; sessionId: string }>(
        `${MOBILE_WORKBENCH_API_PREFIX}/prompt-optimizer/stream-to-session`,
        {
          prompt,
          workingDirectory: options.workingDirectory ?? null,
          targetLanguage: options.targetLanguage,
          sessionId: options.sessionId,
        },
      ),
  },
};
