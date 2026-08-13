/**
 * Health 页面 - 久坐健康提醒
 *
 * Business Logic（为什么需要这个页面）:
 *   长时间久坐工作有害健康;后端 daemon 每分钟采样键鼠活跃度,推进
 *   工作/休息状态机,连续工作达阈值触发久坐提醒(支持免打扰/暂停/贪睡/跳过)。
 *   用户需要在此页快速判断监测是否正常、连续工作是否接近提醒阈值、
 *   今日活跃/休息占比如何,并能直接进入完整配置项。
 *
 * Code Logic（这个组件做什么）:
 *   - refresh:并行取 status + stats(startOfDay 起);每 5s 可见性感知轮询
 *   - 首屏 status 失败展示可重试错误；刷新失败保留数据 + stale 横幅
 *   - 开关 enabled / 暂停 paused:乐观更新本地 status,后端失败回滚并可见提示
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Button, Card, Pill, ProgressBar } from '@/components/primitives';
import type { PillTone } from '@/components/primitives';
import { healthApi } from '@/api/health';
import { createDefaultHealthReminders } from '@/lib/healthReminders';
import type { ActivityStats, HealthStatus, HealthPhase, HabitStats, HealthConfig } from '@/lib/types';
import { HealthIcon, PauseIcon, PlayIcon } from '@/lib/icons';
import styles from './Health.module.css';
import { HabitStatsCard } from './HabitStatsCard';

/** 页面网络刷新间隔(ms)；HealthOverlay 本地倒计时不属于本轮询 */
const REFRESH_INTERVAL_MS = 5000;

/**
 * 将运行时 phase 映射为完整静态 i18n key 字面量(i18next v26 的 t() 对动态
 * 拼接字符串无法做编译期 key 校验,故存完整 key 字面量联合,直接传给 t())。
 */
const PHASE_KEY: Record<HealthPhase, 'health:status.idle' | 'health:status.working' | 'health:status.resting'> = {
  idle: 'health:status.idle',
  working: 'health:status.working',
  resting: 'health:status.resting',
};

/** 当前相位对应的设计系统状态色 */
const PHASE_TONE: Record<HealthPhase, PillTone> = {
  idle: 'neutral',
  working: 'accent',
  resting: 'success',
};

type MonitoringKey = 'health:monitoringOn' | 'health:monitoringOff' | 'health:monitoringPaused';

/**
 * 根据运行时状态派生监测总开关文案 key
 *
 * Business Logic（为什么需要这个函数）:
 *   用户在概览区需要先看到监测是否可用,再看相位。enabled/paused 的组合比单纯 phase
 *   更能表达当前健康提醒是否真的在工作。
 *
 * Code Logic（这个函数做什么）:
 *   接收 HealthStatus,按 disabled > paused > enabled 的优先级返回静态 i18n key。
 */
const getMonitoringKey = (current: HealthStatus): MonitoringKey => {
  if (!current.enabled) return 'health:monitoringOff';
  if (current.paused) return 'health:monitoringPaused';
  return 'health:monitoringOn';
};

/**
 * 把秒数转换成向上取整的分钟数
 *
 * Business Logic（为什么需要这个函数）:
 *   健康提醒界面面向用户展示分钟粒度即可,不需要暴露后端秒级采样细节。
 *
 * Code Logic（这个函数做什么）:
 *   接收秒数,对正数向上取整为分钟;0 或负数返回 0。
 */
const toMinutes = (seconds: number): number => {
  if (seconds <= 0) return 0;
  return Math.ceil(seconds / 60);
};

/**
 * 计算本地当天 0 点秒级时间戳
 *
 * Business Logic（为什么需要这个函数）:
 *   今日统计应按用户本地时区计算,否则跨时区或 UTC 0 点会导致当天数据错位。
 *
 * Code Logic（这个函数做什么）:
 *   创建当前 Date,清零本地时分秒毫秒后转换为 Unix 秒。
 */
const getLocalStartOfDayTs = (): number => {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
};

/**
 * 把秒级时间戳格式化成本地 HH:MM
 *
 * Business Logic（为什么需要这个函数）:
 *   贪睡中的健康提醒需要告诉用户提醒恢复的具体本地时间。
 *
 * Code Logic（这个函数做什么）:
 *   接收 Unix 秒,使用浏览器本地语言环境输出 2 位小时/分钟。
 */
const formatClock = (seconds: number): string => {
  return new Date(seconds * 1000).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
  });
};

/**
 * Business Logic（为什么需要这个函数）:
 *   错误对象形态不统一，UI 需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message，其余 String()。
 */
function healthErrorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Health 页面组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要看到健康监测是否正常、连续工作是否接近阈值，并能进入配置。
 *
 * Code Logic（这个组件做什么）:
 *   可见性轮询 status/stats；首屏失败展示可重试错误；刷新失败保留数据 + stale 横幅。
 *
 * @returns Health 路由的根容器
 */
export function Health() {
  const { t } = useTranslation(['health', 'common']);
  const navigate = useNavigate();
  const [status, setStatus] = useState<HealthStatus | null>(null);
  const [stats, setStats] = useState<ActivityStats | null>(null);
  const [config, setConfig] = useState<HealthConfig | null>(null);
  const [habitStats, setHabitStats] = useState<HabitStats | null>(null);
  const [loading, setLoading] = useState(true);
  /** 首屏/刷新失败可见文案；有 status 时表示 stale */
  const [refreshError, setRefreshError] = useState<string | null>(null);
  /** toggle 失败用户可见提示 */
  const [actionError, setActionError] = useState<string | null>(null);
  const [nowTs, setNowTs] = useState(() => Math.floor(Date.now() / 1000));
  /** 同步读取最新 status，供 refresh 失败分支判断 stale vs 首屏失败 */
  const statusRef = useRef<HealthStatus | null>(null);

  useEffect(() => {
    statusRef.current = status;
  }, [status]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要看到健康监测是否正常；首屏全失败必须可重试，刷新失败保留数据并标 stale。
   *
   * Code Logic（这个函数做什么）:
   *   并行取 status/stats + config/habit；status 成功清 refreshError；
   *   status 失败：有旧 status 则 stale 文案，否则 loadFailed；始终结束 loading。
   */
  const refresh = useCallback(async () => {
    const startOfDay = getLocalStartOfDayTs();
    const [statusRes, statsRes] = await Promise.allSettled([
      healthApi.getStatus(),
      healthApi.getStats(startOfDay),
    ]);
    if (statusRes.status === 'fulfilled') {
      setStatus(statusRes.value);
      setRefreshError(null);
    } else {
      console.error('加载健康状态失败', statusRes.reason);
      setRefreshError(
        statusRef.current ? t('health:staleRefreshFailed') : t('health:loadFailed'),
      );
    }
    if (statsRes.status === 'fulfilled') {
      setStats(statsRes.value);
    } else {
      console.error('加载今日统计失败', statsRes.reason);
    }

    try {
      const [nextConfig, nextHabit] = await Promise.all([
        healthApi.getConfig(),
        healthApi.getHabitStats(7),
      ]);
      setConfig(nextConfig);
      setHabitStats(nextHabit);
    } catch (e) {
      console.error('加载习惯统计失败', e);
    }

    setNowTs(Math.floor(Date.now() / 1000));
    setLoading(false);
  }, [t]);

  // 健康页网络刷新：5s 可见性感知轮询；HealthOverlay 本地倒计时不变
  const { runNow: runHealthNow } = useVisibilityPolling(refresh, {
    intervalMs: REFRESH_INTERVAL_MS,
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   首屏失败与 stale 横幅上的「重试」需立即强制刷新。
   *
   * Code Logic（这个函数做什么）:
   *   force runNow 并对 rejection 静默（错误已写 refreshError）。
   */
  const handleRetryRefresh = useCallback(() => {
    void runHealthNow({ force: true }).catch(() => undefined);
  }, [runHealthNow]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   切换监测开关必须乐观更新；失败回滚并给用户可见错误。
   *
   * Code Logic（这个函数做什么）:
   *   记 prev、写 enabled、await API；失败回滚并 setActionError。
   */
  const toggleEnabled = useCallback(async () => {
    if (!status) return;
    const prev = status.enabled;
    const next = !prev;
    setActionError(null);
    setStatus({ ...status, enabled: next });
    try {
      await healthApi.toggleEnabled(next);
    } catch (e) {
      console.error('toggle_health_enabled failed, rolling back', e);
      setStatus((s) => (s ? { ...s, enabled: prev } : s));
      setActionError(t('health:toggleFailed', { error: healthErrorMessage(e) }));
    }
  }, [status, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   暂停/恢复同样需乐观更新与失败可见。
   *
   * Code Logic（这个函数做什么）:
   *   记 prev、写 paused、await API；失败回滚并 setActionError。
   */
  const togglePaused = useCallback(async () => {
    if (!status) return;
    const prev = status.paused;
    const next = !prev;
    setActionError(null);
    setStatus({ ...status, paused: next });
    try {
      await healthApi.togglePaused(next);
    } catch (e) {
      console.error('toggle_health_paused failed, rolling back', e);
      setStatus((s) => (s ? { ...s, paused: prev } : s));
      setActionError(t('health:toggleFailed', { error: healthErrorMessage(e) }));
    }
  }, [status, t]);

  // hooks 全部在 early return 之前
  if (loading) {
    return <div className={styles.loading}>{t('common:loading')}</div>;
  }

  if (!status) {
    return (
      <div className={styles.errorPanel} role="alert">
        <p className={styles.errorText}>{refreshError ?? t('health:loadFailed')}</p>
        <Button variant="secondary" size="md" onClick={handleRetryRefresh}>
          {t('common:action.retry')}
        </Button>
      </div>
    );
  }

  const elapsedSeconds = status.windowStartTs ? Math.max(0, nowTs - status.windowStartTs) : 0;
  const workProgress = status.workWindowSeconds > 0 ? elapsedSeconds / status.workWindowSeconds : 0;
  const remainingSeconds = Math.max(0, status.workWindowSeconds - elapsedSeconds);
  const activeMinutes = stats?.activeMinutes ?? 0;
  const idleMinutes = stats?.idleMinutes ?? 0;
  const totalTrackedMinutes = activeMinutes + idleMinutes;
  const activeShare = totalTrackedMinutes > 0 ? Math.round((activeMinutes / totalTrackedMinutes) * 100) : 0;
  const snoozeLabel = status.snoozeUntil && status.snoozeUntil > nowTs
    ? t('health:snoozeUntil', { time: formatClock(status.snoozeUntil) })
    : null;

  return (
    <div className={styles.page}>
      <div className={styles.container}>
        <header className={styles.header}>
          <div className={styles.headerText}>
            <span className={styles.eyebrow}>{t('health:eyebrow')}</span>
            <h1 className={styles.title}>{t('health:title')}</h1>
            <p className={styles.lead}>{t('health:lead')}</p>
          </div>
          <div className={styles.headerActions}>
            <Button
              variant="secondary"
              size="md"
              onClick={() => navigate('/settings?tab=health')}
            >
              {t('health:goToSettings')}
            </Button>
            <Button
              variant={status.enabled ? 'secondary' : 'primary'}
              size="md"
              icon={<HealthIcon />}
              onClick={toggleEnabled}
            >
              {status.enabled ? t('health:disableMonitoring') : t('health:enableMonitoring')}
            </Button>
          </div>
        </header>

        {refreshError ? (
          <div className={styles.staleBanner} role="status" data-testid="health-stale-banner">
            <p className={styles.staleText}>{refreshError}</p>
            <Button variant="secondary" size="sm" onClick={handleRetryRefresh}>
              {t('common:action.retry')}
            </Button>
          </div>
        ) : null}

        {actionError ? (
          <p className={styles.toggleError} role="alert" data-testid="health-toggle-error">
            {actionError}
          </p>
        ) : null}

        <Card variant="outlined" padding="md" className={styles.overviewCard}>
          <Card.Header className={styles.cardHeader}>
            <div className={styles.cardTitleGroup}>
              <h2 className={styles.sectionTitle}>{t('health:statusOverview')}</h2>
              <p className={styles.sectionLead}>{t(getMonitoringKey(status))}</p>
            </div>
            <Button
              variant="secondary"
              size="sm"
              icon={status.paused ? <PlayIcon /> : <PauseIcon />}
              onClick={togglePaused}
              disabled={!status.enabled}
            >
              {status.paused ? t('health:resume') : t('health:pause')}
            </Button>
          </Card.Header>
          <Card.Body className={styles.overviewBody}>
            <div className={styles.statusPanel}>
              <div className={styles.statusPills}>
                <Pill tone={PHASE_TONE[status.phase]} dot>
                  {t(PHASE_KEY[status.phase])}
                </Pill>
                {snoozeLabel ? <Pill tone="warn">{snoozeLabel}</Pill> : null}
              </div>
              <div className={styles.phaseName}>{t(getMonitoringKey(status))}</div>
              <div className={styles.progressBlock}>
                <div className={styles.progressMeta}>
                  <span>{t('health:workProgress')}</span>
                  <span>{t('health:remainingToReminder', { n: toMinutes(remainingSeconds) })}</span>
                </div>
                <ProgressBar
                  value={workProgress}
                  tone={status.phase === 'working' ? 'accent' : 'success'}
                  size="lg"
                />
              </div>
              <p className={styles.statusHint}>
                {status.windowStartTs
                  ? t('health:elapsedWork', { n: toMinutes(elapsedSeconds) })
                  : t('health:noActiveWindow')}
              </p>
            </div>

            <div className={styles.metricGrid}>
              <div className={styles.metricTile}>
                <span className={styles.metricLabel}>{t('health:activeToday')}</span>
                <strong className={styles.metricValue}>{t('health:minutesValue', { n: activeMinutes })}</strong>
              </div>
              <div className={styles.metricTile}>
                <span className={styles.metricLabel}>{t('health:idleToday')}</span>
                <strong className={styles.metricValue}>{t('health:minutesValue', { n: idleMinutes })}</strong>
              </div>
              <div className={styles.metricTile}>
                <span className={styles.metricLabel}>{t('health:activeShare')}</span>
                <strong className={styles.metricValue}>{t('health:percentValue', { n: activeShare })}</strong>
              </div>
              <div className={styles.metricTile}>
                <span className={styles.metricLabel}>{t('health:workWindow')}</span>
                <strong className={styles.metricValue}>{t('health:minutesValue', { n: toMinutes(status.workWindowSeconds) })}</strong>
              </div>
              <div className={styles.metricTile}>
                <span className={styles.metricLabel}>{t('health:breakThreshold')}</span>
                <strong className={styles.metricValue}>{t('health:minutesValue', { n: toMinutes(status.breakSeconds) })}</strong>
              </div>
            </div>
          </Card.Body>
        </Card>

        <HabitStatsCard
          stats={habitStats}
          reminders={config?.reminders?.length ? config.reminders : createDefaultHealthReminders()}
          retainDays={config?.retainDays ?? 90}
          nowTs={nowTs}
          onHabitAdded={refresh}
        />
      </div>
    </div>
  );
}

Health.displayName = 'Health';
