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
 *   - refresh:并行取 status + stats + detail(startOfDay 起);每 30s 轮询刷新
 *   - 将运行状态派生成概览卡片、进度条、指标网格和图表面板
 *   - 开关 enabled / 暂停 paused:乐观更新本地 status,后端失败回滚
 *   - hooks 全部在 early return 之前(项目规则 20)
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Button, Card, Pill, ProgressBar } from '@/components/primitives';
import type { PillTone } from '@/components/primitives';
import { healthApi } from '@/api/health';
import type { ActivityStats, ActivityDetail, HealthStatus, HealthPhase } from '@/lib/types';
import { HealthIcon, PauseIcon, PlayIcon } from '@/lib/icons';
import styles from './Health.module.css';
import { StatsChart } from './StatsChart';

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
 * Health 页面组件
 *
 * @returns Health 路由的根容器
 */
export function Health() {
  const { t } = useTranslation(['health', 'common']);
  const navigate = useNavigate();
  const [status, setStatus] = useState<HealthStatus | null>(null);
  const [stats, setStats] = useState<ActivityStats | null>(null);
  const [detail, setDetail] = useState<ActivityDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [nowTs, setNowTs] = useState(() => Math.floor(Date.now() / 1000));

  /**
   * 刷新状态 + 今日统计 + 今日活动明细图表。
   * startOfDay 取「本地当日 0 点」的秒级时间戳(先把 Date 的时/分/秒/毫秒清零,再取整秒),
   * 作为 get_activity_stats / get_activity_detail 的 sinceTs。
   */
  const refresh = useCallback(async () => {
    const startOfDay = getLocalStartOfDayTs();
    const [nextStatus, nextStats, nextDetail] = await Promise.all([
      healthApi.getStatus(),
      healthApi.getStats(startOfDay),
      healthApi.getDetail(startOfDay),
    ]);
    setStatus(nextStatus);
    setStats(nextStats);
    setDetail(nextDetail);
    setNowTs(Math.floor(Date.now() / 1000));
    setLoading(false);
  }, []);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect,setState 在 await 后异步执行 */
  useEffect(() => {
    void refresh();
    const id = setInterval(refresh, REFRESH_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refresh]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /** 切换监测开关:乐观更新本地 status,后端失败时回滚 enabled 并提示 */
  const toggleEnabled = useCallback(async () => {
    if (!status) return;
    const prev = status.enabled;
    const next = !prev;
    setStatus({ ...status, enabled: next });
    try {
      await healthApi.toggleEnabled(next);
    } catch (e) {
      console.error('toggle_health_enabled failed, rolling back', e);
      setStatus((s) => (s ? { ...s, enabled: prev } : s));
    }
  }, [status]);

  /** 切换暂停/恢复:乐观更新本地 status,后端失败时回滚 paused 并提示 */
  const togglePaused = useCallback(async () => {
    if (!status) return;
    const prev = status.paused;
    const next = !prev;
    setStatus({ ...status, paused: next });
    try {
      await healthApi.togglePaused(next);
    } catch (e) {
      console.error('toggle_health_paused failed, rolling back', e);
      setStatus((s) => (s ? { ...s, paused: prev } : s));
    }
  }, [status]);

  if (loading || !status) return <div className={styles.loading}>{t('common:loading')}</div>;

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

        {detail && (
          <Card variant="outlined" padding="md" className={styles.chartCard}>
            <Card.Header className={styles.cardHeader}>
              <div className={styles.cardTitleGroup}>
                <h2 className={styles.sectionTitle}>{t('health:chartsTitle')}</h2>
                <p className={styles.sectionLead}>{t('health:chartsLead')}</p>
              </div>
            </Card.Header>
            <Card.Body className={styles.chartBody}>
              <StatsChart detail={detail} />
            </Card.Body>
          </Card>
        )}
      </div>
    </div>
  );
}

Health.displayName = 'Health';
