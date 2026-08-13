import { describe, test } from 'vitest';
import { register } from 'node:module';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { HabitStats } from '../../lib/types';
import { createDefaultHealthReminders } from '../../lib/healthReminders';

register('./css-stub.mjs', import.meta.url);

describe('HabitStatsCard', () => {
  test('renders today counts and 7-bar sparkline via React SSR harness', async () => {
    const { default: i18n } = await import('../../i18n');
    await i18n.changeLanguage('zh');

    const { HabitStatsCard } = await import('./HabitStatsCard');

    const sampleStats: HabitStats = {
      todayWaterCount: 5,
      waterDailyCounts: [3, 6, 2, 5, 8, 4, 5],
      lastWaterTs: Math.floor(Date.now() / 1000) - 600,
      todayRestCount: 3,
      todayRestTotalSeconds: 720,
      todayReminderCount: 4,
      restDailyCounts: [2, 3, 1, 4, 5, 2, 3],
      templates: [
        {
          id: 'water',
          todayCompleted: 5,
          todayFired: 5,
          todayDurationSeconds: 0,
          dailyCompleted: [3, 6, 2, 5, 8, 4, 5],
          lastCompletedTs: Math.floor(Date.now() / 1000) - 600,
        },
        {
          id: 'rest',
          todayCompleted: 3,
          todayFired: 4,
          todayDurationSeconds: 720,
          dailyCompleted: [2, 3, 1, 4, 5, 2, 3],
          lastCompletedTs: null,
        },
        {
          id: 'kegel',
          todayCompleted: 1,
          todayFired: 1,
          todayDurationSeconds: 30,
          dailyCompleted: [0, 0, 0, 0, 0, 0, 1],
          lastCompletedTs: null,
        },
      ],
    };

    const rendered = renderToStaticMarkup(
      createElement(HabitStatsCard, {
        stats: sampleStats,
        reminders: createDefaultHealthReminders(),
        retainDays: 90,
        nowTs: Math.floor(Date.now() / 1000),
        onHabitAdded: () => undefined,
      }),
    );

    if (!rendered.includes('>5<')) {
      throw new Error('HabitStatsCard missing today water count');
    }

    const barMatches = rendered.match(/class="[^"]*bar[^"]*"/g) ?? [];
    if (barMatches.length < 21) {
      throw new Error(`HabitStatsCard expected >=21 bar elements, got ${barMatches.length}`);
    }

    if (!rendered.includes('>3<')) {
      throw new Error('HabitStatsCard missing today rest count');
    }
    if (!rendered.includes('data-testid="habit-block-kegel"')) {
      throw new Error('HabitStatsCard missing kegel column');
    }
  });
});
