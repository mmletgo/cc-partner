import { describe, test } from 'vitest';
import { register } from 'node:module';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { HabitStats } from '../../lib/types';

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
    };

    const rendered = renderToStaticMarkup(
      createElement(HabitStatsCard, {
        stats: sampleStats,
        waterEnabled: true,
        waterIntervalSeconds: 3600,
        retainDays: 90,
        nowTs: Math.floor(Date.now() / 1000),
        onWaterAdded: () => undefined,
      }),
    );

    // 断言 1: 标题渲染(需要 i18n 已有 habitStatsTitle key,Task 10 才会加;
    // 在 Task 10 之前测试会渲染 key 本身如 "habitStatsTitle",这正常。
    // 这里先断言组件渲染不抛错 + 数字渲染)
    if (!rendered.includes('>5<')) {
      throw new Error('HabitStatsCard missing today water count');
    }

    // 断言 2: 7 柱 sparkline 渲染(饮水 7 柱 + 休息 7 柱 = 至少 14 个 bar class)
    const barMatches = rendered.match(/class="[^"]*bar[^"]*"/g) ?? [];
    if (barMatches.length < 14) {
      throw new Error(`HabitStatsCard expected >=14 bar elements, got ${barMatches.length}`);
    }

    // 断言 3: 休息次数渲染
    if (!rendered.includes('>3<')) {
      throw new Error('HabitStatsCard missing today rest count');
    }
  });
});
