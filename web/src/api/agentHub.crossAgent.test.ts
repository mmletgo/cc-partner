/** @vitest-environment node */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn(async () => ({}));

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => invokeMock(...(args as [])),
  invokeDecoded: vi.fn(),
}));

import {
  agentHubApi,
  AGENT_HUB_COMMANDS,
  CROSS_AGENT_APPLY_NOT_CERTIFIED,
  CROSS_AGENT_DEST_EQUALS_SOURCE,
  CROSS_AGENT_DESTINATION_PATH_OVERRIDE_UNAVAILABLE,
  CROSS_AGENT_DESTINATIONS_DUPLICATE,
  CROSS_AGENT_DESTINATIONS_REQUIRED,
  CROSS_AGENT_FULL_ADAPT_UNAVAILABLE,
  CROSS_AGENT_PROJECT_SCOPE_UNAVAILABLE,
  CROSS_AGENT_SOURCE_MARKDOWN_REQUIRED,
  CROSS_AGENT_TARGET_INVALID,
} from './agentHub';

describe('agentHubApi cross-agent fail-closed envelope', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({});
  });

  it('invokes only valid local-user selective preview with fixed empty paths', async () => {
    await agentHubApi.previewCrossAgentInstruction({
      source: 'claude',
      destinations: ['codex'],
      sourceMarkdown: 'Always run tests.',
      scope: 'user',
    });
    expect(invokeMock).toHaveBeenCalledWith(
      AGENT_HUB_COMMANDS.previewCrossAgentInstruction,
      {
        request: {
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests.',
          scope: 'user',
          destinationPaths: {},
        },
      },
    );
  });

  it.each([
    [
      CROSS_AGENT_PROJECT_SCOPE_UNAVAILABLE,
      { source: 'claude', destinations: ['codex'], sourceMarkdown: 'Body', scope: 'project' },
    ],
    [
      CROSS_AGENT_DESTINATION_PATH_OVERRIDE_UNAVAILABLE,
      {
        source: 'claude',
        destinations: ['codex'],
        sourceMarkdown: 'Body',
        scope: 'user',
        destinationPaths: { codex: '/tmp/override' },
      },
    ],
    [
      CROSS_AGENT_TARGET_INVALID,
      { source: 'unknown', destinations: ['codex'], sourceMarkdown: 'Body', scope: 'user' },
    ],
    [
      CROSS_AGENT_DESTINATIONS_REQUIRED,
      { source: 'claude', destinations: [], sourceMarkdown: 'Body', scope: 'user' },
    ],
    [
      CROSS_AGENT_DESTINATIONS_DUPLICATE,
      {
        source: 'claude',
        destinations: ['codex', 'codex'],
        sourceMarkdown: 'Body',
        scope: 'user',
      },
    ],
    [
      CROSS_AGENT_DEST_EQUALS_SOURCE,
      { source: 'claude', destinations: ['claude'], sourceMarkdown: 'Body', scope: 'user' },
    ],
    [
      CROSS_AGENT_SOURCE_MARKDOWN_REQUIRED,
      { source: 'claude', destinations: ['codex'], sourceMarkdown: '  ', scope: 'user' },
    ],
  ])('rejects %s before invoke', async (code, request) => {
    await expect(agentHubApi.previewCrossAgentInstruction(request)).rejects.toMatchObject({
      code,
    });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects selective apply before invoke', async () => {
    await expect(
      agentHubApi.applyCrossAgentInstruction({
        source: 'claude',
        destinations: ['codex'],
        sourceMarkdown: 'Body',
        scope: 'user',
        planHash: 'plan-1',
        clientRequestId: 'request-1',
      }),
    ).rejects.toMatchObject({ code: CROSS_AGENT_APPLY_NOT_CERTIFIED });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('rejects full preview and full apply before invoke', async () => {
    await expect(
      agentHubApi.previewCrossAgentFull({
        source: 'claude',
        destination: 'codex',
        scope: 'user',
        sourceMarkdown: 'Body',
      }),
    ).rejects.toMatchObject({ code: CROSS_AGENT_FULL_ADAPT_UNAVAILABLE });
    await expect(
      agentHubApi.applyCrossAgentFull({
        source: 'claude',
        destination: 'codex',
        scope: 'user',
        sourceMarkdown: 'Body',
        planHash: 'plan-1',
        clientRequestId: 'request-1',
        items: [],
      }),
    ).rejects.toMatchObject({ code: CROSS_AGENT_FULL_ADAPT_UNAVAILABLE });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
