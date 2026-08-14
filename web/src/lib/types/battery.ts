/**
 * 充电模式快照 / 额度 / 流水类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   footer、设置页与遮罩必须共用同一份 camelCase DTO，避免各处手写结构漂移。
 *
 * Code Logic（这个模块做什么）:
 *   对齐 Rust BatterySnapshotDto / BatteryConfig / BatteryLedgerItemDto。
 */

/** 工作台自我约束模式。 */
export type BatteryMode = 'charging' | 'unlimited';

/** 入账来源 token（toast / 流水 note）。 */
export type BatteryCreditSource =
  | 'water'
  | 'rest'
  | 'kegel'
  | 'custom'
  | 'flashcard'
  | 'welcome'
  | 'game-plugin';

/** 账本流水 kind。 */
export type BatteryLedgerKind =
  | 'credit_health'
  | 'credit_wordgame'
  | 'credit_game_plugin'
  | 'credit_welcome'
  | 'daily_reset'
  | 'debit_tick'
  | 'mode_change';

/** 权威充电快照。 */
export interface BatterySnapshot {
  mode: BatteryMode;
  remainingMs: number;
  maxBalanceMs: number;
  todayEarnedMs: number;
  todaySpentMs: number;
  consuming: boolean;
  creditMinutes?: number;
  creditSource?: BatteryCreditSource;
}

/** 各来源一次入账分钟。 */
export interface BatteryRewards {
  waterMinutes: number;
  restMinutes: number;
  kegelMinutes: number;
  customMinutes: number;
  flashcardMinutes: number;
}

/** 各来源每日次数上限。 */
export interface BatteryDailyCaps {
  water: number;
  rest: number;
  kegel: number;
  custom: number;
  flashcard: number;
}

/** config.json 里的可调额度。 */
export interface BatteryConfig {
  rewards: BatteryRewards;
  dailyCaps: BatteryDailyCaps;
  maxBalanceMinutes: number;
  welcomeGrantMinutes: number;
}

/** 流水行。 */
export interface BatteryLedgerItem {
  id: number;
  ts: number;
  kind: BatteryLedgerKind;
  sourceId: string | null;
  deltaMs: number;
  balanceAfterMs: number;
  note: string | null;
}

/** 与后端 BatteryConfig::default 对齐的默认额度。 */
export const DEFAULT_BATTERY_CONFIG: BatteryConfig = {
  rewards: {
    waterMinutes: 8,
    restMinutes: 20,
    kegelMinutes: 10,
    customMinutes: 10,
    flashcardMinutes: 3,
  },
  dailyCaps: {
    water: 6,
    rest: 8,
    kegel: 4,
    custom: 6,
    flashcard: 30,
  },
  maxBalanceMinutes: 240,
  welcomeGrantMinutes: 25,
};

/** 一分钟毫秒。 */
export const BATTERY_MS_PER_MINUTE = 60_000;

/** 低于该剩余分钟时环用 warn 色。 */
export const BATTERY_WARN_MINUTES = 5;
