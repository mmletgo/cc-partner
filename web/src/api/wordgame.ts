/**
 * 记单词 API — 通过 Tauri invoke 调用本机词库与判题命令。
 *
 * Business Logic（为什么需要这个模块）:
 *   GameHub / 闪卡只通过 invoke 读写本机词库；判题必须在后端完成。
 *
 * Code Logic（这个模块做什么）:
 *   封装 5 个命令，成功 body 经 decoder fail-closed。
 */

import { invoke, invokeDecoded } from './client';
import {
  wordgameCardDecoder,
  wordgameHubStatusDecoder,
  wordgameSubmitResultDecoder,
} from '@/lib/schemas/wordgame';
import type {
  WordgameCard,
  WordgameHubStatus,
  WordgameQuestionType,
  WordgameSubmitResult,
} from '@/lib/types/wordgame';

export const wordgameApi = {
  /** 读取大厅门槛与预热状态。 */
  getHubStatus: () =>
    invokeDecoded('get_wordgame_hub_status', undefined, wordgameHubStatusDecoder),

  /** 重试当前卡住的预热词。 */
  retryPreheat: () =>
    invokeDecoded('retry_wordgame_preheat', undefined, wordgameHubStatusDecoder),

  /** 校验门槛后抽出第一张到期卡。 */
  startRound: () => invokeDecoded('start_wordgame_round', undefined, wordgameCardDecoder),

  /** 提交答案并由后端判题。 */
  submitAnswer: (lemma: string, questionType: WordgameQuestionType, answer: string) =>
    invokeDecoded(
      'submit_wordgame_answer',
      { req: { lemma, questionType, answer } },
      wordgameSubmitResultDecoder,
    ),

  /** 前端关闭游戏面；进度已在 submit 时落库。 */
  abandonRound: () => invoke<void>('abandon_wordgame_round'),
};

export type { WordgameCard, WordgameHubStatus, WordgameSubmitResult };
