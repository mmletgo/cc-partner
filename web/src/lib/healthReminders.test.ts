/**
 * 健康提醒模板纯函数合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   出厂三项、旧遮罩 query 兼容、恢复默认保留自定义、上限 12 都必须在 UI 接线前锁死。
 *
 * Code Logic（做什么）:
 *   直接调用 helper，不挂 React。
 */
import { describe, expect, test } from 'vitest';
import {
  HEALTH_REMINDER_KEGEL_ID,
  HEALTH_REMINDER_MAX_COUNT,
  HEALTH_REMINDER_REST_ID,
  HEALTH_REMINDER_WATER_ID,
  cloneHealthReminders,
  createCustomHealthReminder,
  createDefaultHealthReminders,
  overlaySurfaceCopy,
  resetBuiltinHealthReminders,
  resolveOverlayTemplateId,
} from './healthReminders';

describe('healthReminders', () => {
  test('seeds water/rest/kegel with factory trigger and complete modes', () => {
    const reminders = createDefaultHealthReminders();
    expect(reminders.map((item) => item.id)).toEqual([
      HEALTH_REMINDER_WATER_ID,
      HEALTH_REMINDER_REST_ID,
      HEALTH_REMINDER_KEGEL_ID,
    ]);
    expect(reminders.every((item) => item.builtin && item.enabled)).toBe(true);
    expect(reminders[0]).toMatchObject({
      trigger: 'interval',
      complete: 'instant',
      intervalSeconds: 3600,
    });
    expect(reminders[1]).toMatchObject({
      trigger: 'sedentary',
      complete: 'session',
      thresholdSeconds: 2700,
      sessionSeconds: 300,
    });
    expect(reminders[0]).toMatchObject({ creditMinutes: 8, dailyCap: 6 });
    expect(reminders[1]).toMatchObject({ creditMinutes: 20, dailyCap: 8 });
    expect(reminders[2]).toMatchObject({
      trigger: 'interval',
      complete: 'session',
      intervalSeconds: 7200,
      sessionSeconds: 30,
      name: '提肛',
      creditMinutes: 10,
      dailyCap: 4,
    });
    expect(createCustomHealthReminder('custom-x')).toMatchObject({
      creditMinutes: 10,
      dailyCap: 6,
    });
    expect(reminders[2].body).not.toMatch(/医学|解剖|盆底/);
  });

  test('clones reminders so callers cannot mutate the factory snapshot', () => {
    const source = createDefaultHealthReminders();
    const cloned = cloneHealthReminders(source);
    cloned[0].name = 'mutated';
    expect(source[0].name).toBe('饮水');
    expect(cloned).not.toBe(source);
  });

  test('custom reminder defaults to interval + instant and reset keeps customs', () => {
    const custom = createCustomHealthReminder('custom-1', '未命名提醒');
    expect(custom).toMatchObject({
      id: 'custom-1',
      builtin: false,
      enabled: true,
      trigger: 'interval',
      complete: 'instant',
      intervalSeconds: 3600,
      name: '未命名提醒',
    });

    const current = [
      ...createDefaultHealthReminders().map((item) =>
        item.id === HEALTH_REMINDER_WATER_ID ? { ...item, intervalSeconds: 1800 } : item,
      ),
      custom,
    ];
    const reset = resetBuiltinHealthReminders(current, createDefaultHealthReminders());
    expect(reset).toHaveLength(4);
    expect(reset.find((item) => item.id === HEALTH_REMINDER_WATER_ID)?.intervalSeconds).toBe(3600);
    expect(reset.find((item) => item.id === 'custom-1')?.name).toBe('未命名提醒');
    expect(HEALTH_REMINDER_MAX_COUNT).toBe(12);
  });

  test('resolves overlay template from query and legacy type=', () => {
    const search = (entries: Record<string, string>) => ({
      get: (key: string) => entries[key] ?? null,
    });
    expect(resolveOverlayTemplateId(search({ template: 'kegel' }))).toBe(HEALTH_REMINDER_KEGEL_ID);
    expect(resolveOverlayTemplateId(search({ type: 'water' }))).toBe(HEALTH_REMINDER_WATER_ID);
    expect(resolveOverlayTemplateId(search({ type: 'reminder' }))).toBe(HEALTH_REMINDER_REST_ID);
    expect(resolveOverlayTemplateId(search({}))).toBe(HEALTH_REMINDER_REST_ID);
  });

  test('overlay copy stays empty until the template is known', () => {
    expect(overlaySurfaceCopy(null, false)).toBeNull();
    expect(overlaySurfaceCopy(undefined, false)).toBeNull();
    const water = createDefaultHealthReminders()[0];
    expect(overlaySurfaceCopy(water, true)).toEqual({
      title: water.title,
      body: water.body,
      confirmLabel: water.confirmLabel,
    });
  });
});
