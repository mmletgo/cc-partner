/**
 * Attention snapshot schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import { attentionSnapshotDecoder } from './attention';

const validSnapshot = {
  generatedAt: '2026-07-13T00:00:00.000Z',
  counts: { total: 1, decision: 1, blocked: 0, environment: 0 },
  items: [
    {
      id: 'orchestrator:human-review:t1',
      category: 'decision',
      sourceKind: 'orchestratorHumanReview',
      title: 'Review',
      summary: 's',
      updatedAt: '2026-07-13T00:00:00.000Z',
      freshness: 'live',
      cachedAt: null,
      project: { id: 'p1', name: 'proj', kind: 'local' },
      device: null,
      target: { kind: 'orchestratorTask', projectId: 'p1', taskId: 't1' },
    },
  ],
};

describe('attention schemas', () => {
  test('decodes normal snapshot', () => {
    expect(attentionSnapshotDecoder.decode(validSnapshot)).toEqual(validSnapshot);
  });

  test('malformed category fails closed', () => {
    const bad = structuredClone(validSnapshot);
    bad.items[0].category = 'unknown' as 'decision';
    expect(() => attentionSnapshotDecoder.decode(bad)).toThrow(ContractDecodeError);
  });

  test('malformed target kind fails at path', () => {
    const bad = structuredClone(validSnapshot);
    bad.items[0].target = { kind: 'settings', tab: 'general' } as never;
    try {
      attentionSnapshotDecoder.decode(bad);
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      expect((reason as ContractDecodeError).path).toContain('$.items[0].target');
    }
  });
});
