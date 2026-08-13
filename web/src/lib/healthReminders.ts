import type { HealthReminderTemplate } from '@/lib/types';

/** 内置饮水模板 id。 */
export const HEALTH_REMINDER_WATER_ID = 'water';
/** 内置休息模板 id。 */
export const HEALTH_REMINDER_REST_ID = 'rest';
/** 内置提肛模板 id。 */
export const HEALTH_REMINDER_KEGEL_ID = 'kegel';
/** 提醒模板上限（含三条内置）。 */
export const HEALTH_REMINDER_MAX_COUNT = 12;

export interface OverlaySearchParams {
  get(name: string): string | null;
}

export interface DefaultReminderSeed {
  workWindowSeconds?: number;
  waterIntervalSeconds?: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新安装、占位表单和「恢复默认」都要同一套饮水 / 休息 / 提肛出厂值。
 *
 * Code Logic（这个函数做什么）:
 *   用可选旧标量覆盖 rest 阈值与 water 间隔；kegel 固定 2 小时 / 30 秒。
 */
export function createDefaultHealthReminders(
  seed: DefaultReminderSeed = {},
): HealthReminderTemplate[] {
  const waterIntervalSeconds = seed.waterIntervalSeconds ?? 60 * 60;
  const workWindowSeconds = seed.workWindowSeconds ?? 45 * 60;
  return [
    {
      id: HEALTH_REMINDER_WATER_ID,
      builtin: true,
      enabled: true,
      name: '饮水',
      trigger: 'interval',
      intervalSeconds: waterIntervalSeconds,
      thresholdSeconds: null,
      complete: 'instant',
      sessionSeconds: null,
      title: '该喝水啦',
      body: '记得补充水分,喝口水再继续。',
      confirmLabel: '已喝水',
      unitLabel: '杯',
    },
    {
      id: HEALTH_REMINDER_REST_ID,
      builtin: true,
      enabled: true,
      name: '休息',
      trigger: 'sedentary',
      intervalSeconds: null,
      thresholdSeconds: workWindowSeconds,
      complete: 'session',
      sessionSeconds: 5 * 60,
      title: '该起来活动一下啦',
      body: '连续工作已久,站起来走走、伸展一下吧。',
      confirmLabel: '开始休息',
      unitLabel: '次',
    },
    {
      id: HEALTH_REMINDER_KEGEL_ID,
      builtin: true,
      enabled: true,
      name: '提肛',
      trigger: 'interval',
      intervalSeconds: 2 * 60 * 60,
      thresholdSeconds: null,
      complete: 'session',
      sessionSeconds: 30,
      title: '该活动一下了',
      body: '坐下太久，做一组短动作再继续。',
      confirmLabel: '开始',
      unitLabel: '次',
    },
  ];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   表单草稿、恢复默认和切片合并都不能共享同一数组引用。
 *
 * Code Logic（这个函数做什么）:
 *   浅拷贝每条模板，返回新数组。
 */
export function cloneHealthReminders(
  reminders: HealthReminderTemplate[],
): HealthReminderTemplate[] {
  return reminders.map((item) => ({ ...item }));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   「添加提醒」要立刻得到一条可编辑的 interval+instant 草稿。
 *
 * Code Logic（这个函数做什么）:
 *   返回非内置模板；名称由调用方传入（默认「未命名提醒」）。
 */
export function createCustomHealthReminder(
  id: string,
  name = '未命名提醒',
): HealthReminderTemplate {
  return {
    id,
    builtin: false,
    enabled: true,
    name,
    trigger: 'interval',
    intervalSeconds: 60 * 60,
    thresholdSeconds: null,
    complete: 'instant',
    sessionSeconds: null,
    title: name,
    body: '',
    confirmLabel: '完成',
    unitLabel: '次',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   恢复默认只重置三条内置，用户加过的自定义提醒必须留下。
 *
 * Code Logic（这个函数做什么）:
 *   用出厂三项替换同 id 内置，再按原序追加非内置项。
 */
export function resetBuiltinHealthReminders(
  current: HealthReminderTemplate[],
  factory: HealthReminderTemplate[],
): HealthReminderTemplate[] {
  const customs = current.filter((item) => !item.builtin);
  return cloneHealthReminders([...factory, ...customs]);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新遮罩 URL 带 template=；旧窗口仍可能是 type=water|reminder。
 *
 * Code Logic（这个函数做什么）:
 *   优先 template；否则把 water→water、其余→rest。
 */
export function resolveOverlayTemplateId(search: OverlaySearchParams): string {
  const template = search.get('template')?.trim();
  if (template) return template;
  return search.get('type') === 'water'
    ? HEALTH_REMINDER_WATER_ID
    : HEALTH_REMINDER_REST_ID;
}
