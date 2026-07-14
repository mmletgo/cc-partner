/**
 * Workbench HTTP NDJSON 事件订阅。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 普通浏览器没有 Tauri event，需要通过 `/api/workbench/events` 长连接接收终端输出等事件。
 *
 * Code Logic（这个模块做什么）:
 *   提供 NDJSON chunk parser（含 typed heartbeat），并提供 React hook：
 *   lifecycle AbortController + 每连接 child controller；35s watchdog 仅 abort 子连接并重连。
 */

import { useEffect } from 'react';
import type {
  WorkbenchHttpEvent,
  WorkbenchMergeProgressEvent,
  WorkbenchTerminalOutputEvent,
  WorkbenchTerminalStatusEvent,
} from '@/lib/types';
import type { WorkbenchTerminalBufferStore } from './workbenchTerminalBuffer';

/** 断线后重连延迟（毫秒）。 */
export const WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS = 2_000;
/** 无完整 data/heartbeat frame 时 abort 当前连接的 watchdog（毫秒）。 */
export const WORKBENCH_HTTP_EVENT_WATCHDOG_MS = 35_000;
/** 服务端 typed heartbeat 周期（文档/协议约定，客户端用于预期）。 */
export const WORKBENCH_HTTP_EVENT_HEARTBEAT_INTERVAL_MS = 15_000;

/**
 * Workbench NDJSON parser 状态。
 *
 * Business Logic（为什么需要这个接口）:
 *   HTTP 事件流的网络 chunk 可能停在 JSON 行中间，移动端必须缓存未完成内容等待下一段。
 *
 * Code Logic（字段说明）:
 *   pending 保存尚未遇到换行符的半行字符串；解析到完整行后会被更新为剩余尾部。
 */
export interface WorkbenchNdjsonParserState {
  pending: string;
}

/**
 * NDJSON 帧：业务事件或 heartbeat。
 *
 * Business Logic（为什么需要这个类型）:
 *   heartbeat 不是业务事件，但必须重置 watchdog；parser 需在业务解码前识别。
 *
 * Code Logic（联合形态）:
 *   event 携带 WorkbenchHttpEvent；heartbeat 携带 RFC3339 sentAt。
 */
export type WorkbenchNdjsonFrame =
  | { kind: 'event'; event: WorkbenchHttpEvent }
  | { kind: 'heartbeat'; sentAt: string };

/**
 * Workbench HTTP 事件 hook 参数。
 *
 * Business Logic（为什么需要这个接口）:
 *   移动端只应在启用 HTTP transport 时连接 NDJSON，并把输出写入传入的终端 buffer store。
 *
 * Code Logic（字段说明）:
 *   store 接收 terminalOutput；enabled 控制连接生命周期；
 *   reconnectDelayMs/watchdogMs 供测试注入。
 */
export interface UseWorkbenchHttpEventsOptions {
  store: WorkbenchTerminalBufferStore;
  enabled: boolean;
  reconnectDelayMs?: number;
  watchdogMs?: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   NDJSON 事件来自同源 HTTP route，但前端仍需在运行时确认结构，避免错误数据写入终端 buffer。
 *
 * Code Logic（这个函数做什么）:
 *   判断 value 是否为普通 object record，供后续 payload type guard 复用。
 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   terminalOutput 是移动端当前唯一需要实时消费的事件，结构错误时不能写入未知 session。
 *
 * Code Logic（这个函数做什么）:
 *   检查 payload 是否包含 sessionId/chunk 字符串和 seq/ts 数字。
 */
function isTerminalOutputPayload(value: unknown): value is WorkbenchTerminalOutputEvent {
  if (!isRecord(value)) return false;
  return (
    typeof value.sessionId === 'string' &&
    typeof value.chunk === 'string' &&
    typeof value.seq === 'number' &&
    typeof value.ts === 'number'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   terminalStatus 后续会驱动移动端 tab 状态，parser 应先保留这类事件并验证基本结构。
 *
 * Code Logic（这个函数做什么）:
 *   检查 payload 是否包含 sessionId/status 字符串、exitCode 数字或 null、ts 数字。
 */
function isTerminalStatusPayload(value: unknown): value is WorkbenchTerminalStatusEvent {
  if (!isRecord(value)) return false;
  return (
    typeof value.sessionId === 'string' &&
    typeof value.status === 'string' &&
    (typeof value.exitCode === 'number' || value.exitCode === null) &&
    typeof value.ts === 'number'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   mergeProgress 后续会用于移动端合并阶段 UI，parser 应能安全传递项目和 worktree 维度的事件。
 *
 * Code Logic（这个函数做什么）:
 *   检查 payload 是否包含 projectId/worktreeId 字符串以及基本 stage 对象。
 */
function isMergeProgressPayload(value: unknown): value is WorkbenchMergeProgressEvent {
  if (!isRecord(value)) return false;
  return (
    typeof value.projectId === 'string' &&
    typeof value.worktreeId === 'string' &&
    isRecord(value.stage)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   server 每 15s 发送 typed heartbeat，parser 必须在业务解码前识别以免当未知事件抛错。
 *
 * Code Logic（这个函数做什么）:
 *   type===heartbeat 且 sentAt 为非空字符串。
 */
function isHeartbeatFrame(value: unknown): value is { type: 'heartbeat'; sentAt: string } {
  if (!isRecord(value) || value.type !== 'heartbeat') return false;
  return typeof value.sentAt === 'string' && value.sentAt.trim().length > 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   parser 需要把 JSON.parse 的 unknown 值缩窄成业务事件或 heartbeat。
 *
 * Code Logic（这个函数做什么）:
 *   先识别 heartbeat；再按 serde tag `type` 分支校验 payload；不支持或结构错误时抛出 Error。
 */
export function parseWorkbenchNdjsonFrame(value: unknown): WorkbenchNdjsonFrame {
  if (isHeartbeatFrame(value)) {
    return { kind: 'heartbeat', sentAt: value.sentAt };
  }
  if (!isRecord(value) || typeof value.type !== 'string') {
    throw new Error('Workbench HTTP event 缺少 type');
  }

  if (value.type === 'terminalOutput' && isTerminalOutputPayload(value.payload)) {
    return { kind: 'event', event: { type: value.type, payload: value.payload } };
  }
  if (value.type === 'terminalStatus' && isTerminalStatusPayload(value.payload)) {
    return { kind: 'event', event: { type: value.type, payload: value.payload } };
  }
  if (value.type === 'mergeProgress' && isMergeProgressPayload(value.payload)) {
    return { kind: 'event', event: { type: value.type, payload: value.payload } };
  }

  throw new Error(`不支持的 Workbench HTTP event: ${value.type}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `/api/workbench/events` 使用 NDJSON，移动端必须跨 chunk 拼接完整行，并识别 heartbeat。
 *
 * Code Logic（这个函数做什么）:
 *   将 state.pending 与新 chunk 合并，只解析以换行结束的完整 JSON 行；返回 frames（含 heartbeat）。
 */
export function parseWorkbenchNdjsonFrames(
  state: WorkbenchNdjsonParserState,
  chunk: string,
): WorkbenchNdjsonFrame[] {
  const combined = `${state.pending}${chunk}`;
  const lines = combined.split('\n');
  state.pending = lines.pop() ?? '';

  const frames: WorkbenchNdjsonFrame[] = [];
  lines.forEach((line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    frames.push(parseWorkbenchNdjsonFrame(JSON.parse(trimmed)));
  });
  return frames;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   既有测试与消费路径以业务事件数组为主；heartbeat 不进入业务事件列表。
 *
 * Code Logic（这个函数做什么）:
 *   解析 frames 后仅收集 kind=event 的 WorkbenchHttpEvent。
 */
export function parseWorkbenchNdjsonChunk(
  state: WorkbenchNdjsonParserState,
  chunk: string,
): WorkbenchHttpEvent[] {
  return parseWorkbenchNdjsonFrames(state, chunk)
    .filter((frame): frame is Extract<WorkbenchNdjsonFrame, { kind: 'event' }> => frame.kind === 'event')
    .map((frame) => frame.event);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输出事件可能高频到达，移动端必须写入外部 store，而不是进入 React state 触发整页重渲染。
 *
 * Code Logic（这个函数做什么）:
 *   仅消费 terminalOutput 事件并调用 store.append；其它事件暂时保留给后续 UI。
 */
function consumeWorkbenchHttpEvent(
  store: WorkbenchTerminalBufferStore,
  event: WorkbenchHttpEvent,
): void {
  if (event.type !== 'terminalOutput') return;
  store.append(event.payload.sessionId, event.payload.chunk);
}

/**
 * useWorkbenchHttpEvents（移动端 Workbench HTTP 事件订阅）
 *
 * Business Logic（为什么需要这个 hook）:
 *   `/mobile` 页面需要持续接收同源 HTTP NDJSON terminal 输出，半开连接需 heartbeat watchdog 后重连。
 *
 * Code Logic（这个 hook 做什么）:
 *   lifecycle AbortController 覆盖 hook 生命周期；每次 connect 新建 child controller；
 *   业务帧与 heartbeat 均重置 35s watchdog；watchdog 仅 abort child 并在 lifecycle 仍活跃时重连。
 */
export function useWorkbenchHttpEvents({
  store,
  enabled,
  reconnectDelayMs = WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS,
  watchdogMs = WORKBENCH_HTTP_EVENT_WATCHDOG_MS,
}: UseWorkbenchHttpEventsOptions): void {
  useEffect(() => {
    if (!enabled) return undefined;

    const lifecycleController = new AbortController();
    let stopped = false;
    let reconnectTimer: number | null = null;
    let activeConnectionController: AbortController | null = null;

    /**
     * Business Logic（为什么需要这个函数）:
     *   HTTP 长连接可能因为网络切换、桌面端重启或 heartbeat 丢失而断开，移动端需要自动恢复事件流。
     *
     * Code Logic（这个函数做什么）:
     *   在指定延迟后重新调用 connect；lifecycle 已 abort 或已停止时不再调度。
     */
    function scheduleReconnect(): void {
      if (stopped || lifecycleController.signal.aborted) return;
      reconnectTimer = window.setTimeout(() => {
        void connect();
      }, reconnectDelayMs);
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   移动端 Workbench 需要把同源 NDJSON 事件流持续转成终端 buffer 增量，并检测半开连接。
     *
     * Code Logic（这个函数做什么）:
     *   建立 fetch 长连接（child signal），逐段读取 ReadableStream；
     *   业务帧与 heartbeat 重置 watchdog；异常或 watchdog abort 后若 lifecycle 仍在则重连。
     */
    async function connect(): Promise<void> {
      if (stopped || lifecycleController.signal.aborted) return;

      const connectionController = new AbortController();
      activeConnectionController = connectionController;
      /**
       * Business Logic（为什么需要这个函数）:
       *   lifecycle 结束时必须立刻取消当前连接，避免卸载后继续读写。
       *
       * Code Logic（这个函数做什么）:
       *   abort 当前 child controller。
       */
      const onLifecycleAbort = (): void => {
        connectionController.abort();
      };
      lifecycleController.signal.addEventListener('abort', onLifecycleAbort);

      const parserState: WorkbenchNdjsonParserState = { pending: '' };
      const decoder = new TextDecoder();
      let watchdogTimer: number | null = null;

      /**
       * Business Logic（为什么需要这个函数）:
       *   35 秒无任何完整 data/heartbeat frame 说明半开连接，应只 abort 当前连接。
       *
       * Code Logic（这个函数做什么）:
       *   重置 watchdog 定时器；触发时 abort child，不碰 lifecycle。
       */
      const resetWatchdog = (): void => {
        if (watchdogTimer !== null) {
          window.clearTimeout(watchdogTimer);
        }
        watchdogTimer = window.setTimeout(() => {
          connectionController.abort();
        }, watchdogMs);
      };
      resetWatchdog();

      try {
        const response = await fetch('/api/workbench/events', {
          method: 'GET',
          headers: {
            Accept: 'application/x-ndjson',
          },
          signal: connectionController.signal,
        });
        if (!response.ok) throw new Error(response.statusText || `HTTP ${response.status}`);
        if (!response.body) throw new Error('Workbench HTTP event stream 不可用');

        const reader = response.body.getReader();
        while (!stopped && !lifecycleController.signal.aborted) {
          const { done, value } = await reader.read();
          if (done) break;
          if (!value) continue;
          const chunk = decoder.decode(value, { stream: true });
          parseWorkbenchNdjsonFrames(parserState, chunk).forEach((frame) => {
            // 业务帧与 heartbeat 都重置 watchdog。
            resetWatchdog();
            if (frame.kind === 'event') {
              consumeWorkbenchHttpEvent(store, frame.event);
            }
          });
        }

        const trailingChunk = decoder.decode();
        if (trailingChunk) {
          parseWorkbenchNdjsonFrames(parserState, trailingChunk).forEach((frame) => {
            resetWatchdog();
            if (frame.kind === 'event') {
              consumeWorkbenchHttpEvent(store, frame.event);
            }
          });
        }
      } catch {
        // lifecycle abort：不再重连；child-only abort（watchdog/网络）进入 finally 重连。
        if (lifecycleController.signal.aborted) return;
      } finally {
        if (watchdogTimer !== null) {
          window.clearTimeout(watchdogTimer);
        }
        lifecycleController.signal.removeEventListener('abort', onLifecycleAbort);
        if (activeConnectionController === connectionController) {
          activeConnectionController = null;
        }
        if (!lifecycleController.signal.aborted && !stopped) {
          scheduleReconnect();
        }
      }
    }

    void connect();

    return () => {
      stopped = true;
      lifecycleController.abort();
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
      }
      activeConnectionController?.abort();
    };
  }, [enabled, reconnectDelayMs, store, watchdogMs]);
}
