/**
 * 游戏插件列表 runtime schema。
 *
 * Business Logic（为什么需要这个模块）:
 *     大厅写入 state 前必须拒绝残缺清单，避免把坏插件当可玩。
 *
 * Code Logic（这个模块做什么）:
 *     严格解码 list / summary，reason 可空。
 */

import type { GamePluginList, GamePluginSummary } from '../types/gamePlugin';
import {
  arrayDecoder,
  booleanDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

export const gamePluginSummaryDecoder: Decoder<GamePluginSummary> = objectDecoder(
  'GamePluginSummary',
  {
    id: stringDecoder,
    name: stringDecoder,
    description: stringDecoder,
    entry: stringDecoder,
    rewardMinutes: numberDecoder,
    playable: booleanDecoder,
    reason: nullableDecoder(stringDecoder),
  },
);

export const gamePluginListDecoder: Decoder<GamePluginList> = objectDecoder('GamePluginList', {
  dir: stringDecoder,
  games: arrayDecoder(gamePluginSummaryDecoder),
});
