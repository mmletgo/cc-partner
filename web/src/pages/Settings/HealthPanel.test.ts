/**
 * HealthPanel 健康提醒设置回归测试(脚本式,沿用 settingsState.test.ts 风格)
 *
 * Business Logic（为什么需要这个测试）:
 *   健康 tab 免打扰时间需要使用 24 小时制选择器,避免用户手动输入 HH:MM,
 *   也避免原生 time input 受系统 locale 影响显示成 12 小时制。
 *
 * Code Logic（做什么）:
 *   先注册 css-stub loader(HealthPanel.tsx 经 @/components/primitives 间接 import *.module.css,
 *   tsx 无 CSS loader,需 stub 成空对象);再动态 import HealthPanel 取 timePartsToConfig,
 *   验证空值/null 映射、四个 section、四个 select、00-23 小时选项以及健康网格 CSS 覆盖顺序。
 *   node:module 这一行用 @ts-expect-error 抑制类型错误(见下方行内注释)。
 */

// node:module 类型由 @types/node 提供,但本仓库 tsconfig 未在 compilerOptions.types 显式纳入 node,
// tsx 测试上下文下类型缺失,故局部抑制(运行时 tsx 正常解析;node:module 是 node 内置,无需安装)。
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:module 类型缺失,运行时 tsx 正常
import { register } from 'node:module';
// @ts-expect-error - 同上,node:fs/promises 是脚本式测试读取 CSS 源码所需的 node 内置模块
import { readFile } from 'node:fs/promises';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { PENDING_HEALTH_FORM } from './settingsState';
register('./css-stub.mjs', import.meta.url);

const { default: i18n } = await import('../../i18n');
await i18n.changeLanguage('zh');

const { HealthPanel, splitTimeValue, timePartsToConfig } = await import('./HealthPanel');

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

console.log(`timePartsToConfig: ${cases.length} cases passed`);

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

console.log(`splitTimeValue: ${splitCases.length} cases passed`);

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
if (sectionCount !== 4) {
  throw new Error(`HealthPanel expected 4 settings sections, got ${sectionCount}`);
}

for (const title of ['健康提醒', '提醒方向', '免打扰', '通知与隐私']) {
  if (!rendered.includes(title)) {
    throw new Error(`HealthPanel missing section title: ${title}`);
  }
}

console.log('HealthPanel layout: 4 section cards rendered');

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

console.log('HealthPanel layout: quiet hours use 24-hour picker selects');

const cssSource = await readFile(new URL('./Settings.module.css', import.meta.url), 'utf8');
const fieldSeparatorIndex = cssSource.indexOf('.field + .field');
const healthGridOverrideIndex = cssSource.indexOf('.healthFieldGrid > .field + .field');
if (fieldSeparatorIndex === -1 || healthGridOverrideIndex === -1) {
  throw new Error('HealthPanel CSS expected both generic field separator and health grid override rules');
}
if (healthGridOverrideIndex <= fieldSeparatorIndex) {
  throw new Error('HealthPanel CSS health grid override must appear after .field + .field');
}

console.log('HealthPanel CSS: health grid field separator override follows generic field rule');
