// @vitest-environment jsdom
/**
 * useCrossAgentAdaptController tests.
 *
 * Business Logic（为什么需要）:
 *   不能把源选为目标；peer 上下文 blocked；必须 preview 后才能 apply。
 *
 * Code Logic（做什么）:
 *   mock agentHubApi；renderHook + act/waitFor 验证闸门与 API 调用。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { AgentHubContext } from '../context/agentHubContext';
import { useCrossAgentAdaptController } from './useCrossAgentAdaptController';

const previewMock = vi.fn();
const applyMock = vi.fn();
const inspectMock = vi.fn();

vi.mock('@/api/agentHub', () => ({
  agentHubApi: {
    previewCrossAgentInstruction: (...args: unknown[]) => previewMock(...args),
    applyCrossAgentInstruction: (...args: unknown[]) => applyMock(...args),
    inspectUserInstructionWorkspace: (...args: unknown[]) => inspectMock(...args),
  },
}));

const t = ((key: string) => key) as never;

function localContext(overrides: Partial<AgentHubContext> = {}): AgentHubContext {
  return {
    agent: 'claude',
    scope: 'user',
    deviceId: null,
    projectKey: null,
    tab: 'instructions',
    adaptView: true,
    ...overrides,
  };
}

describe('useCrossAgentAdaptController', () => {
  beforeEach(() => {
    previewMock.mockReset();
    applyMock.mockReset();
    inspectMock.mockReset();
    inspectMock.mockResolvedValue({
      targets: [],
      canonical: { commonContent: 'From inspect body.', targetExtensions: {} },
    });
  });

  test('cannot select source as destination', async () => {
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: 'Always run tests.',
      }),
    );

    await waitFor(() => {
      expect(result.current.destinationOptions).toEqual(['codex', 'opencode']);
    });

    act(() => {
      result.current.toggleDestination('claude');
    });
    expect(result.current.destinations).not.toContain('claude');

    act(() => {
      result.current.toggleDestination('codex');
    });
    // default already includes codex; toggle off
    expect(result.current.destinations).not.toContain('codex');
    expect(result.current.destinations).toContain('opencode');
  });

  test('peer context is blocked for preview and apply', async () => {
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext({ deviceId: 'peer-1' }),
        t,
        initialSourceMarkdown: 'Always run tests.',
      }),
    );

    await waitFor(() => {
      expect(result.current.peerBlocked).toBe(true);
    });
    expect(result.current.canPreview).toBe(false);
    expect(result.current.canApply).toBe(false);

    await act(async () => {
      await result.current.runPreview();
    });
    expect(previewMock).not.toHaveBeenCalled();
    expect(result.current.error).toBe('agentHub:crossAgent.errors.peerBlocked');

    await act(async () => {
      await result.current.runApply();
    });
    expect(applyMock).not.toHaveBeenCalled();
  });

  test('preview required before apply; then applies only canApply destinations', async () => {
    previewMock.mockResolvedValue({
      source: 'claude',
      kind: 'instruction',
      needsAdaptation: false,
      destinations: [
        {
          destination: 'codex',
          mode: 'shared',
          path: '/tmp/.codex/AGENTS.md',
          renderedHash: 'abc',
          unifiedDiff: 'diff',
          partialBlockers: [],
          canApply: true,
        },
        {
          destination: 'opencode',
          mode: 'residual',
          path: '/tmp/.config/opencode/AGENTS.md',
          partialBlockers: ['residual'],
          canApply: false,
        },
      ],
    });
    applyMock.mockResolvedValue([
      {
        destination: 'codex',
        status: 'applied',
        path: '/tmp/.codex/AGENTS.md',
        errorCode: null,
      },
    ]);

    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: 'Always run tests before commit.',
      }),
    );

    // scope not confirmed → cannot preview
    expect(result.current.canPreview).toBe(false);
    await act(async () => {
      await result.current.runApply();
    });
    expect(applyMock).not.toHaveBeenCalled();
    expect(result.current.error).toBe('agentHub:crossAgent.errors.previewRequired');

    act(() => {
      result.current.setScopeConfirmed(true);
    });
    expect(result.current.canPreview).toBe(true);

    await act(async () => {
      await result.current.runPreview();
    });

    await waitFor(() => {
      expect(previewMock).toHaveBeenCalledWith(
        expect.objectContaining({
          source: 'claude',
          destinations: ['codex', 'opencode'],
          sourceMarkdown: 'Always run tests before commit.',
        }),
      );
      expect(result.current.preview).not.toBeNull();
    });

    expect(result.current.applicableCount).toBe(1);
    expect(result.current.canApply).toBe(true);

    await act(async () => {
      await result.current.runApply();
    });

    await waitFor(() => {
      expect(applyMock).toHaveBeenCalledWith(
        expect.objectContaining({
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests before commit.',
        }),
      );
      expect(result.current.applyResults?.[0]?.status).toBe('applied');
    });
  });

  test('loads markdown from inspect when initialSourceMarkdown empty', async () => {
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: '',
      }),
    );

    await waitFor(() => {
      expect(inspectMock).toHaveBeenCalled();
      expect(result.current.sourceMarkdown).toContain('From inspect body');
    });
  });
});
