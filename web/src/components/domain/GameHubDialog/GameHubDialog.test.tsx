// @vitest-environment jsdom

/**
 * GameHubDialog 大厅门槛与两态合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   词库未满 10 个已缓存生词时不能进；游戏中点遮罩不得退出。
 *
 * Code Logic（这个测试做什么）:
 *   mock wordgameApi；断言禁用开始按钮、门槛文案、以及 play 态 Dialog closeOnBackdrop=false。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { GameHubDialog } from './GameHubDialog';

const getHubStatus = vi.fn();
const startRound = vi.fn();
const retryPreheat = vi.fn();
const abandonRound = vi.fn();
const listPlugins = vi.fn();

vi.mock('@/api/wordgame', () => ({
  wordgameApi: {
    getHubStatus: (...args: unknown[]) => getHubStatus(...args),
    startRound: (...args: unknown[]) => startRound(...args),
    retryPreheat: (...args: unknown[]) => retryPreheat(...args),
    abandonRound: (...args: unknown[]) => abandonRound(...args),
    submitAnswer: vi.fn(),
  },
}));

vi.mock('@/api/gamePlugins', () => ({
  gamePluginsApi: {
    list: (...args: unknown[]) => listPlugins(...args),
    credit: vi.fn(),
  },
}));

vi.mock('@/hooks/useBattery', () => ({
  useBattery: () => ({
    snapshot: { mode: 'charging', remainingMs: 0 },
    loading: false,
    error: null,
    toast: null,
    setMode: async () => undefined,
    dismissToast: () => undefined,
  }),
}));

vi.mock('@/hooks/useTheme', () => ({
  useTheme: () => ({ theme: 'dark', setTheme: () => undefined, toggleTheme: () => undefined }),
}));

afterEach(() => {
  cleanup();
});

beforeEach(async () => {
  await i18n.changeLanguage('zh');
  getHubStatus.mockReset();
  startRound.mockReset();
  retryPreheat.mockReset();
  abandonRound.mockReset();
  listPlugins.mockReset();
  listPlugins.mockResolvedValue({ dir: '/tmp/plugins', games: [] });
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

describe('GameHubDialog', () => {
  test('disables play when fewer than 10 cached new words', async () => {
    getHubStatus.mockResolvedValue({
      unfamiliarCount: 3,
      cachedUnfamiliarCount: 3,
      canEnter: false,
      requiredCached: 10,
      preheatStatus: 'generating',
      preheatLemma: 'feature',
      preheatError: null,
      remoteHint: null,
    });

    render(
      <I18nextProvider i18n={i18n}>
        <GameHubDialog open onClose={() => undefined} />
      </I18nextProvider>,
    );

    await waitFor(() => {
      expect(screen.getByText(/正在为「feature」生成题目/)).toBeTruthy();
    });
    expect((screen.getByRole('button', { name: '开始' }) as HTMLButtonElement).disabled).toBe(true);
    expect(startRound).not.toHaveBeenCalled();
    const backdrop = screen
      .getByRole('dialog')
      .parentElement!.querySelector('[data-dialog-backdrop]') as HTMLElement;
    expect(backdrop.getAttribute('data-backdrop-variant')).toBe('scrim');
  });

  test('starts a round when the cache threshold is met', async () => {
    getHubStatus.mockResolvedValue({
      unfamiliarCount: 12,
      cachedUnfamiliarCount: 10,
      canEnter: true,
      requiredCached: 10,
      preheatStatus: 'ready',
      preheatLemma: null,
      preheatError: null,
      remoteHint: null,
    });
    startRound.mockResolvedValue({
      lemma: 'feature',
      questionType: 'enToZh',
      kind: 'choice',
      prompt: 'feature 的中文是？',
      options: ['特性', '失败'],
    });

    render(
      <I18nextProvider i18n={i18n}>
        <GameHubDialog open onClose={() => undefined} />
      </I18nextProvider>,
    );

    await waitFor(() => {
      expect((screen.getByRole('button', { name: '开始' }) as HTMLButtonElement).disabled).toBe(
        false,
      );
    });
    fireEvent.click(screen.getByRole('button', { name: '开始' }));
    await waitFor(() => {
      expect(screen.getByTestId('wordgame-play')).toBeTruthy();
    });
    expect(screen.getByText('feature')).toBeTruthy();
  });

  test('lists plugin games and copies the spec', async () => {
    getHubStatus.mockResolvedValue({
      unfamiliarCount: 12,
      cachedUnfamiliarCount: 10,
      canEnter: true,
      requiredCached: 10,
      preheatStatus: 'ready',
      preheatLemma: null,
      preheatError: null,
      remoteHint: null,
    });
    listPlugins.mockResolvedValue({
      dir: '/tmp/plugins',
      games: [
        {
          id: 'snake',
          name: 'Snake',
          description: 'arcade',
          entry: 'index.html',
          rewardMinutes: 5,
          playable: true,
          reason: null,
        },
        {
          id: 'tower',
          name: 'Tower',
          description: 'td',
          entry: 'dist/index.html',
          rewardMinutes: 0,
          playable: false,
          reason: 'missing_entry',
        },
      ],
    });

    render(
      <I18nextProvider i18n={i18n}>
        <GameHubDialog open onClose={() => undefined} />
      </I18nextProvider>,
    );

    await waitFor(() => {
      expect(screen.getByText('Snake')).toBeTruthy();
    });
    expect(screen.getByText('Tower')).toBeTruthy();
    const buttons = screen.getAllByRole('button', { name: '开始' }) as HTMLButtonElement[];
    const towerBtn = buttons.find((btn) => btn.closest('article')?.textContent?.includes('Tower'));
    expect(towerBtn?.disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: '复制规范' }));
    await waitFor(() => {
      expect(navigator.clipboard.writeText).toHaveBeenCalled();
    });
    const copied = vi.mocked(navigator.clipboard.writeText).mock.calls[0]?.[0] ?? '';
    expect(copied).toContain('game.json');
    const snakeBtn = buttons.find((btn) => btn.closest('article')?.textContent?.includes('Snake'));
    fireEvent.click(snakeBtn as HTMLButtonElement);
    await waitFor(() => {
      expect(screen.getByTestId('game-plugin-player')).toBeTruthy();
    });
  });
});
