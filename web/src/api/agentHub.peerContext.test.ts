/**
 * Agent Hub API peer/project context 门闩测试。
 *
 * Business Logic: peer device 与 remote project 不得静默落到本机 inspect/write。
 * Code Logic: mock invoke；assertLocal 在 invoke 前 fail-closed。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { Decoder } from '@/lib/runtimeSchema';

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
    return new Error(String(reason));
  },
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openPath: vi.fn(),
}));

import {
  AGENT_HUB_PEER_CONTEXT_UNAVAILABLE,
  AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE,
  agentHubApi,
  requiresPeerAgentHubPath,
} from './agentHub';

describe('agentHubApi peer context', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('requiresPeerAgentHubPath treats remote project as peer path', () => {
    expect(requiresPeerAgentHubPath({ deviceId: null, projectRef: 'wb-1' })).toBe(false);
    expect(
      requiresPeerAgentHubPath({ deviceId: null, projectRef: 'remote:dev:inner' }),
    ).toBe(true);
    expect(requiresPeerAgentHubPath({ deviceId: 'peer-1' })).toBe(true);
  });

  test('inspectUserInstructionWorkspace local path still invokes command', async () => {
    // 最小合法 workspace body 会 fail decoder；这里只验证未抛 peer 且调用了 invoke
    mockInvoke.mockRejectedValueOnce(new Error('decode-not-under-test'));
    await expect(
      agentHubApi.inspectUserInstructionWorkspace({ deviceId: null, projectRef: null }),
    ).rejects.toThrow('decode-not-under-test');
    expect(mockInvoke).toHaveBeenCalledWith(
      'agent_hub_inspect_user_instruction_workspace',
      undefined,
    );
  });

  test('inspectUserInstructionWorkspace peer device fails closed without invoke', async () => {
    let thrown: unknown;
    try {
      await agentHubApi.inspectUserInstructionWorkspace({ deviceId: 'peer-1', projectRef: null });
    } catch (reason) {
      thrown = reason;
    }
    expect(thrown).toBeInstanceOf(Error);
    expect((thrown as Error).message).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('inspectUserInstructionWorkspace local project scope fails closed without user fallback', async () => {
    let thrown: unknown;
    try {
      await agentHubApi.inspectUserInstructionWorkspace({
        deviceId: null,
        projectRef: 'workbench-local-project',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(
      AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE,
    );
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('applyUserInstructionPlan peer context fails closed without local write', async () => {
    let thrown: unknown;
    try {
      await agentHubApi.applyUserInstructionPlan({
        planToken: 'p1',
        clientRequestId: 'c1',
        deviceId: 'peer-1',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  test('previewUserInstructionUpdate remote project fails closed', async () => {
    let thrown: unknown;
    try {
      await agentHubApi.previewUserInstructionUpdate({
        commonContent: 'x',
        targetExtensions: {},
        targetSelections: {
          claude: 'managed',
          codex: 'unmanaged',
          opencode: 'unmanaged',
          grok: 'unmanaged',
          gemini: 'unmanaged',
          cursor: 'unmanaged',
          pi: 'unmanaged',
        },
        baseRevisionId: null,
        inventorySnapshotHash: 'h1',
        projectRef: 'remote:dev:inner',
      });
    } catch (reason) {
      thrown = reason;
    }
    expect((thrown as Error & { code?: string }).code).toBe(AGENT_HUB_PEER_CONTEXT_UNAVAILABLE);
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
