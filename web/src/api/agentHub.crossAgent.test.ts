/**
 * @vitest-environment node
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn(async () => ({}));

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
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
});
