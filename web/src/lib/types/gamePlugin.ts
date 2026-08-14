/**
 * 游戏插件大厅 DTO。
 *
 * Business Logic（为什么需要这个模块）:
 *     大厅列出用户目录里的每个游戏，并决定能否开始。
 *
 * Code Logic（这个模块做什么）:
 *     对齐 Rust GamePluginSummary / GamePluginListDto。
 */

export interface GamePluginSummary {
  id: string;
  name: string;
  description: string;
  entry: string;
  rewardMinutes: number;
  playable: boolean;
  reason: string | null;
}

export interface GamePluginList {
  dir: string;
  games: GamePluginSummary[];
}
