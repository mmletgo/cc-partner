/**
 * 切到等待输入的终端时，自动把对应 Inbox 条目标已读。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent 等待输入会投影未读 Inbox 条目。用户切到该终端即表示已经看见，
 *   侧栏徽章应立刻下降，不必再回 Inbox 点「打开」。
 *
 * Code Logic（这个模块做什么）:
 *   可见终端 session 变化或快照新增匹配未读 needsInput 条目时 fire-and-forget markRead；
 *   同一聚焦期内已尝试过的 id 不再重标，避免与手动标未读打架。
 */

import { useEffect, useRef } from 'react';

import { planNeedsInputAttentionAutoRead } from '@/lib/attention';

import { useAttention } from './attentionContext';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面 Workbench 与移动终端面板共用同一条「看见即已读」规则。
 *
 * Code Logic（这个函数做什么）:
 *   enabled+sessionId 作为聚焦 epoch；收集尚未尝试的匹配未读 id 后 markRead；
 *   失败从 epoch 集合移除以便快照变化时重试。
 */
export function useMarkNeedsInputAttentionOnSessionFocus(
  terminalSessionId: string | null,
  enabled: boolean,
): void {
  const { snapshot, markRead, pendingReadIds } = useAttention();
  const epochRef = useRef<{ key: string; ids: Set<string> }>({ key: '', ids: new Set() });

  useEffect(() => {
    const key = enabled && terminalSessionId ? terminalSessionId : '';
    if (epochRef.current.key !== key) {
      epochRef.current = { key, ids: new Set() };
    }
    if (!key || !snapshot) return;

    const ids = planNeedsInputAttentionAutoRead(
      snapshot.items,
      terminalSessionId,
      enabled,
      epochRef.current.ids,
    ).filter((id) => !pendingReadIds.has(id));
    if (ids.length === 0) return;
    for (const id of ids) {
      epochRef.current.ids.add(id);
    }
    void markRead(ids).catch(() => {
      if (epochRef.current.key !== key) return;
      for (const id of ids) {
        epochRef.current.ids.delete(id);
      }
    });
  }, [enabled, markRead, pendingReadIds, snapshot, terminalSessionId]);
}
