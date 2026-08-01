import { describe, test } from 'vitest';
import { mapPermissions } from './permissionEntries';
import type { PermissionsStatus } from './types';

function assertEqual(actual: unknown, expected: unknown, msg?: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`${msg ?? ''} Expected ${String(expected)}, got ${String(actual)}`);
  }
}

describe('permissionEntries', () => {
  test('maps four permission entries including notification', () => {
    // mock t：直接回传 key，便于断言文案 key（mapPermissions 内部 t('permission.notification.title') 等）
    const t = ((key: string) => key) as never;

    const status: PermissionsStatus = {
      screenCapture: { granted: true },
      accessibility: { granted: true },
      inputMonitoring: { granted: false, state: 'notDetermined' },
      notification: { granted: false },
    };

    const entries = mapPermissions(status, t);

    assertEqual(entries.length, 4, '应返回 4 条权限');
    assertEqual(entries[0].id, 'screenCapture');
    assertEqual(entries[1].id, 'accessibility');
    assertEqual(entries[2].id, 'inputMonitoring');
    assertEqual(entries[3].id, 'notification', 'notification 应为第 4 条');
    assertEqual(entries[3].granted, false, 'notification granted 镜像 status');
    assertEqual(entries[3].title, 'permission.notification.title', 'notification 标题文案 key');
  });

  test('maps input monitoring four states to explicit actions', () => {
    const t = ((key: string) => key) as never;
    const base: PermissionsStatus = {
      screenCapture: { granted: true },
      accessibility: { granted: true },
      inputMonitoring: { granted: false, state: 'notDetermined' },
      notification: { granted: true },
    };

    const entryFor = (state: PermissionsStatus['inputMonitoring']['state']) =>
      mapPermissions(
        {
          ...base,
          inputMonitoring: { granted: state === 'granted', state },
        },
        t,
      )[2]!;

    assertEqual(entryFor('notDetermined').action, 'request');
    assertEqual(entryFor('notDetermined').actionLabel, 'permissionCard.requestAccess');
    assertEqual(entryFor('denied').action, 'openSettings');
    assertEqual(entryFor('denied').actionLabel, 'permissionCard.addInSettings');
    assertEqual(
      entryFor('denied').description,
      'permission.inputMonitoring.deniedDescription',
    );
    assertEqual(entryFor('granted').action, 'none');
    assertEqual(entryFor('unavailable').action, 'openSettings');
    assertEqual(entryFor('unavailable').actionLabel, 'permissionCard.openSettings');
    assertEqual(
      entryFor('unavailable').description,
      'permission.inputMonitoring.unavailableDescription',
    );
  });
});
