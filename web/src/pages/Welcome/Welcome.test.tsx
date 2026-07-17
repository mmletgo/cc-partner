// @vitest-environment jsdom
/**
 * Welcome 权限页单元测试（权限与 LAN gate 分离）。
 *
 * Business Logic（为什么需要这个测试）:
 *   Welcome skip 写 skipped 标记（不写 onboarded）；continue 仅在全授权时可点。
 *   开发壳/发布版 key 隔离，不得混用。
 *
 * Code Logic（这个测试做什么）:
 *   mock usePermissions + configApi.appIdentity；断言 skip → permissionSkippedKey。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import {
  PERMISSION_ONBOARDED_KEY,
  permissionSkippedKey,
} from '@/hooks/usePermissions';

vi.mock('@/api/config', () => ({
  configApi: {
    appIdentity: vi.fn(async () => ({ bundleId: 'com.cc-partner.app', flavor: 'release' as const })),
  },
}));

vi.mock('@/hooks/usePermissions', async () => {
  const actual = await vi.importActual<typeof import('@/hooks/usePermissions')>(
    '@/hooks/usePermissions',
  );
  return {
    ...actual,
    usePermissions: () => ({
      status: {
        screenCapture: { granted: false },
        accessibility: { granted: false },
        inputMonitoring: { granted: false },
        notification: { granted: false },
      },
      loading: false,
      refreshing: false,
      error: null,
      requesting: new Set(),
      allRequiredGranted: false,
      allGranted: false,
      request: vi.fn(),
      requestMissing: vi.fn(),
      refresh: vi.fn(),
    }),
  };
});

import { Welcome } from './Welcome';

describe('Welcome', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
  });

  test('skip writes permission skipped marker only', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    await waitFor(() => {
      expect(screen.getByRole('button', { name: '暂时跳过' })).toBeTruthy();
    });
    fireEvent.click(screen.getByRole('button', { name: '暂时跳过' }));
    expect(localStorage.getItem(permissionSkippedKey('release'))).toBe('1');
    expect(localStorage.getItem(PERMISSION_ONBOARDED_KEY)).toBeNull();
  });
});
