import { describe, expect, test } from 'vitest';
import { gamePluginSpecPrompt } from './gamePluginSpecPrompt';

describe('gamePluginSpecPrompt', () => {
  test('zh prompt covers contract keywords', () => {
    const text = gamePluginSpecPrompt('zh');
    expect(text).toContain('game.json');
    expect(text).toContain('cc-partner:host');
    expect(text).toContain('complete');
    expect(text).toContain('rewardMinutes');
    expect(text).toContain('dist/index.html');
    expect(text).toMatch(/不.*invoke|不要调用 Tauri invoke/);
  });

  test('en prompt covers contract keywords', () => {
    const text = gamePluginSpecPrompt('en');
    expect(text).toContain('game.json');
    expect(text).toContain('cc-partner:host');
    expect(text).toContain('complete');
    expect(text).toContain('rewardMinutes');
    expect(text).toContain('dist/index.html');
    expect(text.toLowerCase()).toContain('must not');
    expect(text).toContain('invoke');
  });
});
