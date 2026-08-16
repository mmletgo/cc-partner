/**
 * Agent Runtime 纯 reducer 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   乱序 version、snapshot 全量替换、terminal 最新索引与 owner 隔离是投影正确性核心。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 empty、event merge、旧 version 拒绝、snapshot 替换、selector、freshness。
 */

import { describe, expect, test } from 'vitest';
import {
  applyAgentRuntimeEvent,
  applyAgentRuntimeSnapshot,
  emptyAgentRuntimeState,
  latestAgentForTerminal,
  markAgentRuntimeFreshness,
  shouldAcceptAgentRuntimeUpdate,
  toAgentSessionProjection,
} from './agentRuntimeState';
import type {
  AgentRuntimeEvent,
  AgentRuntimeSnapshot,
  AgentSessionRuntimeDto,
} from './types/agentRuntime';

/**
 * Business Logic（为什么需要这个工厂）:
 *   测试只需覆盖少量字段差异，完整 DTO 样板避免重复。
 *
 * Code Logic（这个函数做什么）:
 *   合并 partial 到默认 working session。
 */
function session(partial: Partial<AgentSessionRuntimeDto> = {}): AgentSessionRuntimeDto {
  return {
    id: 'a',
    projectId: 'p',
    worktreeId: null,
    terminalSessionId: 't',
    orchestratorTaskId: null,
    orchestratorAttempt: null,
    providerId: 'claudeCodeVisible',
    phase: 'working',
    version: 1,
    startedAt: '2026-07-15T00:00:00.000Z',
    lastActivityAt: '2026-07-15T00:00:01.000Z',
    endedAt: null,
    outcomeCode: null,
    resumedFromAgentSessionId: null,
    isActive: true,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   事件测试需要统一 agentSession + 可选 sequence。
 *
 * Code Logic（这个函数做什么）:
 *   构造 AgentRuntimeEvent。
 */
function event(
  partial: Partial<AgentSessionRuntimeDto> & {
    sequence?: number;
    ownerInstanceId?: string;
  } = {},
): AgentRuntimeEvent {
  const { sequence, ownerInstanceId, ...sessionPartial } = partial;
  const result: AgentRuntimeEvent = {
    agentSession: session(sessionPartial),
  };
  if (sequence !== undefined) result.sequence = sequence;
  if (ownerInstanceId !== undefined) result.ownerInstanceId = ownerInstanceId;
  return result;
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   snapshot 替换测试需要完整 baseline。
 *
 * Code Logic（这个函数做什么）:
 *   构造 AgentRuntimeSnapshot。
 */
function snapshot(
  sessions: AgentSessionRuntimeDto[],
  overrides: Partial<AgentRuntimeSnapshot> = {},
): AgentRuntimeSnapshot {
  return {
    ownerInstanceId: 'owner-1',
    asOfSequence: 10,
    projectId: 'p',
    sessions,
    truncated: false,
    ...overrides,
  };
}

describe('agentRuntimeState reducer', () => {
  test('does not let an older version replace a newer terminal agent', () => {
    const current = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({ id: 'a', terminalSessionId: 't', version: 3 }),
    );
    const next = applyAgentRuntimeEvent(
      current,
      event({ id: 'a', terminalSessionId: 't', version: 2, phase: 'failed' }),
    );
    expect(latestAgentForTerminal(next, 't')?.version).toBe(3);
    expect(latestAgentForTerminal(next, 't')?.phase).toBe('working');
  });

  test('accepts same-version event when live usage is newer', () => {
    const current = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({
        id: 'a',
        version: 1,
        usage: {
          modelId: 'claude-sonnet-4-5',
          inputTokens: 10,
          outputTokens: 2,
          extractedAt: '2026-07-15T00:00:10.000Z',
        },
      }),
    );
    const next = applyAgentRuntimeEvent(
      current,
      event({
        id: 'a',
        version: 1,
        usage: {
          modelId: 'claude-sonnet-4-5',
          inputTokens: 40,
          outputTokens: 8,
          extractedAt: '2026-07-15T00:00:20.000Z',
        },
      }),
    );
    expect(latestAgentForTerminal(next, 't')?.usage?.inputTokens).toBe(40);
    expect(latestAgentForTerminal(next, 't')?.version).toBe(1);
  });

  test('rejects same-version event when live usage is older or missing', () => {
    const current = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({
        id: 'a',
        version: 1,
        usage: {
          inputTokens: 40,
          outputTokens: 8,
          extractedAt: '2026-07-15T00:00:20.000Z',
        },
      }),
    );
    const older = applyAgentRuntimeEvent(
      current,
      event({
        id: 'a',
        version: 1,
        usage: {
          inputTokens: 10,
          outputTokens: 2,
          extractedAt: '2026-07-15T00:00:10.000Z',
        },
      }),
    );
    const without = applyAgentRuntimeEvent(current, event({ id: 'a', version: 1 }));
    expect(latestAgentForTerminal(older, 't')?.usage?.inputTokens).toBe(40);
    expect(latestAgentForTerminal(without, 't')?.usage?.inputTokens).toBe(40);
    expect(
      shouldAcceptAgentRuntimeUpdate(latestAgentForTerminal(current, 't') ?? undefined, session({ version: 1 })),
    ).toBe(false);
  });

  test('accepts strictly newer version and updates phase', () => {
    const current = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({ id: 'a', version: 1, phase: 'working' }),
    );
    const next = applyAgentRuntimeEvent(
      current,
      event({ id: 'a', version: 2, phase: 'needsInput', sequence: 5 }),
    );
    expect(latestAgentForTerminal(next, 't')?.phase).toBe('needsInput');
    expect(next.asOfSequence).toBe(5);
  });

  test('snapshot fully replaces previous agents for owner baseline', () => {
    const seeded = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({ id: 'old', terminalSessionId: 't-old', version: 9 }),
    );
    const next = applyAgentRuntimeSnapshot(
      snapshot([session({ id: 'new', terminalSessionId: 't-new', version: 1 })], {
        asOfSequence: 42,
        ownerInstanceId: 'owner-2',
      }),
    );
    expect(seeded.byAgentId.has('old')).toBe(true);
    expect(next.byAgentId.has('old')).toBe(false);
    expect(next.byAgentId.has('new')).toBe(true);
    expect(next.ownerInstanceId).toBe('owner-2');
    expect(next.asOfSequence).toBe(42);
    expect(latestAgentForTerminal(next, 't-new')?.id).toBe('new');
  });

  test('rejects events from a different owner when state already has owner', () => {
    const current = applyAgentRuntimeEvent(
      emptyAgentRuntimeState(),
      event({ id: 'a', version: 1, ownerInstanceId: 'owner-a' }),
    );
    const next = applyAgentRuntimeEvent(
      current,
      event({ id: 'a', version: 99, phase: 'failed', ownerInstanceId: 'owner-b' }),
    );
    expect(latestAgentForTerminal(next, 't')?.version).toBe(1);
    expect(latestAgentForTerminal(next, 't')?.phase).toBe('working');
  });

  test('latestAgentForTerminal picks the new active agent even when the stopped agent has a higher version', () => {
    let state = emptyAgentRuntimeState();
    state = applyAgentRuntimeEvent(
      state,
      event({
        id: 'a1',
        terminalSessionId: 't',
        version: 8,
        phase: 'disconnected',
        isActive: false,
        lastActivityAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = applyAgentRuntimeEvent(
      state,
      event({
        id: 'a2',
        terminalSessionId: 't',
        version: 1,
        phase: 'needsInput',
        isActive: true,
        lastActivityAt: '2026-08-13T00:02:00.000Z',
      }),
    );
    expect(latestAgentForTerminal(state, 't')?.id).toBe('a2');
    expect(latestAgentForTerminal(state, 'missing')).toBeNull();
  });

  test('latestAgentForTerminal compares activity instants rather than RFC3339 text across stopped agents', () => {
    let state = emptyAgentRuntimeState();
    state = applyAgentRuntimeEvent(
      state,
      event({
        id: 'stopped-old',
        terminalSessionId: 't',
        version: 9,
        phase: 'disconnected',
        isActive: false,
        lastActivityAt: '2026-08-13T10:30:00+08:00',
      }),
    );
    state = applyAgentRuntimeEvent(
      state,
      event({
        id: 'stopped-new',
        terminalSessionId: 't',
        version: 1,
        phase: 'disconnected',
        isActive: false,
        lastActivityAt: '2026-08-13T03:00:00Z',
      }),
    );

    expect(latestAgentForTerminal(state, 't')?.id).toBe('stopped-new');
  });

  test('toAgentSessionProjection maps taskId and omits empty optionals', () => {
    const projection = toAgentSessionProjection(
      session({
        orchestratorTaskId: 'task-x',
        worktreeId: '',
      }),
      'cached',
    );
    expect(projection.taskId).toBe('task-x');
    expect(projection.worktreeId).toBeUndefined();
    expect(projection.freshness).toBe('cached');
  });

  test('markAgentRuntimeFreshness rewrites all rows without changing version', () => {
    const state = applyAgentRuntimeSnapshot(snapshot([session({ version: 7 })]));
    const marked = markAgentRuntimeFreshness(state, 'offline');
    expect(latestAgentForTerminal(marked, 't')?.freshness).toBe('offline');
    expect(latestAgentForTerminal(marked, 't')?.version).toBe(7);
  });
});
