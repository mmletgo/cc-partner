import { useState, useCallback } from 'react';
import type { HabitStats } from '@/lib/types';
import { healthApi } from '@/api/health';
import { useTranslation } from 'react-i18next';
import styles from './HabitStatsCard.module.css';

/**
 * Business Logic(为什么需要):
 *   用户在 Health 页一眼看到今日饮水/休息次数 + 近 7 天趋势,
 *   形成"习惯打卡"反馈闭环,激励坚持喝水和定时休息。
 *
 * Code Logic(做什么):
 *   展示 HabitStats 数据:两栏(饮水/休息),每栏大数字 + 小字 + 7 柱 sparkline。
 *   饮水栏含"+1 杯"按钮(带 500ms 节流防误连点,后端已用自增 id 不再丢计数)。
 *   nowTs 由父组件传入,不在 render 期调用 Date.now()。
 *   底部仅展示保留天数说明(历史记录抽屉留待 P1)。
 */
interface HabitStatsCardProps {
  stats: HabitStats | null;
  waterEnabled: boolean;
  waterIntervalSeconds: number;
  retainDays: number;
  /** 当前 Unix 秒,由父组件传入,避免 render 期调用 Date.now()。 */
  nowTs: number;
  onWaterAdded: () => void;
}

/** 计算"距下次提醒"剩余分钟数。返回 null 表示无需展示(饮水禁用)。 */
function computeNextWaterMinutes(
  lastWaterTs: number | null | undefined,
  interval: number,
  nowSec: number,
): number | null {
  const base = lastWaterTs ?? nowSec;
  const remaining = base + interval - nowSec;
  return Math.max(0, Math.ceil(remaining / 60));
}

export function HabitStatsCard({
  stats,
  waterEnabled,
  waterIntervalSeconds,
  retainDays,
  nowTs,
  onWaterAdded,
}: HabitStatsCardProps) {
  const { t } = useTranslation('health');
  const [adding, setAdding] = useState(false);

  const handleAddWater = useCallback(async () => {
    if (adding) return;
    setAdding(true);
    try {
      await healthApi.addWaterManual();
      onWaterAdded();
    } catch (e) {
      console.error('手动加计饮水失败', e);
    } finally {
      // 500ms 节流,防误连点(后端自增 id 已不再丢计数)
      setTimeout(() => setAdding(false), 500);
    }
  }, [adding, onWaterAdded]);

  if (!stats) {
    return (
      <section className={styles.card}>
        <p className={styles.blockLabel}>{t('habitStatsTitle')}</p>
        <p className={styles.blockLabel}>{t('noData')}</p>
      </section>
    );
  }

  const nowSec = nowTs; // 父组件传入,避免 render 期调用 Date.now()
  const nextWaterMin = waterEnabled
    ? computeNextWaterMinutes(stats.lastWaterTs, waterIntervalSeconds, nowSec)
    : null;
  const restTotalMinutes = Math.round(stats.todayRestTotalSeconds / 60);
  const todayWaterIdx = stats.waterDailyCounts.length - 1;
  const todayRestIdx = stats.restDailyCounts.length - 1;

  return (
    <section className={styles.card}>
      <h3 className={styles.title}>{t('habitStatsTitle')}</h3>
      <div className={styles.row}>
        {/* 饮水栏 */}
        <div className={styles.block}>
          <div className={styles.blockHead}>
            <span className={styles.blockLabel}>💧 {t('todayWater')}</span>
            <button
              className={styles.addBtn}
              onClick={handleAddWater}
              disabled={adding}
              aria-label={t('addCup')}
            >
              {t('addCup')}
            </button>
          </div>
          <div className={styles.numLine}>
            <span className={styles.numBig}>{stats.todayWaterCount}</span>
            <span className={styles.numUnit}>{t('cup')}</span>
          </div>
          {nextWaterMin !== null && (
            <div className={styles.numSub}>
              {nextWaterMin > 0
                ? t('nextWaterIn', { n: nextWaterMin })
                : t('waterOverdue')}
            </div>
          )}
          <WeekBars
            counts={stats.waterDailyCounts}
            todayIndex={todayWaterIdx}
            nowTs={nowTs}
            variant="water"
          />
        </div>

        {/* 休息栏 */}
        <div className={styles.block}>
          <div className={styles.blockHead}>
            <span className={styles.blockLabel}>🌿 {t('todayRest')}</span>
          </div>
          <div className={styles.numLine}>
            <span className={`${styles.numBig} ${styles.numBigRest}`}>
              {stats.todayRestCount}
            </span>
            <span className={styles.numUnit}>{t('times')}</span>
          </div>
          <div className={styles.numSub}>
            {t('totalRestMinutes', { n: restTotalMinutes })} ·{' '}
            <span className={styles.pip} title={t('reminderTimesToday', { n: stats.todayReminderCount })}>
              {t('reminderTimesToday', { n: stats.todayReminderCount })}
            </span>
          </div>
          <WeekBars
            counts={stats.restDailyCounts}
            todayIndex={todayRestIdx}
            nowTs={nowTs}
            variant="rest"
          />
        </div>
      </div>
      <div className={styles.footer}>
        <span>{t('habitFooter', { n: retainDays })}</span>
      </div>
    </section>
  );
}

/** 7 柱 sparkline 子组件,用纯 div bar 渲染(数据量小,不引入 recharts)。 */
interface WeekBarsProps {
  counts: number[];
  todayIndex: number;
  /** 当前 Unix 秒,用于按今日倒推 weekday label。 */
  nowTs: number;
  variant: 'water' | 'rest';
}

function WeekBars({ counts, todayIndex, nowTs, variant }: WeekBarsProps) {
  const { t } = useTranslation('health');
  const max = Math.max(1, ...counts);
  // 按今日倒推 weekday label:counts 末位对应今日,往前推是昨天、前天...
  // 后端固定返回索引 0=最早一天,末位=今日,故 label 必须随实际星期几动态生成,
  // 否则今天非周日时末位柱标会错位。
  const weekdayKeys = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'] as const;
  type WeekdayKey = (typeof weekdayKeys)[number];
  const labels: WeekdayKey[] = [];
  for (let i = counts.length - 1; i >= 0; i--) {
    const d = new Date(nowTs * 1000);
    d.setDate(d.getDate() - (counts.length - 1 - i));
    labels.unshift(weekdayKeys[d.getDay()]);
  }
  return (
    <div className={styles.week}>
      <div className={styles.weekBars}>
        {counts.map((c, i) => {
          const heightPct = c === 0 ? 0 : (c / max) * 100;
          const cls = [
            styles.bar,
            variant === 'rest' ? styles.barRest : '',
            i === todayIndex ? (variant === 'rest' ? styles.barRestToday : styles.barToday) : '',
          ]
            .filter(Boolean)
            .join(' ');
          return <div key={i} className={cls} style={{ height: `${heightPct}%` }} />;
        })}
      </div>
      <div className={styles.weekLabels}>
        {labels.map((label, i) => {
          const isToday = i === todayIndex;
          const cls = [
            styles.weekLabel,
            isToday ? (variant === 'rest' ? styles.weekLabelTodayRest : styles.weekLabelToday) : '',
          ]
            .filter(Boolean)
            .join(' ');
          return (
            <span key={label} className={cls}>
              {isToday ? t('today') : t(label)}
            </span>
          );
        })}
      </div>
    </div>
  );
}
