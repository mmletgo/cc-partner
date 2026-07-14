// @vitest-environment jsdom
/**
 * Welcome 权限页单元测试（权限与 LAN gate 分离）。
 *
 * Business Logic（为什么需要这个测试）:
 *   Welcome skip/continue 只写权限 onboarding 标记，不得绕过 LAN disclosure。
 *
 * Code Logic（这个测试做什么）:
 *   mock usePermissions；断言 skip 写入 PERMISSION_ONBOARDED_KEY。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import { PERMISSION_ONBOARDED_KEY } from '@/hooks/usePermissions';

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
      request: vi.fn(),
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

  test('skip writes permission onboarded marker only', async () => {
    await i18n.changeLanguage('zh');
    render(
      <I18nextProvider i18n={i18n}>
        <MemoryRouter>
          <Welcome />
        </MemoryRouter>
      </I18nextProvider>,
    );
    fireEvent.click(screen.getByRole('button', { name: '暂时跳过' }));
    expect(localStorage.getItem(PERMISSION_ONBOARDED_KEY)).toBe('1');
  });
});
