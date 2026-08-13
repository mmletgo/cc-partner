/**
 * ActivityStats 页面 - 今日活动统计
 *
 * Business Logic（为什么需要这个页面）:
 *   应用/窗口排行与小时分布是回顾性数据，不应再嵌在健康提醒控制台里。
 *
 * Code Logic（这个组件做什么）:
 *   可见性轮询 get_activity_detail；首屏失败可重试，刷新失败保留图表 + stale 横幅。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Button, Card } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { ActivityDetail } from '@/lib/types';
import { ActivityIcon } from '@/lib/icons';
import { StatsChart } from '@/pages/Health/StatsChart';
import styles from './ActivityStats.module.css';

/** 页面网络刷新间隔(ms) */
const REFRESH_INTERVAL_MS = 5000;

/**
 * 计算本地当天 0 点秒级时间戳
 *
 * Business Logic（为什么需要这个函数）:
 *   今日统计应按用户本地时区计算。
 *
 * Code Logic（这个函数做什么）:
 *   创建当前 Date,清零本地时分秒毫秒后转换为 Unix 秒。
 */
function getLocalStartOfDayTs(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

/**
 * ActivityStats 页面组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要独立入口查看今日应用/窗口/小时活跃分布，并跳转统计设置。
 *
 * Code Logic（这个组件做什么）:
 *   轮询 detail；hooks 全部在 early return 之前。
 */
export function ActivityStats() {
  const { t } = useTranslation(['health', 'common']);
  const navigate = useNavigate();
  const [detail, setDetail] = useState<ActivityDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const detailRef = useRef<ActivityDetail | null>(null);

  useEffect(() => {
    detailRef.current = detail;
  }, [detail]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   首屏失败必须可重试；刷新失败保留已有图表。
   *
   * Code Logic（这个函数做什么）:
   *   取 startOfDay 后拉 getDetail；成功清 refreshError，失败按是否已有 detail 分 stale/首屏。
   */
  const refresh = useCallback(async () => {
    try {
      const next = await healthApi.getDetail(getLocalStartOfDayTs());
      setDetail(next);
      setRefreshError(null);
    } catch (e) {
      console.error('加载活动明细失败', e);
      setRefreshError(
        detailRef.current ? t('health:staleRefreshFailed') : t('health:loadFailed'),
      );
    } finally {
      setLoading(false);
    }
  }, [t]);

  const { runNow } = useVisibilityPolling(refresh, {
    intervalMs: REFRESH_INTERVAL_MS,
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   首屏失败与 stale 横幅上的「重试」需立即强制刷新。
   *
   * Code Logic（这个函数做什么）:
   *   force runNow 并对 rejection 静默。
   */
  const handleRetryRefresh = useCallback(() => {
    void runNow({ force: true }).catch(() => undefined);
  }, [runNow]);

  if (loading) {
    return <div className={styles.loading}>{t('common:loading')}</div>;
  }

  if (!detail) {
    return (
      <div className={styles.errorPanel} role="alert">
        <p className={styles.errorText}>{refreshError ?? t('health:loadFailed')}</p>
        <Button variant="secondary" size="md" onClick={handleRetryRefresh}>
          {t('common:action.retry')}
        </Button>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.headerText}>
            <span className={styles.eyebrow}>{t('health:activityEyebrow')}</span>
            <h1 className={styles.title}>{t('health:activityTitle')}</h1>
            <p className={styles.lead}>{t('health:activityLead')}</p>
          </div>
          <div className={styles.headerActions}>
            <Button
              variant="secondary"
              size="md"
              icon={<ActivityIcon />}
              onClick={() => navigate('/settings?tab=activity')}
            >
              {t('health:goToActivitySettings')}
            </Button>
          </div>
        </header>

        {refreshError ? (
          <div className={styles.staleBanner} role="status" data-testid="activity-stale-banner">
            <p className={styles.staleText}>{refreshError}</p>
            <Button variant="secondary" size="sm" onClick={handleRetryRefresh}>
              {t('common:action.retry')}
            </Button>
          </div>
        ) : null}

        <Card variant="outlined" padding="md" className={styles.chartCard}>
          <Card.Body className={styles.chartBody}>
            <StatsChart detail={detail} />
          </Card.Body>
        </Card>
      </div>
    </div>
  );
}

ActivityStats.displayName = 'ActivityStats';
