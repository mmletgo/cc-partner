/**
 * 记单词 decoder 合同。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import { wordgameHubStatusDecoder } from './wordgame';

describe('wordgame schemas', () => {
  test('accepts a complete hub status', () => {
    const status = wordgameHubStatusDecoder.decode(
      {
        unfamiliarCount: 12,
        cachedUnfamiliarCount: 10,
        canEnter: true,
        requiredCached: 10,
        preheatStatus: 'ready',
        preheatLemma: null,
        preheatError: null,
        remoteHint: null,
      },
      '$',
    );
    expect(status.canEnter).toBe(true);
  });

  test('rejects a negative cached count', () => {
    expect(() =>
      wordgameHubStatusDecoder.decode(
        {
          unfamiliarCount: 1,
          cachedUnfamiliarCount: -1,
          canEnter: false,
          requiredCached: 10,
          preheatStatus: 'idle',
          preheatLemma: null,
          preheatError: null,
          remoteHint: null,
        },
        '$',
      ),
    ).toThrow(ContractDecodeError);
  });
});
