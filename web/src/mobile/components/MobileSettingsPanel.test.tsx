// @vitest-environment jsdom
/**
 * MobileSettingsPanel 主题切换合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   移动端 Settings 是浅/深主题唯一入口；切换必须立刻更新 data-theme 与 localStorage。
 *
 * Code Logic（这个测试做什么）:
 *   mock 依赖卡，渲染面板并点击 ThemeToggle，断言当前文案与 document/storage。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import { THEME_STORAGE_KEY } from '@/hooks/useTheme';
import { MobileSettingsPanel } from './MobileSettingsPanel';

vi.mock('@/components/domain/WorkbenchDependencyCard', () => ({
  WorkbenchDependencyCard: () => <div data-testid="mobile-settings-dependency-card" />,
}));

function renderPanel() {
  return render(
    <I18nextProvider i18n={i18n}>
      <MobileSettingsPanel />
    </I18nextProvider>,
  );
}

describe('MobileSettingsPanel theme', () => {
  beforeEach(async () => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
    window.localStorage.setItem(THEME_STORAGE_KEY, 'light');
    await i18n.changeLanguage('en');
  });

  afterEach(() => {
    cleanup();
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  test('renders appearance controls and toggles to dark', () => {
    renderPanel();

    expect(screen.getByTestId('mobile-theme-row')).toBeTruthy();
    expect(screen.getByTestId('mobile-theme-current').textContent).toMatch(/Light/i);

    const toggle = screen.getByRole('button', { name: /switch to dark theme/i });
    fireEvent.click(toggle);

    expect(screen.getByTestId('mobile-theme-current').textContent).toMatch(/Dark/i);
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(screen.getByTestId('mobile-settings-dependency-card')).toBeTruthy();
  });
});
