/**
 * MobileAttentionPanel（移动端全局 Inbox）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端需要在导航第二项查看“当前阻塞工作的事项”，并只导航到现有 Automation/Settings 权威界面。
 *
 * Code Logic（这个组件做什么）:
 *   消费 useAttention；复用 groupAttentionItems/action/freshness helpers 渲染紧凑分组列表；
 *   unsupported/stale/empty/error 分态展示；点击后交给父级 target mapper，列表内不执行副作用动作。
 */

import { useMemo, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { AttentionHttpError } from '@/api/attentionHttp';
import { useAttention } from '@/hooks/attentionContext';
import {
  getAttentionActionI18nKey,
  groupAttentionItems,
  isAttentionItemUnread,
  partitionAttentionItemsByLocalDay,
} from '@/lib/attention';
import type { AttentionCategory, AttentionItem, AttentionSourceKind } from '@/lib/types';
import panelStyles from '../MobileWorkbench.module.css';
import styles from './MobileAttentionPanel.module.css';

export interface MobileAttentionPanelProps {
  onOpenItem: (item: AttentionItem) => void;
  notice?: string | null;
  /** 测试注入当前时刻；生产默认本地 now。 */
  now?: Date;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   时间字段来自后端 ISO 字符串，无效值不能让列表崩溃。
 *
 * Code Logic（这个函数做什么）:
 *   Date 解析成功则本地化展示，失败返回原字符串。
 */
function formatAttentionTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   分类标题必须是可见文字，颜色只能辅助，不能替代文案。
 *
 * Code Logic（这个函数做什么）:
 *   映射 category → attention:category.* key。
 */
function categoryLabelKey(
  category: AttentionCategory,
): 'attention:category.decision' | 'attention:category.blocked' | 'attention:category.environment' {
  switch (category) {
    case 'decision':
      return 'attention:category.decision';
    case 'blocked':
      return 'attention:category.blocked';
    case 'environment':
      return 'attention:category.environment';
    default: {
      const _exhaustive: never = category;
      return _exhaustive;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目/设备上下文帮助用户在多项目列表中定位条目。
 *
 * Code Logic（这个函数做什么）:
 *   按 project/device 是否存在拼装本地化 meta 文案。
 */
function projectDeviceLabel(
  item: AttentionItem,
  t: TFunction<readonly ['attention', 'workbench']>,
): string | null {
  const projectName = item.project?.name?.trim() ?? '';
  const deviceName = item.device?.name?.trim() ?? '';
  if (projectName && deviceName) {
    return t('attention:projectDevice', { project: projectName, device: deviceName });
  }
  if (projectName) return t('attention:projectOnly', { project: projectName });
  if (deviceName) return t('attention:deviceOnly', { device: deviceName });
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   动作文案固定按 sourceKind 生成，t() 需要字面量 key 才能通过 i18n 类型检查。
 *
 * Code Logic（这个函数做什么）:
 *   把 getAttentionActionI18nKey 结果收窄为 attention:action.* 联合类型。
 */
function attentionActionLabelKey(
  sourceKind: AttentionSourceKind,
):
  | 'attention:action.review'
  | 'attention:action.viewBlocked'
  | 'attention:action.viewFailed'
  | 'attention:action.openSettings'
  | 'attention:action.openTerminal'
  | 'attention:action.openExperiment'
  | 'attention:action.openAgentHub' {
  return getAttentionActionI18nKey(sourceKind);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   用户从导航进入“待处理”后需要看到分组列表、stale/unsupported 态，并点击进入权威界面。
 *
 * Code Logic（这个组件做什么）:
 *   读取 AttentionProvider；渲染 header/状态/分组列表；条目按钮 min-height 44px；无 Retry/Discard/Deliver。
 */
export function MobileAttentionPanel({
  onOpenItem,
  notice = null,
  now,
}: MobileAttentionPanelProps): ReactElement {
  const { t } = useTranslation(['attention', 'workbench']);
  const [includeEarlier, setIncludeEarlier] = useState(false);
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
    pendingReadIds,
    markError,
  } = useAttention();

  const clock = now ?? new Date();
  const dayPartition = useMemo(
    () => partitionAttentionItemsByLocalDay(snapshot?.items ?? [], clock),
    [snapshot?.items, clock],
  );
  const visibleItems = includeEarlier ? (snapshot?.items ?? []) : dayPartition.today;
  const groups = useMemo(() => groupAttentionItems(visibleItems), [visibleItems]);

  const unsupported =
    error instanceof AttentionHttpError && error.kind === 'unsupported';
  const showInitialLoading = loading && snapshot === null && !unsupported;
  const showInitialError =
    !loading && snapshot === null && error !== null && !unsupported;
  const showEmpty =
    !loading && snapshot !== null && snapshot.items.length === 0 && !unsupported;

  return (
    <section className={styles.panel} aria-labelledby="mobile-attention-title">
      <div className={styles.header}>
        <h1 id="mobile-attention-title" className={styles.title}>
          {t('attention:title')}
        </h1>
        <p className={styles.subtitle}>{t('attention:subtitle')}</p>
      </div>

      <div className={styles.headerActions}>
        {snapshot !== null ? (
          <button
            type="button"
            className={panelStyles.secondaryButton}
            onClick={() => {
              void markAllRead();
            }}
            disabled={
              loading ||
              refreshing ||
              pendingReadIds.size > 0 ||
              (snapshot.counts.unreadTotal ?? 0) === 0
            }
            data-testid="mobile-attention-mark-all-read"
          >
            {t('attention:table.markAllRead')}
          </button>
        ) : null}
        <button
          type="button"
          className={panelStyles.secondaryButton}
          onClick={() => {
            void refresh();
          }}
          disabled={loading || refreshing}
        >
          {snapshot ? t('attention:refresh') : t('attention:reload')}
        </button>
      </div>

      {markError ? (
        <p className={`${styles.banner} ${styles.bannerError}`} role="alert">
          {t('attention:markError')}
        </p>
      ) : null}

      {notice ? <p className={styles.banner}>{notice}</p> : null}

      {unsupported ? (
        <p className={`${styles.banner} ${styles.bannerError}`} role="status">
          {t('attention:unsupported')}
        </p>
      ) : null}

      {stale && snapshot ? (
        <p className={`${styles.banner} ${styles.bannerWarn}`} role="status">
          {t('attention:staleBanner')}
          {lastSucceededAt
            ? ` · ${t('attention:lastUpdated', { time: formatAttentionTime(lastSucceededAt) })}`
            : null}
        </p>
      ) : null}

      {showInitialLoading ? <p className={styles.state}>{t('attention:loading')}</p> : null}

      {showInitialError ? (
        <p className={`${styles.banner} ${styles.bannerError}`} role="alert">
          {t('attention:error')}
          {error?.message ? `: ${error.message}` : null}
        </p>
      ) : null}

      {dayPartition.earlier.length > 0 ? (
        <div
          className={`${styles.banner} ${styles.dayFilter}`}
          data-testid="attention-day-filter"
          role="status"
        >
          <p className={styles.state}>
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
          <button
            type="button"
            className={panelStyles.secondaryButton}
            onClick={() => setIncludeEarlier((current: boolean) => !current)}
            data-testid={includeEarlier ? 'attention-show-today-only' : 'attention-show-earlier'}
          >
            {includeEarlier
              ? t('attention:dayFilter.showTodayOnly')
              : t('attention:dayFilter.showEarlier')}
          </button>
        </div>
      ) : null}

      {showEmpty ? <p className={styles.state}>{t('attention:empty')}</p> : null}

      {groups.length > 0 ? (
        <div aria-label={t('attention:listAriaLabel')}>
          {groups.map((group) => (
            <section className={styles.group} key={group.category}>
              <h2 className={styles.groupHeading}>{t(categoryLabelKey(group.category))}</h2>
              <ul className={styles.list}>
                {group.items.map((item) => {
                  const meta = projectDeviceLabel(item, t);
                  const actionKey = attentionActionLabelKey(item.sourceKind);
                  const unread = isAttentionItemUnread(item);
                  return (
                    <li key={item.id}>
                      <div
                        className={unread ? styles.itemRow : `${styles.itemRow} ${styles.itemRowRead}`}
                        data-testid={`attention-item-${item.id}`}
                        data-read={unread ? 'false' : 'true'}
                      >
                        <button
                          type="button"
                          className={styles.itemButton}
                          onClick={() => {
                            if (unread) {
                              void markRead([item.id]);
                            }
                            onOpenItem(item);
                          }}
                          data-testid={`attention-action-${item.id}`}
                        >
                          <p className={styles.itemTitle}>{item.title}</p>
                          <p className={styles.itemSummary}>{item.summary}</p>
                          <div className={styles.metaRow}>
                            <span className={styles.metaTag}>
                              {t(categoryLabelKey(item.category))}
                            </span>
                            <span className={styles.metaTag}>
                              {item.freshness === 'cached'
                                ? t('attention:freshness.cached')
                                : t('attention:freshness.live')}
                            </span>
                            {meta ? <span>{meta}</span> : null}
                            <span>{formatAttentionTime(item.updatedAt)}</span>
                            {item.freshness === 'cached' && item.cachedAt ? (
                              <span>
                                {t('attention:cachedAt', {
                                  time: formatAttentionTime(item.cachedAt),
                                })}
                              </span>
                            ) : null}
                          </div>
                          <span className={styles.actionLabel}>{t(actionKey)}</span>
                        </button>
                        <button
                          type="button"
                          className={styles.toggleRead}
                          onClick={() => {
                            if (unread) {
                              void markRead([item.id]);
                            } else {
                              void markUnread([item.id]);
                            }
                          }}
                          disabled={pendingReadIds.has(item.id)}
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
    </section>
  );
}
