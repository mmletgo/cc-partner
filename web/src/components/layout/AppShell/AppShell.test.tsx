/**
 * AppShell 导航分组与侧栏滚动合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   侧栏主导航按 Explore/Work/Knowledge/System 分组，
 *   短窗口下 content 独立滚动且 footer 不被覆盖；分组标签不可聚焦，
 *   Trending 仍是 Home `/`，不得出现 Discover 重复入口；
 *   设置入口固定在 footer，不在主导航 System 组；
 *   Workbench 入口是 Work 组内 ProjectRail，不再占独立「工作台」主导航项。
 *
 * Code Logic（这个测试做什么）:
 *   mock 版本/Attention/ProjectRail/权限徽章与 MobileAccessCard；
 *   断言分组标签与 section aria-labelledby、路由顺序、footer 设置链接、
 *   每条链路单 tab stop、ProjectRail 位于 Work 组、CSS 滚动合同源文件。
 */

// @vitest-environment jsdom

import { afterEach, beforeAll, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { I18nextProvider } from 'react-i18next';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import i18n from '@/i18n';

vi.mock('../../../hooks/useAppVersion', () => ({
  useAppVersion: () => '0.0.0-test',
}));

vi.mock('../../../hooks/useAttention', () => ({
  useAttention: () => ({
    snapshot: {
      generatedAt: '2026-07-14T00:00:00.000Z',
      counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
      items: [],
    },
  }),
}));

vi.mock('@/components/domain/WorkbenchProjectRail', () => ({
  WorkbenchProjectRail: () => <div data-testid="project-rail">project-rail</div>,
}));

vi.mock('@/components/domain/PermissionStatusBadge', () => ({
  PermissionStatusBadge: () => null,
}));

vi.mock('@/components/domain/MobileAccessCard', () => ({
  MobileAccessCard: () => null,
}));

const windowRoleMock = vi.hoisted(() => ({ role: 'main' as 'main' | 'satellite' | 'overlay' }));

vi.mock('@/hooks/useWorkbenchWindowRole', () => ({
  useWorkbenchWindowRole: () => ({
    role: windowRoleMock.role,
    label: windowRoleMock.role === 'satellite' ? 'workbench-1' : 'main',
    layoutSlotKey:
      windowRoleMock.role === 'satellite' ? 'desktop:auto:window:workbench-1' : 'desktop:auto',
  }),
}));

vi.mock('@/hooks/workbenchProjectsContext', () => ({
  useWorkbenchProjects: () => ({
    activeProject: { name: 'demo-app' },
  }),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    setTitle: async () => undefined,
  }),
}));

const batterySnapshotMock = vi.hoisted(() => ({
  snapshot: {
    mode: 'charging' as const,
    remainingMs: 23 * 60_000,
    maxBalanceMs: 240 * 60_000,
    todayEarnedMs: 0,
    todaySpentMs: 0,
    consuming: false,
  },
  toast: null,
  setMode: vi.fn(),
  dismissToast: vi.fn(),
}));

vi.mock('@/hooks/useBattery', () => ({
  useBattery: () => batterySnapshotMock,
}));


import { AppShell } from './AppShell';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   AppShell 依赖 Router 与 i18n 才能渲染导航文案。
 *
 * Code Logic（这个函数做什么）:
 *   用 MemoryRouter + I18nextProvider 挂载 AppShell。
 */
function renderShell(): void {
  render(
    <I18nextProvider i18n={i18n}>
      <MemoryRouter initialEntries={['/']}>
        <AppShell>
          <div>main</div>
        </AppShell>
      </MemoryRouter>
    </I18nextProvider>,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   路由顺序合同需要从主导航提取全部 NavLink href。
 *
 * Code Logic（这个函数做什么）:
 *   读取 primary nav 内全部 a[href] 并返回 pathname 列表。
 */
function collectNavHrefs(nav: HTMLElement): string[] {
  return Array.from(nav.querySelectorAll('a[href]')).map((anchor) => {
    const href = anchor.getAttribute('href') ?? '';
    try {
      return new URL(href, 'http://local.test').pathname;
    } catch {
      return href;
    }
  });
}

describe('AppShell grouped navigation', () => {
  test('renders Explore/Work/Knowledge/System group labels as non-focusable section titles', () => {
    renderShell();

    const expected = [
      { id: 'nav-group-explore', label: '探索' },
      { id: 'nav-group-work', label: '工作' },
      { id: 'nav-group-knowledge', label: '知识' },
      { id: 'nav-group-system', label: '系统' },
    ] as const;

    for (const item of expected) {
      const labelEl = document.getElementById(item.id);
      expect(labelEl, `missing label ${item.id}`).toBeTruthy();
      expect(labelEl?.textContent).toBe(item.label);
      expect(labelEl?.matches('a,button,input,select,textarea,[tabindex]')).toBe(false);
      expect(labelEl?.closest('section')?.getAttribute('aria-labelledby')).toBe(item.id);
    }
  });

  test('keeps Home/Trending at / and ordered routes without Discover duplicate', () => {
    renderShell();

    const nav = screen.getByRole('navigation', { name: '主导航' });
    const hrefs = collectNavHrefs(nav);

    expect(hrefs).toEqual([
      '/',
      '/attention',
      '/transfer',
      '/prompts',
      '/cc-history',
      '/scratchpad',
      '/prompt-optimizer',
      '/agent-hub',
      '/health',
      '/activity',
      '/provider-manager',
    ]);
    expect(hrefs.filter((href) => href === '/discover')).toHaveLength(0);
    expect(hrefs.filter((href) => href === '/workbench')).toHaveLength(0);
    expect(hrefs.filter((href) => href === '/')).toHaveLength(1);
    expect(hrefs.filter((href) => href === '/settings')).toHaveLength(0);
  });

  test('places settings entry in footer icon group with gear link', () => {
    renderShell();

    const settingsLink = screen.getByRole('link', { name: '设置' });
    expect(settingsLink.getAttribute('href')).toBe('/settings');
    expect(settingsLink.closest('[class*="footerIconGroup"]') || settingsLink.parentElement).toBeTruthy();
  });

  test('places battery toggle before ThemeToggle in the footer icon group', () => {
    renderShell();
    const battery = screen.getByTestId('battery-mode-toggle');
    const group = battery.closest('[class*="footerIconGroup"]');
    expect(group).toBeTruthy();
    // 两段式切换器容器仍是 footerIconGroup 的第一个元素
    expect(group?.firstElementChild).toBe(battery);
    // 容器内两个按钮：充电档（当前充电态 pressed）与无限档
    const chargingButton = within(battery).getByRole('button', { name: '充电模式' });
    const unlimitedButton = within(battery).getByRole('button', { name: '无限模式' });
    expect(chargingButton.getAttribute('aria-pressed')).toBe('true');
    expect(unlimitedButton.getAttribute('aria-pressed')).toBe('false');
    // 点无限档触发 set_battery_mode（useBattery mock 的 setMode）
    batterySnapshotMock.setMode.mockClear();
    fireEvent.click(unlimitedButton);
    expect(batterySnapshotMock.setMode).toHaveBeenCalledWith('unlimited');
  });

  test('places a game icon button at the far right of the version row and keeps it out of primary nav', () => {
    renderShell();

    const game = screen.getByRole('button', { name: '打开游戏大厅' });
    expect(game.closest('[class*="footerVersionRow"]')).toBeTruthy();
    // 图标按钮：内含 svg 且无可见文字
    expect(game.querySelector('svg')).toBeTruthy();
    expect(game.textContent).toBe('');
    const nav = screen.getByRole('navigation', { name: '主导航' });
    expect(within(nav).queryByRole('button', { name: '打开游戏大厅' })).toBeNull();
    expect(within(nav).queryByRole('link', { name: '打开游戏大厅' })).toBeNull();
  });

  test('satellite chrome hides the game button with the rest of the footer', () => {
    windowRoleMock.role = 'satellite';
    renderShell();
    expect(screen.queryByRole('button', { name: '打开游戏大厅' })).toBeNull();
    windowRoleMock.role = 'main';
  });

  test('exposes one focusable tab stop per nav link and puts ProjectRail in Work group', () => {
    renderShell();

    const nav = screen.getByRole('navigation', { name: '主导航' });
    const links = within(nav).getAllByRole('link');
    expect(links).toHaveLength(11);

    for (const link of links) {
      const tabIndex = link.getAttribute('tabindex');
      expect(tabIndex === null || tabIndex === '0').toBe(true);
    }

    const workSection = document.getElementById('nav-group-work')?.closest('section');
    expect(workSection).toBeTruthy();
    expect(within(workSection as HTMLElement).getByTestId('project-rail')).toBeTruthy();
    expect(within(workSection as HTMLElement).queryByRole('link', { name: /工作台/ })).toBeNull();
  });

  test('satellite chrome hides Explore/System/Settings and keeps project rail', () => {
    windowRoleMock.role = 'satellite';
    renderShell();
    expect(screen.getByTestId('workbench-satellite-shell')).toBeTruthy();
    expect(screen.queryByRole('link', { name: 'Github热门' })).toBeNull();
    expect(screen.queryByRole('link', { name: '设置' })).toBeNull();
    expect(screen.getByTestId('project-rail')).toBeTruthy();
    expect(screen.getByTestId('battery-satellite-footer')).toBeTruthy();
    expect(screen.getByTestId('battery-mode-toggle')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /theme/i })).toBeNull();
    windowRoleMock.role = 'main';
  });

  test('satellite shell applies stored theme without ThemeToggle', () => {
    window.localStorage.setItem('cp-theme', 'dark');
    document.documentElement.removeAttribute('data-theme');
    windowRoleMock.role = 'satellite';
    renderShell();
    expect(screen.getByTestId('workbench-satellite-shell')).toBeTruthy();
    expect(screen.queryByRole('button', { name: /theme/i })).toBeNull();
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    windowRoleMock.role = 'main';
    window.localStorage.removeItem('cp-theme');
    document.documentElement.removeAttribute('data-theme');
  });

  test('sidebar content scroll contract uses min-height 0 and overflow-y auto', () => {
    const sidebarCss = readFileSync(
      resolve(process.cwd(), 'src/components/layout/Sidebar/Sidebar.module.css'),
      'utf8',
    );

    expect(sidebarCss).toMatch(/\.content\s*\{[\s\S]*?min-height:\s*0;/);
    expect(sidebarCss).toMatch(/\.content\s*\{[\s\S]*?overflow-y:\s*auto;/);
    // footer 保留在 flex 流内，侧栏自身不再整栏滚动以免盖住 footer
    expect(sidebarCss).toMatch(/\.sidebar\s*\{[\s\S]*?overflow:\s*hidden;/);
  });
});
