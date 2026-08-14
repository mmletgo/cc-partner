/**
 * 记单词 IPC 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   大厅与闪卡 DTO 必须在写入 UI 前 fail-closed，拒绝损坏或混合版本结构。
 *
 * Code Logic（这个模块做什么）:
 *   严格 decoder：题型枚举、非负计数、可空字符串。
 */

import type {
  WordgameCard,
  WordgameHubStatus,
  WordgameQuestionKind,
  WordgameQuestionType,
  WordgameSubmitResult,
} from '../types/wordgame';
import {
  arrayDecoder,
  booleanDecoder,
  defineDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';
import { ContractDecodeError } from '../runtimeSchema';

/**
 * Business Logic（为什么需要这个函数）:
 *   生词数 / 缓存数不得为负，否则大厅门槛会误开或永远灰掉。
 *
 * Code Logic（这个函数做什么）:
 *   有限非负整数。
 */
const nonNegativeIntDecoder: Decoder<number> = defineDecoder(
  'WordgameNonNegativeInt',
  (value, path = '$') => {
    const n = numberDecoder.decode(value, path);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
      throw new ContractDecodeError('WordgameNonNegativeInt', path, 'primitive');
    }
    return n;
  },
);

export const wordgameQuestionTypeDecoder: Decoder<WordgameQuestionType> = enumDecoder(
  'WordgameQuestionType',
  [
    'enToZh',
    'zhToEn',
    'chooseGloss',
    'cloze',
    'synonym',
    'collocation',
    'errorCorrection',
  ] as const,
);

export const wordgameQuestionKindDecoder: Decoder<WordgameQuestionKind> = enumDecoder(
  'WordgameQuestionKind',
  ['choice', 'input'] as const,
);

export const wordgameCardDecoder: Decoder<WordgameCard> = objectDecoder('WordgameCard', {
  lemma: stringDecoder,
  questionType: wordgameQuestionTypeDecoder,
  kind: wordgameQuestionKindDecoder,
  prompt: stringDecoder,
  options: arrayDecoder(stringDecoder),
});

export const wordgameHubStatusDecoder: Decoder<WordgameHubStatus> = objectDecoder(
  'WordgameHubStatus',
  {
    unfamiliarCount: nonNegativeIntDecoder,
    cachedUnfamiliarCount: nonNegativeIntDecoder,
    canEnter: booleanDecoder,
    requiredCached: nonNegativeIntDecoder,
    preheatStatus: stringDecoder,
    preheatLemma: nullableDecoder(stringDecoder),
    preheatError: nullableDecoder(stringDecoder),
    remoteHint: nullableDecoder(stringDecoder),
  },
);

export const wordgameSubmitResultDecoder: Decoder<WordgameSubmitResult> = objectDecoder(
  'WordgameSubmitResult',
  {
    correct: booleanDecoder,
    expected: stringDecoder,
    familiar: booleanDecoder,
    next: nullableDecoder(wordgameCardDecoder),
    done: booleanDecoder,
  },
);
