/**
 * Attention 桌面 Inbox 页面。
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在一个独立页面查看当前阻塞工作继续的事项，并从列表导航到
 *   Orchestrator 任务详情 / outbox / Settings 依赖页，而不是在列表内执行副作用动作。
 *
 * Code Logic（这个页面做什么）:
 *   读取共享 AttentionProvider 快照；按分组渲染列表；点击行或动作控件仅 navigate；
 *   初次 loading 骨架、初次 error 可重载、stale 横幅保留列表、空态无庆祝/指标。
 */

import { useCallback, useMemo } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/primitives';
import { useAttention } from '@/hooks/useAttention';
import {
  buildDesktopAttentionTargetUrl,
  getAttentionActionI18nKey,
  groupAttentionItems,
} from '@/lib/attention';
import type { AttentionCategory, AttentionItem, AttentionSnapshot } from '@/lib/types';
import styles from './Attention.module.css';

/**
 * Attention 可测试视图 props。
 *
 * Business Logic（为什么需要这个类型）:
 *   视图契约测试应直接注入状态，避免耦合 Provider 异步与路由。
 *
 * Code Logic（字段说明）:
 *   镜像 AttentionContext 的展示字段，并提供 onNavigate / onReload。
 */
export interface AttentionViewProps {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
  onReload: () => void;
  onNavigate: (url: string) => void;
  formatTime?: (iso: string) => string;
}

const CATEGORY_I18N = {
  decision: 'attention:groups.decision',
  blocked: 'attention:groups.blocked',
  environment: 'attention:groups.environment',
} as const satisfies Record<AttentionCategory, string>;

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
 *   桌面 Inbox 需要独立、可测试的列表视图，固定三组语义与导航-only 动作。
 *
 * Code Logic（这个组件做什么）:
 *   按 loading/error/empty/list 分支渲染；不使用 assertive 整表 live region；
 *   空组不渲染；行与 44×44 动作控件均可导航。
 */
export function AttentionView({
  snapshot,
  loading,
  refreshing,
  stale,
  error,
  lastSucceededAt,
  onReload,
  onNavigate,
  formatTime,
}: AttentionViewProps) {
  const { t, i18n } = useTranslation(['attention', 'common']);

  const format =
    formatTime ??
    ((iso: string) => defaultFormatTime(iso, i18n.language.startsWith('zh') ? 'zh' : 'en'));

  const groups = useMemo(
    () => (snapshot ? groupAttentionItems(snapshot.items) : []),
    [snapshot],
  );

  const showSkeleton = loading && snapshot === null && error === null;
  const showFirstError = snapshot === null && error !== null && !loading;
  const showEmpty = snapshot !== null && snapshot.items.length === 0 && !showFirstError;

  /**
   * Business Logic（为什么需要这个函数）:
   *   行点击与动作控件共用导航，禁止在列表内执行 Deliver/Retry 等副作用。
   *
   * Code Logic（这个函数做什么）:
   *   用语义 target 构造桌面 URL 并回调 onNavigate。
   */
  const handleOpenItem = useCallback(
    (item: AttentionItem) => {
      onNavigate(buildDesktopAttentionTargetUrl(item.target));
    },
    [onNavigate],
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
              <Button
                type="button"
                variant="secondary"
                size="sm"
                onClick={onReload}
                disabled={refreshing || loading}
              >
                {t('attention:refresh')}
              </Button>
            ) : null}
          </div>
          {lastSucceededAt && snapshot !== null ? (
            <p className={styles.statusText} data-testid="attention-last-updated">
              {t('attention:staleUpdatedAt', { time: format(lastSucceededAt) })}
            </p>
          ) : null}
        </header>

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

        {snapshot !== null && groups.length > 0 ? (
          <div className={styles.groups} data-testid="attention-groups">
            {groups.map((group) => (
              <section
                key={group.category}
                className={styles.group}
                aria-labelledby={`attention-group-${group.category}`}
                data-testid={`attention-group-${group.category}`}
              >
                <h2 id={`attention-group-${group.category}`} className={styles.groupTitle}>
                  {t(CATEGORY_I18N[group.category])}
                </h2>
                <ul className={styles.list} role="list">
                  {group.items.map((item) => {
                    const actionLabel = t(getAttentionActionI18nKey(item.sourceKind));
                    return (
                      <li key={item.id}>
                        <div
                          className={styles.item}
                          role="button"
                          tabIndex={0}
                          data-testid={`attention-item-${item.id}`}
                          onClick={() => handleOpenItem(item)}
                          onKeyDown={(event) => {
                            if (event.key === 'Enter' || event.key === ' ') {
                              event.preventDefault();
                              handleOpenItem(item);
                            }
                          }}
                        >
                          <div className={styles.itemBody}>
                            <h3 className={styles.itemTitle}>{item.title}</h3>
                            <p className={styles.itemSummary}>{item.summary}</p>
                            <div className={styles.metaRow}>
                              {item.project ? (
                                <span className={styles.metaChip}>
                                  {t('attention:projectLabel', { name: item.project.name })}
                                </span>
                              ) : null}
                              {item.device ? (
                                <span className={styles.metaChip}>
                                  {t('attention:deviceLabel', { name: item.device.name })}
                                </span>
                              ) : null}
                              <span className={styles.metaChip}>{format(item.updatedAt)}</span>
                              {item.freshness === 'cached' ? (
                                <span
                                  className={styles.metaChip}
                                  data-testid={`attention-cached-${item.id}`}
                                >
                                  {t('attention:cachedLabel', {
                                    time: format(item.cachedAt ?? item.updatedAt),
                                  })}
                                </span>
                              ) : (
                                <span className={styles.metaChip}>{t('attention:liveLabel')}</span>
                              )}
                            </div>
                          </div>
                          <button
                            type="button"
                            className={styles.action}
                            data-testid={`attention-action-${item.id}`}
                            aria-label={actionLabel}
                            onClick={(event) => {
                              event.stopPropagation();
                              handleOpenItem(item);
                            }}
                          >
                            {actionLabel}
                          </button>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              </section>
            ))}
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
 *   读取 useAttention，把状态传给 AttentionView；navigate 到 target URL。
 */
export function Attention() {
  const navigate = useNavigate();
  const { snapshot, loading, refreshing, stale, error, lastSucceededAt, refresh } = useAttention();

  const handleReload = useCallback(() => {
    void refresh();
  }, [refresh]);

  const handleNavigate = useCallback(
    (url: string) => {
      navigate(url);
    },
    [navigate],
  );

  return (
    <AttentionView
      snapshot={snapshot}
      loading={loading}
      refreshing={refreshing}
      stale={stale}
      error={error}
      lastSucceededAt={lastSucceededAt}
      onReload={handleReload}
      onNavigate={handleNavigate}
    />
  );
}

export default Attention;
