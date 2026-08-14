/**
 * 游戏插件 API。
 *
 * Business Logic（为什么需要这个模块）:
 *     大厅列出用户游戏并在完成时入账，组件不得直接 invoke。
 *
 * Code Logic（这个模块做什么）:
 *     list_game_plugins / credit_game_plugin，fail-closed 解码。
 */

import { invokeDecoded } from './client';
import { batterySnapshotDecoder } from '@/lib/schemas/battery';
import { gamePluginListDecoder } from '@/lib/schemas/gamePlugin';
import type { BatterySnapshot } from '@/lib/types/battery';
import type { GamePluginList } from '@/lib/types/gamePlugin';

export const gamePluginsApi = {
  /**
   * Business Logic: 大厅打开时列出插件目录。
   * Code Logic: invokeDecoded list_game_plugins。
   */
  list: (): Promise<GamePluginList> =>
    invokeDecoded('list_game_plugins', undefined, gamePluginListDecoder),

  /**
   * Business Logic: 游戏自报完成后按清单分钟入账。
   * Code Logic: invokeDecoded credit_game_plugin；分钟数由后端读清单。
   */
  credit: (gameId: string, sourceId?: string): Promise<BatterySnapshot> =>
    invokeDecoded(
      'credit_game_plugin',
      { gameId, sourceId: sourceId ?? null },
      batterySnapshotDecoder,
    ),
};
