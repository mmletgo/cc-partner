/**
 * Deterministic browser backend harness（L1 Playwright / pure unit）。
 *
 * Business Logic（为什么需要这个模块）:
 *   L1 浏览器 E2E 需要统一、可诊断的 Tauri invoke / 同源 fetch / 事件注入，
 *   避免每个 spec 手写整套 mock，并在结束时检测 pending、未消费预期与泄漏 listener。
 *
 * Code Logic（这个模块做什么）:
 *   提供纯逻辑 `BackendHarnessCore`（Vitest 可测）与 Playwright 适配 `installBackendHarness`；
 *   通过 page 边界注入 `__TAURI_INTERNALS__` / `fetch`，不向生产代码暴露测试开关。
 */

import type { Page } from '@playwright/test';

/** 固定故障剖面，供 fault 行为与后续 T4 复用。 */
export type FaultProfile =
  | 'networkOffline'
  | 'timeout'
  | 'malformedJson'
  | 'permissionDenied'
  | 'conflict'
  | 'dbBusy'
  | 'lanBoundaryRejected'
  | 'crossSiteRejected';

/** 单次调用的确定性响应策略。 */
export type HarnessBehavior =
  | { kind: 'resolve'; value: unknown }
  | { kind: 'reject'; error: unknown }
  | { kind: 'defer'; key: string }
  | { kind: 'fault'; profile: FaultProfile };

/** harness 记录的调用痕迹。 */
export type HarnessCall =
  | {
      type: 'invoke';
      command: string;
      args: unknown;
      at: number;
    }
  | {
      type: 'fetch';
      method: string;
      path: string;
      body: unknown;
      at: number;
    }
  | {
      type: 'event-listen';
      event: string;
      eventId: number;
      at: number;
    }
  | {
      type: 'event-unlisten';
      event: string;
      eventId: number;
      at: number;
    };

/** Playwright / 测试侧控制面合同。 */
export interface BackendHarness {
  command(name: string, behavior: HarnessBehavior | readonly HarnessBehavior[]): void;
  route(
    method: 'GET' | 'POST',
    path: string,
    behavior: HarnessBehavior | readonly HarnessBehavior[],
  ): void;
  emit(event: string, payload: unknown): void;
  resolveDeferred(key: string, value?: unknown): void;
  rejectDeferred(key: string, error?: unknown): void;
  calls(): readonly HarnessCall[];
  assertSettled(options?: AssertSettledOptions): void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   浏览器 E2E 中 App 常驻 listener（截图权限/健康提醒等）在用例结束时仍挂载，
 *   不能与纯单元测试里的“泄漏 listener”同等视为失败。
 *
 * Code Logic（这个类型做什么）:
 *   allowLingeringListeners=true 时跳过 listener 计数检查。
 */
export type AssertSettledOptions = {
  allowLingeringListeners?: boolean;
};

/**
 * Business Logic（为什么需要这个类型）:
 *   路径模板匹配需要把 `:id` 段解析为参数，供 route 命中与调试。
 *
 * Code Logic（这个类型做什么）:
 *   命中时返回 params；未命中返回 null。
 */
export type PathMatch = { params: Record<string, string> } | null;

/**
 * Business Logic（为什么需要这个函数）:
 *   transfer/status 等路由需要按段匹配 `:param`，但不能引入完整正则 DSL。
 *
 * Code Logic（这个函数做什么）:
 *   按 `/` 分段比较 template 与 actual（忽略 query）；`:name` 捕获参数；全等段必须一致。
 *
 * @param template 如 `/api/transfer/status/:id`
 * @param actualPath 实际路径，可带 query
 */
export function matchPathTemplate(template: string, actualPath: string): PathMatch {
  const pathOnly = actualPath.split('?')[0] ?? actualPath;
  const templateParts = splitPath(template);
  const actualParts = splitPath(pathOnly);
  if (templateParts.length !== actualParts.length) {
    return null;
  }
  const params: Record<string, string> = {};
  for (let index = 0; index < templateParts.length; index += 1) {
    const expected = templateParts[index] ?? '';
    const actual = actualParts[index] ?? '';
    if (expected.startsWith(':')) {
      const key = expected.slice(1);
      if (!key) {
        return null;
      }
      params[key] = decodeURIComponent(actual);
      continue;
    }
    if (expected !== actual) {
      return null;
    }
  }
  return { params };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   路径比较需要统一去掉空段，避免首尾 `/` 差异导致误匹配。
 *
 * Code Logic（这个函数做什么）:
 *   按 `/` 分割并过滤空字符串。
 */
function splitPath(path: string): string[] {
  return path.split('/').filter((part) => part.length > 0);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   注册行为既支持“粘性单次策略”，也支持按调用序号消费的序列。
 *
 * Code Logic（这个函数做什么）:
 *   规范化为 `{ sticky, queue }` 内部表示。
 */
function normalizeRegistration(
  behavior: HarnessBehavior | readonly HarnessBehavior[],
): { sticky: HarnessBehavior | null; queue: HarnessBehavior[] } {
  if (Array.isArray(behavior)) {
    return { sticky: null, queue: [...behavior] };
  }
  return { sticky: behavior as HarnessBehavior, queue: [] };
}

/**
 * Business Logic（为什么需要这个错误）:
 *   未注册 invoke/fetch 必须立刻失败并带上精确 command/path，避免 silent undefined。
 *
 * Code Logic（这个类做什么）:
 *   继承 Error，附带 surface 与 name 字段。
 */
export class HarnessUnregisteredError extends Error {
  readonly surface: 'invoke' | 'fetch';
  readonly target: string;

  /**
   * Business Logic（为什么需要这个构造）:
   *   统一未注册调用的错误消息格式，便于断言 exact name/path。
   *
   * Code Logic（这个构造做什么）:
   *   生成带 surface 与 target 的 Error。
   */
  constructor(surface: 'invoke' | 'fetch', target: string) {
    super(`Unregistered ${surface}: ${target}`);
    this.name = 'HarnessUnregisteredError';
    this.surface = surface;
    this.target = target;
  }
}

/**
 * Business Logic（为什么需要这个错误）:
 *   assertSettled 失败时需要可读地列出 pending / 未消费 / 泄漏 listener。
 *
 * Code Logic（这个类做什么）:
 *   聚合 settlement 诊断文本。
 */
export class HarnessSettlementError extends Error {
  /**
   * Business Logic（为什么需要这个构造）:
   *   测试结束时一次性抛出全部 settlement 问题。
   *
   * Code Logic（这个构造做什么）:
   *   用 problems 数组拼 message。
   */
  constructor(problems: string[]) {
    super(`Backend harness not settled:\n- ${problems.join('\n- ')}`);
    this.name = 'HarnessSettlementError';
  }
}

/**
 * Business Logic（为什么需要这个类型）:
 *   命令与路由注册项需要同时支持 sticky 与 sequence queue。
 *
 * Code Logic（这个类型做什么）:
 *   保存 sticky 行为与可消费队列。
 */
type BehaviorRegistration = {
  sticky: HarnessBehavior | null;
  queue: HarnessBehavior[];
};

/**
 * Business Logic（为什么需要这个类型）:
 *   defer 需要在 resolveDeferred/rejectDeferred 时唤醒全部等待者。
 *
 * Code Logic（这个类型做什么）:
 *   保存 Promise 的 resolve/reject。
 */
type DeferredWaiter = {
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
};

/**
 * Business Logic（为什么需要这个类型）:
 *   fetch mock 要把 harness 结果编码成可在页面重建的 Response 描述。
 *
 * Code Logic（这个类型做什么）:
 *   描述 status/headers/bodyText 或抛错形态。
 */
export type HarnessFetchResult =
  | {
      ok: true;
      status: number;
      statusText: string;
      headers: Record<string, string>;
      bodyText: string;
    }
  | {
      ok: false;
      errorName: string;
      errorMessage: string;
    };

/**
 * Business Logic（为什么需要这个类）:
 *   单元测试与 Playwright 桥都需要同一套确定性 registry / defer / fault / settlement。
 *
 * Code Logic（这个类做什么）:
 *   纯内存实现 BackendHarness 核心；不触碰 DOM/Page。
 */
export class BackendHarnessCore implements BackendHarness {
  private readonly commands = new Map<string, BehaviorRegistration>();
  private readonly routes = new Map<string, BehaviorRegistration>();
  private readonly callLog: HarnessCall[] = [];
  private readonly deferredWaiters = new Map<string, DeferredWaiter[]>();
  private readonly activeDeferredKeys = new Set<string>();
  private pendingCount = 0;
  private listenerCount = 0;
  private eventIdSeq = 0;
  private clock = 0;
  private readonly eventHandlers = new Map<string, Set<(payload: unknown) => void>>();

  /**
   * Business Logic（为什么需要这个方法）:
   *   测试为 Tauri command 注册确定性响应（含按次序列）。
   *
   * Code Logic（这个方法做什么）:
   *   写入 commands registry；数组为消费队列，单值为 sticky。
   */
  command(name: string, behavior: HarnessBehavior | readonly HarnessBehavior[]): void {
    this.commands.set(name, normalizeRegistration(behavior));
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   测试为同源 HTTP 路由注册确定性响应。
   *
   * Code Logic（这个方法做什么）:
   *   以 `METHOD pathTemplate` 为键写入 routes registry。
   */
  route(
    method: 'GET' | 'POST',
    path: string,
    behavior: HarnessBehavior | readonly HarnessBehavior[],
  ): void {
    this.routes.set(routeKey(method, path), normalizeRegistration(behavior));
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   主动推送 Tauri 事件，避免等待真实轮询。
   *
   * Code Logic（这个方法做什么）:
   *   同步调用已注册 handler（纯核心侧）。
   */
  emit(event: string, payload: unknown): void {
    const handlers = this.eventHandlers.get(event);
    if (!handlers) {
      return;
    }
    for (const handler of [...handlers]) {
      handler(payload);
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   释放 defer，制造 stale/out-of-order 响应。
   *
   * Code Logic（这个方法做什么）:
   *   resolve 该 key 全部 waiter 并清理 pending 跟踪。
   */
  resolveDeferred(key: string, value: unknown = undefined): void {
    const waiters = this.deferredWaiters.get(key) ?? [];
    this.deferredWaiters.delete(key);
    this.activeDeferredKeys.delete(key);
    for (const waiter of waiters) {
      this.pendingCount = Math.max(0, this.pendingCount - 1);
      waiter.resolve(value);
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   让 defer 以失败结束，覆盖取消/后端错误路径。
   *
   * Code Logic（这个方法做什么）:
   *   reject 该 key 全部 waiter 并清理 pending。
   */
  rejectDeferred(key: string, error: unknown = new Error(`Deferred rejected: ${key}`)): void {
    const waiters = this.deferredWaiters.get(key) ?? [];
    this.deferredWaiters.delete(key);
    this.activeDeferredKeys.delete(key);
    for (const waiter of waiters) {
      this.pendingCount = Math.max(0, this.pendingCount - 1);
      waiter.reject(error);
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   断言与诊断需要只读调用轨迹。
   *
   * Code Logic（这个方法做什么）:
   *   返回 callLog 浅只读视图。
   */
  calls(): readonly HarnessCall[] {
    return this.callLog;
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   用例结束时必须没有挂起请求、未消费序列；纯单测还要无泄漏 listener。
   *
   * Code Logic（这个方法做什么）:
   *   收集问题并抛 HarnessSettlementError；
   *   options.allowLingeringListeners 为真时跳过 listener 计数（Playwright App 常驻监听）。
   */
  assertSettled(options?: AssertSettledOptions): void {
    const problems: string[] = [];
    if (this.pendingCount > 0) {
      problems.push(`pending requests: ${this.pendingCount}`);
    }
    if (this.activeDeferredKeys.size > 0) {
      problems.push(`pending deferred keys: ${[...this.activeDeferredKeys].join(', ')}`);
    }
    for (const [name, reg] of this.commands) {
      if (reg.queue.length > 0) {
        problems.push(`unconsumed command expectations for "${name}": ${reg.queue.length}`);
      }
    }
    for (const [key, reg] of this.routes) {
      if (reg.queue.length > 0) {
        problems.push(`unconsumed route expectations for "${key}": ${reg.queue.length}`);
      }
    }
    if (!options?.allowLingeringListeners && this.listenerCount > 0) {
      problems.push(`leaked event listeners: ${this.listenerCount}`);
    }
    if (problems.length > 0) {
      throw new HarnessSettlementError(problems);
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   纯测试需要订阅事件并在 teardown 检查 unlisten。
   *
   * Code Logic（这个方法做什么）:
   *   注册 handler，返回 unlisten；同步更新 listenerCount 与 call log。
   */
  subscribe(event: string, handler: (payload: unknown) => void): () => void {
    const eventId = ++this.eventIdSeq;
    let handlers = this.eventHandlers.get(event);
    if (!handlers) {
      handlers = new Set();
      this.eventHandlers.set(event, handlers);
    }
    handlers.add(handler);
    this.listenerCount += 1;
    this.callLog.push({ type: 'event-listen', event, eventId, at: ++this.clock });
    let active = true;
    return () => {
      if (!active) {
        return;
      }
      active = false;
      handlers?.delete(handler);
      this.listenerCount = Math.max(0, this.listenerCount - 1);
      this.callLog.push({ type: 'event-unlisten', event, eventId, at: ++this.clock });
    };
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   模拟页面发起的 Tauri invoke。
   *
   * Code Logic（这个方法做什么）:
   *   记录调用 → 取行为 → 应用 resolve/reject/defer/fault，并尊重 AbortSignal。
   */
  async handleInvoke(
    command: string,
    args: unknown = undefined,
    signal?: AbortSignal | null,
  ): Promise<unknown> {
    this.callLog.push({ type: 'invoke', command, args, at: ++this.clock });

    if (command === 'plugin:event|listen') {
      return this.handlePluginEventListen(args);
    }
    if (command === 'plugin:event|unlisten') {
      return this.handlePluginEventUnlisten(args);
    }

    const registration = this.commands.get(command);
    if (!registration) {
      throw new HarnessUnregisteredError('invoke', command);
    }
    const behavior = takeBehavior(registration, `command ${command}`);
    return this.applyBehavior(behavior, signal);
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   模拟页面同源 fetch（mobile/P2P HTTP）。
   *
   * Code Logic（这个方法做什么）:
   *   解析 method/path → 模板匹配 route → 应用行为并编码为 HarnessFetchResult。
   */
  async handleFetch(
    method: string,
    path: string,
    body: unknown = undefined,
    signal?: AbortSignal | null,
  ): Promise<HarnessFetchResult> {
    const normalizedMethod = method.toUpperCase();
    this.callLog.push({
      type: 'fetch',
      method: normalizedMethod,
      path,
      body,
      at: ++this.clock,
    });

    const registration = this.findRoute(normalizedMethod, path);
    if (!registration) {
      throw new HarnessUnregisteredError('fetch', `${normalizedMethod} ${path}`);
    }
    const behavior = takeBehavior(
      registration,
      `route ${normalizedMethod} ${path}`,
    );

    try {
      const value = await this.applyBehavior(behavior, signal, 'fetch');
      if (behavior.kind === 'fault' && behavior.profile === 'malformedJson') {
        return {
          ok: true,
          status: 200,
          statusText: 'OK',
          headers: { 'content-type': 'application/json' },
          bodyText: '{not-json',
        };
      }
      return {
        ok: true,
        status: 200,
        statusText: 'OK',
        headers: { 'content-type': 'application/json' },
        bodyText: JSON.stringify(value ?? null),
      };
    } catch (error) {
      if (behavior.kind === 'fault') {
        return faultToFetchResult(behavior.profile, error);
      }
      if (error instanceof DOMException && error.name === 'AbortError') {
        return {
          ok: false,
          errorName: 'AbortError',
          errorMessage: error.message || 'Aborted',
        };
      }
      if (error instanceof Error && error.name === 'AbortError') {
        return {
          ok: false,
          errorName: 'AbortError',
          errorMessage: error.message || 'Aborted',
        };
      }
      const message = error instanceof Error ? error.message : String(error);
      const name = error instanceof Error ? error.name : 'Error';
      return {
        ok: false,
        errorName: name,
        errorMessage: message,
      };
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   Playwright 页内 event listen 需要把 listener 计数同步回 core。
   *
   * Code Logic（这个方法做什么）:
   *   增加 listenerCount 并写 call log，返回 eventId。
   */
  trackPageListen(event: string): number {
    const eventId = ++this.eventIdSeq;
    this.listenerCount += 1;
    this.callLog.push({ type: 'event-listen', event, eventId, at: ++this.clock });
    return eventId;
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   页内 unlisten 需与 listen 对称扣减，便于 assertSettled。
   *
   * Code Logic（这个方法做什么）:
   *   减少 listenerCount 并写 call log。
   */
  trackPageUnlisten(event: string, eventId: number): void {
    this.listenerCount = Math.max(0, this.listenerCount - 1);
    this.callLog.push({ type: 'event-unlisten', event, eventId, at: ++this.clock });
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   诊断附件需要序列化调用日志。
   *
   * Code Logic（这个方法做什么）:
   *   JSON.stringify callLog。
   */
  formatCallLog(): string {
    return JSON.stringify(this.callLog, null, 2);
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   plugin:event|listen 在纯核心中也要可追踪。
   *
   * Code Logic（这个方法做什么）:
   *   从 args 取 event 名，增加 listener 计数，返回 eventId。
   */
  private handlePluginEventListen(args: unknown): number {
    const event = readEventName(args);
    const eventId = ++this.eventIdSeq;
    this.listenerCount += 1;
    this.callLog.push({ type: 'event-listen', event, eventId, at: ++this.clock });
    return eventId;
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   plugin:event|unlisten 对称清理 listener 计数。
   *
   * Code Logic（这个方法做什么）:
   *   扣减 listenerCount 并记录 unlisten。
   */
  private handlePluginEventUnlisten(args: unknown): void {
    const event = readEventName(args);
    const eventId = readEventId(args);
    this.listenerCount = Math.max(0, this.listenerCount - 1);
    this.callLog.push({ type: 'event-unlisten', event, eventId, at: ++this.clock });
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   实际 path 可能匹配带参数的模板 route。
   *
   * Code Logic（这个方法做什么）:
   *   先 exact key，再遍历模板 matchPathTemplate。
   */
  private findRoute(method: string, path: string): BehaviorRegistration | null {
    const exact = this.routes.get(routeKey(method, path.split('?')[0] ?? path));
    if (exact) {
      return exact;
    }
    for (const [key, registration] of this.routes) {
      const separator = key.indexOf(' ');
      if (separator < 0) {
        continue;
      }
      const registeredMethod = key.slice(0, separator);
      const template = key.slice(separator + 1);
      if (registeredMethod !== method) {
        continue;
      }
      if (matchPathTemplate(template, path)) {
        return registration;
      }
    }
    return null;
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   四种行为与 AbortSignal 需要统一调度，invoke/fetch 共用。
   *
   * Code Logic（这个方法做什么）:
   *   已 abort 立即失败；defer 挂起；fault 映射；resolve/reject 即时。
   */
  private async applyBehavior(
    behavior: HarnessBehavior,
    signal?: AbortSignal | null,
    surface: 'invoke' | 'fetch' = 'invoke',
  ): Promise<unknown> {
    throwIfAborted(signal);

    switch (behavior.kind) {
      case 'resolve':
        return behavior.value;
      case 'reject':
        throw toError(behavior.error);
      case 'defer':
        return this.waitForDeferred(behavior.key, signal);
      case 'fault':
        return this.applyFault(behavior.profile, signal, surface);
      default: {
        const _exhaustive: never = behavior;
        throw new Error(`Unknown harness behavior: ${JSON.stringify(_exhaustive)}`);
      }
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   defer 用于 stale response：后发请求可先返回，早发请求稍后才 settle。
   *
   * Code Logic（这个方法做什么）:
   *   登记 waiter + pending；AbortSignal 触发时 reject 并从队列移除。
   */
  private waitForDeferred(key: string, signal?: AbortSignal | null): Promise<unknown> {
    this.pendingCount += 1;
    this.activeDeferredKeys.add(key);
    return new Promise<unknown>((resolve, reject) => {
      const waiter: DeferredWaiter = {
        resolve: (value) => {
          cleanup();
          resolve(value);
        },
        reject: (error) => {
          cleanup();
          reject(error);
        },
      };

      /**
       * Business Logic（为什么需要这个函数）:
       *   settle 或 abort 后必须去掉 signal 监听，避免泄漏。
       *
       * Code Logic（这个函数做什么）:
       *   removeEventListener abort。
       */
      const cleanup = (): void => {
        if (signal) {
          signal.removeEventListener('abort', onAbort);
        }
      };

      /**
       * Business Logic（为什么需要这个函数）:
       *   调用方 AbortSignal 取消时应结束 defer 等待。
       *
       * Code Logic（这个函数做什么）:
       *   从 waiters 移除自身，扣 pending，reject AbortError。
       */
      const onAbort = (): void => {
        const list = this.deferredWaiters.get(key);
        if (list) {
          const next = list.filter((item) => item !== waiter);
          if (next.length === 0) {
            this.deferredWaiters.delete(key);
            this.activeDeferredKeys.delete(key);
          } else {
            this.deferredWaiters.set(key, next);
          }
        }
        this.pendingCount = Math.max(0, this.pendingCount - 1);
        cleanup();
        reject(createAbortError());
      };

      const list = this.deferredWaiters.get(key) ?? [];
      list.push(waiter);
      this.deferredWaiters.set(key, list);

      if (signal) {
        if (signal.aborted) {
          onAbort();
          return;
        }
        signal.addEventListener('abort', onAbort, { once: true });
      }
    });
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   统一 fault profile 到 invoke/fetch 可消费的错误或占位值。
   *
   * Code Logic（这个方法做什么）:
   *   timeout 挂起至 abort；malformedJson 在 fetch 路径由 handleFetch 写非 JSON body，
   *   在 invoke 路径 **resolve** 非法但可 JSON 序列化的 DTO 形状（生产 invokeDecoded → ContractDecodeError）；
   *   其余 throw 带 code 的 Error。
   */
  private async applyFault(
    profile: FaultProfile,
    signal: AbortSignal | null | undefined,
    surface: 'invoke' | 'fetch',
  ): Promise<unknown> {
    switch (profile) {
      case 'timeout':
        return this.waitForTimeoutFault(signal);
      case 'networkOffline':
        throw createFaultError('NetworkError', 'network offline', 'NETWORK_OFFLINE');
      case 'malformedJson':
        if (surface === 'fetch') {
          // handleFetch 对 malformedJson 覆盖 bodyText 为 '{not-json'；此处返回占位即可。
          return null;
        }
        // invoke：resolve 非法 DTO，让生产 invokeDecoded 抛 ContractDecodeError（禁止 throw SyntaxError）。
        return { notAValidDto: true };
      case 'permissionDenied':
        throw createFaultError('Error', 'permission denied', 'PERMISSION_DENIED');
      case 'conflict':
        throw createFaultError('Error', 'conflict', 'CONFLICT');
      case 'dbBusy':
        throw createFaultError('Error', 'database is busy', 'DB_BUSY');
      case 'lanBoundaryRejected':
        throw createFaultError('Error', 'lan boundary rejected', 'LAN_BOUNDARY_REJECTED');
      case 'crossSiteRejected':
        throw createFaultError('Error', 'cross-site request rejected', 'CROSS_SITE_REJECTED');
      default: {
        const _exhaustive: never = profile;
        throw new Error(`Unknown fault profile: ${_exhaustive}`);
      }
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   timeout fault 应保持 pending 直到 AbortSignal 或测试失败暴露挂起。
   *
   * Code Logic（这个方法做什么）:
   *   增加 pending；abort 时扣减并 reject AbortError；无 signal 则永久 pending。
   */
  private waitForTimeoutFault(signal?: AbortSignal | null): Promise<unknown> {
    this.pendingCount += 1;
    return new Promise<unknown>((_resolve, reject) => {
      /**
       * Business Logic（为什么需要这个函数）:
       *   超时剖面在调用方 abort 时必须结束，模拟真实 fetch/invoke 超时。
       *
       * Code Logic（这个函数做什么）:
       *   扣 pending 并 reject AbortError。
       */
      const onAbort = (): void => {
        this.pendingCount = Math.max(0, this.pendingCount - 1);
        reject(createAbortError());
      };
      if (!signal) {
        return;
      }
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener('abort', onAbort, { once: true });
    });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   route registry 需要稳定主键。
 *
 * Code Logic（这个函数做什么）:
 *   返回 `METHOD path`。
 */
function routeKey(method: string, path: string): string {
  return `${method.toUpperCase()} ${path}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   sticky 与 sequence 消费规则必须一致。
 *
 * Code Logic（这个函数做什么）:
 *   优先 shift queue；否则用 sticky；都没有则抛错。
 */
function takeBehavior(registration: BehaviorRegistration, label: string): HarnessBehavior {
  if (registration.queue.length > 0) {
    const next = registration.queue.shift();
    if (!next) {
      throw new Error(`Empty behavior queue for ${label}`);
    }
    return next;
  }
  if (registration.sticky) {
    return registration.sticky;
  }
  throw new Error(`No remaining harness behavior for ${label}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   reject 行为可能传入字符串或 Error-like。
 *
 * Code Logic（这个函数做什么）:
 *   规范为 Error 实例。
 */
function toError(error: unknown): Error {
  if (error instanceof Error) {
    return error;
  }
  if (typeof error === 'string') {
    return new Error(error);
  }
  return new Error(JSON.stringify(error));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   AbortSignal 已取消时调用不应再执行行为。
 *
 * Code Logic（这个函数做什么）:
 *   aborted 则抛 AbortError。
 */
function throwIfAborted(signal?: AbortSignal | null): void {
  if (signal?.aborted) {
    throw createAbortError();
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器与 Node 对 AbortError 类型略有差异，测试需要稳定 name。
 *
 * Code Logic（这个函数做什么）:
 *   优先 DOMException；否则 Error name=AbortError。
 */
function createAbortError(): Error {
  if (typeof DOMException === 'function') {
    return new DOMException('Aborted', 'AbortError');
  }
  const error = new Error('Aborted');
  error.name = 'AbortError';
  return error;
}

/**
 * FaultProfile → 稳定错误码映射（与 T4 faultRecovery 分类表对齐）。
 *
 * Business Logic（为什么需要这个常量）:
 *   L0/L1 测试与前端分类器需要同一套 profile→code 合同，避免各处手写字符串漂移。
 *
 * Code Logic（这个常量做什么）:
 *   导出只读 Record；timeout 在 invoke 路径常体现为 AbortError，仍保留 TIMEOUT 码供直接映射。
 */
export const FAULT_PROFILE_CODES: Readonly<Record<FaultProfile, string>> = {
  networkOffline: 'NETWORK_OFFLINE',
  timeout: 'TIMEOUT',
  malformedJson: 'MALFORMED_JSON',
  permissionDenied: 'PERMISSION_DENIED',
  conflict: 'CONFLICT',
  dbBusy: 'DB_BUSY',
  lanBoundaryRejected: 'LAN_BOUNDARY_REJECTED',
  crossSiteRejected: 'CROSS_SITE_REJECTED',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与故障分类需要从 FaultProfile 拿到稳定 code，而不依赖 Error 实例形态。
 *
 * Code Logic（这个函数做什么）:
 *   查 FAULT_PROFILE_CODES 表返回字符串码。
 */
export function faultProfileCode(profile: FaultProfile): string {
  return FAULT_PROFILE_CODES[profile];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   fault profile 需要带稳定 code 字段供前端分类；测试可直接构造同类错误。
 *
 * Code Logic（这个函数做什么）:
 *   创建 Error 并挂 name/message/code。
 */
export function createFaultError(name: string, message: string, code: string): Error {
  const error = new Error(message);
  error.name = name;
  (error as Error & { code?: string }).code = code;
  return error;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   fetch 桥不能直接抛所有 fault，部分应映射为网络错误描述。
 *
 * Code Logic（这个函数做什么）:
 *   把 profile 转为 HarnessFetchResult 失败形态。
 */
function faultToFetchResult(profile: FaultProfile, error: unknown): HarnessFetchResult {
  if (profile === 'networkOffline') {
    return {
      ok: false,
      errorName: 'TypeError',
      errorMessage: 'Failed to fetch',
    };
  }
  if (profile === 'timeout') {
    return {
      ok: false,
      errorName: 'AbortError',
      errorMessage: error instanceof Error ? error.message : 'Aborted',
    };
  }
  const message = error instanceof Error ? error.message : String(error);
  const name = error instanceof Error ? error.name : 'Error';
  return {
    ok: false,
    errorName: name,
    errorMessage: message,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Tauri event listen args 形态可能是 { event } 或嵌套。
 *
 * Code Logic（这个函数做什么）:
 *   尽量读取 event 字符串，否则 `unknown`。
 */
function readEventName(args: unknown): string {
  if (args && typeof args === 'object' && 'event' in args) {
    const event = (args as { event?: unknown }).event;
    if (typeof event === 'string') {
      return event;
    }
  }
  return 'unknown';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   unlisten 需要 eventId 以便 call log 对齐。
 *
 * Code Logic（这个函数做什么）:
 *   读取 eventId/id 数字字段，默认 0。
 */
function readEventId(args: unknown): number {
  if (args && typeof args === 'object') {
    const record = args as { eventId?: unknown; id?: unknown };
    if (typeof record.eventId === 'number') {
      return record.eventId;
    }
    if (typeof record.id === 'number') {
      return record.id;
    }
  }
  return 0;
}

/** Playwright 侧控制器：同步 registry + 异步页内 emit。 */
export interface PlaywrightBackendHarness extends BackendHarness {
  /** 底层纯核心，供高级诊断。 */
  readonly core: BackendHarnessCore;
  /**
   * Business Logic（为什么需要这个方法）:
   *   必须在 goto 前安装 init script 与 bridge。
   *
   * Code Logic（这个方法做什么）:
   *   exposeBinding + addInitScript。
   */
  install(page: Page): Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   fixtures 与 E2E 需要一键创建已接线的 harness 控制器。
 *
 * Code Logic（这个函数做什么）:
 *   构造 PlaywrightBackendHarness；install 后把 invoke/fetch 桥到 core。
 */
export function createBackendHarness(): PlaywrightBackendHarness {
  const core = new BackendHarnessCore();
  let page: Page | null = null;
  let installed = false;
  let bindingSeq = 0;

  /**
   * Business Logic（为什么需要这个函数）:
   *   每页 exposeBinding 名必须唯一，避免 fixture 重用冲突。
   *
   * Code Logic（这个函数做什么）:
   *   递增生成 binding 前缀。
   */
  const nextBindingName = (suffix: string): string => {
    bindingSeq += 1;
    return `__ccPartnerHarness_${bindingSeq}_${suffix}`;
  };

  const harness: PlaywrightBackendHarness = {
    core,

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.command。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core.command。
     */
    command(name, behavior) {
      core.command(name, behavior);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.route。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core.route。
     */
    route(method, path, behavior) {
      core.route(method, path, behavior);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   向页面内真实 callback registry 推送事件。
     *
     * Code Logic（这个方法做什么）:
     *   page.evaluate 调用页内 emit；同时 core.emit 覆盖纯订阅。
     */
    emit(event, payload) {
      core.emit(event, payload);
      if (!page) {
        return;
      }
      void page.evaluate(
        ({ eventName, eventPayload }) => {
          const runtime = (
            window as unknown as {
              __ccPartnerHarnessRuntime?: {
                emit: (name: string, data: unknown) => void;
              };
            }
          ).__ccPartnerHarnessRuntime;
          runtime?.emit(eventName, eventPayload);
        },
        { eventName: event, eventPayload: payload },
      );
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.resolveDeferred。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core。
     */
    resolveDeferred(key, value) {
      core.resolveDeferred(key, value);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.rejectDeferred。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core。
     */
    rejectDeferred(key, error) {
      core.rejectDeferred(key, error);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.calls。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core.calls。
     */
    calls() {
      return core.calls();
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   见 BackendHarness.assertSettled。
     *
     * Code Logic（这个方法做什么）:
     *   委托 core.assertSettled。
     */
    assertSettled(options) {
      core.assertSettled(options);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   在浏览器页安装 Tauri/fetch mock，且不污染生产代码路径。
     *
     * Code Logic（这个方法做什么）:
     *   exposeBinding 桥接 core；addInitScript 安装 __TAURI_INTERNALS__ 与 fetch 包装。
     */
    async install(targetPage: Page): Promise<void> {
      if (installed) {
        throw new Error('Backend harness already installed on a page');
      }
      page = targetPage;
      installed = true;

      const invokeBinding = nextBindingName('invoke');
      const fetchBinding = nextBindingName('fetch');
      const listenBinding = nextBindingName('listen');
      const unlistenBinding = nextBindingName('unlisten');

      await targetPage.exposeBinding(
        invokeBinding,
        async (_source, command: string, args: unknown) => {
          try {
            const value = await core.handleInvoke(command, args);
            return { ok: true as const, value };
          } catch (error) {
            return {
              ok: false as const,
              errorName: error instanceof Error ? error.name : 'Error',
              errorMessage: error instanceof Error ? error.message : String(error),
              errorCode:
                error instanceof Error
                  ? (error as Error & { code?: string }).code
                  : undefined,
            };
          }
        },
      );

      await targetPage.exposeBinding(
        fetchBinding,
        async (
          _source,
          method: string,
          path: string,
          body: unknown,
        ): Promise<HarnessFetchResult> => {
          try {
            return await core.handleFetch(method, path, body);
          } catch (error) {
            if (error instanceof HarnessUnregisteredError) {
              return {
                ok: false,
                errorName: error.name,
                errorMessage: error.message,
              };
            }
            return {
              ok: false,
              errorName: error instanceof Error ? error.name : 'Error',
              errorMessage: error instanceof Error ? error.message : String(error),
            };
          }
        },
      );

      await targetPage.exposeBinding(
        listenBinding,
        async (_source, event: string): Promise<number> => core.trackPageListen(event),
      );

      await targetPage.exposeBinding(
        unlistenBinding,
        async (_source, event: string, eventId: number): Promise<void> => {
          core.trackPageUnlisten(event, eventId);
        },
      );

      await targetPage.addInitScript(
        ({ invokeName, fetchName, listenName, unlistenName }) => {
          type InvokeBridgeResult =
            | { ok: true; value: unknown }
            | {
                ok: false;
                errorName: string;
                errorMessage: string;
                errorCode?: string;
              };

          type FetchBridgeResult =
            | {
                ok: true;
                status: number;
                statusText: string;
                headers: Record<string, string>;
                bodyText: string;
              }
            | { ok: false; errorName: string; errorMessage: string };

          type HarnessWindow = Window & {
            __TAURI_INTERNALS__?: {
              invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
              transformCallback: (callback: unknown, once?: boolean) => number;
              unregisterCallback: (id: number) => void;
              metadata?: {
                currentWindow: { label: string };
                currentWebview?: { windowLabel: string; label: string };
              };
            };
            __TAURI_EVENT_PLUGIN_INTERNALS__?: {
              unregisterListener: (event: string, eventId: number) => void;
            };
            __ccPartnerHarnessRuntime?: {
              emit: (event: string, payload: unknown) => void;
            };
          };

          const win = window as HarnessWindow;
          const callbacks = new Map<number, (payload: unknown) => void>();
          const eventIdToCallback = new Map<number, number>();
          const eventIdToEvent = new Map<number, string>();
          let callbackSeq = 0;

          /**
           * Business Logic（为什么需要这个函数）:
           *   页内需要把 bridge 错误还原为可抛 Error。
           *
           * Code Logic（这个函数做什么）:
           *   根据 name/message/code 构造 Error。
           */
          const restoreError = (
            name: string,
            message: string,
            code?: string,
          ): Error => {
            const error = new Error(message);
            error.name = name;
            if (code) {
              (error as Error & { code?: string }).code = code;
            }
            return error;
          };

          /**
           * Business Logic（为什么需要这个函数）:
           *   Tauri listen 回调载荷应类似真实 event envelope。
           *
           * Code Logic（这个函数做什么）:
           *   调用 transformCallback 登记的函数，传入 { event, id, payload }。
           */
          const emit = (event: string, payload: unknown): void => {
            for (const [eventId, eventName] of eventIdToEvent) {
              if (eventName !== event) {
                continue;
              }
              const callbackId = eventIdToCallback.get(eventId);
              if (callbackId == null) {
                continue;
              }
              const callback = callbacks.get(callbackId);
              callback?.({ event, id: eventId, payload });
            }
          };

          win.__ccPartnerHarnessRuntime = { emit };

          /**
           * Business Logic（为什么需要这个函数）:
           *   解析 RequestInfo 为 method/path/body，供 harness route 匹配。
           *
           * Code Logic（这个函数做什么）:
           *   支持 string/URL/Request；path 只保留 pathname+search。
           */
          const parseRequest = async (
            input: RequestInfo | URL,
            init?: RequestInit,
          ): Promise<{ method: string; path: string; body: unknown }> => {
            if (typeof Request !== 'undefined' && input instanceof Request) {
              const method = (init?.method ?? input.method ?? 'GET').toUpperCase();
              const url = new URL(input.url, window.location.origin);
              const path = `${url.pathname}${url.search}`;
              let body: unknown = undefined;
              if (init?.body != null) {
                body = typeof init.body === 'string' ? tryParseJson(init.body) : init.body;
              } else if (method !== 'GET' && method !== 'HEAD') {
                try {
                  const text = await input.clone().text();
                  body = text ? tryParseJson(text) : undefined;
                } catch {
                  body = undefined;
                }
              }
              return { method, path, body };
            }
            const raw = typeof input === 'string' ? input : input.toString();
            const url = new URL(raw, window.location.origin);
            const method = (init?.method ?? 'GET').toUpperCase();
            const path = `${url.pathname}${url.search}`;
            let body: unknown = undefined;
            if (init?.body != null && typeof init.body === 'string') {
              body = tryParseJson(init.body);
            }
            return { method, path, body };
          };

          /**
           * Business Logic（为什么需要这个函数）:
           *   JSON body 尽量解析，失败则保留原字符串。
           *
           * Code Logic（这个方法做什么）:
           *   JSON.parse try/catch。
           */
          const tryParseJson = (text: string): unknown => {
            try {
              return JSON.parse(text);
            } catch {
              return text;
            }
          };

          /**
           * Business Logic（为什么需要这个函数）:
           *   仅拦截同源 /api 与相对 API 路径，静态资源仍走真实 Vite fetch。
           *
           * Code Logic（这个函数做什么）:
           *   pathname 以 /api/ 开头则 true。
           */
          const shouldIntercept = (path: string): boolean => {
            const pathname = path.split('?')[0] ?? path;
            return pathname === '/api' || pathname.startsWith('/api/');
          };

          const originalFetch = window.fetch.bind(window);

          window.fetch = async (
            input: RequestInfo | URL,
            init?: RequestInit,
          ): Promise<Response> => {
            const parsed = await parseRequest(input, init);
            if (!shouldIntercept(parsed.path)) {
              return originalFetch(input as RequestInfo, init);
            }

            const bridge = (
              window as unknown as Record<
                string,
                (method: string, path: string, body: unknown) => Promise<FetchBridgeResult>
              >
            )[fetchName];
            const result = await bridge(parsed.method, parsed.path, parsed.body);
            if (!result.ok) {
              const error = restoreError(result.errorName, result.errorMessage);
              throw error;
            }
            return new Response(result.bodyText, {
              status: result.status,
              statusText: result.statusText,
              headers: result.headers,
            });
          };

          win.__TAURI_INTERNALS__ = {
            metadata: {
              currentWindow: { label: 'main' },
              currentWebview: { windowLabel: 'main', label: 'main' },
            },
            /**
             * Business Logic（为什么需要这个函数）:
             *   生产前端只经 invoke 访问后端；测试用 bridge 替换真实 IPC。
             *
             * Code Logic（这个函数做什么）:
             *   event listen/unlisten 接线页内 callback；其它命令走 Node core。
             */
            invoke: async (cmd: string, args?: Record<string, unknown>) => {
              if (cmd === 'plugin:event|listen') {
                const event =
                  args && typeof args.event === 'string' ? args.event : 'unknown';
                const handlerId =
                  args && typeof args.handler === 'number' ? args.handler : -1;
                const listenBridge = (
                  window as unknown as Record<
                    string,
                    (eventName: string) => Promise<number>
                  >
                )[listenName];
                const eventId = await listenBridge(event);
                if (handlerId >= 0) {
                  eventIdToCallback.set(eventId, handlerId);
                  eventIdToEvent.set(eventId, event);
                }
                return eventId;
              }

              if (cmd === 'plugin:event|unlisten') {
                const eventId =
                  args && typeof args.eventId === 'number'
                    ? args.eventId
                    : args && typeof args.id === 'number'
                      ? args.id
                      : 0;
                const event = eventIdToEvent.get(eventId) ?? readArgEvent(args);
                const unlistenBridge = (
                  window as unknown as Record<
                    string,
                    (eventName: string, id: number) => Promise<void>
                  >
                )[unlistenName];
                await unlistenBridge(event, eventId);
                const callbackId = eventIdToCallback.get(eventId);
                eventIdToCallback.delete(eventId);
                eventIdToEvent.delete(eventId);
                if (callbackId != null) {
                  callbacks.delete(callbackId);
                }
                return undefined;
              }

              const invokeBridge = (
                window as unknown as Record<
                  string,
                  (command: string, invokeArgs: unknown) => Promise<InvokeBridgeResult>
                >
              )[invokeName];
              const result = await invokeBridge(cmd, args);
              if (!result.ok) {
                throw restoreError(
                  result.errorName,
                  result.errorMessage,
                  result.errorCode,
                );
              }
              return result.value;
            },
            /**
             * Business Logic（为什么需要这个函数）:
             *   @tauri-apps/api event.listen 依赖 transformCallback 注册 JS 回调。
             *
             * Code Logic（这个函数做什么）:
             *   分配 id 并保存 callback。
             */
            transformCallback: (callback: unknown) => {
              callbackSeq += 1;
              if (typeof callback === 'function') {
                callbacks.set(
                  callbackSeq,
                  callback as (payload: unknown) => void,
                );
              }
              return callbackSeq;
            },
            /**
             * Business Logic（为什么需要这个函数）:
             *   unlisten 后应释放 callback 防止泄漏。
             *
             * Code Logic（这个函数做什么）:
             *   从 callbacks Map 删除 id。
             */
            unregisterCallback: (id: number) => {
              callbacks.delete(id);
            },
          };

          win.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
            /**
             * Business Logic（为什么需要这个函数）:
             *   Tauri event plugin 卸载 listener 的内部钩子；
             *   部分 unlisten 路径只走 unregisterListener 而不再 invoke plugin:event|unlisten。
             *
             * Code Logic（这个函数做什么）:
             *   删除页内映射，并经 bridge 扣减 harness listener 计数（与 plugin:event|unlisten 对称）。
             */
            unregisterListener: (event: string, eventId: number) => {
              // 若 plugin:event|unlisten 已清理映射，则不再二次扣减 listener 计数
              if (!eventIdToEvent.has(eventId) && !eventIdToCallback.has(eventId)) {
                return;
              }
              const eventName = eventIdToEvent.get(eventId) ?? event;
              eventIdToCallback.delete(eventId);
              eventIdToEvent.delete(eventId);
              const unlistenBridge = (
                window as unknown as Record<
                  string,
                  (eventName: string, id: number) => Promise<void>
                >
              )[unlistenName];
              void unlistenBridge(eventName, eventId);
            },
          };

          /**
           * Business Logic（为什么需要这个函数）:
           *   unlisten args 可能只带 id 不带 event。
           *
           * Code Logic（这个函数做什么）:
           *   读取 args.event 字符串。
           */
          function readArgEvent(args: Record<string, unknown> | undefined): string {
            if (args && typeof args.event === 'string') {
              return args.event;
            }
            return 'unknown';
          }
        },
        {
          invokeName: invokeBinding,
          fetchName: fetchBinding,
          listenName: listenBinding,
          unlistenName: unlistenBinding,
        },
      );
    },
  };

  return harness;
}

/**
 * Business Logic（为什么需要这个常量）:
 *   mobile E2E 只用 per-test viewport，不新增 Playwright browser project。
 *
 * Code Logic（这个常量做什么）:
 *   提供 iPhone 类 390×844 视口预设。
 */
export const MOBILE_VIEWPORT = { width: 390, height: 844 } as const;
