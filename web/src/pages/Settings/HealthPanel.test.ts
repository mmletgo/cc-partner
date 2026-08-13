/**
 * HealthPanel 健康提醒设置回归测试(脚本式,沿用 settingsState.test.ts 风格)
 *
 * Business Logic（为什么需要这个测试）:
 *   健康 tab 免打扰时间需要使用 24 小时制选择器,避免用户手动输入 HH:MM,
 *   也避免原生 time input 受系统 locale 影响显示成 12 小时制；
 *   数字字段 min/max 需对齐后端合法区间，起止相等时展示「全天免打扰」说明。
 *
 * Code Logic（做什么）:
 *   先注册 css-stub loader(HealthPanel.tsx 经 @/components/primitives 间接 import *.module.css,
 *   tsx 无 CSS loader,需 stub 成空对象);再动态 import HealthPanel 取 timePartsToConfig /
 *   HEALTH_RANGE / isAllDayDnd,验证空值/null 映射、三个 section、四个 select、
 *   00-23 小时选项、健康网格 CSS 覆盖顺序、数字输入 min/max 属性与全天免打扰文案。
 *   健康监测开启后久坐/喝水/全屏遮罩始终启用,因此不应再渲染喝水启用或全屏遮罩开关。
 */

import { describe, test } from 'vitest';
import { register } from 'node:module';
import { readFile } from 'node:fs/promises';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { PENDING_HEALTH_FORM } from './settingsState';
register('./css-stub.mjs', import.meta.url);

describe('HealthPanel', () => {
  test('maps time helpers, renders 3 sections, 4 24-hour selects, and CSS override order', { timeout: 20000 }, async () => {
    const { default: i18n } = await import('../../i18n');
    await i18n.changeLanguage('zh');

    const {
      HealthPanel,
      splitTimeValue,
      timePartsToConfig,
      HEALTH_RANGE,
      isAllDayDnd,
    } = await import('./HealthPanel');

    const cases: Array<[string, string, string | null]> = [
      ['', '', null],
      ['09', '', '09:00'],
      ['09', '30', '09:30'],
      ['23', '59', '23:59'],
    ];

    for (const [hour, minute, expected] of cases) {
      const actual = timePartsToConfig(hour, minute);
      if (actual !== expected) {
        throw new Error(`timePartsToConfig('${hour}', '${minute}') expected ${String(expected)}, got ${String(actual)}`);
      }
    }

    const splitCases: Array<[string | null, string, string]> = [
      [null, '', ''],
      ['09:30', '09', '30'],
      ['23:59', '23', '59'],
    ];

    for (const [input, expectedHour, expectedMinute] of splitCases) {
      const actual = splitTimeValue(input);
      if (actual.hour !== expectedHour || actual.minute !== expectedMinute) {
        throw new Error(
          `splitTimeValue('${String(input)}') expected ${expectedHour}:${expectedMinute}, got ${actual.hour}:${actual.minute}`,
        );
      }
    }

    if (HEALTH_RANGE.workWindowMinutes.min !== 1 || HEALTH_RANGE.workWindowMinutes.max !== 480) {
      throw new Error(`HEALTH_RANGE.workWindowMinutes expected 1..480, got ${HEALTH_RANGE.workWindowMinutes.min}..${HEALTH_RANGE.workWindowMinutes.max}`);
    }
    if (HEALTH_RANGE.breakMinutes.min !== 1 || HEALTH_RANGE.breakMinutes.max !== 120) {
      throw new Error(`HEALTH_RANGE.breakMinutes expected 1..120, got ${HEALTH_RANGE.breakMinutes.min}..${HEALTH_RANGE.breakMinutes.max}`);
    }
    if (HEALTH_RANGE.waterIntervalMinutes.min !== 5 || HEALTH_RANGE.waterIntervalMinutes.max !== 1440) {
      throw new Error(`HEALTH_RANGE.waterIntervalMinutes expected 5..1440, got ${HEALTH_RANGE.waterIntervalMinutes.min}..${HEALTH_RANGE.waterIntervalMinutes.max}`);
    }
    if (HEALTH_RANGE.retainDays.min !== 1 || HEALTH_RANGE.retainDays.max !== 3650) {
      throw new Error(`HEALTH_RANGE.retainDays expected 1..3650, got ${HEALTH_RANGE.retainDays.min}..${HEALTH_RANGE.retainDays.max}`);
    }
    if (isAllDayDnd(null, null) || isAllDayDnd('22:00', null) || isAllDayDnd('22:00', '07:00')) {
      throw new Error('isAllDayDnd should only be true when both ends non-null and equal');
    }
    if (!isAllDayDnd('22:00', '22:00')) {
      throw new Error('isAllDayDnd expected true for equal non-null DND ends');
    }

    const rendered = renderToStaticMarkup(
      createElement(HealthPanel, {
        form: PENDING_HEALTH_FORM,
        applied: null,
        onPatch: () => undefined,
        onResetDefaults: () => undefined,
        onApply: () => undefined,
        applying: false,
        error: null,
      }),
    );

    const sectionCount = rendered.match(/<section/g)?.length ?? 0;
    if (sectionCount !== 3) {
      throw new Error(`HealthPanel expected 3 settings sections, got ${sectionCount}`);
    }

    for (const title of ['健康提醒', '免打扰', '通知与隐私']) {
      if (!rendered.includes(title)) {
        throw new Error(`HealthPanel missing section title: ${title}`);
      }
    }

    if (rendered.includes('提醒方向')) {
      throw new Error('HealthPanel must not render a separate reminder style section');
    }

    const waterIntervalIndex = rendered.indexOf('喝水提醒间隔(分钟)');
    const quietHoursIndex = rendered.indexOf('settings-health-quiet-hours-title');
    if (waterIntervalIndex === -1 || quietHoursIndex === -1 || waterIntervalIndex > quietHoursIndex) {
      throw new Error('HealthPanel should render water interval inside the first health reminder section');
    }

    if (rendered.includes('全屏遮罩提醒') || rendered.includes('按间隔显示应用内喝水提醒。')) {
      throw new Error('HealthPanel must not render fullscreen or water reminder enablement toggles');
    }

    if (rendered.includes('记录窗口标题') || rendered.includes('活动明细保留天数')) {
      throw new Error('HealthPanel must not render activity-stats privacy fields');
    }

    const selectCount = rendered.match(/<select/g)?.length ?? 0;
    if (selectCount !== 4) {
      throw new Error(`HealthPanel expected 4 time picker selects, got ${selectCount}`);
    }

    const hourSelectCount = rendered.match(/data-part="hour"/g)?.length ?? 0;
    const minuteSelectCount = rendered.match(/data-part="minute"/g)?.length ?? 0;
    if (hourSelectCount !== 2 || minuteSelectCount !== 2) {
      throw new Error(`HealthPanel expected 2 hour selects and 2 minute selects, got ${hourSelectCount}/${minuteSelectCount}`);
    }

    if (!rendered.includes('<option value="23">23</option>')) {
      throw new Error('HealthPanel expected 24-hour option 23');
    }

    if (rendered.includes('AM') || rendered.includes('PM')) {
      throw new Error('HealthPanel 24-hour picker must not render AM/PM labels');
    }

    for (const attr of ['min="1"', 'max="480"', 'max="120"', 'min="5"', 'max="1440"']) {
      if (!rendered.includes(attr)) {
        throw new Error(`HealthPanel number inputs missing attribute ${attr}`);
      }
    }
    if (rendered.includes('max="3650"')) {
      throw new Error('HealthPanel must not render retain-days input after activity split');
    }

    // 默认 dnd 均为 null 时不应展示全天免打扰
    if (rendered.includes('全天免打扰') || rendered.includes('data-testid="health-all-day-dnd"')) {
      throw new Error('HealthPanel must not show all-day DND when ends are empty');
    }

    // 后端错误文案原样透出
    const errorRendered = renderToStaticMarkup(
      createElement(HealthPanel, {
        form: PENDING_HEALTH_FORM,
        applied: null,
        onPatch: () => undefined,
        onResetDefaults: () => undefined,
        onApply: () => undefined,
        applying: false,
        error: 'health.work_window_seconds 必须在 60..=28800',
      }),
    );
    if (!errorRendered.includes('health.work_window_seconds 必须在 60..=28800')) {
      throw new Error('HealthPanel should render backend field error as-is');
    }

    // dndStart === dndEnd 时展示「全天免打扰」
    const allDayRendered = renderToStaticMarkup(
      createElement(HealthPanel, {
        form: { ...PENDING_HEALTH_FORM, dndStart: '22:00', dndEnd: '22:00' },
        applied: null,
        onPatch: () => undefined,
        onResetDefaults: () => undefined,
        onApply: () => undefined,
        applying: false,
        error: null,
      }),
    );
    if (!allDayRendered.includes('全天免打扰')) {
      throw new Error('HealthPanel should label equal DND ends as 全天免打扰');
    }
    if (!allDayRendered.includes('data-testid="health-all-day-dnd"')) {
      throw new Error('HealthPanel all-day DND helper missing test id');
    }

    const cssSource = await readFile(new URL('./Settings.module.css', import.meta.url), 'utf8');
    const fieldSeparatorIndex = cssSource.indexOf('.field + .field');
    const healthGridOverrideIndex = cssSource.indexOf('.healthFieldGrid > .field + .field');
    if (fieldSeparatorIndex === -1 || healthGridOverrideIndex === -1) {
      throw new Error('HealthPanel CSS expected both generic field separator and health grid override rules');
    }
    if (healthGridOverrideIndex <= fieldSeparatorIndex) {
      throw new Error('HealthPanel CSS health grid override must appear after .field + .field');
    }
  });
});
