/**
 * Agent Runtime schema fixtures。
 *
 * Business Logic（为什么需要这个测试）:
 *   损坏 enum/时间/version 不得进入投影 store；snapshot 替换与事件 DTO 形状必须锁定。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖合法 snapshot/event、坏 phase、坏 RFC3339、非整数 version、缺字段。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  agentRuntimeEventDecoder,
  agentRuntimeSnapshotDecoder,
  agentSessionRuntimeDtoDecoder,
} from './agentRuntime';

const sampleSession = {
  id: 'a1',
  projectId: 'p1',
  worktreeId: 'wt1',
  terminalSessionId: 't1',
  orchestratorTaskId: 'task-1',
  orchestratorAttempt: 1,
  providerId: 'claudeCodeVisible',
  phase: 'working',
  version: 3,
  startedAt: '2026-07-15T00:00:00.000Z',
  lastActivityAt: '2026-07-15T00:01:00.000Z',
  endedAt: null,
  outcomeCode: null,
  resumedFromAgentSessionId: null,
  isActive: true,
};

const sampleSnapshot = {
  ownerInstanceId: 'owner-1',
  asOfSequence: 10,
  projectId: 'p1',
  sessions: [sampleSession],
  truncated: false,
};

const decodedSession = { ...sampleSession, usage: undefined };

describe('agentRuntime schemas', () => {
  test('decodes valid session and snapshot', () => {
    expect(agentSessionRuntimeDtoDecoder.decode(sampleSession)).toEqual(decodedSession);
    expect(agentRuntimeSnapshotDecoder.decode(sampleSnapshot)).toEqual({
      ...sampleSnapshot,
      sessions: [decodedSession],
    });
  });

  test('decodes event with optional owner/sequence', () => {
    const event = {
      agentSession: sampleSession,
      ownerInstanceId: 'owner-1',
      sequence: 12,
    };
    expect(agentRuntimeEventDecoder.decode(event)).toEqual({
      ...event,
      agentSession: decodedSession,
    });
  });

  test('decodes optional live usage and rejects bad extractedAt', () => {
    const withUsage = {
      ...sampleSession,
      usage: {
        modelId: 'claude-sonnet-4-5',
        inputTokens: 1200,
        outputTokens: 80,
        cacheReadTokens: 400,
        cacheWriteTokens: 20,
        extractedAt: '2026-07-15T00:02:00.000Z',
      },
    };
    expect(agentSessionRuntimeDtoDecoder.decode(withUsage)).toEqual(withUsage);
    const badUsage = {
      ...sampleSession,
      usage: { extractedAt: 'not-a-date', inputTokens: 1 },
    };
    expect(() => agentSessionRuntimeDtoDecoder.decode(badUsage)).toThrow(ContractDecodeError);
  });

  test('rejects unknown phase', () => {
    const bad = { ...sampleSession, phase: 'thinking' };
    expect(() => agentSessionRuntimeDtoDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('rejects non-RFC3339 lastActivityAt', () => {
    const bad = { ...sampleSession, lastActivityAt: 'not-a-date' };
    expect(() => agentSessionRuntimeDtoDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('rejects non-integer version', () => {
    const bad = { ...sampleSession, version: 1.5 };
    expect(() => agentSessionRuntimeDtoDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('rejects negative asOfSequence', () => {
    const bad = { ...sampleSnapshot, asOfSequence: -1 };
    expect(() => agentRuntimeSnapshotDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('rejects missing terminalSessionId', () => {
    const rest = { ...sampleSession } as Record<string, unknown>;
    delete rest.terminalSessionId;
    expect(() => agentSessionRuntimeDtoDecoder.decode(rest)).toThrow(ContractDecodeError);
  });

  test('rejects nativeSessionId-only corrupt shapes as incomplete DTO', () => {
    // native 字段若出现在 JSON 中被忽略（前向兼容额外字段），但缺 id 仍失败
    const corrupt = { nativeSessionId: 'secret', phase: 'working' };
    expect(() => agentSessionRuntimeDtoDecoder.decode(corrupt)).toThrow(ContractDecodeError);
  });
});
