/**
 * Agent Runtime snapshot/event 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   A1 IPC/HTTP 边界可能损坏或混合版本；写入前端 store 前必须 fail-closed 拒绝坏 DTO，
 *   且不得把 Orchestrator 旧 Claude 字段当成 Agent 真值。
 *
 * Code Logic（这个模块做什么）:
 *   解码 phase/DTO/snapshot/event；RFC3339 时间与有限 version 严格校验。
 */

import type {
  AgentLiveUsage,
  AgentPhase,
  AgentRuntimeEvent,
  AgentRuntimeSnapshot,
  AgentSessionRuntimeDto,
} from '../types/agentRuntime';
import {
  arrayDecoder,
  booleanDecoder,
  ContractDecodeError,
  defineDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * Business Logic（为什么需要这个函数）:
 *   lastActivityAt/startedAt 必须是可解析时间，避免 NaN 排序与错误展示。
 *
 * Code Logic（这个函数做什么）:
 *   要求非空字符串且 Date.parse 有限；错误仅暴露 kind，不写 payload。
 */
function isRfc3339Like(value: string): boolean {
  if (value.trim().length === 0) return false;
  const ms = Date.parse(value);
  return Number.isFinite(ms);
}

/**
 * RFC3339-like 时间字符串 decoder。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   Agent 投影时间字段必须可比较；垃圾字符串不得进入 store。
 *
 * Code Logic（这个 decoder 做什么）:
 *   先 stringDecoder，再 isRfc3339Like 校验。
 */
export const agentRuntimeTimestampDecoder: Decoder<string> = defineDecoder(
  'AgentRuntimeTimestamp',
  (value, path = '$') => {
    const text = stringDecoder.decode(value, path);
    if (!isRfc3339Like(text)) {
      throw new ContractDecodeError('AgentRuntimeTimestamp', path, 'primitive');
    }
    return text;
  },
);

/**
 * 非负有限整数 version decoder。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   CAS 乱序保护依赖 version 可比；NaN/负值/浮点会破坏 latest 选择。
 *
 * Code Logic（这个 decoder 做什么）:
 *   number 有限、>=0、且为整数。
 */
export const agentRuntimeVersionDecoder: Decoder<number> = defineDecoder(
  'AgentRuntimeVersion',
  (value, path = '$') => {
    const n = numberDecoder.decode(value, path);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
      throw new ContractDecodeError('AgentRuntimeVersion', path, 'primitive');
    }
    return n;
  },
);

/**
 * live usage decoder（tokens + model + extractedAt）。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   usage 是可选投影；损坏数字不得进入状态卡，以免把垃圾当成 tok/s。
 *
 * Code Logic（这个 decoder 做什么）:
 *   extractedAt 必须是 RFC3339-like；token 字段可选可空有限数。
 */
export const agentLiveUsageDecoder: Decoder<AgentLiveUsage> = objectDecoder('AgentLiveUsage', {
  modelId: optionalDecoder(nullableDecoder(stringDecoder)),
  inputTokens: optionalDecoder(nullableDecoder(numberDecoder)),
  outputTokens: optionalDecoder(nullableDecoder(numberDecoder)),
  cacheReadTokens: optionalDecoder(nullableDecoder(numberDecoder)),
  cacheWriteTokens: optionalDecoder(nullableDecoder(numberDecoder)),
  extractedAt: agentRuntimeTimestampDecoder,
});

/** Agent phase 七态严格枚举。 */
export const agentPhaseDecoder: Decoder<AgentPhase> = enumDecoder('AgentPhase', [
  'launching',
  'working',
  'needsInput',
  'idle',
  'completed',
  'failed',
  'disconnected',
] as const);

/**
 * 单条 AgentSessionRuntimeDto decoder。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   snapshot/event 的 session 是投影最小单元，缺字段或坏 enum 必须拒绝。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格必填 id/projectId/terminalSessionId/providerId/phase/version/时间/isActive；
 *   可选 worktree/task/attempt/ended/outcome/resume。
 */
export const agentSessionRuntimeDtoDecoder: Decoder<AgentSessionRuntimeDto> = objectDecoder(
  'AgentSessionRuntimeDto',
  {
    id: stringDecoder,
    projectId: stringDecoder,
    worktreeId: optionalDecoder(nullableDecoder(stringDecoder)),
    terminalSessionId: stringDecoder,
    orchestratorTaskId: optionalDecoder(nullableDecoder(stringDecoder)),
    orchestratorAttempt: optionalDecoder(nullableDecoder(numberDecoder)),
    providerId: stringDecoder,
    phase: agentPhaseDecoder,
    version: agentRuntimeVersionDecoder,
    startedAt: agentRuntimeTimestampDecoder,
    lastActivityAt: agentRuntimeTimestampDecoder,
    endedAt: optionalDecoder(nullableDecoder(agentRuntimeTimestampDecoder)),
    outcomeCode: optionalDecoder(nullableDecoder(stringDecoder)),
    resumedFromAgentSessionId: optionalDecoder(nullableDecoder(stringDecoder)),
    isActive: booleanDecoder,
    usage: optionalDecoder(nullableDecoder(agentLiveUsageDecoder)),
  },
);

/**
 * AgentRuntimeSnapshot decoder。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   Gap baseline 必须完整 owner/asOfSequence/sessions，损坏则整次 handshake 失败。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 snapshot 顶层与 sessions 数组。
 */
export const agentRuntimeSnapshotDecoder: Decoder<AgentRuntimeSnapshot> = objectDecoder(
  'AgentRuntimeSnapshot',
  {
    ownerInstanceId: stringDecoder,
    asOfSequence: agentRuntimeVersionDecoder,
    projectId: optionalDecoder(nullableDecoder(stringDecoder)),
    sessions: arrayDecoder(agentSessionRuntimeDtoDecoder),
    truncated: booleanDecoder,
  },
);

/**
 * AgentRuntimeEvent decoder（增量事件）。
 *
 * Business Logic（为什么需要这个 decoder）:
 *   live/buffer 事件必须含合法 agentSession；可选 owner/sequence 供排序。
 *
 * Code Logic（这个 decoder 做什么）:
 *   agentSession 严格；ownerInstanceId/sequence 可选。
 */
export const agentRuntimeEventDecoder: Decoder<AgentRuntimeEvent> = objectDecoder(
  'AgentRuntimeEvent',
  {
    agentSession: agentSessionRuntimeDtoDecoder,
    ownerInstanceId: optionalDecoder(stringDecoder),
    sequence: optionalDecoder(agentRuntimeVersionDecoder),
  },
);
