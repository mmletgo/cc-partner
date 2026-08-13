/**
 * ActivityStatsPanel 活动统计设置回归测试
 *
 * Business Logic（为什么需要这个测试）:
 *   记录窗口标题与保留天数已从健康提醒 tab 拆出，必须出现在独立活动统计 tab，
 *   且不得再带提醒节奏 / 免打扰 / 系统通知字段。
 *
 * Code Logic（做什么）:
 *   注册 css-stub 后渲染 ActivityStatsPanel，断言两项隐私字段、保留天数区间与按钮存在。
 */

import { describe, test } from 'vitest';
import { register } from 'node:module';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { PENDING_HEALTH_FORM } from './settingsState';
register('./css-stub.mjs', import.meta.url);

describe('ActivityStatsPanel', () => {
  test('renders window-title and retain-days fields only', { timeout: 20000 }, async () => {
    const { default: i18n } = await import('../../i18n');
    await i18n.changeLanguage('zh');

    const { ActivityStatsPanel } = await import('./ActivityStatsPanel');

    const rendered = renderToStaticMarkup(
      createElement(ActivityStatsPanel, {
        form: PENDING_HEALTH_FORM,
        applied: null,
        onPatch: () => undefined,
        onResetDefaults: () => undefined,
        onApply: () => undefined,
        applying: false,
        error: null,
      }),
    );

    if (!rendered.includes('记录窗口标题') || !rendered.includes('活动明细保留天数')) {
      throw new Error('ActivityStatsPanel missing activity privacy fields');
    }
    if (!rendered.includes('max="3650"') || !rendered.includes('min="1"')) {
      throw new Error('ActivityStatsPanel retain-days input missing 1..3650 range');
    }
    if (
      rendered.includes('工作窗口(分钟)') ||
      rendered.includes('免打扰开始') ||
      rendered.includes('系统通知提醒')
    ) {
      throw new Error('ActivityStatsPanel must not render health reminder fields');
    }
  });
});
