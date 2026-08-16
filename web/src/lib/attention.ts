/**
 * Attention 纯规则 helper（badge / 分组 / 动作 key / 桌面 deep link）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面与移动端 Inbox 必须共享完全相同的数量展示、分类顺序与导航目标语义，
 *   避免页面各自拼装 badge 文案或 deep link 导致两端行为漂移。
 *
 * Code Logic（这个模块做什么）:
 *   提供无副作用纯函数：badge 文本、分组、排序保护、sourceKind→i18n key、
 *   语义 target→桌面 URL、切到等待输入终端时应收的 Inbox 条目。不发起网络请求，不依赖 React。
 */

import type {
  AttentionCategory,
  AttentionCounts,
  AttentionItem,
  AttentionSnapshot,
  AttentionSourceKind,
  AttentionTarget,
} from './types';

/** 固定分组顺序：需要决策 → 运行受阻 → 环境受阻。 */
export const ATTENTION_CATEGORY_ORDER: readonly AttentionCategory[] = [
  'decision',
  'blocked',
  'environment',
] as const;

/**
 * 单组 Attention 条目。
 *
 * Business Logic（为什么需要这个类型）:
 *   Inbox 页面按分类渲染区块，需要稳定的 category + items 结构。
 *
 * Code Logic（字段说明）:
 *   category 为三分类之一；items 为该组内条目（调用方可再排序）。
 */
export interface AttentionGroup {
  category: AttentionCategory;
  items: AttentionItem[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏与移动导航 badge 必须统一：0 不显示、1..99 显示真实数字、超过 99 显示 99+。
 *
 * Code Logic（这个函数做什么）:
 *   total<=0 或非有限数返回 null；1..99 返回数字字符串；>=100 返回 "99+"。
 */
/** 空计数，供测试夹具与乐观更新回落。 */
export const EMPTY_ATTENTION_COUNTS: AttentionCounts = {
  total: 0,
  decision: 0,
  blocked: 0,
  environment: 0,
  unreadTotal: 0,
  unreadDecision: 0,
  unreadBlocked: 0,
  unreadEnvironment: 0,
};

/**
 * Business Logic（为什么需要这个函数）:
 *   已读是本设备元数据；空串/缺省都视为未读，避免把 omit 字段当已读。
 *
 * Code Logic（这个函数做什么）:
 *   readAt 非空字符串为已读。
 */
export function isAttentionItemUnread(item: AttentionItem): boolean {
  return item.readAt == null || item.readAt === '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户切到正在等待输入的终端时，只应收该 window 对应的 Inbox 未读条目，
 *   不得误收其它 session、失败条目或非 Agent 来源。
 *
 * Code Logic（这个函数做什么）:
 *   过滤未读、sourceKind=agentNeedsInput、target 为匹配 terminalSessionId 的 agentSession。
 */
export function collectUnreadAgentNeedsInputItemIds(
  items: readonly AttentionItem[],
  terminalSessionId: string,
): string[] {
  if (terminalSessionId.length === 0) return [];
  const ids: string[] = [];
  for (const item of items) {
    if (!isAttentionItemUnread(item)) continue;
    if (item.sourceKind !== 'agentNeedsInput') continue;
    if (item.target.kind !== 'agentSession') continue;
    if (item.target.terminalSessionId !== terminalSessionId) continue;
    ids.push(item.id);
  }
  return ids;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动已读必须可单测：未看见终端、已尝试过的 id、空快照都不得再 mark。
 *
 * Code Logic（这个函数做什么）:
 *   enabled 且 session 非空时收集匹配未读 id，再去掉 alreadyMarkedIds。
 */
export function planNeedsInputAttentionAutoRead(
  items: readonly AttentionItem[] | undefined,
  terminalSessionId: string | null,
  enabled: boolean,
  alreadyMarkedIds: ReadonlySet<string>,
): string[] {
  if (!enabled || !terminalSessionId || !items) return [];
  return collectUnreadAgentNeedsInputItemIds(items, terminalSessionId).filter(
    (id) => !alreadyMarkedIds.has(id),
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   乐观更新与测试夹具必须与聚合器同一口径派生 total/unread_*。
 *
 * Code Logic（这个函数做什么）:
 *   单次循环统计分类计数与未读子集。
 */
export function countAttentionItems(items: readonly AttentionItem[]): AttentionCounts {
  const counts: AttentionCounts = { ...EMPTY_ATTENTION_COUNTS };
  for (const item of items) {
    counts.total += 1;
    const unread = isAttentionItemUnread(item);
    if (unread) counts.unreadTotal += 1;
    if (item.category === 'decision') {
      counts.decision += 1;
      if (unread) counts.unreadDecision += 1;
    } else if (item.category === 'blocked') {
      counts.blocked += 1;
      if (unread) counts.unreadBlocked += 1;
    } else {
      counts.environment += 1;
      if (unread) counts.unreadEnvironment += 1;
    }
  }
  return counts;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 乐观已读/撤销时不得等下一轮聚合才改 badge。
 *
 * Code Logic（这个函数做什么）:
 *   按 id 集合写入或清除 readAt，再重算 counts。
 */
export function applyAttentionReadState(
  snapshot: AttentionSnapshot,
  itemIds: readonly string[],
  read: boolean,
  readAt: string,
): AttentionSnapshot {
  const idSet = new Set(itemIds);
  const items = snapshot.items.map((item) => {
    if (!idSet.has(item.id)) return item;
    if (read) {
      return { ...item, readAt: item.readAt && item.readAt !== '' ? item.readAt : readAt };
    }
    return { ...item, readAt: undefined };
  });
  return {
    ...snapshot,
    items,
    counts: countAttentionItems(items),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   表格来源列需要稳定 i18n key，不能复用动作文案。
 *
 * Code Logic（这个函数做什么）:
 *   映射 sourceKind 到 attention:sources.*。
 */
export type AttentionSourceI18nKey =
  | 'attention:sources.orchestratorHumanReview'
  | 'attention:sources.orchestratorBlocked'
  | 'attention:sources.remoteOutboxFailed'
  | 'attention:sources.workbenchDependency'
  | 'attention:sources.agentNeedsInput'
  | 'attention:sources.agentFailed'
  | 'attention:sources.experimentNeedsDecision'
  | 'attention:sources.agentHubConflict'
  | 'attention:sources.agentHubProjectionBlocked';

export function getAttentionSourceI18nKey(sourceKind: AttentionSourceKind): AttentionSourceI18nKey {
  switch (sourceKind) {
    case 'orchestratorHumanReview':
      return 'attention:sources.orchestratorHumanReview';
    case 'orchestratorBlocked':
      return 'attention:sources.orchestratorBlocked';
    case 'remoteOutboxFailed':
      return 'attention:sources.remoteOutboxFailed';
    case 'workbenchDependency':
      return 'attention:sources.workbenchDependency';
    case 'agentNeedsInput':
      return 'attention:sources.agentNeedsInput';
    case 'agentFailed':
      return 'attention:sources.agentFailed';
    case 'experimentNeedsDecision':
      return 'attention:sources.experimentNeedsDecision';
    case 'agentHubConflict':
      return 'attention:sources.agentHubConflict';
    case 'agentHubProjectionBlocked':
      return 'attention:sources.agentHubProjectionBlocked';
    default: {
      const _exhaustive: never = sourceKind;
      return _exhaustive;
    }
  }
}

export function formatAttentionBadgeCount(total: number): string | null {
  if (!Number.isFinite(total) || total <= 0) return null;
  if (total > 99) return '99+';
  return String(Math.floor(total));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   聚合器已排序，但前端仍需在客户端保护分类序与同分类 updatedAt/id 次序，防止旧快照或脏数据打乱 UI。
 *
 * Code Logic（这个函数做什么）:
 *   复制 items 后按 category rank → updatedAt 降序 → id 升序排序。
 */
export function protectAttentionItemOrder(items: AttentionItem[]): AttentionItem[] {
  const rank = (category: AttentionCategory): number => {
    const index = ATTENTION_CATEGORY_ORDER.indexOf(category);
    return index < 0 ? ATTENTION_CATEGORY_ORDER.length : index;
  };

  return [...items].sort((a, b) => {
    const categoryDelta = rank(a.category) - rank(b.category);
    if (categoryDelta !== 0) return categoryDelta;
    if (a.updatedAt !== b.updatedAt) {
      return a.updatedAt < b.updatedAt ? 1 : -1;
    }
    if (a.id === b.id) return 0;
    return a.id < b.id ? -1 : 1;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Inbox 页面按三组语义渲染；空分组不得占位，避免“空的需要你的决定”干扰。
 *
 * Code Logic（这个函数做什么）:
 *   先保护排序，再按 ATTENTION_CATEGORY_ORDER 分桶，仅返回 items 非空的组。
 */
export function groupAttentionItems(items: AttentionItem[]): AttentionGroup[] {
  const ordered = protectAttentionItemOrder(items);
  const buckets = new Map<AttentionCategory, AttentionItem[]>();
  for (const category of ATTENTION_CATEGORY_ORDER) {
    buckets.set(category, []);
  }
  for (const item of ordered) {
    const bucket = buckets.get(item.category);
    if (bucket) {
      bucket.push(item);
    }
  }
  const groups: AttentionGroup[] = [];
  for (const category of ATTENTION_CATEGORY_ORDER) {
    const groupItems = buckets.get(category) ?? [];
    if (groupItems.length > 0) {
      groups.push({ category, items: groupItems });
    }
  }
  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   动作文案固定按 sourceKind 生成（前往复核 / 查看阻塞原因 / 查看失败项 / 打开设置），
 *   页面只消费 i18n key，不写死中文/英文。
 *
 * Code Logic（这个函数做什么）:
 *   映射 sourceKind 到 attention namespace 下的 action.* key。
 */
export type AttentionActionI18nKey =
  | 'attention:action.review'
  | 'attention:action.viewBlocked'
  | 'attention:action.viewFailed'
  | 'attention:action.openSettings'
  | 'attention:action.openTerminal'
  | 'attention:action.openExperiment'
  | 'attention:action.openAgentHub';

export function getAttentionActionI18nKey(sourceKind: AttentionSourceKind): AttentionActionI18nKey {
  switch (sourceKind) {
    case 'orchestratorHumanReview':
      return 'attention:action.review';
    case 'orchestratorBlocked':
      return 'attention:action.viewBlocked';
    case 'remoteOutboxFailed':
      return 'attention:action.viewFailed';
    case 'workbenchDependency':
      return 'attention:action.openSettings';
    case 'agentNeedsInput':
    case 'agentFailed':
      return 'attention:action.openTerminal';
    case 'experimentNeedsDecision':
      return 'attention:action.openExperiment';
    case 'agentHubConflict':
    case 'agentHubProjectionBlocked':
      return 'attention:action.openAgentHub';
    default: {
      const _exhaustive: never = sourceKind;
      return _exhaustive;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端只允许导航到权威 URL：任务/outbox automation、设置、terminal session、experiment。
 *
 * Code Logic（这个函数做什么）:
 *   将语义 target 映射为 `/workbench?...` 或 `/settings?tab=dependencies`。
 */
export function buildDesktopAttentionTargetUrl(target: AttentionTarget): string {
  switch (target.kind) {
    case 'orchestratorTask': {
      const params = new URLSearchParams();
      params.set('projectId', target.projectId);
      params.set('view', 'automation');
      params.set('taskId', target.taskId);
      return `/workbench?${params.toString()}`;
    }
    case 'remoteOutbox': {
      const params = new URLSearchParams();
      params.set('projectId', target.projectId);
      params.set('view', 'automation');
      params.set('outboxId', target.outboxId);
      return `/workbench?${params.toString()}`;
    }
    case 'settings':
      return '/settings?tab=dependencies';
    case 'agentSession': {
      const params = new URLSearchParams();
      params.set('projectId', target.projectId);
      if (target.worktreeId) params.set('worktreeId', target.worktreeId);
      params.set('sessionId', target.terminalSessionId);
      return `/workbench?${params.toString()}`;
    }
    case 'experiment': {
      // A4 落地前先定位到项目 automation 表面，不打开不存在的 experiment 面板。
      const params = new URLSearchParams();
      params.set('projectId', target.projectId);
      params.set('view', 'automation');
      return `/workbench?${params.toString()}`;
    }
    case 'agentHubAsset': {
      const params = new URLSearchParams();
      params.set('assetId', target.assetId);
      if (target.conflictId) params.set('conflictId', target.conflictId);
      return `/agent-hub?${params.toString()}`;
    }
    default: {
      const _exhaustive: never = target;
      return _exhaustive;
    }
  }
}
