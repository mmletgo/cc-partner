import type { HealthReminderTemplate } from '@/lib/types';

/** 内置饮水模板 id。 */
export const HEALTH_REMINDER_WATER_ID = 'water';
/** 内置休息模板 id。 */
export const HEALTH_REMINDER_REST_ID = 'rest';
/** 内置提肛模板 id。 */
export const HEALTH_REMINDER_KEGEL_ID = 'kegel';
/** 提醒模板上限（含三条内置）。 */
export const HEALTH_REMINDER_MAX_COUNT = 12;

/** 出厂充入额度，对齐现电池默认：water 8/6、rest 20/8、kegel 10/4、custom 10/6。 */
export const DEFAULT_TEMPLATE_CREDITS = {
  water: { creditMinutes: 8, dailyCap: 6 },
  rest: { creditMinutes: 20, dailyCap: 8 },
  kegel: { creditMinutes: 10, dailyCap: 4 },
  custom: { creditMinutes: 10, dailyCap: 6 },
} as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   旧模板没有额度字段，表单和入账都要按内置 id 回退出厂值。
 *
 * Code Logic（这个函数做什么）:
 *   water/rest/kegel 精确匹配，其余自定义。
 */
export function defaultCreditForTemplate(id: string): {
  creditMinutes: number;
  dailyCap: number;
} {
  if (id === HEALTH_REMINDER_WATER_ID) return DEFAULT_TEMPLATE_CREDITS.water;
  if (id === HEALTH_REMINDER_REST_ID) return DEFAULT_TEMPLATE_CREDITS.rest;
  if (id === HEALTH_REMINDER_KEGEL_ID) return DEFAULT_TEMPLATE_CREDITS.kegel;
  return DEFAULT_TEMPLATE_CREDITS.custom;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   加载旧配置时要把缺省额度写成具体数字，保存后不再依赖电池页回退。
 *
 * Code Logic（这个函数做什么）:
 *   缺 creditMinutes/dailyCap 时填入出厂值。
 */
export function withResolvedTemplateCredits(
  template: HealthReminderTemplate,
): HealthReminderTemplate {
  const defaults = defaultCreditForTemplate(template.id);
  return {
    ...template,
    creditMinutes: template.creditMinutes ?? defaults.creditMinutes,
    dailyCap: template.dailyCap ?? defaults.dailyCap,
  };
}

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
      creditMinutes: DEFAULT_TEMPLATE_CREDITS.water.creditMinutes,
      dailyCap: DEFAULT_TEMPLATE_CREDITS.water.dailyCap,
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
      creditMinutes: DEFAULT_TEMPLATE_CREDITS.rest.creditMinutes,
      dailyCap: DEFAULT_TEMPLATE_CREDITS.rest.dailyCap,
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
      creditMinutes: DEFAULT_TEMPLATE_CREDITS.kegel.creditMinutes,
      dailyCap: DEFAULT_TEMPLATE_CREDITS.kegel.dailyCap,
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
    creditMinutes: DEFAULT_TEMPLATE_CREDITS.custom.creditMinutes,
    dailyCap: DEFAULT_TEMPLATE_CREDITS.custom.dailyCap,
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

/** 遮罩主文案；模板未知时不回退旧休息文案。 */
export interface OverlaySurfaceCopy {
  title: string;
  body: string;
  confirmLabel: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   配置尚未返回时不能先画出旧的休息文案，否则透明窗残留会叠在饮水文案上。
 *
 * Code Logic（这个函数做什么）:
 *   loaded 为 false 或没有模板时返回 null；否则用模板自身 title/body/confirmLabel。
 */
export function overlaySurfaceCopy(
  template: HealthReminderTemplate | null | undefined,
  loaded: boolean,
): OverlaySurfaceCopy | null {
  if (!loaded || !template) return null;
  return {
    title: template.title.trim(),
    body: template.body.trim(),
    confirmLabel: template.confirmLabel.trim(),
  };
}
