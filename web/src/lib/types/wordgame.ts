/**
 * 记单词 / Game Hub 前端类型（与 Rust DTO camelCase 对齐）。
 *
 * Business Logic（为什么需要这个模块）:
 *   大厅门槛、闪卡题型和提交结果必须与后端命令面一致，避免字符串漂移。
 *
 * Code Logic（这个模块做什么）:
 *   定义题型、大厅状态、闪卡与提交结果；不含原文或路径。
 */

/** 七种题型 wire token。 */
export type WordgameQuestionType =
  | 'enToZh'
  | 'zhToEn'
  | 'chooseGloss'
  | 'cloze'
  | 'synonym'
  | 'collocation'
  | 'errorCorrection';

/** 前端渲染分支。 */
export type WordgameQuestionKind = 'choice' | 'input';

/**
 * 大厅门槛与预热状态。
 */
export interface WordgameHubStatus {
  unfamiliarCount: number;
  cachedUnfamiliarCount: number;
  canEnter: boolean;
  requiredCached: number;
  preheatStatus: string;
  preheatLemma: string | null;
  preheatError: string | null;
  remoteHint: string | null;
}

/**
 * 一张闪卡（不含标准答案）。
 */
export interface WordgameCard {
  lemma: string;
  questionType: WordgameQuestionType;
  kind: WordgameQuestionKind;
  prompt: string;
  options: string[];
}

/**
 * 提交答案后的反馈。
 */
export interface WordgameSubmitResult {
  correct: boolean;
  expected: string;
  familiar: boolean;
  next: WordgameCard | null;
  done: boolean;
}
