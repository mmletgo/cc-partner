/**
 * useAgentLedgerForAgent — 当前 active agent session 的 ledger 单条快照。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Workbench 右侧「当前会话」卡需要把 agent session 的 cumulative tokens（input/output/cache）
 *   与 modelId 投影到 UI，用于终态速率与 context %。Agent session 终态由后端
 *   `agent_session_ledger` 表持有 metadata-only 历史，本机明细通过 `list_agent_ledger` 拉取。
 *   working 阶段没有 ledger 数据，无需拉取；终态进入时拉一次，agentSessionId 变化再拉。
 *
 * Code Logic（这个模块做什么）:
 *   - 接受 projectId / agentSessionId / phase 三个输入；
 *   - phase 命中终态且 agentSessionId 非空才触发拉取，避免无意义请求；
 *   - 使用 single-flight + requestSeq 防过期响应覆盖；
 *   - 单页拉取 limit=50，client-side filter 匹配 agentSessionId；
 *   - 仅缓存最近一条匹配 entry；返回 ledgerEntry + loading + error；
 *   - 卸载时 abort 当前请求，projectId/agentSessionId/phase 变化抬 requestSeq。
 */

import { useEffect, useRef, useState } from 'react';

import { workbenchApi } from '@/api/workbench';
import type { AgentLedgerEntry } from '@/lib/types/agentLedger';
import type { AgentPhase } from '@/lib/types/agentRuntime';

/** 单页拉取的 ledger 上限（够覆盖近期终态）。 */
const MAX_LIST_LIMIT = 50;

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
 *   状态卡的 4 个 agent session 指标（输入/输出速率、上下文长度、上下文 %）依赖 ledger 单条；
 *   working 阶段统一显示「—」避免误导。
 *
 * Code Logic（这个 hook 做什么）:
 *   监听 phase 到达终态 + agentSessionId 变化 → 拉一页 list → filter → 写入 state。
 *   requestSeq 守卫过期响应；effect 卸载 abort。
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
  const [ledgerEntry, setLedgerEntry] = useState<AgentLedgerEntry | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const requestSeqRef = useRef(0);

  // Reset on project/agent/phase 身份变化
  useEffect(() => {
    setLedgerEntry(null);
    setError(null);
    setLoading(false);
    requestSeqRef.current += 1;
  }, [projectId, agentSessionId, phase]);

  useEffect(() => {
    const seq = ++requestSeqRef.current;
    if (!projectId || !agentSessionId || !isAgentTerminalPhase(phase)) {
      return undefined;
    }

    let cancelled = false;
    setLoading(true);
    setError(null);

    (async () => {
      try {
        const page = await workbenchApi.agentLedger.list({
          projectId,
          limit: MAX_LIST_LIMIT,
        });
        if (cancelled || seq !== requestSeqRef.current) return;
        const matched = page.items.find((entry) => entry.agentSessionId === agentSessionId) ?? null;
        setLedgerEntry(matched);
        setError(null);
      } catch (err) {
        if (cancelled || seq !== requestSeqRef.current) return;
        // Ledger 拉取失败不应让状态卡整体报错；保留历史值，仅设 error 供调用方诊断。
        setError(err instanceof Error ? err : new Error(String(err)));
        setLedgerEntry(null);
      } finally {
        if (!cancelled && seq === requestSeqRef.current) {
          setLoading(false);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [projectId, agentSessionId, phase]);

  return { ledgerEntry, loading, error };
}