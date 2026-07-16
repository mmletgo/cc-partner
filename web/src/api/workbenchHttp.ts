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
  OrchestratorRemoteOutboxItem,
  OrchestratorReviewDiff,
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
import type { Decoder } from '@/lib/runtimeSchema';
import { ContractDecodeError, nullableDecoder } from '@/lib/runtimeSchema';
import {
  orchestratorRemoteOutboxItemDecoder,
  orchestratorReviewDiffResponseDecoder,
  orchestratorRuntimeSnapshotDecoder,
  orchestratorTaskViewDecoder,
  orchestratorTaskViewListResponseDecoder,
} from '@/lib/schemas/orchestrator';
import {
  workbenchPathInfoDecoder,
  workbenchProjectsDecoder,
  workbenchProjectDecoder,
  workbenchSaveTextResultDecoder,
  workbenchSessionsDecoder,
  workbenchSessionDecoder,
  workbenchWorktreesDecoder,
  workbenchWorktreeDecoder,
  workbenchMergeResultDecoder,
  workbenchMutationEnvelopeDecoder,
  workbenchMutationOperationDecoder,
  workbenchRemoveResultDecoder,
} from '@/lib/schemas/workbench';
import type { WorkbenchMutationOperation } from '@/lib/types';
import type { WorkbenchPaneSplitDirection } from './workbench';
import type { OrchestratorCreateAction } from './orchestrator';
import {
  OrchestratorRuntimeTransportError,
  toOrchestratorRuntimeTransportError,
} from './orchestratorRuntimeTransportError';
import type { WorkbenchTransport } from './workbenchTransport';
import {
  mutationUnknown,
  type WorkbenchMutationEnvelope,
} from '@/lib/asyncState/mutationOutcome';

export type HttpCreateOrchestratorTaskAction = OrchestratorCreateAction;

/** query 整体预算（含 body decode），单位毫秒。 */
export const HTTP_QUERY_TIMEOUT_MS = 15_000;
/** mutation 默认整体预算，单位毫秒。 */
export const HTTP_MUTATION_TIMEOUT_MS = 30_000;
/** longMutation 默认整体预算，单位毫秒。 */
export const HTTP_LONG_MUTATION_TIMEOUT_MS = 180_000;
/** query 只读请求最大自动重试次数（不含首次）。 */
export const HTTP_QUERY_MAX_RETRIES = 2;
/** query 重试退避基线（毫秒），实际为 base * 2^attempt。 */
export const HTTP_QUERY_RETRY_BACKOFF_BASE_MS = 250;

/**
 * Mobile HTTP 请求策略。
 *
 * Business Logic（为什么需要这个类型）:
 *   弱网下 query/mutation/长操作/事件流的 timeout、取消与重试语义不同，调用方必须显式选择。
 *
 * Code Logic（字段说明）:
 *   kind 决定默认超时与是否允许自动重试；timeoutMs 覆盖默认；signal 为调用方取消。
 */
export type HttpRequestPolicy = {
  kind: 'query' | 'mutation' | 'longMutation' | 'eventStream';
  timeoutMs?: number;
  signal?: AbortSignal;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   各 kind 有稳定的默认 overall 预算；事件流不使用 overall timeout。
 *
 * Code Logic（这个函数做什么）:
 *   若 policy.timeoutMs 为正数则返回之；eventStream 返回 null；否则按 kind 取默认。
 */
export function resolveHttpTimeoutMs(policy: HttpRequestPolicy): number | null {
  if (typeof policy.timeoutMs === 'number' && policy.timeoutMs > 0) {
    return policy.timeoutMs;
  }
  if (policy.kind === 'eventStream') {
    return null;
  }
  if (policy.kind === 'query') {
    return HTTP_QUERY_TIMEOUT_MS;
  }
  if (policy.kind === 'longMutation') {
    return HTTP_LONG_MUTATION_TIMEOUT_MS;
  }
  return HTTP_MUTATION_TIMEOUT_MS;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   fetch AbortError 需要区分调用方取消与 overall 超时，才能映射到正确 transport kind。
 *
 * Code Logic（这个函数做什么）:
 *   识别 DOMException/Error 的 name===AbortError。
 */
function isAbortError(reason: unknown): boolean {
  if (reason instanceof DOMException && reason.name === 'AbortError') {
    return true;
  }
  return reason instanceof Error && reason.name === 'AbortError';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   decode 阶段无法把 AbortSignal 传给 response.json()，需要 race 才能让 overall 预算覆盖 decode。
 *
 * Code Logic（这个函数做什么）:
 *   signal 已 abort 时立即拒绝；否则监听 abort 与 promise 结算，先到者胜。
 */
function raceWithAbort<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  if (signal.aborted) {
    return Promise.reject(new DOMException('The operation was aborted.', 'AbortError'));
  }
  return new Promise<T>((resolve, reject) => {
    /**
     * Business Logic（为什么需要这个函数）:
     *   overall timeout / 调用方 abort 必须打断仍在进行的 body decode。
     *
     * Code Logic（这个函数做什么）:
     *   将 AbortError 抛给外层 catch 分类。
     */
    const onAbort = (): void => {
      reject(new DOMException('The operation was aborted.', 'AbortError'));
    };
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (reason: unknown) => {
        signal.removeEventListener('abort', onAbort);
        reject(reason);
      },
    );
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   页面 hidden 时自动重试会在后台烧流量且用户看不到结果。
 *
 * Code Logic（这个函数做什么）:
 *   document 不可用视为可见；否则要求 visibilityState==='visible'。
 */
function isDocumentVisible(): boolean {
  if (typeof document === 'undefined') {
    return true;
  }
  return document.visibilityState === 'visible';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   query 重试需要有界退避，且可被调用方 abort 打断。
 *
 * Code Logic（这个函数做什么）:
 *   delayMs 后 resolve；若 signal abort 则 reject AbortError。
 */
function sleepWithAbort(delayMs: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) {
    return Promise.reject(new DOMException('The operation was aborted.', 'AbortError'));
  }
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
      resolve();
    }, delayMs);
    /**
     * Business Logic（为什么需要这个函数）:
     *   退避等待期间用户取消必须立即停止重试。
     *
     * Code Logic（这个函数做什么）:
     *   clearTimeout 并以 AbortError reject。
     */
    const onAbort = (): void => {
      clearTimeout(timer);
      reject(new DOMException('The operation was aborted.', 'AbortError'));
    };
    if (signal) {
      signal.addEventListener('abort', onAbort, { once: true });
    }
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   timeout/network 才可对 query 安全重试；protocol/decode/callerAbort 重试无意义或有害。
 *
 * Code Logic（这个函数做什么）:
 *   OrchestratorRuntimeTransportError 且 kind 为 timeout|network 时返回 true。
 */
function isRetryableTransportError(reason: unknown): boolean {
  return (
    reason instanceof OrchestratorRuntimeTransportError &&
    (reason.kind === 'timeout' || reason.kind === 'network')
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   abort 原因要稳定映射到 timeout 或 callerAbort，禁止 message 关键词猜测。
 *
 * Code Logic（这个函数做什么）:
 *   timedOut 优先；否则外部 signal aborted → callerAbort；兜底 timeout。
 */
function transportErrorFromAbort(
  timedOut: boolean,
  externalSignal: AbortSignal | undefined,
): OrchestratorRuntimeTransportError {
  if (externalSignal?.aborted && !timedOut) {
    return new OrchestratorRuntimeTransportError('请求已取消', 'callerAbort');
  }
  if (timedOut) {
    return new OrchestratorRuntimeTransportError('请求超时', 'timeout');
  }
  if (externalSignal?.aborted) {
    return new OrchestratorRuntimeTransportError('请求已取消', 'callerAbort');
  }
  return new OrchestratorRuntimeTransportError('请求超时', 'timeout');
}

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
  /**
   * 逻辑提交幂等键。调用方在一次提交开始时生成并在失败重试间复用；
   * 未提供时 transport 才会 mint 新 id（兼容旧调用方，但移动端应显式传入）。
   */
  clientRequestId?: string;
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
 *   检查 Response.ok；失败时抛出 protocol 错误；成功时解析 JSON；
 *   decoder 失败映射为 decode kind（ContractDecodeError 保留 message）。
 */
async function parseJsonResponse<T>(response: Response, decoder?: Decoder<T>): Promise<T> {
  if (!response.ok) {
    const message = await readHttpErrorMessage(response);
    // HTTP 非 2xx 是协议/业务失败：用 protocol，禁止 hook 因 message 含“连接”误判 offline。
    throw new OrchestratorRuntimeTransportError(message, 'protocol');
  }
  let raw: unknown;
  try {
    raw = await response.json();
  } catch (reason) {
    const message = reason instanceof Error ? reason.message : String(reason);
    throw new OrchestratorRuntimeTransportError(message || '响应不是合法 JSON', 'decode');
  }
  if (!decoder) {
    return raw as T;
  }
  try {
    return decoder.decode(raw, '$');
  } catch (reason) {
    if (reason instanceof ContractDecodeError) {
      throw new OrchestratorRuntimeTransportError(reason.message, 'decode');
    }
    const message = reason instanceof Error ? reason.message : String(reason);
    throw new OrchestratorRuntimeTransportError(message || '响应契约校验失败', 'decode');
  }
}

/**
 * postJson 可选参数。
 *
 * Business Logic（为什么需要这个类型）:
 *   关键 HTTP 成功体需要可选 runtime decoder，且每个调用必须声明 request policy。
 *
 * Code Logic（字段说明）:
 *   policy 必填；decoder 仅用于 2xx body。
 */
export interface PostJsonOptions<T> {
  policy: HttpRequestPolicy;
  decoder?: Decoder<T>;
}

/**
 * getJson 可选参数。
 *
 * Business Logic（为什么需要这个类型）:
 *   Attention 等路径需要 AbortSignal / 超时；新调用方应传 policy，旧 timeoutMs/signal 仍兼容映射到 query。
 *
 * Code Logic（字段说明）:
 *   policy 优先；否则由 timeoutMs/signal 合成 query policy；decoder 仅用于 2xx body。
 */
export interface GetJsonOptions<T = unknown> {
  policy?: HttpRequestPolicy;
  signal?: AbortSignal;
  timeoutMs?: number;
  decoder?: Decoder<T>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   getJson 需兼容 Attention 既有 timeoutMs，同时支持显式 policy。
 *
 * Code Logic（这个函数做什么）:
 *   有 policy 则原样返回；否则合成 kind=query 的 policy。
 */
function resolveGetJsonPolicy(options: GetJsonOptions<unknown>): HttpRequestPolicy {
  if (options.policy) {
    return options.policy;
  }
  return {
    kind: 'query',
    timeoutMs: options.timeoutMs,
    signal: options.signal,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   GET/POST 共用 overall timeout + AbortSignal 合成与错误分类，避免两处漂移。
 *
 * Code Logic（这个函数做什么）:
 *   合成 controller；timeout 仅在 decode 结束后清理；abort 映射 timeout/callerAbort；
 *   fetch 失败为 network；成功走 parseJsonResponse。
 */
async function executeJsonRequest<T>(
  path: string,
  init: RequestInit,
  policy: HttpRequestPolicy,
  decoder?: Decoder<T>,
): Promise<T> {
  const externalSignal = policy.signal;
  if (externalSignal?.aborted) {
    throw new OrchestratorRuntimeTransportError('请求已取消', 'callerAbort');
  }

  const controller = new AbortController();
  let timedOut = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  const timeoutMs = resolveHttpTimeoutMs(policy);

  /**
   * Business Logic（为什么需要这个函数）:
   *   调用方 AbortSignal 必须取消 in-flight fetch/decode race。
   *
   * Code Logic（这个函数做什么）:
   *   转发 abort 到内部 controller。
   */
  const onExternalAbort = (): void => {
    controller.abort();
  };
  if (externalSignal) {
    externalSignal.addEventListener('abort', onExternalAbort);
  }
  if (timeoutMs !== null) {
    timeoutId = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
  }

  try {
    /**
     * Business Logic（为什么需要这个函数）:
     *   overall 预算必须覆盖 fetch 与 body decode；mock/半开场景下 AbortSignal 也可能不被底层遵守。
     *
     * Code Logic（这个函数做什么）:
     *   执行 fetch + parseJsonResponse 全链路，外层 raceWithAbort 保证 timeout/callerAbort 可打断。
     */
    const run = async (): Promise<T> => {
      let response: Response;
      try {
        response = await fetch(path, {
          ...init,
          signal: controller.signal,
        });
      } catch (reason) {
        if (isAbortError(reason) || controller.signal.aborted) {
          throw transportErrorFromAbort(timedOut, externalSignal);
        }
        // fetch 本身失败（断网/DNS/CORS 等）是传输层 network，不靠文案匹配。
        throw toOrchestratorRuntimeTransportError(reason, 'network');
      }
      return parseJsonResponse<T>(response, decoder);
    };

    try {
      return await raceWithAbort(run(), controller.signal);
    } catch (reason) {
      if (reason instanceof OrchestratorRuntimeTransportError) {
        throw reason;
      }
      if (isAbortError(reason) || controller.signal.aborted) {
        throw transportErrorFromAbort(timedOut, externalSignal);
      }
      throw toOrchestratorRuntimeTransportError(reason, 'unknown');
    }
  } finally {
    // 仅在 decode 结束后清理 overall timeout，确保预算覆盖 body 解析。
    if (timeoutId !== null) {
      clearTimeout(timeoutId);
    }
    if (externalSignal) {
      externalSignal.removeEventListener('abort', onExternalAbort);
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只读 query 在弱网下可有限重试；mutation 禁止 transport 盲重放。
 *
 * Code Logic（这个函数做什么）:
 *   kind=query 时对 timeout/network 最多重试 2 次（有界退避），仅 document 可见且非 callerAbort。
 */
async function executeJsonRequestWithRetry<T>(
  path: string,
  init: RequestInit,
  policy: HttpRequestPolicy,
  decoder?: Decoder<T>,
): Promise<T> {
  let attempt = 0;
  while (true) {
    try {
      return await executeJsonRequest(path, init, policy, decoder);
    } catch (reason) {
      if (
        policy.kind !== 'query' ||
        !isRetryableTransportError(reason) ||
        attempt >= HTTP_QUERY_MAX_RETRIES ||
        !isDocumentVisible() ||
        policy.signal?.aborted
      ) {
        if (
          policy.signal?.aborted &&
          reason instanceof OrchestratorRuntimeTransportError &&
          reason.kind === 'timeout'
        ) {
          throw new OrchestratorRuntimeTransportError('请求已取消', 'callerAbort');
        }
        throw reason;
      }
      const delayMs = HTTP_QUERY_RETRY_BACKOFF_BASE_MS * 2 ** attempt;
      attempt += 1;
      try {
        await sleepWithAbort(delayMs, policy.signal);
      } catch (sleepReason) {
        if (isAbortError(sleepReason) || policy.signal?.aborted) {
          throw new OrchestratorRuntimeTransportError('请求已取消', 'callerAbort');
        }
        throw sleepReason;
      }
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTTP routes 大多通过 POST 接收 camelCase JSON 请求体，移动端需要统一入口与 policy。
 *
 * Code Logic（这个函数做什么）:
 *   以 application/json 发送 body；按 policy 应用 overall timeout/取消/query 重试；可选 decoder。
 */
export async function postJson<T>(
  path: string,
  body: unknown,
  options: PostJsonOptions<T>,
): Promise<T> {
  return executeJsonRequestWithRetry(
    path,
    {
      method: 'POST',
      headers: {
        Accept: 'application/json',
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    },
    options.policy,
    options.decoder,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   少量 Workbench/mobile routes 使用 GET，需要与 POST helper 一致的错误分类与 overall 预算。
 *
 * Code Logic（这个函数做什么）:
 *   发起同源 GET；policy 或 legacy timeoutMs/signal；成功解析 JSON，可选 decoder。
 */
export async function getJson<T>(path: string, options: GetJsonOptions<T> = {}): Promise<T> {
  const policy = resolveGetJsonPolicy(options);
  return executeJsonRequestWithRetry(
    path,
    {
      method: 'GET',
      headers: {
        Accept: 'application/json',
      },
    },
    policy,
    options.decoder,
  );
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
 * Business Logic（为什么需要这个函数）:
 *   create/createView 必须始终携带非空 clientRequestId；若调用方已在逻辑提交开始时生成，
 *   则必须原样复用，避免“响应丢失 + 用户重试”因换键产生重复任务。
 *
 * Code Logic（这个函数做什么）:
 *   若 request.clientRequestId 去空白后非空则返回该值；否则 mint 新 id。
 */
export function resolveHttpOrchestratorClientRequestId(
  request: Pick<HttpCreateOrchestratorTaskRequest, 'clientRequestId'>,
): string {
  const existing = request.clientRequestId?.trim();
  if (existing) {
    return existing;
  }
  return createHttpOrchestratorClientRequestId();
}

/**
 * HTTP Orchestrator Transport。
 *
 * Business Logic（为什么需要这个常量）:
 *   手机端 `/mobile` 需要通过同源 HTTP 操作当前本机项目的 Orchestrator 项目级任务，而不能调用桌面 Tauri invoke。
 *
 * Code Logic（这个常量做什么）:
 *   将任务 list/create/action/evidence 映射到 `/api/orchestrator/tasks/...` routes；create/createView 显式携带
 *   createAction、tracker 预留字段和非空 clientRequestId（优先复用调用方传入的逻辑提交 id）。
 */
export const httpOrchestratorTransport = {
  tasks: {
    list: async (projectId: string): Promise<OrchestratorTask[]> => {
      const response = await postJson<HttpOrchestratorTaskListResponse>(
        '/api/orchestrator/tasks/list',
        { projectId },
        { policy: { kind: 'query' } },
      );
      return response.tasks;
    },
    listViews: async (projectId: string): Promise<OrchestratorTaskView[]> => {
      const response = await postJson<HttpOrchestratorTaskViewListResponse>(
        '/api/orchestrator/task-views/list',
        { projectId },
        { policy: { kind: 'query' }, decoder: orchestratorTaskViewListResponseDecoder },
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
        clientRequestId: resolveHttpOrchestratorClientRequestId(request),
      }, { policy: { kind: 'mutation' } }),
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
          clientRequestId: resolveHttpOrchestratorClientRequestId(request),
        }, { policy: { kind: 'mutation' }, decoder: orchestratorTaskViewDecoder }),
    completePrompt: (
      request: HttpCompleteOrchestratorTaskPromptRequest,
    ): Promise<OrchestratorTaskPromptCompletion> =>
      postJson<OrchestratorTaskPromptCompletion>('/api/orchestrator/tasks/complete-prompt', {
        projectId: request.projectId?.trim() || null,
        prompt: request.prompt.trim(),
        workingDirectory: request.workingDirectory?.trim() || null,
      }, { policy: { kind: 'query' } }),
    queue: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/queue', { taskId }, { policy: { kind: 'mutation' } }),
    retry: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/retry', { taskId }, { policy: { kind: 'mutation' } }),
    abort: (taskId: string): Promise<OrchestratorTask> =>
      postJson<OrchestratorTask>('/api/orchestrator/tasks/abort', { taskId }, { policy: { kind: 'mutation' } }),
    listEvidence: async (projectId: string, taskId: string): Promise<OrchestratorEvidence[]> => {
      const response = await postJson<HttpOrchestratorEvidenceListResponse>(
        '/api/orchestrator/tasks/evidence',
        { projectId, taskId },
        { policy: { kind: 'query' } },
      );
      return response.evidence;
    },
    /**
     * Business Logic（为什么需要这个方法）:
     *   手机端 Human Review 详情需要 inspection-only 展示有界 review diff，不得直连 owning device。
     *
     * Code Logic（这个函数做什么）:
     *   POST `/api/mobile/orchestrator/tasks/review-diff` body `{projectId,taskId}`，解码 `{diff}` 后返回 diff。
     */
    getReviewDiff: async (projectId: string, taskId: string): Promise<OrchestratorReviewDiff> => {
      const response = await postJson<{ diff: OrchestratorReviewDiff }>(
        '/api/mobile/orchestrator/tasks/review-diff',
        { projectId, taskId },
        { policy: { kind: 'query' }, decoder: orchestratorReviewDiffResponseDecoder },
      );
      return response.diff;
    },
  },
  /**
   * Business Logic（为什么需要这个方法）:
   *   手机自动化面板需要拉取本机/远端 shortcut 的 runtime snapshot，且不能直连 owning device P2P base URL。
   *
   * Code Logic（这个函数做什么）:
   *   POST `/api/mobile/orchestrator/runtime-snapshot`，body `{projectId}`，返回 camelCase snapshot DTO。
   */

  /**
   * Business Logic（为什么需要这个对象）:
   *   手机 Automation 面板需要对本机 failed outbox 执行 Retry/Discard，且只打本机同源 HTTP。
   *
   * Code Logic（这个对象做什么）:
   *   POST `/api/orchestrator/outbox/{retry,discard}`，body `{projectId,outboxId}`，返回 outbox DTO。
   */
  outbox: {
    retry: (projectId: string, outboxId: string): Promise<OrchestratorRemoteOutboxItem> =>
      postJson<OrchestratorRemoteOutboxItem>('/api/orchestrator/outbox/retry', {
          projectId,
          outboxId,
        }, { policy: { kind: 'mutation' }, decoder: orchestratorRemoteOutboxItemDecoder }),
    discard: (projectId: string, outboxId: string): Promise<OrchestratorRemoteOutboxItem> =>
      postJson<OrchestratorRemoteOutboxItem>('/api/orchestrator/outbox/discard', {
          projectId,
          outboxId,
        }, { policy: { kind: 'mutation' }, decoder: orchestratorRemoteOutboxItemDecoder }),
  },
  getRuntimeSnapshot: (projectId: string): Promise<OrchestratorRuntimeSnapshot> =>
    postJson<OrchestratorRuntimeSnapshot>('/api/mobile/orchestrator/runtime-snapshot', {
        projectId,
      }, { policy: { kind: 'query' }, decoder: orchestratorRuntimeSnapshotDecoder }),
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
    list: () =>
      getJson<WorkbenchProject[]>(`${MOBILE_WORKBENCH_API_PREFIX}/projects/list`, {
        decoder: workbenchProjectsDecoder,
      }),
    open: (path) =>
      postJson<WorkbenchProject>(`${MOBILE_WORKBENCH_API_PREFIX}/projects/open`, { path }, { policy: { kind: 'mutation' }, decoder: workbenchProjectDecoder }),
  },
  worktrees: {
    list: (projectId) =>
      postJson<WorkbenchWorktree[]>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/list`, { projectId }, { policy: { kind: 'query' }, decoder: workbenchWorktreesDecoder }),
    create: (projectId, branchName, baseBranch) =>
      postJson<WorkbenchWorktree>(`${MOBILE_WORKBENCH_API_PREFIX}/worktrees/create`, {
          projectId,
          branchName,
          baseBranch: baseBranch ?? null,
        }, { policy: { kind: 'mutation' }, decoder: workbenchWorktreeDecoder }),
    // Business Logic: 兼容旧调用方；新 Git/worktree 面板应直接消费 workbenchHttp.git envelope + 对账。
    // Code Logic: mint 临时 clientOperationId；succeeded 解包，unknown 抛中文错误（禁止盲重放）。
    commit: async (worktreeId, message) => {
      const envelope = await workbenchHttp.git.commit({
        worktreeId,
        message: message ?? null,
        clientOperationId: createHttpOrchestratorClientRequestId(),
      });
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    push: async (worktreeId) => {
      const envelope = await workbenchHttp.git.push({
        worktreeId,
        clientOperationId: createHttpOrchestratorClientRequestId(),
      });
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    merge: async (worktreeId) => {
      const envelope = await workbenchHttp.git.merge({
        worktreeId,
        clientOperationId: createHttpOrchestratorClientRequestId(),
      });
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
    remove: async (worktreeId, force = false) => {
      const envelope = await workbenchHttp.git.remove({
        worktreeId,
        force,
        clientOperationId: createHttpOrchestratorClientRequestId(),
      });
      if (envelope.kind === 'succeeded') return envelope.value;
      throw new Error('操作结果未知，请刷新后人工核对');
    },
  },
  sessions: {
    list: (projectId) =>
      postJson<WorkbenchSession[]>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/list`, {
          projectId: projectId ?? null,
        }, { policy: { kind: 'query' }, decoder: workbenchSessionsDecoder }),
    create: (projectId, initialSize, worktreeId) =>
      postJson<WorkbenchSession>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/create`, {
          projectId,
          worktreeId: worktreeId ?? null,
          initialCols: initialSize?.cols ?? null,
          initialRows: initialSize?.rows ?? null,
        }, { policy: { kind: 'mutation' }, decoder: workbenchSessionDecoder }),
    writeInput: (sessionId, data) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/write`, {
        sessionId,
        data,
      }, { policy: { kind: 'mutation' } }),
    resize: (sessionId, cols, rows) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/resize`, {
        sessionId,
        cols,
        rows,
      }, { policy: { kind: 'mutation' } }),
    replay: (sessionId) =>
      postJson<WorkbenchSessionReplay>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/replay`, { sessionId }, { policy: { kind: 'query' } }),
    focus: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/focus`, {
        sessionId,
      }, { policy: { kind: 'mutation' } }),
    focused: (projectId, worktreeId) =>
      postJson<{ sessionId: string | null }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/focused`, {
        projectId,
        worktreeId: worktreeId ?? null,
      }, { policy: { kind: 'query' } }),
    splitPane: (sessionId, direction) =>
      postJson<{ ok: boolean; sessionId: string; direction: WorkbenchPaneSplitDirection }>(
        `${MOBILE_WORKBENCH_API_PREFIX}/sessions/split-pane`,
        {
          sessionId,
          direction,
        },
        { policy: { kind: 'mutation' } },
      ),
    switchPane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/switch-pane`, {
        sessionId,
      }, { policy: { kind: 'mutation' } }),
    zoomPane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/zoom-pane`, {
        sessionId,
      }, { policy: { kind: 'mutation' } }),
    closePane: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string; closedWindow: boolean }>(
        `${MOBILE_WORKBENCH_API_PREFIX}/sessions/close-pane`,
        { sessionId },
        { policy: { kind: 'mutation' } },
      ),
    close: (sessionId) =>
      postJson<{ ok: boolean; sessionId: string }>(`${MOBILE_WORKBENCH_API_PREFIX}/sessions/close`, {
        sessionId,
      }, { policy: { kind: 'mutation' } }),
  },
  files: {
    listDir: (projectId, path, worktreeId) =>
      postJson<WorkbenchFileNode[]>(`${MOBILE_WORKBENCH_API_PREFIX}/files/list-dir`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path: path ?? null,
      }, { policy: { kind: 'query' } }),
    info: (projectId, path, worktreeId) =>
      postJson<WorkbenchPathInfo>(`${MOBILE_WORKBENCH_API_PREFIX}/files/info`, {
          projectId,
          worktreeId: worktreeId ?? null,
          path,
        }, { policy: { kind: 'query' }, decoder: workbenchPathInfoDecoder }),
    open: (projectId, path, worktreeId) =>
      postJson<WorkbenchOpenFile>(`${MOBILE_WORKBENCH_API_PREFIX}/files/open`, {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
      }, { policy: { kind: 'query' } }),
    saveText: (projectId, path, content, baseHash, worktreeId) =>
      postJson<WorkbenchSaveTextResult>(`${MOBILE_WORKBENCH_API_PREFIX}/files/save-text`, {
          projectId,
          worktreeId: worktreeId ?? null,
          path,
          content,
          baseHash,
        }, { policy: { kind: 'mutation' }, decoder: workbenchSaveTextResultDecoder }),
  },
  git: {
    listCommits: (projectId, worktreeId, limit = 30) =>
      postJson<WorkbenchGitCommit[]>(`${MOBILE_WORKBENCH_API_PREFIX}/git/commits`, {
        projectId,
        worktreeId: worktreeId ?? null,
        limit,
      }, { policy: { kind: 'query' } }),
  },
  browser: {
    discover: (projectId, worktreeId) =>
      postJson<WorkbenchBrowserDiscovery>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/discover`, {
        projectId,
        worktreeId: worktreeId ?? null,
      }, { policy: { kind: 'query' } }),
    createPreview: (projectId, worktreeId, targetUrl) =>
      postJson<WorkbenchBrowserPreview>(`${MOBILE_WORKBENCH_API_PREFIX}/browser/preview`, {
        projectId,
        worktreeId: worktreeId ?? null,
        targetUrl,
      }, { policy: { kind: 'mutation' } }),
    // mobile 首版：经 P2P owner 路由（同源 /api/workbench/... 由 mobile 代理或直接不可用时 optional）
    startVerification: (previewId, requestId) =>
      postJson('/api/workbench/browser-verification/create', {
        previewId,
        requestId,
      }, { policy: { kind: 'mutation' } }),
    getVerification: (runId) =>
      postJson('/api/workbench/browser-verification/get', { runId }, { policy: { kind: 'query' } }),
    cancelVerification: (runId) =>
      postJson('/api/workbench/browser-verification/cancel', { runId }, {
        policy: { kind: 'mutation' },
      }),
    getVerificationArtifact: (runId, artifactId) =>
      postJson('/api/workbench/browser-verification/artifact', { runId, artifactId }, {
        policy: { kind: 'query' },
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
        { policy: { kind: 'longMutation' } },
      ),
  },
};

/**
 * Workbench Git mutation 请求体（HTTP envelope 路径）。
 *
 * Business Logic（为什么需要这个类型）:
 *   commit/push/merge/remove 必须携带稳定 clientOperationId，禁止 transport 盲重放。
 *
 * Code Logic（字段说明）:
 *   worktreeId 目标；clientOperationId 幂等键；message/force 按动作可选。
 */
export interface WorkbenchHttpGitCommitRequest {
  worktreeId: string;
  message?: string | null;
  clientOperationId: string;
  policy?: HttpRequestPolicy;
}

export interface WorkbenchHttpGitPushRequest {
  worktreeId: string;
  clientOperationId: string;
  policy?: HttpRequestPolicy;
}

export interface WorkbenchHttpGitMergeRequest {
  worktreeId: string;
  clientOperationId: string;
  policy?: HttpRequestPolicy;
}

export interface WorkbenchHttpGitRemoveRequest {
  worktreeId: string;
  force?: boolean;
  clientOperationId: string;
  policy?: HttpRequestPolicy;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   mutation 在 timeout/network 时必须返回 typed unknown envelope，供领域 controller 对账，禁止猜 notStarted。
 *
 * Code Logic（这个函数做什么）:
 *   执行 postJson；成功 decoder envelope；仅 timeout/network catch 映射 mutationUnknown；其它错误上抛。
 */
async function postWorkbenchMutationEnvelope<T>(
  path: string,
  body: unknown,
  clientOperationId: string,
  policy: HttpRequestPolicy,
  valueDecoder: Decoder<T>,
): Promise<WorkbenchMutationEnvelope<T>> {
  try {
    return await postJson<WorkbenchMutationEnvelope<T>>(path, body, {
      policy,
      decoder: workbenchMutationEnvelopeDecoder(valueDecoder),
    });
  } catch (reason) {
    if (
      reason instanceof OrchestratorRuntimeTransportError &&
      (reason.kind === 'timeout' || reason.kind === 'network')
    ) {
      return mutationUnknown(clientOperationId, reason.kind);
    }
    throw reason;
  }
}

/**
 * Mobile Workbench Git mutation wrappers（成功通道 envelope）。
 *
 * Business Logic（为什么需要这个对象）:
 *   Task 6 要求 commit/push/merge/remove 强制 clientOperationId，timeout/network → unknown 且不重放。
 *
 * Code Logic（这个对象做什么）:
 *   POST `/api/mobile/workbench/worktrees/{commit,push,merge,remove}`，decoder envelope，catch 仅 timeout/network。
 */
export const workbenchHttp = {
  git: {
    /**
     * Business Logic（为什么需要这个函数）:
     *   Mobile commit 必须带稳定 operation id，timeout 后对账而不是盲重放。
     *
     * Code Logic（这个函数做什么）:
     *   longMutation POST commit；返回 succeeded|unknown envelope。
     */
    commit: (
      request: WorkbenchHttpGitCommitRequest,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>> => {
      const clientOperationId = request.clientOperationId.trim();
      if (!clientOperationId) {
        return Promise.reject(
          new OrchestratorRuntimeTransportError('clientOperationId 不能为空', 'protocol'),
        );
      }
      return postWorkbenchMutationEnvelope(
        `${MOBILE_WORKBENCH_API_PREFIX}/worktrees/commit`,
        {
          worktreeId: request.worktreeId,
          message: request.message ?? null,
          clientOperationId,
        },
        clientOperationId,
        request.policy ?? { kind: 'longMutation' },
        workbenchWorktreeDecoder,
      );
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   Mobile push 超时后只能 unknown 对账，禁止 transport 自动重试。
     *
     * Code Logic（这个函数做什么）:
     *   longMutation POST push + envelope decoder。
     */
    push: (
      request: WorkbenchHttpGitPushRequest,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>> => {
      const clientOperationId = request.clientOperationId.trim();
      if (!clientOperationId) {
        return Promise.reject(
          new OrchestratorRuntimeTransportError('clientOperationId 不能为空', 'protocol'),
        );
      }
      return postWorkbenchMutationEnvelope(
        `${MOBILE_WORKBENCH_API_PREFIX}/worktrees/push`,
        {
          worktreeId: request.worktreeId,
          clientOperationId,
        },
        clientOperationId,
        request.policy ?? { kind: 'longMutation' },
        workbenchWorktreeDecoder,
      );
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   merge 是长操作，timeout/network 必须 unknown。
     *
     * Code Logic（这个函数做什么）:
     *   longMutation POST merge + envelope decoder。
     */
    merge: (
      request: WorkbenchHttpGitMergeRequest,
    ): Promise<WorkbenchMutationEnvelope<WorkbenchMergeResult>> => {
      const clientOperationId = request.clientOperationId.trim();
      if (!clientOperationId) {
        return Promise.reject(
          new OrchestratorRuntimeTransportError('clientOperationId 不能为空', 'protocol'),
        );
      }
      return postWorkbenchMutationEnvelope(
        `${MOBILE_WORKBENCH_API_PREFIX}/worktrees/merge`,
        {
          worktreeId: request.worktreeId,
          clientOperationId,
        },
        clientOperationId,
        request.policy ?? { kind: 'longMutation' },
        workbenchMergeResultDecoder,
      );
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   remove 带 force 与 clientOperationId，网络不确定时 unknown。
     *
     * Code Logic（这个函数做什么）:
     *   mutation POST remove + envelope decoder。
     */
    remove: (
      request: WorkbenchHttpGitRemoveRequest,
    ): Promise<WorkbenchMutationEnvelope<{ ok: boolean; worktreeId: string }>> => {
      const clientOperationId = request.clientOperationId.trim();
      if (!clientOperationId) {
        return Promise.reject(
          new OrchestratorRuntimeTransportError('clientOperationId 不能为空', 'protocol'),
        );
      }
      return postWorkbenchMutationEnvelope(
        `${MOBILE_WORKBENCH_API_PREFIX}/worktrees/remove`,
        {
          worktreeId: request.worktreeId,
          force: request.force ?? false,
          clientOperationId,
        },
        clientOperationId,
        request.policy ?? { kind: 'mutation' },
        workbenchRemoveResultDecoder,
      );
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   unknown envelope 后 Mobile controller 必须查询 owning ledger 取得 intent，禁止盲重放。
     *
     * Code Logic（这个函数做什么）:
     *   POST `/api/mobile/workbench/worktrees/mutation-operation`，query 策略，解码 nullable operation。
     */
    getMutationOperation: (
      clientOperationId: string,
      policy?: HttpRequestPolicy,
    ): Promise<WorkbenchMutationOperation | null> => {
      const id = clientOperationId.trim();
      if (!id) {
        return Promise.reject(
          new OrchestratorRuntimeTransportError('clientOperationId 不能为空', 'protocol'),
        );
      }
      return postJson<WorkbenchMutationOperation | null>(
        `${MOBILE_WORKBENCH_API_PREFIX}/worktrees/mutation-operation`,
        { clientOperationId: id },
        {
          policy: policy ?? { kind: 'query' },
          decoder: nullableDecoder(workbenchMutationOperationDecoder),
        },
      );
    },
  },
} as const;
