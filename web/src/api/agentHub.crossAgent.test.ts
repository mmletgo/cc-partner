/**
 * @vitest-environment node
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn(async () => ({}));

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => invokeMock(...(args as [])),
  invokeDecoded: vi.fn(),
}));

import { agentHubApi, AGENT_HUB_COMMANDS } from './agentHub';

describe('agentHubApi cross-agent IPC envelope', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({});
  });

  it('always sends destinationPaths on preview even when caller omits it', async () => {
    await agentHubApi.previewCrossAgentInstruction({
      source: 'claude',
      destinations: ['codex'],
      sourceMarkdown: 'Always run tests.',
    });

    expect(invokeMock).toHaveBeenCalledWith(
      AGENT_HUB_COMMANDS.previewCrossAgentInstruction,
      {
        request: {
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests.',
          destinationPaths: {},
        },
      },
    );
  });

  it('always sends destinationPaths on apply even when caller omits it', async () => {
    await agentHubApi.applyCrossAgentInstruction({
      source: 'claude',
      destinations: ['codex'],
      sourceMarkdown: 'Always run tests.',
      clientRequestId: 'req-1',
    });

    expect(invokeMock).toHaveBeenCalledWith(
      AGENT_HUB_COMMANDS.applyCrossAgentInstruction,
      {
        request: {
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests.',
          destinationPaths: {},
          clientRequestId: 'req-1',
        },
      },
    );
  });

  it('previewCrossAgentFull always sends portableAssets and deviceId defaults', async () => {
    await agentHubApi.previewCrossAgentFull({
      source: 'claude',
      destination: 'codex',
      scope: 'user',
      sourceMarkdown: 'Always run tests.',
    });

    expect(invokeMock).toHaveBeenCalledWith(AGENT_HUB_COMMANDS.previewCrossAgentFull, {
      request: {
        source: 'claude',
        destination: 'codex',
        scope: 'user',
        sourceMarkdown: 'Always run tests.',
        portableAssets: [],
        deviceId: null,
      },
    });
  });

  it('applyCrossAgentFull sends planHash and item selections', async () => {
    await agentHubApi.applyCrossAgentFull({
      source: 'claude',
      destination: 'codex',
      scope: 'user',
      sourceMarkdown: 'Always run tests.',
      planHash: 'abc123',
      clientRequestId: 'req-full-1',
      items: [{ logicalKey: 'instruction:user', included: true }],
    });

    expect(invokeMock).toHaveBeenCalledWith(AGENT_HUB_COMMANDS.applyCrossAgentFull, {
      request: {
        source: 'claude',
        destination: 'codex',
        scope: 'user',
        sourceMarkdown: 'Always run tests.',
        planHash: 'abc123',
        clientRequestId: 'req-full-1',
        items: [{ logicalKey: 'instruction:user', included: true }],
        portableAssets: [],
        deviceId: null,
      },
    });
  });
});
