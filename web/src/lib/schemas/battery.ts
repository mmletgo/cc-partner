/**
 * 充电模式 IPC DTO decoder。
 *
 * Business Logic（为什么需要这个模块）:
 *   footer / 设置页写入 state 前必须拒绝残缺快照与额度，避免把非法余额当权威。
 *
 * Code Logic（这个模块做什么）:
 *   组合 object/enum/optional/nullable decoder，未知额外字段允许前向兼容。
 */

import type {
  BatteryConfig,
  BatteryCreditSource,
  BatteryDailyCaps,
  BatteryLedgerItem,
  BatteryLedgerKind,
  BatteryMode,
  BatteryRewards,
  BatterySnapshot,
} from '../types/battery';
import {
  booleanDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

const batteryModeDecoder: Decoder<BatteryMode> = enumDecoder('BatteryMode', [
  'charging',
  'unlimited',
] as const);

const batteryCreditSourceDecoder: Decoder<BatteryCreditSource> = enumDecoder(
  'BatteryCreditSource',
  ['water', 'rest', 'kegel', 'custom', 'flashcard', 'welcome'] as const,
);

const batteryLedgerKindDecoder: Decoder<BatteryLedgerKind> = enumDecoder('BatteryLedgerKind', [
  'credit_health',
  'credit_wordgame',
  'credit_welcome',
  'debit_tick',
  'mode_change',
] as const);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   余额与今日累计是遮罩 / 环的权威输入。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码快照必填字段；creditMinutes/creditSource 仅入账事件出现。
 */
export const batterySnapshotDecoder: Decoder<BatterySnapshot> = objectDecoder('BatterySnapshot', {
  mode: batteryModeDecoder,
  remainingMs: numberDecoder,
  maxBalanceMs: numberDecoder,
  todayEarnedMs: numberDecoder,
  todaySpentMs: numberDecoder,
  consuming: booleanDecoder,
  creditMinutes: optionalDecoder(numberDecoder),
  creditSource: optionalDecoder(batteryCreditSourceDecoder),
});

const batteryRewardsDecoder: Decoder<BatteryRewards> = objectDecoder('BatteryRewards', {
  waterMinutes: numberDecoder,
  restMinutes: numberDecoder,
  kegelMinutes: numberDecoder,
  customMinutes: numberDecoder,
  flashcardMinutes: numberDecoder,
});

const batteryDailyCapsDecoder: Decoder<BatteryDailyCaps> = objectDecoder('BatteryDailyCaps', {
  water: numberDecoder,
  rest: numberDecoder,
  kegel: numberDecoder,
  custom: numberDecoder,
  flashcard: numberDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   设置页保存前后必须确认额度数字齐全。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 rewards / dailyCaps / 上限 / 欢迎赠送。
 */
export const batteryConfigDecoder: Decoder<BatteryConfig> = objectDecoder('BatteryConfig', {
  rewards: batteryRewardsDecoder,
  dailyCaps: batteryDailyCapsDecoder,
  maxBalanceMinutes: numberDecoder,
  welcomeGrantMinutes: numberDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   流水列表不能因一行损坏把整页打挂，但单行必须 fail-closed。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 id/ts/kind/sourceId/deltaMs/balanceAfterMs/note。
 */
export const batteryLedgerItemDecoder: Decoder<BatteryLedgerItem> = objectDecoder(
  'BatteryLedgerItem',
  {
    id: numberDecoder,
    ts: numberDecoder,
    kind: batteryLedgerKindDecoder,
    sourceId: nullableDecoder(stringDecoder),
    deltaMs: numberDecoder,
    balanceAfterMs: numberDecoder,
    note: nullableDecoder(stringDecoder),
  },
);
