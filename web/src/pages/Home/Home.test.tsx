// @vitest-environment jsdom
/**
 * Home/Trending 默认首页表征测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   N4 导航改造前必须锁定 `/` 继续展示既有 GitHub Weekly Trending，
 *   冷启动、侧栏 Home 激活、浏览器刷新都不得变成 dashboard/discover。
 *
 * Code Logic（这个测试做什么）:
 *   mock githubTrendingApi；用 MemoryRouter 模拟 `/` 冷启动、从其它路由点回 Home、
 *   以及 remount 刷新；断言 Trending 标题/列表出现，且页面列表不用嵌套 main landmark。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { Link, MemoryRouter, Route, Routes } from 'react-router-dom';

import i18n from '@/i18n';
import type { GithubTrendingResponse } from '@/lib/types';

const listTrendingMock = vi.fn();

vi.mock('@/api/githubTrending', () => ({
  githubTrendingApi: {
    list: (...args: unknown[]) => listTrendingMock(...args),
  },
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn(async () => undefined),
}));

import { Home } from './Home';

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享一份合法 Trending 响应，避免各自拼 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回含 1 个 repo 的 GithubTrendingResponse，可用 overrides 覆盖。
 */
function buildTrendingResponse(
  overrides: Partial<GithubTrendingResponse> = {},
): GithubTrendingResponse {
  return {
    repos: [
      {
        rank: 1,
        owner: 'acme',
        name: 'widget',
        fullName: 'acme/widget',
        url: 'https://github.com/acme/widget',
        description: 'A demo trending repo',
        language: 'TypeScript',
        stars: 1200,
        forks: 80,
        starsThisWeek: 42,
        explanationZh: '演示用热门仓库',
        explanationEn: 'Demo trending repository',
      },
    ],
    fetchedAt: '2026-07-14T08:00:00.000Z',
    expiresAt: '2026-07-15T08:00:00.000Z',
    fromCache: true,
    stale: false,
    aiStatus: 'ready',
    aiError: null,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   表征测试需模拟冷启动/侧栏/刷新三种进入 Home 的路径。
 *
 * Code Logic（这个函数做什么）:
 *   用 MemoryRouter 挂载 `/`→Home 与一个 stub 路由，外加侧栏 Home 链接。
 */
function renderHomeAt(path: string) {
  return render(
    <I18nextProvider i18n={i18n}>
      <MemoryRouter initialEntries={[path]}>
        <nav aria-label="Primary">
          <Link to="/">GitHub Trending</Link>
          <Link to="/settings">Settings</Link>
        </nav>
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/settings" element={<div>Settings stub</div>} />
        </Routes>
      </MemoryRouter>
    </I18nextProvider>,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用例反复断言「仍在展示既有 Trending 首页」需要稳定选择器。
 *
 * Code Logic（这个函数做什么）:
 *   waitFor 标题与列表 aria 标签出现。
 */
async function expectTrendingHomeVisible(): Promise<void> {
  await waitFor(() => {
    expect(
      screen.getByRole('heading', { level: 1, name: 'GitHub Weekly Trending' }),
    ).toBeTruthy();
  });
  expect(screen.getByText('GitHub Trending · Weekly')).toBeTruthy();
  expect(
    screen.getByRole('region', { name: 'GitHub weekly trending repositories' }),
  ).toBeTruthy();
}

beforeAll(async () => {
  await i18n.changeLanguage('en');
});

beforeEach(() => {
  listTrendingMock.mockReset();
  listTrendingMock.mockResolvedValue(buildTrendingResponse());
});

afterEach(() => {
  cleanup();
});

describe('Home trending default route characterization', () => {
  test('cold launch at / renders existing Trending heading and content', async () => {
    renderHomeAt('/');

    await expectTrendingHomeVisible();
    expect(await screen.findByRole('heading', { level: 2, name: 'widget' })).toBeTruthy();
    expect(screen.getByText('acme')).toBeTruthy();
    expect(listTrendingMock).toHaveBeenCalled();
  });

  test('sidebar Home activation from another route still shows Trending', async () => {
    renderHomeAt('/settings');

    expect(screen.getByText('Settings stub')).toBeTruthy();
    expect(screen.queryByRole('heading', { level: 1, name: 'GitHub Weekly Trending' })).toBeNull();

    fireEvent.click(screen.getByRole('link', { name: 'GitHub Trending' }));

    await expectTrendingHomeVisible();
    expect(await screen.findByRole('heading', { level: 2, name: 'widget' })).toBeTruthy();
  });

  test('browser refresh at / remounts the same Trending home', async () => {
    const first = renderHomeAt('/');
    await expectTrendingHomeVisible();
    first.unmount();

    renderHomeAt('/');
    await expectTrendingHomeVisible();
    expect(await screen.findByRole('heading', { level: 2, name: 'widget' })).toBeTruthy();
    expect(listTrendingMock.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  test('Home list is a labelled region, not a nested main landmark', async () => {
    render(
      <I18nextProvider i18n={i18n}>
        <Home />
      </I18nextProvider>,
    );

    await expectTrendingHomeVisible();

    // AppShell 才是唯一 main；Home 内部列表必须是 section/region
    expect(document.querySelectorAll('main')).toHaveLength(0);
    const region = screen.getByRole('region', {
      name: 'GitHub weekly trending repositories',
    });
    expect(region.tagName.toLowerCase()).toBe('section');
    expect(within(region).getByRole('heading', { level: 2, name: 'widget' })).toBeTruthy();
  });

  test('preserves loading then ready contract without inventing dashboard API', async () => {
    const deferred: {
      resolve: ((value: GithubTrendingResponse) => void) | null;
    } = { resolve: null };
    listTrendingMock.mockImplementation(
      () =>
        new Promise<GithubTrendingResponse>((resolve) => {
          deferred.resolve = resolve;
        }),
    );

    render(
      <I18nextProvider i18n={i18n}>
        <Home />
      </I18nextProvider>,
    );

    expect(screen.getByRole('heading', { level: 1, name: 'GitHub Weekly Trending' })).toBeTruthy();
    // skeleton 阶段：尚无 repo 内容
    expect(screen.queryByRole('heading', { level: 2, name: 'widget' })).toBeNull();

    expect(deferred.resolve).not.toBeNull();
    deferred.resolve?.(buildTrendingResponse());
    expect(await screen.findByRole('heading', { level: 2, name: 'widget' })).toBeTruthy();
  });
});
