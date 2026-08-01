import { describe, test } from 'vitest';
import { mapPermissions } from './permissionEntries';
import type { PermissionsStatus } from './types';

function assertEqual(actual: unknown, expected: unknown, msg?: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`${msg ?? ''} Expected ${String(expected)}, got ${String(actual)}`);
  }
}

describe('permissionEntries', () => {
  test('maps three displayed permission entries and excludes input monitoring', () => {
    // mock t：直接回传 key，便于断言文案 key（mapPermissions 内部 t('permission.notification.title') 等）
    const t = ((key: string) => key) as never;

    const status: PermissionsStatus = {
      screenCapture: { granted: true },
      accessibility: { granted: true },
      inputMonitoring: { granted: false, state: 'notDetermined' },
      notification: { granted: false },
    };

    const entries = mapPermissions(status, t);

    assertEqual(entries.length, 3, '应返回 3 条权限');
    assertEqual(entries[0].id, 'screenCapture');
    assertEqual(entries[1].id, 'accessibility');
    assertEqual(entries[2].id, 'notification', 'notification 应为第 3 条');
    assertEqual(entries[2].granted, false, 'notification granted 镜像 status');
    assertEqual(entries[2].title, 'permission.notification.title', 'notification 标题文案 key');
    assertEqual(
      entries.some((entry) => entry.id === 'inputMonitoring'),
      false,
      '输入监控不应进入产品权限交互',
    );
  });
});
