/**
 * 工作台等待/完成 hint 纯规则测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   项目卡 / worktree / window 数字必须按 window 去重，等待优先于完成，
 *   激活只 ack completed，刷新不得让已看完成复活。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 apply session、tone、聚合、ack、persist cap/TTL、needsInput 不被 ack。
 */

import { describe, expect, test } from 'vitest';
import type { AgentPhase, AgentSessionRuntimeDto } from './types/agentRuntime';
import {
  ACKED_COMPLETED_CAP,
  ACKED_COMPLETED_STORAGE_KEY,
  SEEN_COMPLETED_CAP,
  SEEN_COMPLETED_STORAGE_KEY,
  SEEN_COMPLETED_TTL_MS,
  ackCompletedForTerminal,
  applyAgentHintSession,
  emptyAgentHintState,
  hintAriaKind,
  hintsForProject,
  hintsForTerminal,
  hintsForWorktree,
  loadPersistedHintExtras,
  replaceActiveWaitingFromSnapshot,
  restoreSeenCompleted,
  serializeAckedCompleted,
  serializeSeenCompleted,
  type SeenCompletedEdge,
} from './workbenchAgentHints';

function session(
  partial: Partial<AgentSessionRuntimeDto> & { phase: AgentPhase },
): AgentSessionRuntimeDto {
  return {
    id: 'agent-a',
    projectId: 'proj-1',
    worktreeId: 'wt-1',
    terminalSessionId: 'term-1',
    orchestratorTaskId: null,
    orchestratorAttempt: null,
    providerId: 'openCodeVisible',
    version: 1,
    startedAt: '2026-08-13T00:00:00.000Z',
    lastActivityAt: '2026-08-13T00:00:01.000Z',
    endedAt: null,
    outcomeCode: null,
    resumedFromAgentSessionId: null,
    isActive: partial.phase === 'needsInput' || partial.phase === 'working' || partial.phase === 'idle',
    ...partial,
  };
}

describe('workbenchAgentHints', () => {
  test('needsInput 计入 waiting，tone 为 wait，ack 不能清掉', () => {
    let state = applyAgentHintSession(emptyAgentHintState(), session({ phase: 'needsInput' }));
    expect(hintsForTerminal(state, 'term-1')).toEqual({
      waitingCount: 1,
      stoppedCount: 0,
      completedCount: 0,
      count: 1,
      tone: 'wait',
    });
    state = ackCompletedForTerminal(state, 'term-1');
    expect(hintsForTerminal(state, 'term-1').waitingCount).toBe(1);
    expect(hintsForTerminal(state, 'term-1').tone).toBe('wait');
  });

  test('未 ack 的 completed 计入绿色完成', () => {
    const state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'completed', isActive: false, endedAt: '2026-08-13T00:01:00.000Z' }),
    );
    expect(hintsForTerminal(state, 'term-1')).toEqual({
      waitingCount: 0,
      stoppedCount: 1,
      completedCount: 1,
      count: 1,
      tone: 'complete',
    });
  });

  test('同一 window 先完成再等待时 waiting 优先，completed 暂不计', () => {
    let state = applyAgentHintSession(
      emptyAgentHintState(),
      session({
        id: 'agent-done',
        phase: 'completed',
        version: 2,
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = applyAgentHintSession(
      state,
      session({ id: 'agent-wait', phase: 'needsInput', version: 3 }),
    );
    expect(hintsForTerminal(state, 'term-1')).toEqual({
      waitingCount: 1,
      stoppedCount: 0,
      completedCount: 0,
      count: 1,
      tone: 'wait',
    });
  });

  test('idle/failed/disconnected 计入已停止，working 清等待但保留未看完成', () => {
    let state = applyAgentHintSession(
      emptyAgentHintState(),
      session({
        id: 'agent-done',
        phase: 'completed',
        version: 2,
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = applyAgentHintSession(
      state,
      session({ id: 'agent-next', phase: 'working', version: 3 }),
    );
    expect(hintsForTerminal(state, 'term-1')).toEqual({
      waitingCount: 0,
      stoppedCount: 1,
      completedCount: 1,
      count: 1,
      tone: 'complete',
    });
    state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'needsInput', version: 1 }),
    );
    state = applyAgentHintSession(state, session({ phase: 'idle', version: 2 }));
    expect(hintsForTerminal(state, 'term-1')).toEqual({
      waitingCount: 0,
      stoppedCount: 1,
      completedCount: 1,
      count: 1,
      tone: 'complete',
    });
    state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'needsInput', version: 1 }),
    );
    state = applyAgentHintSession(state, session({ phase: 'failed', version: 2, isActive: false }));
    expect(hintsForTerminal(state, 'term-1').stoppedCount).toBe(1);
  });

  test('ack 只清该 window 的 completed，并把 agentSessionId 记入 acked', () => {
    let state = applyAgentHintSession(
      emptyAgentHintState(),
      session({
        id: 'agent-done',
        phase: 'completed',
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = applyAgentHintSession(
      state,
      session({
        id: 'agent-other',
        terminalSessionId: 'term-2',
        phase: 'completed',
        isActive: false,
        endedAt: '2026-08-13T00:02:00.000Z',
      }),
    );
    state = ackCompletedForTerminal(state, 'term-1');
    expect(hintsForTerminal(state, 'term-1').count).toBe(0);
    expect(hintsForTerminal(state, 'term-2').completedCount).toBe(1);
    expect(state.ackedCompletedIds.has('agent-done')).toBe(true);
    state = applyAgentHintSession(
      state,
      session({
        id: 'agent-done',
        phase: 'completed',
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    expect(hintsForTerminal(state, 'term-1').count).toBe(0);
  });

  test('项目聚合：等待优先，数字为等待窗口 + 未看完成窗口', () => {
    let state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'needsInput', terminalSessionId: 't-wait' }),
    );
    state = applyAgentHintSession(
      state,
      session({
        id: 'done-1',
        phase: 'completed',
        terminalSessionId: 't-done',
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = applyAgentHintSession(
      state,
      session({
        id: 'other-proj',
        projectId: 'proj-2',
        phase: 'needsInput',
        terminalSessionId: 't-other',
      }),
    );
    expect(hintsForProject(state, 'proj-1')).toEqual({
      waitingCount: 1,
      stoppedCount: 1,
      completedCount: 1,
      count: 2,
      tone: 'wait',
    });
    expect(hintsForWorktree(state, 'proj-1', 'wt-1')).toEqual({
      waitingCount: 1,
      stoppedCount: 1,
      completedCount: 1,
      count: 2,
      tone: 'wait',
    });
    expect(hintsForWorktree(state, 'proj-1', 'wt-missing').count).toBe(0);
  });

  test('acked persist FIFO cap 500，seen completed cap 200 且过期丢弃', () => {
    const acked = Array.from({ length: ACKED_COMPLETED_CAP + 20 }, (_, i) => `ack-${i}`);
    const serializedAcked = serializeAckedCompleted(acked);
    const extras = loadPersistedHintExtras({
      [ACKED_COMPLETED_STORAGE_KEY]: serializedAcked,
      [SEEN_COMPLETED_STORAGE_KEY]: '[]',
    });
    expect(extras.ackedCompletedIds).toHaveLength(ACKED_COMPLETED_CAP);
    expect(extras.ackedCompletedIds[0]).toBe('ack-20');
    expect(extras.ackedCompletedIds.at(-1)).toBe(`ack-${ACKED_COMPLETED_CAP + 19}`);

    const now = Date.parse('2026-08-13T00:00:00.000Z');
    const edges = [
      {
        agentSessionId: 'old',
        terminalSessionId: 't-old',
        projectId: 'proj-1',
        version: 1,
        endedAt: new Date(now - SEEN_COMPLETED_TTL_MS - 1000).toISOString(),
      },
      {
        agentSessionId: 'fresh',
        terminalSessionId: 't-fresh',
        projectId: 'proj-1',
        worktreeId: 'wt-1',
        version: 2,
        endedAt: new Date(now - 1000).toISOString(),
      },
    ];
    const overflow = Array.from({ length: SEEN_COMPLETED_CAP + 5 }, (_, i) => ({
      agentSessionId: `edge-${i}`,
      terminalSessionId: `t-${i}`,
      projectId: 'proj-1',
      version: i + 1,
      endedAt: new Date(now - 1000).toISOString(),
    }));
    const parsed = loadPersistedHintExtras({
      [ACKED_COMPLETED_STORAGE_KEY]: '[]',
      [SEEN_COMPLETED_STORAGE_KEY]: serializeSeenCompleted([...edges, ...overflow]),
    });
    expect(parsed.seenCompleted.length).toBeLessThanOrEqual(SEEN_COMPLETED_CAP);
    expect(parsed.seenCompleted.some((edge: SeenCompletedEdge) => edge.agentSessionId === 'old')).toBe(false);

    const state = restoreSeenCompleted(emptyAgentHintState(), parsed.seenCompleted, now);
    expect(hintsForTerminal(state, 't-fresh').completedCount).toBe(1);
    expect(hintsForTerminal(state, 't-old').count).toBe(0);
  });

  test('snapshot 只重建 waiting，保留未看 completed', () => {
    let state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'needsInput', terminalSessionId: 't-old-wait' }),
    );
    state = applyAgentHintSession(
      state,
      session({
        id: 'done-1',
        phase: 'completed',
        terminalSessionId: 't-done',
        isActive: false,
        endedAt: '2026-08-13T00:01:00.000Z',
      }),
    );
    state = replaceActiveWaitingFromSnapshot(state, [
      session({ id: 'still-wait', phase: 'needsInput', terminalSessionId: 't-new-wait' }),
    ]);
    expect(hintsForTerminal(state, 't-old-wait').waitingCount).toBe(0);
    expect(hintsForTerminal(state, 't-new-wait').waitingCount).toBe(1);
    expect(hintsForTerminal(state, 't-done').completedCount).toBe(1);
  });

  test('hintAriaKind 0/0 也分段为 both，方便始终读出数字', () => {
    expect(hintAriaKind(hintsForTerminal(emptyAgentHintState(), 'x'))).toBe('both');
    expect(
      hintAriaKind({
        waitingCount: 1,
        stoppedCount: 0,
        completedCount: 0,
        count: 1,
        tone: 'wait',
      }),
    ).toBe('waiting');
    expect(
      hintAriaKind({
        waitingCount: 0,
        stoppedCount: 2,
        completedCount: 2,
        count: 2,
        tone: 'complete',
      }),
    ).toBe('completed');
    expect(
      hintAriaKind({
        waitingCount: 1,
        stoppedCount: 1,
        completedCount: 1,
        count: 2,
        tone: 'wait',
      }),
    ).toBe('both');
  });

  test('worktreeId 缺失时可由 session 索引补齐', () => {
    const state = applyAgentHintSession(
      emptyAgentHintState(),
      session({ phase: 'needsInput', worktreeId: null }),
      { sessionWorktreeByTerminal: { 'term-1': 'wt-fallback' } },
    );
    expect(hintsForWorktree(state, 'proj-1', 'wt-fallback').waitingCount).toBe(1);
  });
});
