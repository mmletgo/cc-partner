/**
 * Agent Metadata Ledger schema 单测。
 */
import { describe, expect, it } from 'vitest';
import {
  agentLedgerEntryDecoder,
  agentLedgerPageDecoder,
  agentLedgerSummaryDecoder,
} from './agentLedger';

const validEntry = {
  id: 'e1',
  agentSessionId: 'a1',
  projectId: 'p1',
  worktreeId: null,
  providerId: 'claudeCodeVisible',
  modelId: null,
  startedAt: '2026-07-15T00:00:00Z',
  endedAt: '2026-07-15T00:01:00Z',
  durationMs: 60000,
  outcome: 'completed',
  inputTokens: null,
  outputTokens: null,
  cacheReadTokens: null,
  cacheWriteTokens: null,
  costMinorUnits: null,
  costCurrency: null,
  createdAt: '2026-07-15T00:01:00Z',
  updatedAt: '2026-07-15T00:01:00Z',
};

describe('agentLedger schema', () => {
  it('decodes entry with null tokens', () => {
    const entry = agentLedgerEntryDecoder.decode(validEntry);
    expect(entry.inputTokens).toBeNull();
    expect(entry.outcome).toBe('completed');
  });

  it('rejects negative token counts', () => {
    expect(() =>
      agentLedgerEntryDecoder.decode({ ...validEntry, inputTokens: -1 }),
    ).toThrow();
  });

  it('decodes page and summary coverage', () => {
    const page = agentLedgerPageDecoder.decode({
      items: [validEntry],
      nextCursor: 'cur',
    });
    expect(page.items).toHaveLength(1);
    expect(page.nextCursor).toBe('cur');

    const summary = agentLedgerSummaryDecoder.decode({
      window: '7d',
      projectId: 'p1',
      sessions: 2,
      completed: 1,
      failed: 1,
      cancelled: 0,
      disconnected: 0,
      durationMs: 10,
      inputTokens: 10,
      outputTokens: null,
      cacheReadTokens: 7,
      costByCurrency: [{ currency: 'USD', minorUnits: 3 }],
      usageCoverage: 'partial',
    });
    expect(summary.usageCoverage).toBe('partial');
    expect(summary.outputTokens).toBeNull();
    expect(summary.cacheReadTokens).toBe(7);
  });

  it('rejects forbidden window tokens', () => {
    expect(() =>
      agentLedgerSummaryDecoder.decode({
        window: 'all',
        sessions: 0,
        completed: 0,
        failed: 0,
        cancelled: 0,
        disconnected: 0,
        durationMs: 0,
        inputTokens: null,
        outputTokens: null,
        cacheReadTokens: null,
        costByCurrency: [],
        usageCoverage: 'unavailable',
      }),
    ).toThrow();
  });
});
