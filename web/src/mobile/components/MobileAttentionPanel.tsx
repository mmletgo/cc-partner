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

import { useMemo } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { AttentionHttpError } from '@/api/attentionHttp';
import { useAttention } from '@/hooks/attentionContext';
import {
  getAttentionActionI18nKey,
  groupAttentionItems,
} from '@/lib/attention';
import type { AttentionCategory, AttentionItem, AttentionSourceKind } from '@/lib/types';
import panelStyles from '../MobileWorkbench.module.css';
import styles from './MobileAttentionPanel.module.css';

export interface MobileAttentionPanelProps {
  onOpenItem: (item: AttentionItem) => void;
  notice?: string | null;
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
  | 'attention:action.openSettings' {
  return getAttentionActionI18nKey(sourceKind) as
    | 'attention:action.review'
    | 'attention:action.viewBlocked'
    | 'attention:action.viewFailed'
    | 'attention:action.openSettings';
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
}: MobileAttentionPanelProps): ReactElement {
  const { t } = useTranslation(['attention', 'workbench']);
  const {
    snapshot,
    loading,
    refreshing,
    stale,
    error,
    lastSucceededAt,
    refresh,
  } = useAttention();

  const groups = useMemo(
    () => groupAttentionItems(snapshot?.items ?? []),
    [snapshot?.items],
  );

  const unsupported =
    error instanceof AttentionHttpError && error.kind === 'unsupported';
  const showInitialLoading = loading && snapshot === null && !unsupported;
  const showInitialError =
    !loading && snapshot === null && error !== null && !unsupported;
  const showEmpty =
    !loading && snapshot !== null && groups.length === 0 && !unsupported;

  return (
    <section className={styles.panel} aria-labelledby="mobile-attention-title">
      <div className={styles.header}>
        <p className={styles.kicker}>{t('workbench:mobile.kicker')}</p>
        <h1 id="mobile-attention-title" className={styles.title}>
          {t('attention:title')}
        </h1>
        <p className={styles.subtitle}>{t('attention:subtitle')}</p>
      </div>

      <div className={styles.headerActions}>
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
                  return (
                    <li key={item.id}>
                      <button
                        type="button"
                        className={styles.itemButton}
                        onClick={() => onOpenItem(item)}
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
                        <p className={styles.actionLabel}>{t(actionKey)}</p>
                      </button>
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
