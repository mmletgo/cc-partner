// @vitest-environment jsdom

import { afterEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { GamePluginPlayer } from './GamePluginPlayer';
import type { GamePluginSummary } from '@/lib/types/gamePlugin';

const game: GamePluginSummary = {
  id: 'snake',
  name: 'Snake',
  description: 's',
  entry: 'index.html',
  rewardMinutes: 5,
  playable: true,
  reason: null,
};

afterEach(() => {
  cleanup();
});

describe('GamePluginPlayer', () => {
  test('uses a sandboxed gameplugin iframe', () => {
    render(
      <I18nextProvider i18n={i18n}>
        <GamePluginPlayer
          game={game}
          theme="dark"
          batteryMode="charging"
          remainingMs={0}
          locale="zh"
          onBack={() => undefined}
          onCredit={async () => undefined}
        />
      </I18nextProvider>,
    );
    const frame = document.querySelector('iframe') as HTMLIFrameElement;
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts allow-pointer-lock');
    expect(frame.getAttribute('sandbox')).not.toContain('allow-same-origin');
    expect(frame.src).toContain('gameplugin://localhost/snake/');
  });

  test('credits only messages from this iframe', () => {
    const onCredit = vi.fn(async () => undefined);
    render(
      <I18nextProvider i18n={i18n}>
        <GamePluginPlayer
          game={game}
          theme="dark"
          batteryMode="charging"
          remainingMs={0}
          locale="zh"
          onBack={() => undefined}
          onCredit={onCredit}
        />
      </I18nextProvider>,
    );
    const frame = document.querySelector('iframe') as HTMLIFrameElement;
    fireEvent(
      window,
      new MessageEvent('message', {
        data: { type: 'cc-partner:game', action: 'complete' },
        source: frame.contentWindow,
      }),
    );
    expect(onCredit).toHaveBeenCalledTimes(1);
    fireEvent(
      window,
      new MessageEvent('message', {
        data: { type: 'cc-partner:game', action: 'complete' },
        source: window,
      }),
    );
    expect(onCredit).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('game-plugin-player')).toBeTruthy();
  });
});
