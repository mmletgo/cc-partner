/**
 * @vitest-environment jsdom
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { CrossAgentSyncDialog } from './CrossAgentSyncDialog';

const previewMock = vi.fn();
const applyMock = vi.fn();

vi.mock('@/api/agentHub', () => ({
  agentHubApi: {
    previewCrossAgentInstruction: (...args: unknown[]) => previewMock(...args),
    applyCrossAgentInstruction: (...args: unknown[]) => applyMock(...args),
  },
}));

const t = ((key: string, opts?: Record<string, unknown>) => {
  if (opts?.count != null) return `${key}:${opts.count}`;
  if (opts?.defaultValue) return String(opts.defaultValue);
  return key;
}) as never;

describe('CrossAgentSyncDialog', () => {
  beforeEach(() => {
    previewMock.mockReset();
    applyMock.mockReset();
  });

  it('previews then applies one-shot cross-agent write via shipped API', async () => {
    previewMock.mockResolvedValue({
      source: 'claude',
      kind: 'instruction',
      needsAdaptation: false,
      planHash: 'plan-1',
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

    render(
      <CrossAgentSyncDialog
        t={t}
        open
        sourceMarkdown="Always run tests before commit."
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('cross-agent-sync-dialog')).toBeTruthy();
    fireEvent.click(screen.getByTestId('cross-agent-preview'));

    await waitFor(() => {
      expect(previewMock).toHaveBeenCalledWith(
        expect.objectContaining({
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests before commit.',
          scope: 'user',
          destinationPaths: {},
        }),
      );
    });

    await waitFor(() => {
      expect(screen.getByTestId('cross-agent-preview-codex')).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId('cross-agent-apply'));

    await waitFor(() => {
      expect(applyMock).toHaveBeenCalledWith(
        expect.objectContaining({
          source: 'claude',
          destinations: ['codex'],
          sourceMarkdown: 'Always run tests before commit.',
          scope: 'user',
          destinationPaths: {},
          planHash: 'plan-1',
        }),
      );
    });

    await waitFor(() => {
      expect(screen.getByTestId('cross-agent-apply-result').textContent).toMatch(/applied/);
    });
  });
});
