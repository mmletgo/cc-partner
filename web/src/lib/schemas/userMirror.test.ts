/**
 * User-mirror inventory / plan / result schema 合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   IPC 边界必须 fail-closed 拒绝损坏/混合版本 DTO；MCP 不得把 token/value/secret 带进 typed 模型。
 *
 * Code Logic（这个测试做什么）:
 *   解码合法 inventory/plan/result；拒绝未知枚举、非有限 size、缺字段；
 *   未知额外字段忽略；MCP 结构出现 token/value/secret 必须失败。
 */

import { describe, expect, expectTypeOf, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import type {
  UserMirrorInventoryDto,
  UserMirrorMcpCredentialFactDto,
  UserMirrorPlanDto,
  UserMirrorResultDto,
} from '../types/userMirror';
import {
  userMirrorInventoryDecoder,
  userMirrorMcpCredentialFactDecoder,
  userMirrorPlanDecoder,
  userMirrorPortableItemDecoder,
  userMirrorResultDecoder,
  userMirrorSelectionFilterDecoder,
} from './userMirror';

/** 合法 MCP 凭据事实（仅 present/hash）。 */
export const validMcpCredential = {
  present: true,
  hash: 'cred-hash-1',
};

/** 合法 portable MCP 项。 */
export const validMcpItem = {
  kind: 'mcp' as const,
  nativeId: 'github',
  displayName: 'GitHub MCP',
  contentHash: 'mcp-content',
  treeHash: null,
  actualEnabled: true,
  mcpCredential: validMcpCredential,
  warnings: [] as string[],
};

/** 合法 skill 项。 */
export const validSkillItem = {
  kind: 'skill' as const,
  nativeId: 'skill-a',
  displayName: 'Skill A',
  contentHash: 'skill-hash',
  treeHash: 'skill-tree',
  actualEnabled: true,
  mcpCredential: null,
  warnings: [] as string[],
};

/** 合法原生文件事实。 */
export const validNativeFile = {
  logicalId: 'claude.native.CLAUDE.md',
  contentHash: 'file-hash',
  exists: true,
  size: 128,
};

/** 合法单 Agent inventory。 */
export const validAgentInventory = {
  target: 'claude' as const,
  slots: {
    common: 'slot-common',
    adapted: 'slot-adapted',
    exclusive: null,
  },
  nativeFiles: [validNativeFile],
  items: [validSkillItem, validMcpItem],
};

/** 合法全 Agent inventory 快照。 */
export const validInventory = {
  sourceDeviceId: 'dev-a',
  inventorySnapshotHash: 'inv-hash-1',
  refreshedAt: '2026-08-23T00:00:00.000Z',
  agents: [validAgentInventory],
  credentialBearingCount: 1,
};

/** 合法 preview plan。 */
export const validPlan = {
  planToken: 'plan-1',
  expiresAt: '2026-08-23T00:15:00.000Z',
  direction: 'pull' as const,
  sourceDeviceId: 'dev-a',
  destinationDeviceId: 'dev-local',
  remoteInventorySnapshotHash: 'remote-hash',
  localInventorySnapshotHash: 'local-hash',
  credentialBearingCount: 1,
  hasCredentialBearingAssets: true,
  agents: [
    {
      target: 'claude' as const,
      instructionWrites: [
        {
          logicalId: 'claude.native.CLAUDE.md',
          op: 'replace' as const,
          sourceHash: 'src-hash',
          destHash: 'dst-hash',
        },
      ],
      portableUpserts: [
        {
          kind: 'skill' as const,
          nativeId: 'skill-a',
          displayName: 'Skill A',
          op: 'write' as const,
          credentialBearing: false,
        },
      ],
      portableDeletes: [
        {
          kind: 'command' as const,
          nativeId: 'cmd-x',
          displayName: 'Cmd X',
          op: 'delete' as const,
          credentialBearing: false,
        },
      ],
      pluginDisables: [
        {
          kind: 'plugin' as const,
          nativeId: 'plug-x',
          displayName: 'Plug X',
          op: 'disable' as const,
          credentialBearing: false,
        },
      ],
      mcpDeletes: [
        {
          kind: 'mcp' as const,
          nativeId: 'github',
          displayName: 'GitHub MCP',
          op: 'delete' as const,
          credentialBearing: true,
        },
      ],
    },
  ],
  blockingReasons: [] as string[],
};

/** 合法 apply 结果。 */
export const validResult = {
  planToken: 'plan-1',
  clientRequestId: 'req-1',
  sourceDeviceId: 'dev-a',
  destinationDeviceId: 'dev-local',
  partial: false,
  agents: [
    {
      target: 'claude' as const,
      state: 'succeeded' as const,
      errorCode: null,
      message: null,
    },
  ],
};

describe('user-mirror schemas', () => {
  test('decodes valid inventory/plan/result', () => {
    const inventory = userMirrorInventoryDecoder.decode(validInventory);
    expect(inventory.inventorySnapshotHash).toBe('inv-hash-1');
    expect(inventory.agents).toHaveLength(1);
    expect(inventory.agents[0].items).toHaveLength(2);
    expect(inventory.agents[0].items[1].mcpCredential).toEqual({
      present: true,
      hash: 'cred-hash-1',
    });

    const plan = userMirrorPlanDecoder.decode(validPlan);
    expect(plan.planToken).toBe('plan-1');
    expect(plan.direction).toBe('pull');
    expect(plan.agents[0].instructionWrites[0].op).toBe('replace');
    expect(plan.agents[0].pluginDisables[0].op).toBe('disable');

    const result = userMirrorResultDecoder.decode(validResult);
    expect(result.partial).toBe(false);
    expect(result.agents[0].state).toBe('succeeded');
  });

  test('rejects unknown direction/op/state enums', () => {
    expect(() =>
      userMirrorPlanDecoder.decode({ ...validPlan, direction: 'sync' }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      userMirrorPlanDecoder.decode({
        ...validPlan,
        agents: [
          {
            ...validPlan.agents[0],
            instructionWrites: [
              { ...validPlan.agents[0].instructionWrites[0], op: 'upsert' },
            ],
          },
        ],
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      userMirrorResultDecoder.decode({
        ...validResult,
        agents: [{ ...validResult.agents[0], state: 'ok' }],
      }),
    ).toThrow(ContractDecodeError);
  });

  test('rejects missing required fields and non-finite size', () => {
    const noHash = { ...validInventory } as Record<string, unknown>;
    delete noHash.inventorySnapshotHash;
    expect(() => userMirrorInventoryDecoder.decode(noHash)).toThrow(ContractDecodeError);

    expect(() =>
      userMirrorInventoryDecoder.decode({
        ...validInventory,
        agents: [
          {
            ...validAgentInventory,
            nativeFiles: [{ ...validNativeFile, size: Number.NaN }],
          },
        ],
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      userMirrorPlanDecoder.decode({
        ...validPlan,
        credentialBearingCount: Number.POSITIVE_INFINITY,
      }),
    ).toThrow(ContractDecodeError);
  });

  test('allows unknown extra fields for forward compatibility', () => {
    const withExtra = {
      ...validPlan,
      futureField: { nested: true },
      agents: [
        {
          ...validPlan.agents[0],
          experimentalBadge: 'beta',
        },
      ],
    };
    const decoded = userMirrorPlanDecoder.decode(withExtra);
    expect(decoded.planToken).toBe('plan-1');
    expect('futureField' in decoded).toBe(false);
    expect('experimentalBadge' in decoded.agents[0]).toBe(false);
  });

  test('MCP structs that contain token/value/secret keys fail decode', () => {
    for (const key of ['token', 'value', 'secret'] as const) {
      expect(() =>
        userMirrorMcpCredentialFactDecoder.decode({
          present: true,
          hash: 'abc123',
          [key]: 'sk-live-leaked',
        }),
      ).toThrow(ContractDecodeError);

      expect(() =>
        userMirrorPortableItemDecoder.decode({
          ...validMcpItem,
          [key]: 'sk-live-leaked',
        }),
      ).toThrow(ContractDecodeError);

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
                    hash: 'cred-hash-1',
                    [key]: 'sk-live-leaked',
                  },
                },
              ],
            },
          ],
        }),
      ).toThrow(ContractDecodeError);
    }
  });

  test('MCP credential typed DTO only exposes present/hash', () => {
    const decoded = userMirrorMcpCredentialFactDecoder.decode({
      present: true,
      hash: 'abc123',
      futureHint: 'ignored',
    });
    expect(decoded).toEqual({ present: true, hash: 'abc123' });
    expectTypeOf<UserMirrorMcpCredentialFactDto>().toEqualTypeOf<{
      present: boolean;
      hash: string | null;
    }>();
    expectTypeOf(decoded).not.toHaveProperty('secret');
    expectTypeOf(decoded).not.toHaveProperty('token');
    expectTypeOf(decoded).not.toHaveProperty('value');
  });

  test('decode errors do not leak secret payload', () => {
    try {
      userMirrorMcpCredentialFactDecoder.decode({
        present: true,
        hash: 'abc123',
        secret: 'must-not-appear',
      });
      expect.unreachable('should throw');
    } catch (err) {
      expect(err).toBeInstanceOf(ContractDecodeError);
      const message = String(err);
      expect(message).not.toContain('must-not-appear');
      expect(message).not.toContain('sk-');
    }
  });

  test('decoder output types match DTO aliases', () => {
    expectTypeOf(userMirrorInventoryDecoder.decode(validInventory)).toEqualTypeOf<UserMirrorInventoryDto>();
    expectTypeOf(userMirrorPlanDecoder.decode(validPlan)).toEqualTypeOf<UserMirrorPlanDto>();
    expectTypeOf(userMirrorResultDecoder.decode(validResult)).toEqualTypeOf<UserMirrorResultDto>();
  });

  test('plan decodes selection as missing, null, or a filter object', () => {
    // 缺字段（旧对端）：optional 字段不报错，解码为 undefined。
    const withoutSelection = userMirrorPlanDecoder.decode(validPlan);
    expect(withoutSelection.selection).toBeUndefined();

    // null（Rust Option 序列化缺省）：等价全量。
    const nullSelection = userMirrorPlanDecoder.decode({ ...validPlan, selection: null });
    expect(nullSelection.selection).toBeNull();

    // 对象：严格解码键列表。
    const withSelection = userMirrorPlanDecoder.decode({
      ...validPlan,
      selection: {
        includeInstructions: false,
        portableKeys: [{ kind: 'skill', nativeId: 'skill-a' }],
      },
    });
    expect(withSelection.selection).toEqual({
      includeInstructions: false,
      portableKeys: [{ kind: 'skill', nativeId: 'skill-a' }],
    });
  });

  test('selection filter decoder rejects bad enum or wrong key type', () => {
    expect(() =>
      userMirrorSelectionFilterDecoder.decode({
        includeInstructions: true,
        portableKeys: [{ kind: 'unknown-kind', nativeId: 'x' }],
      }),
    ).toThrow(ContractDecodeError);
    expect(() =>
      userMirrorSelectionFilterDecoder.decode({
        includeInstructions: true,
        portableKeys: [{ kind: 'skill', nativeId: 42 }],
      }),
    ).toThrow(ContractDecodeError);
  });
});
