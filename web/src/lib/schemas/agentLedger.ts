/**
 * Agent Metadata Ledger 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC 边界可能损坏；写入 drawer/Fleet 前 fail-closed，且拒绝把 unknown token 当 0。
 *
 * Code Logic（这个模块做什么）:
 *   严格 decoder：window/coverage/outcome、非负计数、可空 token。
 */

import type {
  AgentLedgerEntry,
  AgentLedgerOutcome,
  AgentLedgerPage,
  AgentLedgerSummary,
  CurrencyAmount,
  LedgerUsageCoverage,
  LedgerWindow,
} from '../types/agentLedger';
import {
  arrayDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';
import { ContractDecodeError, defineDecoder } from '../runtimeSchema';

/**
 * Business Logic（为什么需要这个函数）:
 *   sessions/tokens 不得为负，否则 badge 与聚合误导。
 *
 * Code Logic（这个函数做什么）:
 *   有限非负整数。
 */
export const nonNegativeIntDecoder: Decoder<number> = defineDecoder(
  'NonNegativeInt',
  (value, path = '$') => {
    const n = numberDecoder.decode(value, path);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
      throw new ContractDecodeError('NonNegativeInt', path, 'primitive');
    }
    return n;
  },
);

export const ledgerWindowDecoder: Decoder<LedgerWindow> = enumDecoder('LedgerWindow', [
  '24h',
  '7d',
  '30d',
] as const);

export const ledgerUsageCoverageDecoder: Decoder<LedgerUsageCoverage> = enumDecoder(
  'LedgerUsageCoverage',
  ['complete', 'partial', 'unavailable'] as const,
);

export const agentLedgerOutcomeDecoder: Decoder<AgentLedgerOutcome> = enumDecoder(
  'AgentLedgerOutcome',
  ['completed', 'failed', 'cancelled', 'disconnected'] as const,
);

export const currencyAmountDecoder: Decoder<CurrencyAmount> = objectDecoder('CurrencyAmount', {
  currency: stringDecoder,
  minorUnits: nonNegativeIntDecoder,
});

export const agentLedgerEntryDecoder: Decoder<AgentLedgerEntry> = objectDecoder(
  'AgentLedgerEntry',
  {
    id: stringDecoder,
    agentSessionId: stringDecoder,
    projectId: stringDecoder,
    worktreeId: nullableDecoder(stringDecoder),
    providerId: stringDecoder,
    modelId: nullableDecoder(stringDecoder),
    startedAt: stringDecoder,
    endedAt: stringDecoder,
    durationMs: nonNegativeIntDecoder,
    outcome: agentLedgerOutcomeDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheReadTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheWriteTokens: nullableDecoder(nonNegativeIntDecoder),
    costMinorUnits: nullableDecoder(nonNegativeIntDecoder),
    costCurrency: nullableDecoder(stringDecoder),
    createdAt: stringDecoder,
    updatedAt: stringDecoder,
  },
);

export const agentLedgerPageDecoder: Decoder<AgentLedgerPage> = objectDecoder('AgentLedgerPage', {
  items: arrayDecoder(agentLedgerEntryDecoder),
  nextCursor: nullableDecoder(stringDecoder),
});

export const agentLedgerSummaryDecoder: Decoder<AgentLedgerSummary> = objectDecoder(
  'AgentLedgerSummary',
  {
    window: ledgerWindowDecoder,
    projectId: optionalDecoder(nullableDecoder(stringDecoder)),
    sessions: nonNegativeIntDecoder,
    completed: nonNegativeIntDecoder,
    failed: nonNegativeIntDecoder,
    cancelled: nonNegativeIntDecoder,
    disconnected: nonNegativeIntDecoder,
    durationMs: nonNegativeIntDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    costByCurrency: arrayDecoder(currencyAmountDecoder),
    usageCoverage: ledgerUsageCoverageDecoder,
  },
);

/**
 * Business Logic（为什么需要这个函数）:
 *   清除返回删除行数，必须是非负整数。
 *
 * Code Logic（这个函数做什么）:
 *   nonNegativeIntDecoder。
 */
export const clearAgentLedgerResultDecoder: Decoder<number> = nonNegativeIntDecoder;
