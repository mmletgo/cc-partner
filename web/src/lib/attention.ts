/**
 * Attention 纯规则 helper（badge / 分组 / 动作 key / 桌面 deep link）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面与移动端 Inbox 必须共享完全相同的数量展示、分类顺序与导航目标语义，
 *   避免页面各自拼装 badge 文案或 deep link 导致两端行为漂移。
 *
 * Code Logic（这个模块做什么）:
 *   提供无副作用纯函数：badge 文本、分组、排序保护、sourceKind→i18n key、
 *   语义 target→桌面 URL。不发起网络请求，不依赖 React。
 */

import type {
  AttentionCategory,
  AttentionItem,
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
export function getAttentionActionI18nKey(sourceKind: AttentionSourceKind): string {
  switch (sourceKind) {
    case 'orchestratorHumanReview':
      return 'attention:action.review';
    case 'orchestratorBlocked':
      return 'attention:action.viewBlocked';
    case 'remoteOutboxFailed':
      return 'attention:action.viewFailedOutbox';
    case 'workbenchDependency':
      return 'attention:action.openSettings';
    default: {
      const _exhaustive: never = sourceKind;
      return _exhaustive;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端只允许导航到三个权威 URL：任务 automation、outbox automation、设置 dependencies。
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
    default: {
      const _exhaustive: never = target;
      return _exhaustive;
    }
  }
}
