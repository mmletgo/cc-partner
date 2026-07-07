/**
 * Claude Code 资产页筛选纯函数
 *
 * Business Logic（为什么需要这个文件）:
 *   Claude Code 资产页的 local tab 和 remote（局域网拉取）tab 共享同一套筛选维度
 *   （类别 / 关键字 / 启用状态），把匹配逻辑提取为纯函数供两处复用，避免逻辑漂移
 *   导致两个 tab 行为不一致。
 *
 * Code Logic（这个文件做什么）:
 *   导出 KindFilter / EnabledFilter 类型、KIND_OPTIONS / ENABLED_OPTIONS 常量，
 *   以及 matchesClaudeCodeAsset 纯函数（按 kind + search + enabledFilter 三维度
 *   对单个 ClaudeCodeAsset 求值，全部命中返回 true）。
 */

import type { ClaudeCodeAsset, ClaudeCodeAssetKind } from './types';

export type KindFilter = ClaudeCodeAssetKind | 'all';
export type EnabledFilter = 'all' | 'enabled' | 'disabled';

export const KIND_OPTIONS: KindFilter[] = ['all', 'skill', 'command', 'plugin', 'mcp'];
export const ENABLED_OPTIONS: EnabledFilter[] = ['all', 'enabled', 'disabled'];

/**
 * 判断资产是否同时命中 kind + search + enabledFilter 三个筛选维度。
 */
export function matchesClaudeCodeAsset(
  asset: ClaudeCodeAsset,
  kind: KindFilter,
  search: string,
  enabledFilter: EnabledFilter,
): boolean {
  const matchesKind = kind === 'all' || asset.kind === kind;
  const matchesEnabled =
    enabledFilter === 'all' ||
    (enabledFilter === 'enabled' && asset.enabled) ||
    (enabledFilter === 'disabled' && !asset.enabled);
  if (!matchesKind || !matchesEnabled) return false;
  const q = search.trim().toLowerCase();
  if (!q) return true;
  const haystack = `${asset.name} ${asset.id} ${asset.source} ${asset.description ?? ''}`.toLowerCase();
  return haystack.includes(q);
}
