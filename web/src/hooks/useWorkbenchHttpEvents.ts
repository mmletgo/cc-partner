/**
 * Workbench HTTP NDJSON 事件订阅。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 普通浏览器没有 Tauri event，需要通过 `/api/workbench/events` 长连接接收终端输出等事件。
 *
 * Code Logic（这个模块做什么）:
 *   提供 NDJSON chunk parser，并提供 React hook 将 terminalOutput 事件直接写入外部终端 buffer store。
 */

import { useEffect } from 'react';
import type {
  WorkbenchHttpEvent,
  WorkbenchMergeProgressEvent,
  WorkbenchTerminalOutputEvent,
  WorkbenchTerminalStatusEvent,
} from '@/lib/types';
import type { WorkbenchTerminalBufferStore } from './workbenchTerminalBuffer';

const WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS = 2_000;

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
 * Workbench HTTP 事件 hook 参数。
 *
 * Business Logic（为什么需要这个接口）:
 *   移动端只应在启用 HTTP transport 时连接 NDJSON，并把输出写入传入的终端 buffer store。
 *
 * Code Logic（字段说明）:
 *   store 接收 terminalOutput；enabled 控制连接生命周期；reconnectDelayMs 供测试或未来 UI 调整重连间隔。
 */
export interface UseWorkbenchHttpEventsOptions {
  store: WorkbenchTerminalBufferStore;
  enabled: boolean;
  reconnectDelayMs?: number;
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
 *   parser 需要把 JSON.parse 的 unknown 值缩窄成 WorkbenchHttpEvent，避免调用方处理半可信数据。
 *
 * Code Logic（这个函数做什么）:
 *   按 serde tag `type` 分支校验 payload；不支持或结构错误时抛出 Error。
 */
function parseWorkbenchHttpEvent(value: unknown): WorkbenchHttpEvent {
  if (!isRecord(value) || typeof value.type !== 'string') {
    throw new Error('Workbench HTTP event 缺少 type');
  }

  if (value.type === 'terminalOutput' && isTerminalOutputPayload(value.payload)) {
    return { type: value.type, payload: value.payload };
  }
  if (value.type === 'terminalStatus' && isTerminalStatusPayload(value.payload)) {
    return { type: value.type, payload: value.payload };
  }
  if (value.type === 'mergeProgress' && isMergeProgressPayload(value.payload)) {
    return { type: value.type, payload: value.payload };
  }

  throw new Error(`不支持的 Workbench HTTP event: ${value.type}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `/api/workbench/events` 使用 NDJSON，移动端必须跨 chunk 拼接完整行，不能因为中文/emoji 分包丢字符。
 *
 * Code Logic（这个函数做什么）:
 *   将 state.pending 与新 chunk 合并，只解析以换行结束的完整 JSON 行；空行忽略，剩余半行写回 pending。
 */
export function parseWorkbenchNdjsonChunk(
  state: WorkbenchNdjsonParserState,
  chunk: string,
): WorkbenchHttpEvent[] {
  const combined = `${state.pending}${chunk}`;
  const lines = combined.split('\n');
  state.pending = lines.pop() ?? '';

  const events: WorkbenchHttpEvent[] = [];
  lines.forEach((line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    events.push(parseWorkbenchHttpEvent(JSON.parse(trimmed)));
  });
  return events;
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
 *   `/mobile` 页面需要持续接收同源 HTTP NDJSON terminal 输出，并在断线后自动重连。
 *
 * Code Logic（这个 hook 做什么）:
 *   enabled 为 true 时 fetch `/api/workbench/events`，用 TextDecoder(stream) 解码 reader chunk，
 *   经 parser 得到事件后把 terminalOutput 写入传入 store；cleanup 时 abort 并清理重连 timer。
 */
export function useWorkbenchHttpEvents({
  store,
  enabled,
  reconnectDelayMs = WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS,
}: UseWorkbenchHttpEventsOptions): void {
  useEffect(() => {
    if (!enabled) return undefined;

    const abortController = new AbortController();
    let stopped = false;
    let reconnectTimer: number | null = null;

    /**
     * Business Logic（为什么需要这个函数）:
     *   HTTP 长连接可能因为网络切换、桌面端重启或浏览器挂起而断开，移动端需要自动恢复事件流。
     *
     * Code Logic（这个函数做什么）:
     *   在指定延迟后重新调用 connect；已停止或已 abort 时不再调度。
     */
    function scheduleReconnect(): void {
      if (stopped || abortController.signal.aborted) return;
      reconnectTimer = window.setTimeout(() => {
        void connect();
      }, reconnectDelayMs);
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   移动端 Workbench 需要把同源 NDJSON 事件流持续转成终端 buffer 增量。
     *
     * Code Logic（这个函数做什么）:
     *   建立 fetch 长连接，逐段读取 ReadableStream，用 TextDecoder 流式解码后交给 parser；
     *   正常结束或异常断开后，如果未 cleanup 则调度重连。
     */
    async function connect(): Promise<void> {
      const parserState: WorkbenchNdjsonParserState = { pending: '' };
      const decoder = new TextDecoder();

      try {
        const response = await fetch('/api/workbench/events', {
          method: 'GET',
          headers: {
            Accept: 'application/x-ndjson',
          },
          signal: abortController.signal,
        });
        if (!response.ok) throw new Error(response.statusText || `HTTP ${response.status}`);
        if (!response.body) throw new Error('Workbench HTTP event stream 不可用');

        const reader = response.body.getReader();
        while (!stopped) {
          const { done, value } = await reader.read();
          if (done) break;
          if (!value) continue;
          const chunk = decoder.decode(value, { stream: true });
          parseWorkbenchNdjsonChunk(parserState, chunk).forEach((event) => {
            consumeWorkbenchHttpEvent(store, event);
          });
        }

        const trailingChunk = decoder.decode();
        if (trailingChunk) {
          parseWorkbenchNdjsonChunk(parserState, trailingChunk).forEach((event) => {
            consumeWorkbenchHttpEvent(store, event);
          });
        }
      } catch (error) {
        if (abortController.signal.aborted) return;
      } finally {
        scheduleReconnect();
      }
    }

    void connect();

    return () => {
      stopped = true;
      abortController.abort();
      if (reconnectTimer !== null) {
        window.clearTimeout(reconnectTimer);
      }
    };
  }, [enabled, reconnectDelayMs, store]);
}
