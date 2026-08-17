// @vitest-environment jsdom
/**
 * useCrossAgentAdaptController preview-only tests.
 *
 * Business Logic（为什么需要）:
 *   只允许本机用户级 selective preview；所有 apply/full 为零调用，旧响应不得串入新输入。
 *
 * Code Logic（做什么）:
 *   mock API，以 renderHook + deferred promise 验证 gate、严格响应匹配和 generation 失效。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import { useCrossAgentAdaptController } from './useCrossAgentAdaptController';

const previewMock = vi.fn();
const applyMock = vi.fn();
const previewFullMock = vi.fn();
const applyFullMock = vi.fn();
const inspectMock = vi.fn();

vi.mock('@/api/agentHub', () => ({
  agentHubApi: {
    previewCrossAgentInstruction: (...args: unknown[]) => previewMock(...args),
    applyCrossAgentInstruction: (...args: unknown[]) => applyMock(...args),
    previewCrossAgentFull: (...args: unknown[]) => previewFullMock(...args),
    applyCrossAgentFull: (...args: unknown[]) => applyFullMock(...args),
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
    instructionLane: 'common',
    adaptView: true,
    ...overrides,
  };
}

function previewResponse(source: AgentTarget, destinations: AgentTarget[]) {
  return {
    source,
    kind: 'instruction',
    needsAdaptation: false,
    planHash: `plan-${source}`,
    destinations: destinations.map((destination) => ({
      destination,
      mode: 'shared',
      path: `/tmp/${destination}/AGENTS.md`,
      renderedHash: 'rendered',
      observedHash: 'observed',
      unifiedDiff: '--- before\n+++ after',
      partialBlockers: ['CROSS_AGENT_PREVIEW_ONLY'],
      canApply: false,
    })),
  };
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe('useCrossAgentAdaptController', () => {
  beforeEach(() => {
    previewMock.mockReset();
    applyMock.mockReset();
    previewFullMock.mockReset();
    applyFullMock.mockReset();
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
      expect(result.current.destinationOptions).toEqual(['codex', 'opencode', 'grok', 'gemini', 'cursor', 'pi']);
    });
    act(() => result.current.toggleDestination('claude'));
    expect(result.current.destinations).not.toContain('claude');
    act(() => result.current.toggleDestination('codex'));
    expect(result.current.destinations).toEqual(['opencode', 'grok', 'gemini', 'cursor', 'pi']);
  });

  test('peer and project contexts are preview-blocked', async () => {
    const { result, rerender } = renderHook(
      ({ context }) =>
        useCrossAgentAdaptController({
          context,
          t,
          initialSourceMarkdown: 'Always run tests.',
        }),
      { initialProps: { context: localContext({ deviceId: 'peer-1' }) } },
    );

    expect(result.current.peerBlocked).toBe(true);
    await act(async () => result.current.runPreview());
    expect(result.current.error).toBe('agentHub:crossAgent.errors.peerBlocked');

    rerender({ context: localContext({ scope: 'project', projectKey: 'project-a' }) });
    await waitFor(() => expect(result.current.peerBlocked).toBe(false));
    await act(async () => result.current.runPreview());
    expect(result.current.error).toBe('agentHub:crossAgent.errors.projectBlocked');
    expect(previewMock).not.toHaveBeenCalled();
  });

  test('valid selective preview remains read-only', async () => {
    previewMock.mockResolvedValue(
      previewResponse('claude', ['codex', 'opencode', 'grok', 'gemini', 'cursor', 'pi']),
    );
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: 'Always run tests before commit.',
      }),
    );

    act(() => result.current.setScopeConfirmed(true));
    await act(async () => result.current.runPreview());

    expect(previewMock).toHaveBeenCalledWith(
      expect.objectContaining({
        source: 'claude',
        destinations: ['codex', 'opencode', 'grok', 'gemini', 'cursor', 'pi'],
        sourceMarkdown: 'Always run tests before commit.',
        scope: 'user',
      }),
    );
    expect(result.current.preview?.planHash).toBe('plan-claude');
    expect(result.current.applicableCount).toBe(0);
    expect(result.current.canApply).toBe(false);

    await act(async () => result.current.runApply());
    expect(result.current.error).toBe('agentHub:crossAgent.errors.applyUnavailable');
    expect(applyMock).not.toHaveBeenCalled();
    expect(previewFullMock).not.toHaveBeenCalled();
    expect(applyFullMock).not.toHaveBeenCalled();
  });

  test('rejects a preview whose source or destination set does not match request', async () => {
    previewMock.mockResolvedValue(previewResponse('codex', ['opencode']));
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: 'Body',
      }),
    );
    act(() => result.current.setScopeConfirmed(true));
    await act(async () => result.current.runPreview());
    expect(result.current.preview).toBeNull();
    expect(result.current.error).toBe('agentHub:crossAgent.errors.invalidPreview');
  });

  test('body and context changes discard deferred old preview responses', async () => {
    const first = deferred<unknown>();
    previewMock.mockReturnValueOnce(first.promise);
    const { result, rerender } = renderHook(
      ({ context, initialSourceMarkdown }) =>
        useCrossAgentAdaptController({ context, t, initialSourceMarkdown }),
      {
        initialProps: {
          context: localContext(),
          initialSourceMarkdown: 'Old body',
        },
      },
    );
    act(() => result.current.setScopeConfirmed(true));
    let firstOperation!: Promise<void>;
    act(() => {
      firstOperation = result.current.runPreview();
    });
    await waitFor(() => expect(previewMock).toHaveBeenCalledTimes(1));
    act(() => result.current.setSourceMarkdown('New body'));
    first.resolve(previewResponse('claude', ['codex', 'opencode']));
    await act(async () => firstOperation);
    expect(result.current.preview).toBeNull();

    const second = deferred<unknown>();
    previewMock.mockReturnValueOnce(second.promise);
    act(() => result.current.setScopeConfirmed(true));
    let secondOperation!: Promise<void>;
    act(() => {
      secondOperation = result.current.runPreview();
    });
    await waitFor(() => expect(previewMock).toHaveBeenCalledTimes(2));
    rerender({
      context: localContext({ agent: 'codex' }),
      initialSourceMarkdown: 'Codex body',
    });
    second.resolve(previewResponse('claude', ['codex', 'opencode']));
    await act(async () => secondOperation);
    expect(result.current.source).toBe('codex');
    expect(result.current.preview).toBeNull();
  });

  test('loads markdown from inspect when initial source is empty', async () => {
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

  test('destination changes do not strand an in-flight source reload', async () => {
    const sourceLoad = deferred<unknown>();
    inspectMock.mockReturnValueOnce(sourceLoad.promise);
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: '',
      }),
    );

    await waitFor(() => {
      expect(inspectMock).toHaveBeenCalledTimes(1);
      expect(result.current.contentLoading).toBe(true);
    });
    act(() => result.current.toggleDestination('codex'));
    expect(result.current.contentLoading).toBe(true);
    expect(result.current.canPreview).toBe(false);

    sourceLoad.resolve({
      targets: [],
      canonical: { commonContent: 'Fresh source body.', targetExtensions: {} },
    });
    await waitFor(() => {
      expect(result.current.contentLoading).toBe(false);
      expect(result.current.sourceMarkdown).toContain('Fresh source body');
    });
  });

  test('lane changes reset confirmation and reject old preview even when Agent is unchanged', async () => {
    const pending = deferred<unknown>();
    previewMock.mockReturnValueOnce(pending.promise);
    const { result, rerender } = renderHook(
      ({ context }) =>
        useCrossAgentAdaptController({
          context,
          t,
          initialSourceMarkdown: 'Common body',
        }),
      { initialProps: { context: localContext() } },
    );
    act(() => result.current.setScopeConfirmed(true));
    let operation!: Promise<void>;
    act(() => {
      operation = result.current.runPreview();
    });
    await waitFor(() => expect(previewMock).toHaveBeenCalledTimes(1));

    rerender({ context: localContext({ instructionLane: 'adapted' }) });
    await waitFor(() => expect(result.current.scopeConfirmed).toBe(false));
    pending.resolve(previewResponse('claude', ['codex', 'opencode']));
    await act(async () => operation);
    expect(result.current.preview).toBeNull();
  });

  test('full mode and every apply path remain unavailable', async () => {
    const { result } = renderHook(() =>
      useCrossAgentAdaptController({
        context: localContext(),
        t,
        initialSourceMarkdown: 'Body',
      }),
    );
    act(() => result.current.setMode('full'));
    expect(result.current.mode).toBe('selective');
    expect(result.current.error).toBe('agentHub:crossAgent.errors.fullUnavailable');
    await act(async () => result.current.runApply());
    expect(applyMock).not.toHaveBeenCalled();
    expect(previewFullMock).not.toHaveBeenCalled();
    expect(applyFullMock).not.toHaveBeenCalled();
  });
});
