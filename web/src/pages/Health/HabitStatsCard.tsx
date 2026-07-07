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
 *   饮水栏含"+1 杯"按钮(带 500ms 节流防同秒主键冲突)。
 *   底部"查看历史记录"链接(暂为占位,P1 增量做删除 UI)。
 */
interface HabitStatsCardProps {
  stats: HabitStats | null;
  waterEnabled: boolean;
  waterIntervalSeconds: number;
  retainDays: number;
  onWaterAdded: () => void;
}

/** 计算"距下次提醒"剩余分钟数。返回 null 表示无需展示(饮水禁用)。 */
function computeNextWaterMinutes(
  lastWaterTs: number | undefined,
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
      // 500ms 节流,防止同秒 ts 主键冲突
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

  const nowSec = Math.floor(Date.now() / 1000);
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
            variant="rest"
          />
        </div>
      </div>
      <div className={styles.footer}>
        <span>{t('habitFooter', { n: retainDays })}</span>
        <span className={styles.footerLink}>{t('viewHistory')}</span>
      </div>
    </section>
  );
}

/** 7 柱 sparkline 子组件,用纯 div bar 渲染(数据量小,不引入 recharts)。 */
interface WeekBarsProps {
  counts: number[];
  todayIndex: number;
  variant: 'water' | 'rest';
}

function WeekBars({ counts, todayIndex, variant }: WeekBarsProps) {
  const { t } = useTranslation('health');
  const max = Math.max(1, ...counts);
  const labels = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
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
