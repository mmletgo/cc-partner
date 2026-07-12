/**
 * Attention API / HTTP capability gate 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   桌面命令名与 Mobile attention.v1 gate 是跨端契约，legacy 后端必须 unsupported。
 *
 * Code Logic（这个测试做什么）:
 *   锁定 ATTENTION_DESKTOP_COMMAND；验证 supportsAttentionV1 与 listAttentionSnapshotHttp gate。
 */

import { describe, expect, test, vi } from 'vitest';

import { ATTENTION_DESKTOP_COMMAND, attentionApi } from './attention';
import {
  ATTENTION_CAPABILITY_V1,
  ATTENTION_MOBILE_HTTP_PATH,
  AttentionHttpError,
  listAttentionSnapshotHttp,
  supportsAttentionV1,
} from './attentionHttp';
import type { AttentionSnapshot } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   HTTP loader 测试需要最小快照。
 *
 * Code Logic（这个函数做什么）:
 *   返回空 counts/items 的 AttentionSnapshot。
 */
function emptySnapshot(): AttentionSnapshot {
  return {
    generatedAt: '2026-07-11T10:00:00.000Z',
    counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
    items: [],
  };
}

describe('attention desktop api', () => {
  test('uses list_attention_items command', () => {
    expect(ATTENTION_DESKTOP_COMMAND).toBe('list_attention_items');
    expect(typeof attentionApi.listSnapshot).toBe('function');
  });
});

describe('supportsAttentionV1', () => {
  test('requires protocol_version >= 1 and exact capability token', () => {
    expect(supportsAttentionV1(undefined)).toBe(false);
    expect(supportsAttentionV1({ protocol_version: 0, capabilities: [ATTENTION_CAPABILITY_V1] })).toBe(
      false,
    );
    expect(supportsAttentionV1({ protocol_version: 1, capabilities: [] })).toBe(false);
    expect(
      supportsAttentionV1({
        protocol_version: 1,
        capabilities: ['errors.envelope.v1'],
      }),
    ).toBe(false);
    expect(
      supportsAttentionV1({
        protocol_version: 1,
        capabilities: ['errors.envelope.v1', ATTENTION_CAPABILITY_V1],
      }),
    ).toBe(true);
  });
});

describe('listAttentionSnapshotHttp', () => {
  test('throws unsupported for legacy health without attention.v1', async () => {
    await expect(
      listAttentionSnapshotHttp({
        fetchHealth: async () => ({ protocol_version: 1, capabilities: ['errors.envelope.v1'] }),
        fetchSnapshot: async () => emptySnapshot(),
      }),
    ).rejects.toMatchObject({
      name: 'AttentionHttpError',
      kind: 'unsupported',
      capability: ATTENTION_CAPABILITY_V1,
    });
  });

  test('does not call snapshot endpoint when unsupported', async () => {
    const fetchSnapshot = vi.fn(async () => emptySnapshot());
    await expect(
      listAttentionSnapshotHttp({
        fetchHealth: async () => ({ protocol_version: 0, capabilities: [] }),
        fetchSnapshot,
      }),
    ).rejects.toBeInstanceOf(AttentionHttpError);
    expect(fetchSnapshot).not.toHaveBeenCalled();
  });

  test('loads snapshot when attention.v1 is advertised', async () => {
    const snapshot = emptySnapshot();
    const result = await listAttentionSnapshotHttp({
      fetchHealth: async () => ({
        protocol_version: 1,
        capabilities: [ATTENTION_CAPABILITY_V1],
      }),
      fetchSnapshot: async () => snapshot,
    });
    expect(result).toEqual(snapshot);
    expect(ATTENTION_MOBILE_HTTP_PATH).toBe('/api/mobile/attention');
  });
});
