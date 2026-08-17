/**
 * Owner adapter catalog loader（Settings/Orchestrator/Workbench 共用 fail-closed 数据源）。
 *
 * Business Logic: OpenCode bridge/blocked/completion 必须在自动化表面可见。
 * Code Logic: mount 时 listOrchestratorAgentAdapters；seq 防 stale；失败空数组。
 */

import { useEffect, useRef, useState } from 'react';
import { listOrchestratorAgentAdapters } from '@/api/orchestrator';
import { listEffectivelyAvailableAgentAdapters } from '@/lib/agentAdapterPresentation';
import type { OrchestratorAgentAdapterCatalogItem } from '@/lib/types';

/**
 * Business Logic: 拉取 redacted adapter catalog（无 path/env secrets）。
 * Code Logic: useState + mount effect + request seq。
 */
export function useAgentAdapterCatalog(): OrchestratorAgentAdapterCatalogItem[] {
  const [agentAdapters, setAgentAdapters] = useState<OrchestratorAgentAdapterCatalogItem[]>([]);
  const adapterLoadSeqRef = useRef(0);

  useEffect(() => {
    const seq = ++adapterLoadSeqRef.current;
    void listOrchestratorAgentAdapters()
      .then((catalog) => {
        if (adapterLoadSeqRef.current !== seq) return;
        setAgentAdapters(catalog.adapters ?? []);
      })
      .catch(() => {
        if (adapterLoadSeqRef.current !== seq) return;
        setAgentAdapters([]);
      });
  }, []);

  return agentAdapters;
}

/**
 * Business Logic: 比较实验只能派到当前有效可用的 Agent；不可用 adapter 不得占 candidate 名额。
 * Code Logic: 从 effectively available 中优先 Claude → OpenCode → Codex → 其余，最多两条。
 */
export function buildExperimentCandidates(
  agentAdapters: OrchestratorAgentAdapterCatalogItem[],
): Array<{ providerId: string; strategyLabel: string }> {
  const available = listEffectivelyAvailableAgentAdapters(agentAdapters);
  if (available.length === 0) return [];

  const findAvailable = (provider: string) =>
    available.find((item) => item.provider === provider);

  const first = findAvailable('claudeCodeVisible') ?? available[0];
  if (!first) return [];
  const rest = available.filter((item) => item.provider !== first.provider);
  const second =
    (first.provider === 'claudeCodeVisible'
      ? (findAvailable('openCodeVisible') ?? findAvailable('codexVisible') ?? rest[0])
      : rest[0]) ?? null;

  return [first, second]
    .filter((item): item is OrchestratorAgentAdapterCatalogItem => item != null)
    .map((item, index) => ({
      providerId: String(item.provider),
      strategyLabel: experimentStrategyLabel(String(item.provider), index === 0),
    }));
}

/**
 * Business Logic: 实验 candidate 需要稳定的策略标签，供看板区分 baseline / 对照。
 * Code Logic: Claude 首位用 baseline；OpenCode/Codex 用既有可见策略名；其余回落 provider id。
 */
function experimentStrategyLabel(provider: string, isFirst: boolean): string {
  if (isFirst && provider === 'claudeCodeVisible') return 'baseline';
  if (provider === 'openCodeVisible') return 'opencode-visible';
  if (provider === 'codexVisible') return 'codex-visible';
  return provider;
}
