/**
 * User-mirror preview / apply / get API 契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   前端命令名与 request 形状必须在 sidecar 落地前锁定；
 *   成功 body fail-closed；MCP 含 secret 的 JSON 不得进入 typed 模型。
 *
 * Code Logic（这个测试做什么）:
 *   mock invokeDecoded；锁定三条 snake_case 命令、参数与 decoder 路径。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Decoder } from '@/lib/runtimeSchema';
import { ContractDecodeError } from '@/lib/runtimeSchema';
import {
  validAgentInventory,
  validInventory,
  validMcpItem,
  validPlan,
  validResult,
} from '@/lib/schemas/userMirror.test';
import { userMirrorInventoryDecoder } from '@/lib/schemas/userMirror';

const mockInvoke = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
  invokeDecoded: async <T>(
    cmd: string,
    args: Record<string, unknown> | undefined,
    decoder: Decoder<T>,
  ): Promise<T> => {
    const raw = await mockInvoke(cmd, args);
    return decoder.decode(raw, '$');
  },
  normalizeError: (reason: unknown) => {
    if (reason instanceof Error) return reason;
    if (reason && typeof reason === 'object') {
      const obj = reason as Record<string, unknown>;
      const msg = typeof obj.error === 'string' ? obj.error : String(reason);
      const err = new Error(msg);
      if (typeof obj.code === 'string') return Object.assign(err, { code: obj.code });
      return err;
    }
    return new Error(String(reason));
  },
}));

import { USER_MIRROR_COMMANDS, userMirrorApi } from './userMirror';

describe('user-mirror API', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('command constants lock sidecar snake_case names', () => {
    expect(USER_MIRROR_COMMANDS).toEqual({
      preview: 'agent_hub_preview_user_mirror',
      apply: 'agent_hub_apply_user_mirror',
      get: 'agent_hub_get_user_mirror',
    });
  });

  test('preview passes request object and decodes plan', async () => {
    mockInvoke.mockResolvedValueOnce(validPlan);
    const request = {
      direction: 'pull' as const,
      sourceDeviceId: 'dev-a',
      peerDeviceIds: [] as string[],
    };
    const result = await userMirrorApi.preview(request);
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_preview_user_mirror', { request });
    expect(result.planToken).toBe('plan-1');
    expect(result.direction).toBe('pull');
  });

  test('apply and get use planToken/clientRequestId shapes', async () => {
    mockInvoke.mockResolvedValueOnce(validResult);
    await userMirrorApi.apply({
      planToken: 'plan-1',
      clientRequestId: 'req-1',
    });
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_apply_user_mirror', {
      request: { planToken: 'plan-1', clientRequestId: 'req-1' },
    });

    mockInvoke.mockResolvedValueOnce(validResult);
    await userMirrorApi.get('req-1');
    expect(mockInvoke).toHaveBeenCalledWith('agent_hub_get_user_mirror', {
      clientRequestId: 'req-1',
    });
  });

  test('fail-closed success body decode rejects malformed plan', async () => {
    mockInvoke.mockResolvedValueOnce({ planToken: 'x', agents: [] });
    await expect(
      userMirrorApi.preview({ direction: 'pull', peerDeviceIds: [] }),
    ).rejects.toBeInstanceOf(ContractDecodeError);
  });

  test('secret-bearing MCP JSON in nested inventory fails closed via decoder', () => {
    expect(() =>
      userMirrorInventoryDecoder.decode({
        ...validInventory,
        agents: [
          {
            ...validAgentInventory,
            items: [
              {
                ...validMcpItem,
                mcpCredential: {
                  present: true,
                  hash: 'h',
                  secret: 'sk-live-leaked',
                  token: 't',
                  value: 'v',
                },
              },
            ],
          },
        ],
      }),
    ).toThrow(ContractDecodeError);
  });

  test('preserves stable backend error codes on transport failures', async () => {
    mockInvoke.mockRejectedValueOnce(
      Object.assign(new Error('peer missing capability'), {
        code: 'USER_MIRROR_CAPABILITY_UNSUPPORTED',
      }),
    );
    try {
      await userMirrorApi.preview({ direction: 'push', peerDeviceIds: ['dev-b'] });
      expect.unreachable('should throw');
    } catch (err) {
      expect(err).toBeInstanceOf(Error);
      expect((err as Error & { code?: string }).code).toBe(
        'USER_MIRROR_CAPABILITY_UNSUPPORTED',
      );
    }
  });

  test('source does not invent optional defaults and has no any', () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const src = readFileSync(join(here, 'userMirror.ts'), 'utf8');
    expect(src).not.toMatch(/\bany\b/);
    expect(src).toContain('agent_hub_preview_user_mirror');
    expect(src).toContain('agent_hub_apply_user_mirror');
    expect(src).toContain('agent_hub_get_user_mirror');
  });
});
