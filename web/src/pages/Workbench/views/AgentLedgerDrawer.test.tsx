// @vitest-environment jsdom
/**
 * AgentLedgerDrawer 视图单测。
 */
import type { ReactElement } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { AgentLedgerDrawer } from './AgentLedgerDrawer';
import type { AgentLedgerPage, AgentLedgerSummary } from '@/lib/types/agentLedger';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

const emptyPage: AgentLedgerPage = { items: [], nextCursor: null };

const summaryPartial: AgentLedgerSummary = {
  window: '7d',
  projectId: 'p1',
  sessions: 2,
  completed: 1,
  failed: 1,
  cancelled: 0,
  disconnected: 0,
  durationMs: 1000,
  inputTokens: null,
  outputTokens: null,
  costByCurrency: [{ currency: 'USD', minorUnits: 3 }],
  usageCoverage: 'partial',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要 i18n 上下文。
 *
 * Code Logic（这个函数做什么）:
 *   I18nextProvider 包裹 render。
 */
function renderDrawer(
  props: Partial<React.ComponentProps<typeof AgentLedgerDrawer>> = {},
): ReturnType<typeof render> {
  return render(
    (
      <I18nextProvider i18n={i18n}>
        <AgentLedgerDrawer
          open
          onClose={vi.fn()}
          localOnlyAvailable
          page={emptyPage}
          summary={summaryPartial}
          {...props}
        />
      </I18nextProvider>
    ) as ReactElement,
  );
}

describe('AgentLedgerDrawer', () => {
  it('renders unavailable usage as 未提供 rather than zero', () => {
    renderDrawer();
    expect(screen.getAllByText('未提供').length).toBeGreaterThan(0);
    expect(screen.queryByText('0 tokens')).toBeNull();
    expect(screen.getByTestId('ledger-input-tokens').textContent).toBe('未提供');
  });

  it('shows partial coverage label', () => {
    renderDrawer();
    expect(screen.getByText(/部分/)).toBeTruthy();
  });

  it('shows multi-currency without conversion', () => {
    renderDrawer();
    expect(screen.getByText(/USD:\s*3/)).toBeTruthy();
  });

  it('shows local-only message for remote projects', () => {
    renderDrawer({ localOnlyAvailable: false, page: null, summary: null });
    expect(screen.getByText(/仅本机/)).toBeTruthy();
  });

  it('does not render forbidden metadata text', () => {
    renderDrawer({
      page: {
        items: [
          {
            id: 'e1',
            agentSessionId: 'a1',
            projectId: 'p1',
            worktreeId: null,
            providerId: 'claudeCodeVisible',
            modelId: null,
            startedAt: '2026-07-15T00:00:00Z',
            endedAt: '2026-07-15T00:01:00Z',
            durationMs: 1000,
            outcome: 'completed',
            inputTokens: null,
            outputTokens: null,
            cacheReadTokens: null,
            cacheWriteTokens: null,
            costMinorUnits: null,
            costCurrency: null,
            createdAt: '2026-07-15T00:01:00Z',
            updatedAt: '2026-07-15T00:01:00Z',
          },
        ],
        nextCursor: null,
      },
    });
    // 明细列表不得展示 session 正文类字段；subtitle 可说明「不保存 Prompt」
    expect(screen.queryByText(/transcriptPath/i)).toBeNull();
    expect(screen.queryByText(/nativeSessionId/i)).toBeNull();
    expect(screen.queryByText(/credential/i)).toBeNull();
  });
});
