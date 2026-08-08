// @vitest-environment jsdom
/**
 * CrossAgentAdaptPage smoke tests.
 *
 * Business Logic: 独立页渲染源/目标/scope/内容/preview 闸门；peer 展示 blocked。
 * Code Logic: mock API + i18n；assert testids。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { DEFAULT_AGENT_HUB_CONTEXT } from '../context/agentHubContext';
import { CrossAgentAdaptPage } from './CrossAgentAdaptPage';

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

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

beforeEach(() => {
  previewMock.mockReset();
  applyMock.mockReset();
  previewFullMock.mockReset();
  applyFullMock.mockReset();
  inspectMock.mockReset();
  inspectMock.mockResolvedValue({
    targets: [],
    canonical: { commonContent: '', targetExtensions: {} },
  });
});

afterEach(() => {
  cleanup();
});

describe('CrossAgentAdaptPage', () => {
  test('renders selective adapt sections and blocks apply without preview', async () => {
    const onExit = vi.fn();
    render(
      <I18nextProvider i18n={i18n}>
        <CrossAgentAdaptPage
          context={{ ...DEFAULT_AGENT_HUB_CONTEXT, adaptView: true }}
          initialSourceMarkdown="Always run tests."
          onExit={onExit}
        />
      </I18nextProvider>,
    );

    expect(screen.getByTestId('cross-agent-adapt-page')).toBeTruthy();
    expect(screen.getByTestId('cross-agent-adapt-source').textContent).toMatch(/Claude/i);
    expect(screen.getByTestId('cross-agent-adapt-dest-codex')).toBeTruthy();
    expect(screen.queryByTestId('cross-agent-adapt-dest-claude')).toBeNull();

    const applyBtn = screen.getByTestId('cross-agent-adapt-apply') as HTMLButtonElement;
    expect(applyBtn.disabled).toBe(true);

    fireEvent.click(screen.getByTestId('cross-agent-adapt-scope-confirm'));
    const previewBtn = screen.getByTestId('cross-agent-adapt-preview') as HTMLButtonElement;
    await waitFor(() => {
      expect(previewBtn.disabled).toBe(false);
    });

    fireEvent.click(screen.getByTestId('cross-agent-adapt-back'));
    expect(onExit).toHaveBeenCalled();
  });

  test('peer context shows blocked banner and disables actions', () => {
    render(
      <I18nextProvider i18n={i18n}>
        <CrossAgentAdaptPage
          context={{
            ...DEFAULT_AGENT_HUB_CONTEXT,
            adaptView: true,
            deviceId: 'peer-1',
          }}
          initialSourceMarkdown="Always run tests."
          onExit={() => undefined}
        />
      </I18nextProvider>,
    );

    expect(screen.getByTestId('cross-agent-adapt-peer-blocked')).toBeTruthy();
    expect((screen.getByTestId('cross-agent-adapt-preview') as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  test('mode toggle switches to full single-destination UI', async () => {
    render(
      <I18nextProvider i18n={i18n}>
        <CrossAgentAdaptPage
          context={{ ...DEFAULT_AGENT_HUB_CONTEXT, adaptView: true }}
          initialSourceMarkdown="Always run tests."
          onExit={() => undefined}
        />
      </I18nextProvider>,
    );

    expect(screen.getByTestId('cross-agent-adapt-mode-selective')).toBeTruthy();
    fireEvent.click(screen.getByTestId('cross-agent-adapt-mode-full'));
    await waitFor(() => {
      expect(screen.getByTestId('cross-agent-adapt-full-destination')).toBeTruthy();
      expect(screen.getByTestId('cross-agent-adapt-full-dest-codex')).toBeTruthy();
    });
    expect(screen.queryByTestId('cross-agent-adapt-destinations')).toBeNull();
  });
});
