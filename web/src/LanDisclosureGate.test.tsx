// @vitest-environment jsdom
/**
 * LanDisclosureGate 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   gate 必须在 required 时挡住 children，确认后放行，并展示无身份校验风险文案。
 *
 * Code Logic（这个测试做什么）:
 *   mock hook；断言 children 可见性与风险文案。
 */

import { describe, expect, test, vi, beforeEach, afterEach } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { MemoryRouter } from 'react-router-dom';
import i18n from '@/i18n';
import { LanDisclosureGate } from './LanDisclosureGate';

const hookState = vi.hoisted(() => ({
  phase: 'required' as string,
  status: {
    required: true,
    version: 1,
    localAddresses: ['192.168.1.10'],
    preferredPort: 62116,
    mdnsPort: 5353,
    alreadyRunning: false,
    actualHttpPort: null as number | null,
  },
  startResult: null as null,
  error: null as string | null,
  acknowledge: vi.fn(),
  retry: vi.fn(),
  openDiagnostics: vi.fn(),
}));

vi.mock('@/hooks/useLanDisclosureStartup', () => ({
  useLanDisclosureStartup: () => hookState,
}));

describe('LanDisclosureGate', () => {
  beforeEach(() => {
    hookState.phase = 'required';
    hookState.error = null;
  });

  afterEach(() => {
    cleanup();
  });

  test('blocks children when disclosure required', async () => {
    await i18n.changeLanguage('zh');
    render(
      <MemoryRouter initialEntries={['/']}>
        <I18nextProvider i18n={i18n}>
          <LanDisclosureGate>
            <div data-testid="app-child">app</div>
          </LanDisclosureGate>
        </I18nextProvider>
      </MemoryRouter>,
    );
    expect(screen.queryByTestId('app-child')).toBeNull();
    expect(screen.getByTestId('lan-disclosure-gate')).toBeTruthy();
    expect(
      screen.getByText(/同一可达网络任意设备均可读写执行|系统不验证调用者身份/),
    ).toBeTruthy();
  });

  test('renders children when phase is pass', () => {
    hookState.phase = 'pass';
    render(
      <MemoryRouter initialEntries={['/']}>
        <I18nextProvider i18n={i18n}>
          <LanDisclosureGate>
            <div data-testid="app-child">app</div>
          </LanDisclosureGate>
        </I18nextProvider>
      </MemoryRouter>,
    );
    expect(screen.getByTestId('app-child')).toBeTruthy();
  });
});
