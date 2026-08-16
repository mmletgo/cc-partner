/**
 * Attention 桌面 Inbox 页面。
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在一个独立页面用表格查看当前阻塞工作继续的事项，标已读/未读，
 *   并从操作列导航到权威界面，而不是在列表内执行 Deliver/Retry。
 *
 * Code Logic（这个页面做什么）:
 *   读取共享 AttentionProvider 快照；8 列表格按分组渲染；默认只展示当天 updatedAt；
 *   打开条目同时标已读；顶部全部已读 + 分类已读；已读行灰显保留。
 */

import { useCallback, useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';

import { Button, StatusMessage } from '@/components/primitives';
import { workbenchApi } from '@/api/workbench';
import { useAttention } from '@/hooks/useAttention';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import {
  buildDesktopAttentionTargetUrl,
  getAttentionActionI18nKey,
  getAttentionSourceI18nKey,
  groupAttentionItems,
  isAttentionItemUnread,
  partitionAttentionItemsByLocalDay,
} from '@/lib/attention';
import type { AttentionCategory, AttentionItem, AttentionSnapshot } from '@/lib/types';
import {
  openWorkbenchDeepLink,
  parseWorkbenchUrlAsDeepLink,
} from '@/pages/Workbench/workbenchWindowNavigation';
import styles from './Attention.module.css';

/**
 * Attention 可测试视图 props。
 *
 * Business Logic（为什么需要这个类型）:
 *   视图契约测试应直接注入状态，避免耦合 Provider 异步与路由。
 *
 * Code Logic（字段说明）:
 *   镜像 AttentionContext 的展示字段，并提供导航与已读写回调。
 */
export interface AttentionViewProps {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
  pendingReadIds: ReadonlySet<string>;
  markError: Error | null;
  onReload: () => void;
  onNavigate: (url: string) => void;
  onMarkRead: (itemIds: string[]) => void;
  onMarkUnread: (itemIds: string[]) => void;
  onMarkAllRead: () => void;
  onMarkCategoryRead: (category: AttentionCategory) => void;
  formatTime?: (iso: string) => string;
  /** 测试注入当前时刻；生产默认本地 now。 */
  now?: Date;
}

const CATEGORY_I18N = {
  decision: 'attention:groups.decision',
  blocked: 'attention:groups.blocked',
  environment: 'attention:groups.environment',
} as const satisfies Record<AttentionCategory, string>;

const TABLE_HEADERS = [
  { key: 'project', labelKey: 'attention:table.headers.project' },
  { key: 'device', labelKey: 'attention:table.headers.device' },
  { key: 'source', labelKey: 'attention:table.headers.source' },
  { key: 'category', labelKey: 'attention:table.headers.category' },
  { key: 'updatedAt', labelKey: 'attention:table.headers.updatedAt' },
  { key: 'title', labelKey: 'attention:table.headers.title' },
  { key: 'summary', labelKey: 'attention:table.headers.summary' },
  { key: 'action', labelKey: 'attention:table.headers.action' },
] as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   列表时间与 cachedAt 需要可读本地化短时间，测试可注入固定格式。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 字符串格式化为 locale 短日期时间；非法日期返回原串。
 */
function defaultFormatTime(iso: string, language: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString(language === 'zh' ? 'zh-CN' : 'en-US', {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Business Logic（为什么需要这个组件）:
 *   桌面 Inbox 需要独立、可测试的表格视图，固定 8 列与导航-only 动作。
 *
 * Code Logic（这个组件做什么）:
 *   按 loading/error/empty/table 分支渲染；默认只展示当天；已读灰显；操作列拆成打开 + 已读切换。
 */
export function AttentionView({
  snapshot,
  loading,
  refreshing,
  stale,
  error,
  lastSucceededAt,
  pendingReadIds,
  markError,
  onReload,
  onNavigate,
  onMarkRead,
  onMarkUnread,
  onMarkAllRead,
  onMarkCategoryRead,
  formatTime,
  now,
}: AttentionViewProps) {
  const { t, i18n } = useTranslation(['attention', 'common']);
  const [includeEarlier, setIncludeEarlier] = useState(false);

  const format =
    formatTime ??
    ((iso: string) => defaultFormatTime(iso, i18n.language.startsWith('zh') ? 'zh' : 'en'));

  const clock = now ?? new Date();
  const dayPartition = useMemo(
    () => partitionAttentionItemsByLocalDay(snapshot?.items ?? [], clock),
    [snapshot, clock],
  );
  const visibleItems = includeEarlier ? (snapshot?.items ?? []) : dayPartition.today;
  const groups = useMemo(() => groupAttentionItems(visibleItems), [visibleItems]);

  const showSkeleton = loading && snapshot === null && error === null;
  const showFirstError = snapshot === null && error !== null && !loading;
  const showEmpty = snapshot !== null && snapshot.items.length === 0 && !showFirstError;
  const unreadTotal = snapshot?.counts.unreadTotal ?? 0;
  const markingBusy = pendingReadIds.size > 0;

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开权威界面后该条不应继续占未读徽章。
   *
   * Code Logic（这个函数做什么）:
   *   未读则 fire-and-forget markRead，再导航。
   */
  const handleOpenItem = useCallback(
    (item: AttentionItem) => {
      if (isAttentionItemUnread(item)) {
        onMarkRead([item.id]);
      }
      onNavigate(buildDesktopAttentionTargetUrl(item.target));
    },
    [onMarkRead, onNavigate],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   已读切换必须与打开按钮分离，避免误导航。
   *
   * Code Logic（这个函数做什么）:
   *   按当前 readAt 调用 markRead 或 markUnread。
   */
  const handleToggleRead = useCallback(
    (item: AttentionItem) => {
      if (isAttentionItemUnread(item)) {
        onMarkRead([item.id]);
        return;
      }
      onMarkUnread([item.id]);
    },
    [onMarkRead, onMarkUnread],
  );

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.titleRow}>
            <div className={styles.titleBlock}>
              <h1 className={styles.title}>{t('attention:title')}</h1>
              <p className={styles.subtitle}>{t('attention:subtitle')}</p>
            </div>
            {snapshot !== null ? (
              <div className={styles.headerActions}>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={onMarkAllRead}
                  disabled={refreshing || loading || markingBusy || unreadTotal === 0}
                >
                  {t('attention:table.markAllRead')}
                </Button>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  onClick={onReload}
                  disabled={refreshing || loading}
                >
                  {t('attention:refresh')}
                </Button>
              </div>
            ) : null}
          </div>
          {lastSucceededAt && snapshot !== null ? (
            <p className={styles.statusText} data-testid="attention-last-updated">
              {t('attention:staleUpdatedAt', { time: format(lastSucceededAt) })}
            </p>
          ) : null}
        </header>

        {markError ? (
          <StatusMessage tone="danger" data-testid="attention-mark-error">
            {t('attention:markError')}
          </StatusMessage>
        ) : null}

        {stale && snapshot !== null ? (
          <div className={styles.banner} data-testid="attention-stale-banner" role="status">
            <p className={styles.bannerText}>{t('attention:staleBanner')}</p>
            <Button type="button" variant="secondary" size="sm" onClick={onReload}>
              {t('attention:refresh')}
            </Button>
          </div>
        ) : null}

        {showSkeleton ? (
          <div className={styles.skeleton} data-testid="attention-skeleton" aria-hidden="true">
            <div className={styles.skeletonBlock} />
            <div className={styles.skeletonBlock} />
            <div className={styles.skeletonBlock} />
          </div>
        ) : null}

        {showFirstError ? (
          <div className={`${styles.banner} ${styles.bannerError}`} data-testid="attention-error">
            <p className={styles.bannerText}>{t('attention:loadError')}</p>
            <Button type="button" variant="secondary" size="sm" onClick={onReload}>
              {t('attention:reload')}
            </Button>
          </div>
        ) : null}

        {showEmpty ? (
          <div className={styles.empty} data-testid="attention-empty">
            {t('attention:empty')}
          </div>
        ) : null}

        {dayPartition.earlier.length > 0 ? (
          <div className={styles.banner} data-testid="attention-day-filter" role="status">
            <p className={styles.bannerText}>
              {includeEarlier
                ? t('attention:dayFilter.showingEarlier', {
                    count: dayPartition.earlier.length,
                  })
                : dayPartition.today.length === 0
                  ? t('attention:dayFilter.todayEmpty')
                  : t('attention:dayFilter.earlierHidden', {
                      count: dayPartition.earlier.length,
                    })}
            </p>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              onClick={() => setIncludeEarlier((current: boolean) => !current)}
              data-testid={includeEarlier ? 'attention-show-today-only' : 'attention-show-earlier'}
            >
              {includeEarlier
                ? t('attention:dayFilter.showTodayOnly')
                : t('attention:dayFilter.showEarlier')}
            </Button>
          </div>
        ) : null}

        {snapshot !== null && groups.length > 0 ? (
          <div className={styles.groups} data-testid="attention-groups">
            {groups.map((group) => {
              const unreadInGroup = group.items.filter((item) => isAttentionItemUnread(item)).length;
              return (
                <section
                  key={group.category}
                  className={styles.group}
                  aria-labelledby={`attention-group-${group.category}`}
                  data-testid={`attention-group-${group.category}`}
                >
                  <div className={styles.groupHeader}>
                    <h2 id={`attention-group-${group.category}`} className={styles.groupTitle}>
                      {t(CATEGORY_I18N[group.category])}
                    </h2>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => onMarkCategoryRead(group.category)}
                      disabled={markingBusy || unreadInGroup === 0}
                      data-testid={`attention-mark-category-${group.category}`}
                    >
                      {t('attention:table.markCategoryRead')}
                    </Button>
                  </div>
                  <div
                    className={styles.table}
                    role="table"
                    aria-label={t('attention:table.ariaLabel')}
                  >
                    <div className={styles.headerRow} role="row">
                      {TABLE_HEADERS.map((column) => (
                        <div
                          key={column.key}
                          className={styles.headerCell}
                          role="columnheader"
                        >
                          {t(column.labelKey)}
                        </div>
                      ))}
                    </div>
                    {group.items.map((item) => {
                      const unread = isAttentionItemUnread(item);
                      const pending = pendingReadIds.has(item.id);
                      const actionLabel = t(getAttentionActionI18nKey(item.sourceKind));
                      const rowClass = unread
                        ? styles.itemRow
                        : `${styles.itemRow} ${styles.itemRowRead}`;
                      return (
                        <div
                          key={item.id}
                          className={rowClass}
                          role="row"
                          data-testid={`attention-item-${item.id}`}
                          data-read={unread ? 'false' : 'true'}
                        >
                          <div className={styles.cell} role="cell">
                            {item.project?.name ?? '—'}
                          </div>
                          <div className={styles.cell} role="cell">
                            {item.device?.name ?? '—'}
                          </div>
                          <div className={styles.cell} role="cell">
                            {t(getAttentionSourceI18nKey(item.sourceKind))}
                          </div>
                          <div className={styles.cell} role="cell">
                            {t(CATEGORY_I18N[item.category])}
                          </div>
                          <div className={styles.cell} role="cell">
                            <span>{format(item.updatedAt)}</span>
                            {item.freshness === 'cached' ? (
                              <span
                                className={styles.freshness}
                                data-testid={`attention-cached-${item.id}`}
                              >
                                {t('attention:cachedLabel', {
                                  time: format(item.cachedAt ?? item.updatedAt),
                                })}
                              </span>
                            ) : (
                              <span className={styles.freshness}>{t('attention:liveLabel')}</span>
                            )}
                          </div>
                          <div className={`${styles.cell} ${styles.titleCell}`} role="cell">
                            {item.title}
                          </div>
                          <div className={styles.cell} role="cell">
                            {item.summary}
                          </div>
                          <div className={`${styles.cell} ${styles.actionCell}`} role="cell">
                            <Button
                              type="button"
                              variant="secondary"
                              size="sm"
                              onClick={() => handleOpenItem(item)}
                              data-testid={`attention-action-${item.id}`}
                            >
                              {actionLabel}
                            </Button>
                            <Button
                              type="button"
                              variant="ghost"
                              size="sm"
                              onClick={() => handleToggleRead(item)}
                              disabled={pending}
                              data-testid={`attention-toggle-read-${item.id}`}
                              aria-label={
                                unread
                                  ? t('attention:markReadAria', { title: item.title })
                                  : t('attention:markUnreadAria', { title: item.title })
                              }
                            >
                              {unread
                                ? t('attention:table.markRead')
                                : t('attention:table.markUnread')}
                            </Button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </section>
              );
            })}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   路由页需要挂接 Provider 状态与 React Router 导航。
 *
 * Code Logic（这个组件做什么）:
 *   读取 useAttention，把状态传给 AttentionView；打开条目走 deep link。
 */
export function Attention() {
  const navigate = useNavigate();
  const {
    snapshot,
    loading,
    refreshing,
    stale,
    error,
    lastSucceededAt,
    refresh,
    markRead,
    markUnread,
    markAllRead,
    markCategoryRead,
    pendingReadIds,
    markError,
  } = useAttention();
  const { currentWindowLabel, occupancy } = useWorkbenchProjects();

  const handleReload = useCallback(() => {
    void refresh();
  }, [refresh]);

  const handleNavigate = useCallback(
    (url: string) => {
      const target = parseWorkbenchUrlAsDeepLink(url);
      if (!target) {
        navigate(url);
        return;
      }
      void openWorkbenchDeepLink({
        target,
        currentLabel: currentWindowLabel,
        occupancy,
        navigate,
        claim: workbenchApi.windows.claim,
        focus: workbenchApi.windows.focus,
        applyOnWindow: workbenchApi.windows.applyDeepLink,
      });
    },
    [currentWindowLabel, navigate, occupancy],
  );

  const handleMarkRead = useCallback(
    (itemIds: string[]) => {
      void markRead(itemIds);
    },
    [markRead],
  );

  const handleMarkUnread = useCallback(
    (itemIds: string[]) => {
      void markUnread(itemIds);
    },
    [markUnread],
  );

  const handleMarkAllRead = useCallback(() => {
    void markAllRead();
  }, [markAllRead]);

  const handleMarkCategoryRead = useCallback(
    (category: AttentionCategory) => {
      void markCategoryRead(category);
    },
    [markCategoryRead],
  );

  return (
    <AttentionView
      snapshot={snapshot}
      loading={loading}
      refreshing={refreshing}
      stale={stale}
      error={error}
      lastSucceededAt={lastSucceededAt}
      pendingReadIds={pendingReadIds}
      markError={markError}
      onReload={handleReload}
      onNavigate={handleNavigate}
      onMarkRead={handleMarkRead}
      onMarkUnread={handleMarkUnread}
      onMarkAllRead={handleMarkAllRead}
      onMarkCategoryRead={handleMarkCategoryRead}
    />
  );
}

export default Attention;
