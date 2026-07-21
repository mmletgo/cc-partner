/**
 * Workbench HTTP NDJSON 事件订阅。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 普通浏览器没有 Tauri event，需要通过 `/api/workbench/events` 长连接接收终端输出等事件。
 *   ring lag / owner 切换 / after 早于 ring 时 server 发显式 Gap 帧；客户端必须 fail-closed 暂停 live、权威 resync，再以 after 游标重连。
 *
 * Code Logic（这个模块做什么）:
 *   提供 NDJSON chunk parser（含 typed heartbeat / Gap）、stream cursor 与 after URL 纯 helper，并提供 React hook：
 *   lifecycle AbortController + 每连接 child controller；35s watchdog 仅 abort 子连接并重连；
 *   Gap 后 pause live → sessions.list+running replay → store.reset →
 *   投影全部 listed session 的 terminalStatus → 推进或保留 pre-gap cursor 后重连。
 */

import { useEffect, useRef } from 'react';
import type {
  WorkbenchHttpEvent,
  WorkbenchMergeProgressEvent,
  WorkbenchSessionReplay,
  WorkbenchTerminalOutputEvent,
  WorkbenchTerminalResyncHttpPayload,
  WorkbenchTerminalStatusEvent,
} from '@/lib/types';
import type { AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';
import { agentSessionRuntimeDtoDecoder } from '@/lib/schemas/agentRuntime';
import {
  httpWorkbenchTransport,
  listActiveBridgeDevicesHttp,
  listActiveMappedProjectsHttp,
} from '@/api/workbenchHttp';
import { OrchestratorRuntimeTransportError } from '@/api/orchestratorRuntimeTransportError';
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
 * `/api/workbench/events` 流游标（afterOwnerInstanceId + afterSequence）。
 *
 * Business Logic（为什么需要这个接口）:
 *   断线/Gap 后重连必须带 after 游标做 catch-up；incomplete resync 必须保留 pre-gap recovery，禁止 brand-new None。
 *
 * Code Logic（字段说明）:
 *   ownerInstanceId 为 bus owner；sequence 为已提交的最新 sequence。
 */
export interface WorkbenchHttpStreamCursor {
  ownerInstanceId: string;
  sequence: number;
}

/**
 * Gap 帧 payload 形状（与 server NDJSON 对齐）。
 *
 * Business Logic（为什么需要这个接口）:
 *   ring lag / owner 切换后 server 显式 Gap；客户端用其做 resync 与 cursor 推进决策。
 *
 * Code Logic（字段说明）:
 *   ownerInstanceId / oldestAvailable / latest 均来自 gap.payload。
 */
export interface WorkbenchHttpGapFrame {
  ownerInstanceId: string;
  oldestAvailable: number;
  latest: number;
}

/**
 * NDJSON 帧：业务事件、heartbeat、Gap 或未知 type（忽略）。
 *
 * Business Logic（为什么需要这个类型）:
 *   heartbeat 不是业务事件但必须重置 watchdog；Gap 是一等恢复帧；未知 type 不得断开流。
 *
 * Code Logic（联合形态）:
 *   event 携带 WorkbenchHttpEvent 与可选 envelope owner/sequence；
 *   heartbeat 携带 RFC3339 sentAt；gap 携带游标字段；ignored 供前向兼容。
 */
export type WorkbenchNdjsonFrame =
  | {
      kind: 'event';
      event: WorkbenchHttpEvent;
      ownerInstanceId?: string;
      sequence?: number;
    }
  | { kind: 'heartbeat'; sentAt: string }
  | { kind: 'gap'; ownerInstanceId: string; oldestAvailable: number; latest: number }
  | { kind: 'ignored'; type: string };

/**
 * Gap resync 所需的 sessions 依赖（可注入，便于单测）。
 *
 * Business Logic（为什么需要这个接口）:
 *   Gap 后必须对 running session 做权威 replay，测试不能打真实 HTTP。
 *   R37 M1：无 project 的 sessions.list 会漏 remote shortcut 会话，需可注入 listProjects。
 *   R38 M1 / R39 M1：offline remote shortcut 不得 fail-closed 挡本机恢复；
 *   production 默认注入 active bridge 设备，仅这些 remote fail-closed。
 *   R42 M2：进一步改为 active mapped local project ids（同设备失效 shortcut 不得挡 resync）。
 *
 * Code Logic（字段说明）:
 *   list 返回至少含 id/status，可选 exitCode（R41 M5 投影 terminalStatus 时用）；
 *   replay 返回 buffer/lastSeq/可选 ownerInstanceId；
 *   listProjects 可选：返回至少 id/kind（可选 deviceId），remote 项目再按 projectId list；
 *   listActiveMappedProjects 可选（优先）：一旦提供（含空数组）即按 local project id inventory；
 *   listActiveBridgeDevices 可选：mapped 未提供时的 device 级兼容路径。
 */
export interface WorkbenchHttpEventsSessionDeps {
  list: (
    projectId?: string | null,
  ) => Promise<Array<{ id: string; status: string; exitCode?: number | null }>>;
  replay: (sessionId: string) => Promise<WorkbenchSessionReplay>;
  /**
   * Business Logic（为什么需要这个字段）:
   *   Mobile Gap inventory 必须覆盖本机与 remote shortcut 的 running sessions。
   *
   * Code Logic（字段说明）:
   *   返回至少 id + kind；可选 deviceId；kind===remote 时再按策略 list(projectId)。
   */
  listProjects?: () => Promise<Array<{ id: string; kind?: string; deviceId?: string }>>;
  /**
   * Business Logic（为什么需要这个字段）:
   *   R42 M2：对齐桌面 bridges.active_mapped_projects——仅 inventory 活跃 bridge 上
   *   已映射的 local shortcut projectId；同设备 stale P2 不得因 list 失败阻塞 P1。
   *
   * Code Logic（字段说明）:
   *   可选：active mapped local project id 列表。
   *   **已提供且调用成功**（含空数组）→ 权威：空集合跳过全部 remote；非空仅 list 命中 id 且
   *   fail-closed。**调用失败且为 404/unsupported** → device 级兼容回退（R43 M2）。
   *   **其他调用失败** → throw fail-closed（禁止 skip-offline soft-success）。
   *   **未提供** → 走 device 级/fallback。
   */
  listActiveMappedProjects?: () => Promise<string[]>;
  /**
   * Business Logic（为什么需要这个字段）:
   *   对齐桌面 active-bridge：仅活跃 bridge 上的 remote 失败才应 fail-closed，
   *   无关 offline remote 不得永久阻塞本机 Gap recovery。
   *
   * Code Logic（字段说明）:
   *   可选：活跃 bridge 设备 id 列表（mapped inventory 未注入时的兼容路径）。
   *   **已提供**（含空数组）→ 始终构造 Set：空集合跳过全部 remote；非空仅 inventory 集合内
   *   device 且 fail-closed。**未提供** → 兼容 fallback：全部 remote 走 skip-offline。
   */
  listActiveBridgeDevices?: () => Promise<string[]>;
}

/**
 * Gap inventory 中的 session 行（list 去重后）。
 *
 * Business Logic（为什么需要这个类型）:
 *   R41 M5：Gap 期间 running→exited 等状态迁移必须投影到 Mobile UI，
 *   inventory 行是唯一权威来源（live terminalStatus 可能已丢）。
 *
 * Code Logic（字段说明）:
 *   id/status 必填；exitCode 可选，缺省投影为 null。
 */
export interface WorkbenchGapInventorySession {
  id: string;
  status: string;
  exitCode?: number | null;
}

/**
 * resyncWorkbenchSessionsAfterGap 可选副作用（状态投影等）。
 *
 * Business Logic（为什么需要这个接口）:
 *   running replay 只恢复 buffer；status 迁移须单独投影，避免 Mobile 卡在 running。
 *
 * Code Logic（字段说明）:
 *   onTerminalStatus：inventory 成功后对每个 listed session 调用（含 exited/disconnected）。
 */
export interface ResyncWorkbenchSessionsAfterGapOptions {
  onTerminalStatus?: (payload: WorkbenchTerminalStatusEvent) => void;
}

/**
 * Workbench HTTP 事件 hook 参数。
 *
 * Business Logic（为什么需要这个接口）:
 *   移动端只应在启用 HTTP transport 时连接 NDJSON，并把输出写入传入的终端 buffer store；
 *   terminalStatus/agentRuntime 由页面 reducer 消费，不写 terminal buffer。
 *
 * Code Logic（字段说明）:
 *   store 接收 terminalOutput；enabled 控制连接生命周期；
 *   onTerminalStatus/onAgentRuntime 可选回调；reconnectDelayMs/watchdogMs/sessions 供测试注入。
 */
export interface UseWorkbenchHttpEventsOptions {
  store: WorkbenchTerminalBufferStore;
  enabled: boolean;
  reconnectDelayMs?: number;
  watchdogMs?: number;
  /** terminalStatus 实时回调（Mobile tab 状态）。 */
  onTerminalStatus?: (payload: WorkbenchTerminalStatusEvent) => void;
  /** agentRuntime 实时回调（Agent phase 投影）。 */
  onAgentRuntime?: (payload: { agentSession: AgentSessionRuntimeDto }) => void;
  /** Gap resync 会话依赖；默认 httpWorkbenchTransport.sessions。 */
  sessions?: WorkbenchHttpEventsSessionDeps;
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
 *   Gap resync 权威 terminalResync 必须结构正确才能 store.reset，否则会抹掉正确 buffer。
 *
 * Code Logic（这个函数做什么）:
 *   校验 sessionId/buffer/truncated/lastSeq，可选 ownerInstanceId。
 */
function isTerminalResyncPayload(value: unknown): value is WorkbenchTerminalResyncHttpPayload {
  if (!isRecord(value)) return false;
  if (
    typeof value.sessionId !== 'string' ||
    typeof value.buffer !== 'string' ||
    typeof value.truncated !== 'boolean' ||
    typeof value.lastSeq !== 'number'
  ) {
    return false;
  }
  if (value.ownerInstanceId !== undefined && typeof value.ownerInstanceId !== 'string') {
    return false;
  }
  return true;
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
 *   agentRuntime 是 A2 投影源；结构错误时必须协议失败，不得 silent 写坏 phase。
 *
 * Code Logic（这个函数做什么）:
 *   用 agentSessionRuntimeDtoDecoder 严格解码 payload.agentSession。
 */
function isAgentRuntimePayload(
  value: unknown,
): value is { agentSession: AgentSessionRuntimeDto } {
  if (!isRecord(value) || !('agentSession' in value)) return false;
  try {
    agentSessionRuntimeDtoDecoder.decode(value.agentSession);
    return true;
  } catch {
    return false;
  }
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
 *   Gap 是 ring lag / owner 切换的一等恢复帧；payload 畸形不得 silent ignore 成 live 继续。
 *
 * Code Logic（这个函数做什么）:
 *   校验 payload 含 ownerInstanceId 字符串与 oldestAvailable/latest 有限数字。
 */
export function isWorkbenchHttpGapPayload(value: unknown): value is WorkbenchHttpGapFrame {
  if (!isRecord(value)) return false;
  return (
    typeof value.ownerInstanceId === 'string' &&
    typeof value.oldestAvailable === 'number' &&
    Number.isFinite(value.oldestAvailable) &&
    typeof value.latest === 'number' &&
    Number.isFinite(value.latest)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   业务帧 envelope 的 ownerInstanceId+sequence 用于 after 游标推进，与 terminal payload.seq 分离。
 *
 * Code Logic（这个函数做什么）:
 *   读取 top-level ownerInstanceId（非空字符串）与 sequence（有限数字）；缺省返回 undefined。
 */
function readEnvelopeCursor(
  value: Record<string, unknown>,
): { ownerInstanceId?: string; sequence?: number } {
  const ownerInstanceId =
    typeof value.ownerInstanceId === 'string' && value.ownerInstanceId.trim().length > 0
      ? value.ownerInstanceId
      : undefined;
  const sequence =
    typeof value.sequence === 'number' && Number.isFinite(value.sequence)
      ? value.sequence
      : undefined;
  return { ownerInstanceId, sequence };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live 业务帧到达后必须推进 stream cursor，供断线/Gap 后 after 重连，避免 brand-new catch-up。
 *
 * Code Logic（这个函数做什么）:
 *   owner 变化时重置为新 owner+sequence；同 owner 仅当 sequence 更大时推进；非法输入返回 current。
 */
export function advanceWorkbenchHttpStreamCursor(
  current: WorkbenchHttpStreamCursor | null,
  ownerInstanceId: string | undefined,
  sequence: number | undefined,
): WorkbenchHttpStreamCursor | null {
  if (typeof ownerInstanceId !== 'string' || ownerInstanceId.trim().length === 0) {
    return current;
  }
  if (typeof sequence !== 'number' || !Number.isFinite(sequence) || sequence < 0) {
    return current;
  }
  if (!current || current.ownerInstanceId !== ownerInstanceId) {
    return { ownerInstanceId, sequence };
  }
  if (sequence > current.sequence) {
    return { ownerInstanceId, sequence };
  }
  return current;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Gap resync 成功后应 attach 到 gap.latest（含 latest=0 的新 owner cutover），
 *   否则旧 preGapCursor 会让客户端继续按旧 owner 追赶，形成无限 Gap 循环。
 *   失败或不完整必须保留 recovery，禁止 cursor=None brand-new。
 *   R32 M1：首帧 Gap（pre-gap cursor 为空）且 resync 未成功时，recovery 用 gap owner + sequence 0。
 *
 * Code Logic（这个函数做什么）:
 *   resyncSucceeded 且 owner 非空 → 始终 `{owner, latest}`（允许 latest=0）；
 *   否则优先 preGapCursor；若 pre 为空且 gap owner 合法 → `{owner, 0}`（sequence 0 recovery）。
 */
export function resolveCursorAfterGap(
  preGapCursor: WorkbenchHttpStreamCursor | null,
  gap: WorkbenchHttpGapFrame,
  resyncSucceeded: boolean,
): WorkbenchHttpStreamCursor | null {
  if (
    resyncSucceeded &&
    typeof gap.ownerInstanceId === 'string' &&
    gap.ownerInstanceId.trim().length > 0
  ) {
    return { ownerInstanceId: gap.ownerInstanceId, sequence: gap.latest };
  }
  if (preGapCursor) {
    return preGapCursor;
  }
  // R32 M1：first-frame Gap + incomplete resync → recovery cursor = gap owner + seq 0。
  if (
    typeof gap.ownerInstanceId === 'string' &&
    gap.ownerInstanceId.trim().length > 0
  ) {
    return { ownerInstanceId: gap.ownerInstanceId, sequence: 0 };
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   重连必须附 afterOwnerInstanceId+afterSequence，server 才能 catch-up 或显式 Gap。
 *   R32 M1：recoveryPending 期间禁止裸 URL（无 after），即使 cursor 暂时为空也 fail-closed 用
 *   兜底 recovery cursor 构造 after query。
 *
 * Code Logic（这个函数做什么）:
 *   有效 cursor 时附 camelCase query；recoveryPending 且无 cursor 时若提供 fallback 仍附 after；
 *   仅 brand-new（非 recovery）才返回裸 `/api/workbench/events`。
 */
export function buildWorkbenchEventsUrl(
  cursor: WorkbenchHttpStreamCursor | null,
  options?: { recoveryPending?: boolean; recoveryFallback?: WorkbenchHttpStreamCursor | null },
): string {
  const effective =
    cursor &&
    typeof cursor.ownerInstanceId === 'string' &&
    cursor.ownerInstanceId.trim().length > 0 &&
    typeof cursor.sequence === 'number' &&
    Number.isFinite(cursor.sequence) &&
    cursor.sequence >= 0
      ? cursor
      : options?.recoveryPending
        ? options.recoveryFallback &&
          typeof options.recoveryFallback.ownerInstanceId === 'string' &&
          options.recoveryFallback.ownerInstanceId.trim().length > 0 &&
          typeof options.recoveryFallback.sequence === 'number' &&
          Number.isFinite(options.recoveryFallback.sequence) &&
          options.recoveryFallback.sequence >= 0
          ? options.recoveryFallback
          : null
        : null;

  if (!effective) {
    // recoveryPending 且仍无有效 after：宁可 delay 重连也不 brand-new。
    // 调用方应保证 recoveryPending 时至少有 recovery cursor；此处仍返回裸 URL 作最后兜底
    // 但 hook 层会在 recoveryPending 时拒绝无 after 的 fetch。
    return '/api/workbench/events';
  }
  const params = new URLSearchParams({
    afterOwnerInstanceId: effective.ownerInstanceId,
    afterSequence: String(effective.sequence),
  });
  return `/api/workbench/events?${params.toString()}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   recovery 未完成时不得以 brand-new consumer 重连，否则首帧 Gap 后的 incomplete resync 会掩盖 gap。
 *
 * Code Logic（这个函数做什么）:
 *   当 recoveryPending 为 true 时要求 cursor 有效（含 sequence 0）；否则允许 null（brand-new 首连）。
 */
export function canOpenWorkbenchEventsRequest(
  cursor: WorkbenchHttpStreamCursor | null,
  recoveryPending: boolean,
): boolean {
  if (!recoveryPending) return true;
  return (
    !!cursor &&
    typeof cursor.ownerInstanceId === 'string' &&
    cursor.ownerInstanceId.trim().length > 0 &&
    typeof cursor.sequence === 'number' &&
    Number.isFinite(cursor.sequence) &&
    cursor.sequence >= 0
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Gap inventory 需要把 remote shortcut 归属到 owning device，
 *   以便按 active bridge 过滤，避免 offline 设备永久挡本机恢复。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 project.deviceId；否则解析 `remote:<deviceId>:<rest>` 的 device 段；
 *   解析失败返回 null。
 */
export function resolveRemoteProjectDeviceId(project: {
  id: string;
  deviceId?: string;
}): string | null {
  if (typeof project.deviceId === 'string' && project.deviceId.trim().length > 0) {
    return project.deviceId;
  }
  const match = /^remote:([^:]+):/.exec(project.id);
  if (!match) return null;
  const deviceId = match[1]?.trim();
  return deviceId && deviceId.length > 0 ? deviceId : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Gap 期间丢弃的 terminalStatus 帧不会重放；inventory 行是 Mobile 恢复 tab 状态的权威来源。
 *   仅 replay running 会让 running→exited 会话继续显示 running 并允许危险写操作。
 *
 * Code Logic（这个函数做什么）:
 *   对每个 inventory session 构造 WorkbenchTerminalStatusEvent（exitCode 缺省 null，ts=now），
 *   调用 onTerminalStatus；无回调时 no-op。
 */
export function projectInventoryTerminalStatuses(
  sessions: Iterable<WorkbenchGapInventorySession>,
  onTerminalStatus?: (payload: WorkbenchTerminalStatusEvent) => void,
): void {
  if (!onTerminalStatus) return;
  const ts = Date.now();
  for (const session of sessions) {
    if (!session.id) continue;
    onTerminalStatus({
      sessionId: session.id,
      status: session.status,
      exitCode: session.exitCode ?? null,
      ts,
    });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   R43 M2：Gap mapped inventory 失败时，仅旧后端无路由（404/unsupported）才允许
 *   device 级兼容回退；network/timeout/5xx 等必须 fail-closed，禁止 soft-success。
 *
 * Code Logic（这个函数做什么）:
 *   识别明确的 unsupported/404 错误：
 *   - 对象带 status/statusCode/httpStatus === 404
 *   - message 含 404 / not found / unsupported（大小写不敏感）
 *   - OrchestratorRuntimeTransportError 且 kind==='protocol' 且 message 暗示 404/unsupported
 *   不对任意 network/timeout 失败返回 true。
 */
export function isMappedInventoryUnsupportedError(error: unknown): boolean {
  if (error == null) return false;

  const statusCandidates: unknown[] = [];
  if (typeof error === 'object') {
    const record = error as Record<string, unknown>;
    statusCandidates.push(record.status, record.statusCode, record.httpStatus);
  }
  for (const status of statusCandidates) {
    if (status === 404 || status === '404') return true;
  }

  const message =
    error instanceof Error
      ? error.message
      : typeof error === 'string'
        ? error
        : typeof error === 'object' &&
            error !== null &&
            'message' in error &&
            typeof (error as { message: unknown }).message === 'string'
          ? (error as { message: string }).message
          : '';
  const lower = message.toLowerCase();
  const looksUnsupported =
    lower.includes('unsupported') ||
    lower.includes('404') ||
    lower.includes('not found');

  if (
    error instanceof OrchestratorRuntimeTransportError &&
    error.kind === 'protocol' &&
    looksUnsupported
  ) {
    return true;
  }

  // 普通 Error / 协议消息：仅当 message 明确暗示 404/unsupported。
  if (looksUnsupported && !(error instanceof OrchestratorRuntimeTransportError)) {
    return true;
  }

  // protocol kind 但 message 无 404/unsupported 暗示：不是旧路由兼容失败。
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Gap 后不能继续消费 laggy live 尾部；必须对 running session 做权威 replay 覆盖 buffer。
 *   R37 M1：裸 sessions.list() 无 project 会漏 remote shortcut；存在 remote 时必须按项目 inventory。
 *   R38 M1 / R39 M1 / R40 M1：production 曾注入 listActiveBridgeDevices；
 *   仅活跃 bridge remote fail-closed；**空 active set 跳过全部 remote**（不探测离线 shortcut）。
 *   R41 M5：inventory 成功后须投影全部 listed session 的 terminalStatus（含 exited/disconnected），
 *   不能只 replay running；否则 Mobile UI 会卡在 running。
 *   R42 M2：优先 listActiveMappedProjects（active bridge 已映射 local project ids）；
 *   同设备活跃 P1 + 失效 P2 时只 inventory P1，stale P2 list 失败不得挡整次 resync。
 *   R43 M2：mapped inventory 失败不得 soft-success 推进 Gap cursor。
 *   仅明确 404/unsupported（旧后端无路由）才 device 级兼容回退；其余失败 throw fail-closed。
 *
 * Code Logic（这个函数做什么）:
 *   本机 sessions.list()（无 projectId）失败仍 throw。
 *   有 listProjects 时取 kind===remote 的项目：
 *   - listActiveMappedProjects **已提供且调用成功**（含空数组）：仅 inventory 命中 project.id；
 *     空集合跳过全部 remote；命中 id fail-closed。
 *   - listActiveMappedProjects **调用失败且 isMappedInventoryUnsupportedError**：
 *     device 级兼容回退到 listActiveBridgeDevices（active set 空→跳过全部 remote；
 *     device 在 set 内 sessions.list fail-closed；不在 set→skip）。
 *   - listActiveMappedProjects **其他失败**（network/timeout/5xx/protocol 等）：**throw**，
 *     禁止 skip-offline soft-success，禁止推进 Gap cursor。
 *   - mapped **未提供**且 listActiveBridgeDevices **已提供**：device 级兼容路径（R39/R40）。
 *   - 两者都未提供：兼容 fallback——每个 remote try list，成功 merge、失败 skip。
 *   无 listProjects 时退回 sessions.list()。
 *   仅 status===running 调 replay → store.reset(sessionId, buffer, lastSeq, ownerInstanceId?)。
 *   inventory + running replay 完成后：projectInventoryTerminalStatuses 对全部 listed 行投影 status。
 */
export async function resyncWorkbenchSessionsAfterGap(
  store: WorkbenchTerminalBufferStore,
  sessions: WorkbenchHttpEventsSessionDeps,
  options?: ResyncWorkbenchSessionsAfterGapOptions,
): Promise<void> {
  const byId = new Map<string, WorkbenchGapInventorySession>();

  /**
   * Business Logic（为什么需要这个函数）:
   *   inventory 结果按 session id 去重合并，避免 local/remote 重复 replay / 重复 status 投影。
   *
   * Code Logic（这个函数做什么）:
   *   将 list 返回的行写入 byId Map（后写覆盖同 id）。
   */
  const mergeListed = (rows: Array<{ id: string; status: string; exitCode?: number | null }>): void => {
    for (const row of rows) {
      if (!row.id) continue;
      byId.set(row.id, row);
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   skip-offline 路径：offline / unavailable remote 不得挡本机 Gap recovery。
   *
   * Code Logic（这个函数做什么）:
   *   try list(projectId)；成功 merge，失败吞掉。
   */
  const listRemoteSkipOffline = async (projectId: string): Promise<void> => {
    try {
      mergeListed(await sessions.list(projectId));
    } catch {
      // R38 M1：offline / unavailable remote 不得挡本机 Gap recovery。
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   device 级兼容路径：mapped 未注入或旧后端 mapped 路由 404/unsupported 时，
   *   仍须按 active bridge devices fail-closed inventory，不得 skip-offline soft-success。
   *
   * Code Logic（这个函数做什么）:
   *   有 listActiveBridgeDevices 则构造 active set（空 set 跳过全部 remote；
   *   device 在 set 内 list fail-closed；不在 set 跳过）；否则全部 remote skip-offline。
   */
  const inventoryRemoteByDeviceCompat = async (
    remoteProjects: Array<{ id: string; kind?: string; deviceId?: string }>,
  ): Promise<void> => {
    const hasActiveBridgeInventory = typeof sessions.listActiveBridgeDevices === 'function';
    const activeSet = hasActiveBridgeInventory
      ? new Set(
          (await sessions.listActiveBridgeDevices!()).filter((id) => id.trim().length > 0),
        )
      : null;

    for (const project of remoteProjects) {
      if (activeSet) {
        const deviceId = resolveRemoteProjectDeviceId(project);
        if (!deviceId || !activeSet.has(deviceId)) {
          // 非活跃 / 空 active set：完全跳过，不 list、不唤醒离线远端。
          continue;
        }
        // 活跃 bridge remote：fail-closed。
        mergeListed(await sessions.list(project.id));
        continue;
      }

      await listRemoteSkipOffline(project.id);
    }
  };

  // 本机全局 list（无 projectId）失败必须 throw，不得被 remote skip 掩盖。
  mergeListed(await sessions.list());

  if (sessions.listProjects) {
    const projects = await sessions.listProjects();
    const remoteProjects = projects.filter((project) => project.kind === 'remote');

    // R42 M2 / R43 M2：mapped projects 优先；仅 404/unsupported 回退 device 级。
    const hasMappedInventory = typeof sessions.listActiveMappedProjects === 'function';
    let mappedSet: Set<string> | null = null;
    let mappedUnsupportedFallback = false;
    if (hasMappedInventory) {
      try {
        mappedSet = new Set(
          (await sessions.listActiveMappedProjects!()).filter((id) => id.trim().length > 0),
        );
      } catch (error) {
        // R43 M2：仅旧后端无 mapped 路由（404/unsupported）才 device 级兼容；
        // network/timeout/5xx 等必须 throw，禁止 skip-offline soft-success 推进 cursor。
        if (!isMappedInventoryUnsupportedError(error)) {
          throw error;
        }
        mappedUnsupportedFallback = true;
        mappedSet = null;
      }
    }

    if (mappedSet) {
      // 权威 mapped inventory：空集合跳过全部 remote；命中 id fail-closed。
      for (const project of remoteProjects) {
        if (!mappedSet.has(project.id)) {
          continue;
        }
        mergeListed(await sessions.list(project.id));
      }
    } else if (mappedUnsupportedFallback || !hasMappedInventory) {
      // mapped 404/unsupported 回退，或 mapped 未注入：device 级兼容 / skip-offline。
      await inventoryRemoteByDeviceCompat(remoteProjects);
    }
  }

  for (const session of byId.values()) {
    if (session.status !== 'running') continue;
    const replay = await sessions.replay(session.id);
    store.reset(
      session.id,
      replay.buffer,
      replay.lastSeq,
      replay.ownerInstanceId ?? null,
    );
  }

  // R41 M5：成功 inventory 后投影全部 listed session 状态（不只 running）。
  projectInventoryTerminalStatuses(byId.values(), options?.onTerminalStatus);
}


/**
 * Business Logic（为什么需要这个函数）:
 *   parser 需要把 JSON.parse 的 unknown 值缩窄成业务事件、heartbeat 或 Gap。
 *
 * Code Logic（这个函数做什么）:
 *   先识别 heartbeat；再识别 gap；再按 serde tag `type` 分支校验 payload；
 *   未知 type → ignored（不重连）；已知 type 但 payload 畸形 → 抛 Error。
 *   业务 event 额外附带 top-level ownerInstanceId/sequence（若存在）。
 */
export function parseWorkbenchNdjsonFrame(value: unknown): WorkbenchNdjsonFrame {
  if (isHeartbeatFrame(value)) {
    return { kind: 'heartbeat', sentAt: value.sentAt };
  }
  if (!isRecord(value) || typeof value.type !== 'string') {
    throw new Error('Workbench HTTP event 缺少 type');
  }

  if (value.type === 'gap') {
    if (!isWorkbenchHttpGapPayload(value.payload)) {
      throw new Error('Workbench HTTP event gap payload 非法');
    }
    return {
      kind: 'gap',
      ownerInstanceId: value.payload.ownerInstanceId,
      oldestAvailable: value.payload.oldestAvailable,
      latest: value.payload.latest,
    };
  }

  const envelope = readEnvelopeCursor(value);

  if (value.type === 'terminalOutput') {
    if (!isTerminalOutputPayload(value.payload)) {
      throw new Error('Workbench HTTP event terminalOutput payload 非法');
    }
    return {
      kind: 'event',
      event: { type: value.type, payload: value.payload },
      ...envelope,
    };
  }
  if (value.type === 'terminalStatus') {
    if (!isTerminalStatusPayload(value.payload)) {
      throw new Error('Workbench HTTP event terminalStatus payload 非法');
    }
    return {
      kind: 'event',
      event: { type: value.type, payload: value.payload },
      ...envelope,
    };
  }
  if (value.type === 'mergeProgress') {
    if (!isMergeProgressPayload(value.payload)) {
      throw new Error('Workbench HTTP event mergeProgress payload 非法');
    }
    return {
      kind: 'event',
      event: { type: value.type, payload: value.payload },
      ...envelope,
    };
  }
  if (value.type === 'agentRuntime') {
    if (!isAgentRuntimePayload(value.payload)) {
      throw new Error('Workbench HTTP event agentRuntime payload 非法');
    }
    return {
      kind: 'event',
      event: {
        type: 'agentRuntime',
        payload: {
          agentSession: agentSessionRuntimeDtoDecoder.decode(
            (value.payload as { agentSession: unknown }).agentSession,
          ),
        },
      },
      ...envelope,
    };
  }
  if (value.type === 'terminalResync') {
    if (!isTerminalResyncPayload(value.payload)) {
      throw new Error('Workbench HTTP event terminalResync payload 非法');
    }
    return {
      kind: 'event',
      event: { type: value.type, payload: value.payload },
      ...envelope,
    };
  }

  // 未知 type：前向兼容，不抛错、不重连
  return { kind: 'ignored', type: value.type };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `/api/workbench/events` 使用 NDJSON，移动端必须跨 chunk 拼接完整行，并识别 heartbeat/Gap。
 *
 * Code Logic（这个函数做什么）:
 *   将 state.pending 与新 chunk 合并，只解析以换行结束的完整 JSON 行；返回 frames（含 heartbeat/gap）。
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
 *   既有测试与消费路径以业务事件数组为主；heartbeat/gap 不进入业务事件列表。
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
 *   终端输出事件可能高频到达，移动端必须写入外部 store；status/agent 走回调。
 *   seq/owner 必须传入 append，与桌面 authority/lastSeq 去重对齐。
 *   R37 H2：terminalResync 是 Gap cutover 权威快照，必须 store.reset。
 *
 * Code Logic（这个函数做什么）:
 *   terminalOutput → store.append(sessionId, chunk, seq, owner)；
 *   terminalResync → store.reset(sessionId, buffer, lastSeq, owner)；
 *   terminalStatus/agentRuntime → 可选回调。
 */
export function consumeWorkbenchHttpEvent(
  store: WorkbenchTerminalBufferStore,
  event: WorkbenchHttpEvent,
  callbacks: {
    onTerminalStatus?: (payload: WorkbenchTerminalStatusEvent) => void;
    onAgentRuntime?: (payload: { agentSession: AgentSessionRuntimeDto }) => void;
  } = {},
  envelopeOwnerInstanceId?: string,
): void {
  if (event.type === 'terminalOutput') {
    const authorityId = event.payload.ownerInstanceId ?? envelopeOwnerInstanceId ?? null;
    store.append(event.payload.sessionId, event.payload.chunk, event.payload.seq, authorityId);
    return;
  }
  if (event.type === 'terminalResync') {
    store.reset(
      event.payload.sessionId,
      event.payload.buffer,
      event.payload.lastSeq,
      event.payload.ownerInstanceId ?? envelopeOwnerInstanceId ?? null,
    );
    return;
  }
  if (event.type === 'terminalStatus') {
    callbacks.onTerminalStatus?.(event.payload);
    return;
  }
  if (event.type === 'agentRuntime') {
    callbacks.onAgentRuntime?.(event.payload);
  }
}

/**
 * useWorkbenchHttpEvents（移动端 Workbench HTTP 事件订阅）
 *
 * Business Logic（为什么需要这个 hook）:
 *   `/mobile` 页面需要持续接收同源 HTTP NDJSON terminal 输出与 agent 状态；
 *   半开连接需 heartbeat watchdog 后重连；Gap 必须 pause live + 权威 resync + after 游标重连。
 *
 * Code Logic（这个 hook 做什么）:
 *   lifecycle AbortController 覆盖 hook 生命周期；每次 connect 新建 child controller；
 *   业务帧推进 stream cursor；Gap 暂停 live、resync running sessions、投影 listed terminalStatus、
 *   按结果推进/保留 cursor 后 abort 重连；
 *   业务帧与 heartbeat 均重置 35s watchdog；watchdog 仅 abort child 并在 lifecycle 仍活跃时重连。
 */
export function useWorkbenchHttpEvents({
  store,
  enabled,
  reconnectDelayMs = WORKBENCH_HTTP_EVENT_RECONNECT_DELAY_MS,
  watchdogMs = WORKBENCH_HTTP_EVENT_WATCHDOG_MS,
  onTerminalStatus,
  onAgentRuntime,
  sessions,
}: UseWorkbenchHttpEventsOptions): void {
  const onTerminalStatusRef = useRef(onTerminalStatus);
  const onAgentRuntimeRef = useRef(onAgentRuntime);
  const sessionsRef = useRef<WorkbenchHttpEventsSessionDeps>(
    sessions ?? {
      list: (projectId) => httpWorkbenchTransport.sessions.list(projectId),
      replay: (sessionId) => httpWorkbenchTransport.sessions.replay(sessionId),
      // R37 M1 / R42 M2：默认注入 projects.list + active mapped projects；
      // mapped 优先 inventory；device 级 API 仅作 mapped 未注入时的兼容 fallback。
      listProjects: () => httpWorkbenchTransport.projects.list(),
      listActiveMappedProjects: () => listActiveMappedProjectsHttp(),
      listActiveBridgeDevices: () => listActiveBridgeDevicesHttp(),
    },
  );
  useEffect(() => {
    onTerminalStatusRef.current = onTerminalStatus;
    onAgentRuntimeRef.current = onAgentRuntime;
  }, [onTerminalStatus, onAgentRuntime]);
  useEffect(() => {
    if (sessions) {
      sessionsRef.current = sessions;
    }
  }, [sessions]);

  useEffect(() => {
    if (!enabled) return undefined;

    const lifecycleController = new AbortController();
    let stopped = false;
    let reconnectTimer: number | null = null;
    let activeConnectionController: AbortController | null = null;
    /** 已提交 live cursor；Gap incomplete 时作 recovery，禁止 brand-new None。 */
    let streamCursor: WorkbenchHttpStreamCursor | null = null;
    /**
     * R32 M1：Gap 后 resync 未完成（失败或首帧 Gap 无 pre cursor）时置 true。
     * recovery 完成（成功 attach latest 或 live 业务帧推进）后清 false。
     * true 期间禁止 bare events URL（无 after）。
     */
    let recoveryPending = false;

    /**
     * Business Logic（为什么需要这个函数）:
     *   HTTP 长连接可能因为网络切换、桌面端重启、Gap 或 heartbeat 丢失而断开，移动端需要自动恢复事件流。
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
     *   移动端 Workbench 需要把同源 NDJSON 事件流持续转成终端 buffer 增量，并在 Gap 时权威恢复。
     *
     * Code Logic（这个函数做什么）:
     *   建立 fetch 长连接（child signal + after 游标），逐段读取 ReadableStream；
     *   业务帧推进 cursor 并消费；Gap 则 pause live、resync、解析 next cursor 后 abort 重连；
     *   业务帧与 heartbeat 重置 watchdog；异常或 watchdog abort 后若 lifecycle 仍在则重连。
     *   R32 M1：recoveryPending 时绝不 bare reconnect。
     */
    async function connect(): Promise<void> {
      if (stopped || lifecycleController.signal.aborted) return;

      // R32 M1：recovery 未完成且 cursor 无效时，禁止 brand-new fetch；延后重试。
      if (!canOpenWorkbenchEventsRequest(streamCursor, recoveryPending)) {
        scheduleReconnect();
        return;
      }

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
      /** Gap 后禁止继续应用本连接 live 帧（防 laggy 尾部与 resync 双写）。 */
      let livePaused = false;
      /** 本连接已因 Gap 调度过重连，finally 不再二次 schedule。 */
      let gapHandled = false;

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

      /**
       * Business Logic（为什么需要这个函数）:
       *   Gap 帧意味着 silent loss 风险；必须立刻 pause live 并权威 resync，再以正确 cursor 重连。
       *   新 owner latest=0 的成功 cutover 也必须清 recoveryPending，避免卡在 Gap 循环。
       *   R41 M5：resync 还须把 inventory 中的 terminalStatus（含 exited）投影给 UI。
       *
       * Code Logic（这个函数做什么）:
       *   冻结 pre-gap cursor → resync running sessions + project listed terminalStatus →
       *   resolveCursorAfterGap →
       *   resync 成功且 cursor 已 attach 到 gap owner（含 sequence 0）则清 recoveryPending；
       *   否则保持 recoveryPending → abort child。
       */
      const handleGap = async (gap: WorkbenchHttpGapFrame): Promise<void> => {
        if (gapHandled || stopped || lifecycleController.signal.aborted) return;
        gapHandled = true;
        livePaused = true;
        const preGapCursor = streamCursor;
        let resyncSucceeded = false;
        try {
          await resyncWorkbenchSessionsAfterGap(store, sessionsRef.current, {
            onTerminalStatus: onTerminalStatusRef.current,
          });
          resyncSucceeded = true;
        } catch {
          resyncSucceeded = false;
        }
        streamCursor = resolveCursorAfterGap(preGapCursor, gap, resyncSucceeded);
        // R35 M1：成功 attach 到 gap owner（含 latest=0）即清 recovery；否则保持 pending。
        if (
          resyncSucceeded &&
          streamCursor &&
          streamCursor.ownerInstanceId === gap.ownerInstanceId
        ) {
          recoveryPending = false;
        } else if (!streamCursor) {
          // resolve 应至少给出 gap owner+0；仍为空则保持 pending，禁止 bare。
          recoveryPending = true;
        } else if (!resyncSucceeded) {
          recoveryPending = true;
        } else {
          // 兜底：成功但未 attach 到 gap owner 时仍保持 pending。
          recoveryPending = true;
        }
        connectionController.abort();
      };

      try {
        const eventsUrl = buildWorkbenchEventsUrl(streamCursor, {
          recoveryPending,
          recoveryFallback: streamCursor,
        });
        // 二次守护：recovery 期间 URL 必须含 after。
        if (recoveryPending && !eventsUrl.includes('afterOwnerInstanceId=')) {
          scheduleReconnect();
          return;
        }
        const response = await fetch(eventsUrl, {
          method: 'GET',
          headers: {
            Accept: 'application/x-ndjson',
          },
          signal: connectionController.signal,
        });
        if (!response.ok) throw new Error(response.statusText || `HTTP ${response.status}`);
        if (!response.body) throw new Error('Workbench HTTP event stream 不可用');

        const reader = response.body.getReader();
        while (!stopped && !lifecycleController.signal.aborted && !livePaused) {
          const { done, value } = await reader.read();
          if (done) break;
          if (!value) continue;
          const chunk = decoder.decode(value, { stream: true });
          const frames = parseWorkbenchNdjsonFrames(parserState, chunk);
          for (const frame of frames) {
            // 业务帧 / heartbeat / gap / ignored 都重置 watchdog（仍是合法 NDJSON 行）。
            resetWatchdog();
            if (livePaused) break;
            if (frame.kind === 'gap') {
              await handleGap(frame);
              break;
            }
            if (frame.kind === 'event') {
              streamCursor = advanceWorkbenchHttpStreamCursor(
                streamCursor,
                frame.ownerInstanceId,
                frame.sequence,
              );
              // live 业务帧成功推进后 recovery 完成。
              if (streamCursor && streamCursor.sequence > 0) {
                recoveryPending = false;
              }
              consumeWorkbenchHttpEvent(
                store,
                frame.event,
                {
                  onTerminalStatus: onTerminalStatusRef.current,
                  onAgentRuntime: onAgentRuntimeRef.current,
                },
                frame.ownerInstanceId,
              );
            }
          }
        }

        if (!livePaused) {
          const trailingChunk = decoder.decode();
          if (trailingChunk) {
            const frames = parseWorkbenchNdjsonFrames(parserState, trailingChunk);
            for (const frame of frames) {
              resetWatchdog();
              if (livePaused) break;
              if (frame.kind === 'gap') {
                await handleGap(frame);
                break;
              }
              if (frame.kind === 'event') {
                streamCursor = advanceWorkbenchHttpStreamCursor(
                  streamCursor,
                  frame.ownerInstanceId,
                  frame.sequence,
                );
                if (streamCursor && streamCursor.sequence > 0) {
                  recoveryPending = false;
                }
                consumeWorkbenchHttpEvent(
                  store,
                  frame.event,
                  {
                    onTerminalStatus: onTerminalStatusRef.current,
                    onAgentRuntime: onAgentRuntimeRef.current,
                  },
                  frame.ownerInstanceId,
                );
              }
            }
          }
        }
      } catch {
        // lifecycle abort：不再重连；child-only abort（watchdog/网络/Gap）进入 finally 重连。
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
