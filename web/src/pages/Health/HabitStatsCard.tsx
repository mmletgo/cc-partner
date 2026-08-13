import { useState, useCallback } from 'react';
import type { HabitStats, HealthReminderTemplate, TemplateHabitStats } from '@/lib/types';
import { healthApi } from '@/api/health';
import { useTranslation } from 'react-i18next';
import styles from './HabitStatsCard.module.css';

/**
 * Business Logic(为什么需要):
 *   用户在 Health 页按已启用模板看到今日完成次数 + 近 7 天趋势。
 *
 * Code Logic(做什么):
 *   按 templates 渲染多栏;instant 显示 +1;interval 显示距下次。
 */
interface HabitStatsCardProps {
  stats: HabitStats | null;
  reminders: HealthReminderTemplate[];
  retainDays: number;
  /** 当前 Unix 秒,由父组件传入,避免 render 期调用 Date.now()。 */
  nowTs: number;
  onHabitAdded: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   interval 模板需要告诉用户还有多久到下次。
 *
 * Code Logic（这个函数做什么）:
 *   lastCompleted + interval - now，向下取整到分钟且不小于 0。
 */
function computeNextMinutes(
  lastCompletedTs: number | null | undefined,
  interval: number,
  nowSec: number,
): number {
  const base = lastCompletedTs ?? nowSec;
  return Math.max(0, Math.ceil((base + interval - nowSec) / 60));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧 HabitStats 可能还没有 templates[]，要用兼容字段兜底出厂两项。
 *
 * Code Logic（这个函数做什么）:
 *   优先 stats.templates；否则用水/休息聚合字段合成。
 */
function statsForTemplate(stats: HabitStats, id: string): TemplateHabitStats {
  const found = stats.templates?.find((item) => item.id === id);
  if (found) return found;
  if (id === 'water') {
    return {
      id,
      todayCompleted: stats.todayWaterCount,
      todayFired: 0,
      todayDurationSeconds: 0,
      dailyCompleted: stats.waterDailyCounts,
      lastCompletedTs: stats.lastWaterTs ?? null,
    };
  }
  if (id === 'rest') {
    return {
      id,
      todayCompleted: stats.todayRestCount,
      todayFired: stats.todayReminderCount,
      todayDurationSeconds: stats.todayRestTotalSeconds,
      dailyCompleted: stats.restDailyCounts,
      lastCompletedTs: null,
    };
  }
  return {
    id,
    todayCompleted: 0,
    todayFired: 0,
    todayDurationSeconds: 0,
    dailyCompleted: Array.from({ length: 7 }, () => 0),
    lastCompletedTs: null,
  };
}

export function HabitStatsCard({
  stats,
  reminders,
  retainDays,
  nowTs,
  onHabitAdded,
}: HabitStatsCardProps) {
  const { t } = useTranslation('health');
  const [addingId, setAddingId] = useState<string | null>(null);
  const enabled = reminders.filter((item) => item.enabled);

  const handleAdd = useCallback(
    async (templateId: string) => {
      if (addingId) return;
      setAddingId(templateId);
      try {
        await healthApi.addHabitManual(templateId);
        onHabitAdded();
      } catch (e) {
        console.error('手动加计习惯失败', e);
      } finally {
        setTimeout(() => setAddingId(null), 500);
      }
    },
    [addingId, onHabitAdded],
  );

  if (!stats) {
    return (
      <section className={styles.card}>
        <p className={styles.blockLabel}>{t('habitStatsTitle')}</p>
        <p className={styles.blockLabel}>{t('noData')}</p>
      </section>
    );
  }

  return (
    <section className={styles.card}>
      <h3 className={styles.title}>{t('habitStatsTitle')}</h3>
      <div className={enabled.length > 2 ? styles.rowWide : styles.row}>
        {enabled.map((reminder) => {
          const item = statsForTemplate(stats, reminder.id);
          const nextMin =
            reminder.trigger === 'interval' && reminder.intervalSeconds
              ? computeNextMinutes(item.lastCompletedTs, reminder.intervalSeconds, nowTs)
              : null;
          const durationMin = Math.round(item.todayDurationSeconds / 60);
          return (
            <div key={reminder.id} className={styles.block} data-testid={`habit-block-${reminder.id}`}>
              <div className={styles.blockHead}>
                <span className={styles.blockLabel}>{t('todayCount', { name: reminder.name })}</span>
                {reminder.complete === 'instant' ? (
                  <button
                    className={styles.addBtn}
                    onClick={() => void handleAdd(reminder.id)}
                    disabled={addingId === reminder.id}
                    aria-label={t('addOne', { unit: reminder.unitLabel || t('times') })}
                  >
                    {t('addOne', { unit: reminder.unitLabel || t('times') })}
                  </button>
                ) : null}
              </div>
              <div className={styles.numLine}>
                <span className={reminder.complete === 'session' ? `${styles.numBig} ${styles.numBigRest}` : styles.numBig}>
                  {item.todayCompleted}
                </span>
                <span className={styles.numUnit}>{reminder.unitLabel || t('times')}</span>
              </div>
              {nextMin !== null ? (
                <div className={styles.numSub}>
                  {nextMin > 0 ? t('nextIn', { n: nextMin }) : t('overdue')}
                </div>
              ) : null}
              {reminder.complete === 'session' ? (
                <div className={styles.numSub}>
                  {t('totalDurationMinutes', { n: durationMin })}
                  {item.todayFired > 0 ? ` · ${t('firedTimesToday', { n: item.todayFired })}` : ''}
                </div>
              ) : null}
              <WeekBars
                counts={item.dailyCompleted}
                todayIndex={item.dailyCompleted.length - 1}
                nowTs={nowTs}
                variant={reminder.complete === 'session' ? 'rest' : 'water'}
              />
            </div>
          );
        })}
      </div>
      <div className={styles.footer}>
        <span>{t('habitFooter', { n: retainDays })}</span>
      </div>
    </section>
  );
}

interface WeekBarsProps {
  counts: number[];
  todayIndex: number;
  nowTs: number;
  variant: 'water' | 'rest';
}

function WeekBars({ counts, todayIndex, nowTs, variant }: WeekBarsProps) {
  const { t } = useTranslation('health');
  const max = Math.max(1, ...counts);
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
            <span key={`${label}-${i}`} className={cls}>
              {isToday ? t('today') : t(label)}
            </span>
          );
        })}
      </div>
    </div>
  );
}
