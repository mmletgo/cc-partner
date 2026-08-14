import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import { gamePluginListDecoder } from './gamePlugin';

const validList = {
  dir: '/tmp/plugins',
  games: [
    {
      id: 'snake',
      name: 'Snake',
      description: 's',
      entry: 'index.html',
      rewardMinutes: 5,
      playable: true,
      reason: null,
    },
  ],
};

describe('gamePlugin schemas', () => {
  test('decodes a plugin list', () => {
    const decoded = gamePluginListDecoder.decode(validList);
    expect(decoded.games[0]?.id).toBe('snake');
    expect(decoded.games[0]?.playable).toBe(true);
  });

  test('rejects missing playable', () => {
    const game = { ...validList.games[0] };
    delete (game as { playable?: boolean }).playable;
    expect(() =>
      gamePluginListDecoder.decode({ ...validList, games: [game] }),
    ).toThrow(ContractDecodeError);
  });
});
