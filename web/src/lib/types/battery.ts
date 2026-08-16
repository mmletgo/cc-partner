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
export type BatteryCreditSource = 'health' | 'flashcard' | 'game-plugin';

/** 账本流水 kind。 */
export type BatteryLedgerKind =
  | 'credit_health'
  | 'credit_wordgame'
  | 'credit_game_plugin'
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

/** config.json 里的可调额度。健康习惯额度写在 HealthReminderTemplate 上。 */
export interface BatteryConfig {
  /** 闪卡答对一次充入分钟。 */
  flashcardMinutes: number;
  /** 闪卡每日张数上限。 */
  flashcardCap: number;
  /** 余额上限（分钟）。 */
  maxBalanceMinutes: number;
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
  flashcardMinutes: 3,
  flashcardCap: 30,
  maxBalanceMinutes: 240,
};

/** 一分钟毫秒。 */
export const BATTERY_MS_PER_MINUTE = 60_000;

/** 低于该剩余分钟时环用 warn 色。 */
export const BATTERY_WARN_MINUTES = 5;
