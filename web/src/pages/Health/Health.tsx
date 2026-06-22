/**
 * Health 页面 - 久坐健康提醒
 *
 * Business Logic（为什么需要这个页面）:
 *   长时间久坐工作有害健康;后端 daemon 每分钟采样键鼠活跃度,推进
 *   工作/休息状态机,连续工作达阈值触发久坐提醒(支持免打扰/暂停/贪睡/跳过)。
 *   用户需要在此页:查看当前状态相位、开关监测、手动暂停/恢复、查看今日活跃统计。
 *   Plan 1 聚焦「状态 + 开关 + 暂停 + 基础统计」闭环;完整配置表单
 *   (工作窗口/休息/免打扰/记录标题)在 Plan 2 补全为受控表单调 updateConfig。
 *
 * Code Logic（这个组件做什么）:
 *   - refresh:并行取 status + stats(startOfDay 起);每 30s 轮询刷新
 *   - 开关 enabled / 暂停 paused:乐观更新本地 status 再调后端
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Pill, Button } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { ActivityStats, ActivityDetail, HealthStatus, HealthPhase } from '@/lib/types';
import styles from './Health.module.css';
import { StatsChart } from './StatsChart';

/** 页面刷新间隔(ms) */
const REFRESH_INTERVAL_MS = 30000;

/** 一天的秒数,用于计算 startOfDay 传给 get_activity_stats */
const SECONDS_PER_DAY = 86400;

/**
 * 将运行时 phase 映射为完整静态 i18n key 字面量(i18next v26 的 t() 对动态
 * 拼接字符串无法做编译期 key 校验,故存完整 key 字面量联合,直接传给 t())。
 */
const PHASE_KEY: Record<HealthPhase, 'health:status.idle' | 'health:status.working' | 'health:status.resting'> = {
  idle: 'health:status.idle',
  working: 'health:status.working',
  resting: 'health:status.resting',
};

/**
 * Health 页面组件
 *
 * @returns Health 路由的根容器
 */
export function Health() {
  const { t } = useTranslation(['health', 'common']);
  const [status, setStatus] = useState<HealthStatus | null>(null);
  const [stats, setStats] = useState<ActivityStats | null>(null);
  const [detail, setDetail] = useState<ActivityDetail | null>(null);
  const [loading, setLoading] = useState(true);

  /**
   * 刷新状态 + 今日统计 + 今日活动明细图表。
   * startOfDay 取当日 UTC 0 点的秒级时间戳,作为 get_activity_stats / get_activity_detail 的 sinceTs。
   */
  const refresh = useCallback(async () => {
    setStatus(await healthApi.getStatus());
    const startOfDay =
      Math.floor(Date.now() / 1000) - (Math.floor(Date.now() / 1000) % SECONDS_PER_DAY);
    setStats(await healthApi.getStats(startOfDay));
    setDetail(await healthApi.getDetail(startOfDay));
    setLoading(false);
  }, []);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect,setState 在 await 后异步执行 */
  useEffect(() => {
    void refresh();
    const id = setInterval(refresh, REFRESH_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refresh]);
  /* eslint-enable react-hooks/set-state-in-effect */

  if (loading || !status) return <div className={styles.loading}>{t('common:loading')}</div>;

  /** 切换监测开关(乐观更新本地 status) */
  const toggleEnabled = async () => {
    const next = !status.enabled;
    setStatus({ ...status, enabled: next });
    await healthApi.toggleEnabled(next);
  };

  /** 切换暂停/恢复(乐观更新本地 status) */
  const togglePaused = async () => {
    const next = !status.paused;
    setStatus({ ...status, paused: next });
    await healthApi.togglePaused(next);
  };

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <h1 className={styles.title}>{t('health:title')}</h1>
        </header>

        {/* 状态 + 开关 + 暂停 */}
        <Card variant="outlined" padding="md" className={styles.section}>
          <div className={styles.statusRow}>
            <Pill tone={status.phase === 'working' ? 'accent' : 'neutral'}>
              {t(PHASE_KEY[status.phase])}
            </Pill>
            <label className={styles.toggle}>
              <input
                type="checkbox"
                checked={status.enabled}
                onChange={toggleEnabled}
                className={styles.checkbox}
              />
              {t('health:enableMonitoring')}
            </label>
            <Button
              variant="secondary"
              size="sm"
              onClick={togglePaused}
              disabled={!status.enabled}
            >
              {status.paused ? t('health:resume') : t('health:pause')}
            </Button>
          </div>
        </Card>

        {/* 今日统计 */}
        <Card variant="outlined" padding="md" className={styles.section}>
          <h3 className={styles.subtitle}>{t('health:todayStats')}</h3>
          {stats && (
            <ul className={styles.statsList}>
              <li>{t('health:activeMinutes', { n: stats.activeMinutes })}</li>
              <li>{t('health:idleMinutes', { n: stats.idleMinutes })}</li>
            </ul>
          )}
        </Card>

        {/* 今日活动明细图表(app 使用时长排行 + 24 小时活跃分布) */}
        {detail && (
          <Card variant="outlined" padding="md" className={styles.section}>
            <StatsChart detail={detail} />
          </Card>
        )}

        {/* 工作窗口/休息/通知/记录标题/免打扰 配置项:Plan 2 完善为完整表单,
            Plan 1 先留状态展示(阈值读自 status) */}
        <Card variant="outlined" padding="md" className={styles.section}>
          <h3 className={styles.subtitle}>{t('health:workWindowMinutes')}</h3>
          <p className={styles.hint}>
            {Math.round(status.workWindowSeconds / 60)} / {t('health:breakMinutes')}:
            {Math.round(status.breakSeconds / 60)}
          </p>
        </Card>
      </div>
    </div>
  );
}

Health.displayName = 'Health';
