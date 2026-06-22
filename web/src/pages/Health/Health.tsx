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
import { Settings } from './Settings';

/** 页面刷新间隔(ms) */
const REFRESH_INTERVAL_MS = 30000;

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
   * startOfDay 取「本地当日 0 点」的秒级时间戳(先把 Date 的时/分/秒/毫秒清零,再取整秒),
   * 作为 get_activity_stats / get_activity_detail 的 sinceTs。
   */
  const refresh = useCallback(async () => {
    setStatus(await healthApi.getStatus());
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    const startOfDay = Math.floor(d.getTime() / 1000);
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

  /** 切换监测开关:乐观更新本地 status,后端失败时回滚 enabled 并提示 */
  const toggleEnabled = async () => {
    const prev = status.enabled;
    const next = !prev;
    setStatus({ ...status, enabled: next });
    try {
      await healthApi.toggleEnabled(next);
    } catch (e) {
      console.error('toggle_health_enabled failed, rolling back', e);
      setStatus((s) => (s ? { ...s, enabled: prev } : s));
    }
  };

  /** 切换暂停/恢复:乐观更新本地 status,后端失败时回滚 paused 并提示 */
  const togglePaused = async () => {
    const prev = status.paused;
    const next = !prev;
    setStatus({ ...status, paused: next });
    try {
      await healthApi.togglePaused(next);
    } catch (e) {
      console.error('toggle_health_paused failed, rolling back', e);
      setStatus((s) => (s ? { ...s, paused: prev } : s));
    }
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

        {/* 完整配置表单(Plan 2 Task 5):工作窗口/休息/通知/全屏/记录标题/喝水/免打扰/保留天数/总开关,
            受控表单每次提交完整 HealthConfig 对象,避免整体覆盖式回写清零未传字段 */}
        <Settings />
      </div>
    </div>
  );
}

Health.displayName = 'Health';
