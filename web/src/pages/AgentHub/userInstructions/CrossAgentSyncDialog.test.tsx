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

  it('shows a strict read-only preview and never exposes apply', async () => {
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
          observedHash: 'before',
          canApply: false,
        },
      ],
    });
    render(
      <CrossAgentSyncDialog
        t={t}
        open
        sourceMarkdown="Always run tests before commit."
        onClose={() => undefined}
      />,
    );

    expect(screen.getByTestId('cross-agent-sync-dialog')).toBeTruthy();
    expect(screen.getByTestId('cross-agent-preview-only')).toBeTruthy();
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

    expect(screen.queryByTestId('cross-agent-apply')).toBeNull();
    expect(applyMock).not.toHaveBeenCalled();
  });
});
