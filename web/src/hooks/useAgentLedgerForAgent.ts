/**
 * useAgentLedgerForAgent — 当前 active agent session 的 ledger 单条快照。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Workbench 右侧「当前会话」卡需要把 agent session 的 cumulative tokens（input/output/cache）
 *   与 modelId 投影到 UI，用于终态速率与 context %。Agent session 终态由后端
 *   `agent_session_ledger` 表持有 metadata-only 历史，本机明细通过 `list_agent_ledger` 拉取。
 *   working 阶段没有 ledger 数据，无需拉取；终态进入时拉取，首次未命中按有界间隔重试，
 *   等待异步 note_usage；agentSessionId 变化再拉。状态卡优先 live usage，本 hook 只服务终态回退。
 *
 * Code Logic（这个模块做什么）:
 *   - 接受 projectId / agentSessionId / phase 三个输入；
 *   - phase 命中终态且 agentSessionId 非空才触发拉取，避免无意义请求；
 *   - 使用 single-flight + requestSeq 防过期响应覆盖；
 *   - 单页拉取 limit=50，client-side filter 匹配 agentSessionId；
 *   - 首次未命中按 LEDGER_RETRY_DELAYS_MS 再拉；仅缓存最近一条匹配 entry；
 *   - 返回 ledgerEntry + loading + error；卸载 abort，身份变化抬 requestSeq。
 */

import { useEffect, useRef, useState } from 'react';

import { workbenchApi } from '@/api/workbench';
import type { AgentLedgerEntry } from '@/lib/types/agentLedger';
import type { AgentPhase } from '@/lib/types/agentRuntime';

/** 单页拉取的 ledger 上限（够覆盖近期终态）。 */
const MAX_LIST_LIMIT = 50;

/** 终态首次未命中时的有界重试间隔（等待异步 note_usage）。 */
const LEDGER_RETRY_DELAYS_MS = [250, 800] as const;

/** Phase 是否已经稳定到终态（有 ledger 行）。 */
export function isAgentTerminalPhase(phase: AgentPhase | null | undefined): boolean {
  if (!phase) return false;
  return phase === 'completed' || phase === 'failed' || phase === 'disconnected';
}

/**
 * useAgentLedgerForAgent 返回值。
 */
export interface UseAgentLedgerForAgentResult {
  /** 最近一次匹配的 ledger 行；无匹配或未拉取时为 null。 */
  ledgerEntry: AgentLedgerEntry | null;
  /** 当前是否正在拉取（仅终态首次触发为 true，之后保持 false 直到 phase/id 变化）。 */
  loading: boolean;
  /** 最近一次拉取错误；成功或未触发时为 null。 */
  error: Error | null;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   状态卡终态回退依赖 ledger 单条；working/needsInput 走 live usage，本 hook 不拉取。
 *
 * Code Logic（这个 hook 做什么）:
 *   监听 phase 到达终态 + agentSessionId 变化 → 拉一页 list → filter → 写入 state。
 *   首次未命中按 LEDGER_RETRY_DELAYS_MS 再拉；requestSeq 守卫过期响应；effect 卸载 abort。
 *
 * @param projectId 当前项目 ID；null/空则不拉取
 * @param agentSessionId 当前 active agent session 的 ID；null/空则不拉取
 * @param phase 当前 agent phase
 */
export function useAgentLedgerForAgent(
  projectId: string | null | undefined,
  agentSessionId: string | null | undefined,
  phase: AgentPhase | null | undefined,
): UseAgentLedgerForAgentResult {
  const shouldFetch = Boolean(projectId && agentSessionId && isAgentTerminalPhase(phase));
  const identityKey = `${projectId ?? ''}:${agentSessionId ?? ''}:${phase ?? ''}`;
  const [ledgerEntry, setLedgerEntry] = useState<AgentLedgerEntry | null>(null);
  const [loading, setLoading] = useState(shouldFetch);
  const [error, setError] = useState<Error | null>(null);
  const [seenKey, setSeenKey] = useState(identityKey);
  const requestSeqRef = useRef(0);

  if (seenKey !== identityKey) {
    setSeenKey(identityKey);
    setLedgerEntry(null);
    setError(null);
    setLoading(shouldFetch);
  }

  useEffect(() => {
    const seq = ++requestSeqRef.current;
    if (!shouldFetch || !projectId || !agentSessionId) {
      return undefined;
    }

    let cancelled = false;

    void (async () => {
      try {
        let matched: AgentLedgerEntry | null = null;
        let lastError: Error | null = null;
        const delays: number[] = [0, ...LEDGER_RETRY_DELAYS_MS];
        for (let i = 0; i < delays.length; i += 1) {
          const waitMs = delays[i];
          if (waitMs > 0) {
            await new Promise<void>((resolve) => {
              setTimeout(resolve, waitMs);
            });
          }
          if (cancelled || seq !== requestSeqRef.current) return;
          try {
            const page = await workbenchApi.agentLedger.list({
              projectId,
              limit: MAX_LIST_LIMIT,
            });
            if (cancelled || seq !== requestSeqRef.current) return;
            matched = page.items.find((entry) => entry.agentSessionId === agentSessionId) ?? null;
            lastError = null;
            if (matched) break;
          } catch (err) {
            lastError = err instanceof Error ? err : new Error(String(err));
          }
        }
        if (cancelled || seq !== requestSeqRef.current) return;
        setLedgerEntry(matched);
        setError(lastError);
      } finally {
        if (!cancelled && seq === requestSeqRef.current) {
          setLoading(false);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [agentSessionId, phase, projectId, shouldFetch]);

  return { ledgerEntry, loading, error };
}
