/**
 * Token 统计页 schema / pure helper 单测。
 *
 * Business Logic（为什么需要）:
 *   Decoder 必须 fail-closed；helper 必须按照用户合同保障派生指标与货币 picker 行为，
 *   防止 KPI 卡显示坏数据或 UI 在多币种下错配。
 *
 * Code Logic（做什么）:
 *   解码 positive / negative fixture 验证 strict；
 *   helper 验证 null 分支与排序。
 */

import { describe, expect, it } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  agentLedgerGroupRowDecoder,
  agentLedgerSummaryDecoder,
  bucketizeTrendByISO,
  computeCacheHitRate,
  computeRealConsumed,
  currencyAmountDecoder,
  pickPrimaryCurrencyCost,
} from './tokenStats';

function makeSummary(overrides: Record<string, unknown> = {}): unknown {
  return {
    window: '7d',
    projectId: 'p1',
    sessions: 10,
    completed: 8,
    failed: 1,
    cancelled: 1,
    disconnected: 0,
    durationMs: 60_000,
    inputTokens: 100,
    outputTokens: 50,
    cacheReadTokens: 30,
    cacheWriteTokens: 5,
    realConsumedTokens: 155,
    cacheHitRate: 0.23076923076923078,
    requestsCount: 10,
    costByCurrency: [{ currency: 'USD', minorUnits: 420 }],
    totalCostByCurrency: [{ currency: 'USD', minorUnits: 420 }],
    usageCoverage: 'complete',
    byModel: [],
    byProvider: [],
    byProject: [],
    trend: [{ bucketStart: '2026-08-15T00:00:00Z', inputTokens: 1, outputTokens: 1, cacheReadTokens: 0, cacheWriteTokens: 0, costByCurrency: [] }],
    bucket: 'day',
    ...overrides,
  };
}

describe('tokenStats schema', () => {
  it('currencyAmountDecoder accepts well-formed', () => {
    expect(currencyAmountDecoder.decode({ currency: 'USD', minorUnits: 0 })).toEqual({
      currency: 'USD',
      minorUnits: 0,
    });
  });

  it('currencyAmountDecoder rejects negative minorUnits', () => {
    expect(() => currencyAmountDecoder.decode({ currency: 'USD', minorUnits: -1 })).toThrow(
      ContractDecodeError,
    );
  });

  it('agentLedgerGroupRowDecoder accepts unknown fields stripped', () => {
    const row = {
      key: 'claude-opus-4-1',
      label: 'Opus',
      sessions: 3,
      completed: 2,
      failed: 1,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 100,
      outputTokens: 20,
      cacheReadTokens: 50,
      cacheWriteTokens: null,
      costByCurrency: [{ currency: 'USD', minorUnits: 100 }],
      runtimeTag: 'should-be-ignored',
    };
    const decoded = agentLedgerGroupRowDecoder.decode(row);
    expect(decoded.key).toBe('claude-opus-4-1');
    expect((decoded as unknown as { runtimeTag?: unknown }).runtimeTag).toBeUndefined();
  });

  it('agentLedgerSummaryDecoder fails closed when sessions missing', () => {
    const bad = makeSummary();
    delete (bad as { sessions?: number }).sessions;
    expect(() => agentLedgerSummaryDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  it('agentLedgerSummaryDecoder fails closed when bucket missing', () => {
    const bad = makeSummary();
    delete (bad as { bucket?: string }).bucket;
    expect(() => agentLedgerSummaryDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  it('agentLedgerSummaryDecoder fails closed on bad window enum', () => {
    const bad = makeSummary({ window: 'forever' });
    expect(() => agentLedgerSummaryDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  it('agentLedgerSummaryDecoder accepts sparse empty-window wire from skip_serializing sidecar', () => {
    const sparse = {
      window: '7d',
      sessions: 0,
      completed: 0,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      durationMs: 0,
      inputTokens: null,
      outputTokens: null,
      cacheReadTokens: null,
      requestsCount: 0,
      costByCurrency: [],
      usageCoverage: 'unavailable',
      trend: [],
      bucket: 'day',
    };
    const decoded = agentLedgerSummaryDecoder.decode(sparse);
    expect(decoded.cacheWriteTokens).toBeNull();
    expect(decoded.realConsumedTokens).toBeNull();
    expect(decoded.cacheHitRate).toBeNull();
    expect(decoded.totalCostByCurrency).toEqual([]);
    expect(decoded.byModel).toEqual([]);
    expect(decoded.byProvider).toEqual([]);
    expect(decoded.byProject).toEqual([]);
  });

  it('agentLedgerGroupRowDecoder treats omitted token fields as null', () => {
    const decoded = agentLedgerGroupRowDecoder.decode({
      key: 'claude-opus-4-1',
      sessions: 1,
      completed: 1,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      costByCurrency: [],
    });
    expect(decoded.inputTokens).toBeNull();
    expect(decoded.outputTokens).toBeNull();
    expect(decoded.cacheReadTokens).toBeNull();
    expect(decoded.cacheWriteTokens).toBeNull();
  });
});

describe('tokenStats helpers', () => {
  it('computeCacheHitRate divides when denominator non-zero', () => {
    expect(computeCacheHitRate(30, 70)).toBeCloseTo(0.3, 6);
  });

  it('computeCacheHitRate returns null when denominator zero', () => {
    expect(computeCacheHitRate(0, 0)).toBeNull();
    expect(computeCacheHitRate(null, 5)).toBeNull();
    expect(computeCacheHitRate(5, null)).toBeNull();
    expect(computeCacheHitRate(null, null)).toBeNull();
  });

  it('computeRealConsumed sums three inputs when all provided', () => {
    expect(computeRealConsumed(100, 5, 50)).toBe(155);
  });

  it('computeRealConsumed returns null when all three null', () => {
    expect(computeRealConsumed(null, null, null)).toBeNull();
    expect(computeRealConsumed(undefined, undefined, undefined)).toBeNull();
  });

  it('computeRealConsumed tolerates single non-null', () => {
    expect(computeRealConsumed(null, 7, null)).toBe(7);
    expect(computeRealConsumed(null, null, 9)).toBe(9);
    expect(computeRealConsumed(4, null, null)).toBe(4);
  });

  it('pickPrimaryCurrencyCost puts USD first, others sorted desc', () => {
    const rows = [
      { currency: 'EUR', minorUnits: 200 },
      { currency: 'USD', minorUnits: 999 },
      { currency: 'CNY', minorUnits: 50 },
    ];
    const result = pickPrimaryCurrencyCost(rows, 'en-US');
    expect(result.primary?.currency).toBe('USD');
    expect(result.others.map((r) => r.currency)).toEqual(['EUR', 'CNY']);
    expect(result.others.map((r) => r.minorUnits)).toEqual([200, 50]);
  });

  it('pickPrimaryCurrencyCost prefers CNY when system locale is zh', () => {
    const rows = [
      { currency: 'USD', minorUnits: 50 },
      { currency: 'CNY', minorUnits: 999 },
    ];
    const result = pickPrimaryCurrencyCost(rows, 'zh-CN');
    expect(result.primary?.currency).toBe('CNY');
    expect(result.others[0]?.currency).toBe('USD');
  });

  it('pickPrimaryCurrencyCost falls back to max minorUnits when no common currency', () => {
    const rows = [
      { currency: 'AUD', minorUnits: 10 },
      { currency: 'CAD', minorUnits: 999 },
    ];
    const result = pickPrimaryCurrencyCost(rows, 'en-US');
    expect(result.primary?.currency).toBe('CAD');
    expect(result.others[0]?.currency).toBe('AUD');
  });

  it('pickPrimaryCurrencyCost returns null primary for empty rows', () => {
    expect(pickPrimaryCurrencyCost([], 'en-US')).toEqual({ primary: null, others: [] });
  });

  it('bucketizeTrendByISO sorts ascending and dedupes', () => {
    const points = [
      { bucketStart: '2026-08-15T02:00:00Z', inputTokens: 1, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, costByCurrency: [] },
      { bucketStart: '2026-08-15T00:00:00Z', inputTokens: 1, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, costByCurrency: [] },
      { bucketStart: '2026-08-15T02:00:00Z', inputTokens: 7, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, costByCurrency: [] },
      { bucketStart: '2026-08-15T01:00:00Z', inputTokens: 1, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0, costByCurrency: [] },
    ];
    const sorted = bucketizeTrendByISO(points);
    expect(sorted.map((p) => p.bucketStart)).toEqual([
      '2026-08-15T00:00:00Z',
      '2026-08-15T01:00:00Z',
      '2026-08-15T02:00:00Z',
    ]);
    // 重复 bucket 保留首次（inputTokens=1）
    expect(sorted[2]?.inputTokens).toBe(1);
  });
});
