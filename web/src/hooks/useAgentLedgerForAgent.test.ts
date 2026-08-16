/**
 * useAgentLedgerForAgent 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   hook 是工作台状态卡 4 个 agent 指标的来源；working 阶段不应拉取、
、phase 到达终态必须拉取、agentSessionId 变化必须重取——这三条是用户可见行为的根。
 *
 * Code Logic（这个测试文件做什么）:
 *   - 用 vi.mock 替换 workbenchApi.agentLedger.list；
 *   - 通过 rerender 切换 phase / projectId / agentSessionId；
 *   - 断言调用次数与返回的 ledgerEntry 正确性；
 *   - 验证 working 阶段不调用、终态切换调用、id 切换调用。
 */
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AgentLedgerEntry } from '@/lib/types/agentLedger';
import type { AgentPhase } from '@/lib/types/agentRuntime';

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    agentLedger: {
      list: vi.fn(),
    },
  },
}));

import { workbenchApi } from '@/api/workbench';
import { isAgentTerminalPhase, useAgentLedgerForAgent } from './useAgentLedgerForAgent';

const mockedList = vi.mocked(workbenchApi.agentLedger.list);

const sampleEntry: AgentLedgerEntry = {
  id: 'ledger-1',
  agentSessionId: 'agent-1',
  projectId: 'project-1',
  worktreeId: 'wt-1',
  providerId: 'claudeCodeVisible',
  modelId: 'claude-sonnet-4-5',
  startedAt: '2026-08-16T10:00:00Z',
  endedAt: '2026-08-16T10:05:00Z',
  durationMs: 300_000,
  outcome: 'completed',
  inputTokens: 1200,
  outputTokens: 600,
  cacheReadTokens: 800,
  cacheWriteTokens: 200,
  terminalTitle: null,
  costMinorUnits: null,
  costCurrency: null,
  createdAt: '2026-08-16T10:05:00Z',
  updatedAt: '2026-08-16T10:05:00Z',
};

describe('isAgentTerminalPhase', () => {
  it.each([
    ['completed', true],
    ['failed', true],
    ['disconnected', true],
    ['working', false],
    ['idle', false],
    ['needsInput', false],
    ['launching', false],
  ] as Array<[AgentPhase, boolean]>)('phase=%s → %s', (phase, expected) => {
    expect(isAgentTerminalPhase(phase)).toBe(expected);
  });

  it('null/undefined → false', () => {
    expect(isAgentTerminalPhase(null)).toBe(false);
    expect(isAgentTerminalPhase(undefined)).toBe(false);
  });
});

describe('useAgentLedgerForAgent', () => {
  beforeEach(() => {
    mockedList.mockReset();
    mockedList.mockResolvedValue({ items: [sampleEntry], nextCursor: null });
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('working 阶段不拉取 ledger', async () => {
    const { result } = renderHook(() =>
      useAgentLedgerForAgent('project-1', 'agent-1', 'working'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockedList).not.toHaveBeenCalled();
    expect(result.current.ledgerEntry).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it('completed 阶段拉取一次并写入 ledgerEntry', async () => {
    const { result } = renderHook(() =>
      useAgentLedgerForAgent('project-1', 'agent-1', 'completed'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockedList).toHaveBeenCalledTimes(1);
    expect(mockedList).toHaveBeenCalledWith({ projectId: 'project-1', limit: 50 });
    expect(result.current.ledgerEntry?.agentSessionId).toBe('agent-1');
    expect(result.current.ledgerEntry?.outputTokens).toBe(600);
  });

  it('agentSessionId 不在 page.items 中时返回 null（不报错）', async () => {
    mockedList.mockResolvedValueOnce({ items: [], nextCursor: null });
    const { result } = renderHook(() =>
      useAgentLedgerForAgent('project-1', 'agent-missing', 'failed'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockedList).toHaveBeenCalledTimes(1);
    expect(result.current.ledgerEntry).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it('agentSessionId 变化触发重新拉取', async () => {
    const firstEntry: AgentLedgerEntry = { ...sampleEntry, agentSessionId: 'agent-1' };
    const secondEntry: AgentLedgerEntry = { ...sampleEntry, agentSessionId: 'agent-2', id: 'ledger-2' };
    mockedList
      .mockResolvedValueOnce({ items: [firstEntry], nextCursor: null })
      .mockResolvedValueOnce({ items: [secondEntry], nextCursor: null });

    const { result, rerender } = renderHook(
      ({ agentSessionId }: { agentSessionId: string }) =>
        useAgentLedgerForAgent('project-1', agentSessionId, 'completed'),
      { initialProps: { agentSessionId: 'agent-1' } },
    );

    await waitFor(() => expect(result.current.ledgerEntry?.agentSessionId).toBe('agent-1'));
    rerender({ agentSessionId: 'agent-2' });
    await waitFor(() => expect(result.current.ledgerEntry?.agentSessionId).toBe('agent-2'));
    expect(mockedList).toHaveBeenCalledTimes(2);
  });

  it('projectId 为 null 时不拉取', async () => {
    const { result } = renderHook(() =>
      useAgentLedgerForAgent(null, 'agent-1', 'completed'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockedList).not.toHaveBeenCalled();
    expect(result.current.ledgerEntry).toBeNull();
  });

  it('agentSessionId 为 null 时不拉取', async () => {
    const { result } = renderHook(() =>
      useAgentLedgerForAgent('project-1', null, 'completed'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(mockedList).not.toHaveBeenCalled();
    expect(result.current.ledgerEntry).toBeNull();
  });

  it('ledger 拉取失败保留 error，ledgerEntry 置空', async () => {
    mockedList.mockRejectedValueOnce(new Error('network down'));
    const { result } = renderHook(() =>
      useAgentLedgerForAgent('project-1', 'agent-1', 'completed'),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error?.message).toBe('network down');
    expect(result.current.ledgerEntry).toBeNull();
  });
});