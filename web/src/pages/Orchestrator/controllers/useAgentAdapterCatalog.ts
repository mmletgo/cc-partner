/**
 * Owner adapter catalog loader（Settings/Orchestrator/Workbench 共用 fail-closed 数据源）。
 *
 * Business Logic: OpenCode bridge/blocked/completion 必须在自动化表面可见。
 * Code Logic: mount 时 listOrchestratorAgentAdapters；seq 防 stale；失败空数组。
 */

import { useEffect, useRef, useState } from 'react';
import { listOrchestratorAgentAdapters } from '@/api/orchestrator';
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
 * Business Logic: 创建实验 candidates 时 OpenCode 必须 bridge ready，否则回落 Codex。
 * Code Logic: 返回两条 candidate 配置。
 */
export function buildExperimentCandidates(
  agentAdapters: OrchestratorAgentAdapterCatalogItem[],
): Array<{ providerId: string; strategyLabel: string }> {
  const openCode = agentAdapters.find((item) => item.provider === 'openCodeVisible');
  const openCodeReady = openCode?.available === true && openCode.bridgeStatus === 'ready';
  const second = openCodeReady
    ? { providerId: 'openCodeVisible', strategyLabel: 'opencode-visible' }
    : { providerId: 'codexVisible', strategyLabel: 'codex-visible' };
  return [{ providerId: 'claudeCodeVisible', strategyLabel: 'baseline' }, second];
}
