// @vitest-environment jsdom
/**
 * RouteErrorBoundary 契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   路由 render 错误不得拖垮 AppShell 侧栏与其它独立 overlay；用户需要本地化摘要、
 *   重试当前路由、pathname 变化自动复位，且生产环境不得泄漏 stack。
 *
 * Code Logic（这个测试做什么）:
 *   1) 子树 render throw 时展示 fallback，外壳仍在
 *   2) 重试会 remount 子节点
 *   3) resetKey（pathname）变化清除错误
 *   4) 生产模式不展示 stack
 *   5) 独立 boundary 的 overlay 失败互不影响
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { MemoryRouter } from 'react-router-dom';
import type { ReactNode } from 'react';

import i18n from '@/i18n';

import { RouteErrorBoundary } from './RouteErrorBoundary';

/**
 * Business Logic（为什么需要这个控制块）:
 *   React 19 concurrent 可能对 throw 的组件重试 render；用稳定开关而不是计数器，
 *   避免「第一次 throw、重试成功」绕过 boundary。
 *
 * Code Logic（这个对象做什么）:
 *   fail=true 时 FlakyChild 始终 throw；retry 前设为 false 后 remount 成功。
 */
const throwControl = { fail: true };

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  throwControl.fail = true;
  vi.spyOn(console, 'error').mockImplementation(() => undefined);

  /**
   * Business Logic（为什么需要这个处理）:
 *   React 19 concurrent 在 boundary 捕获前可能把 render 错误上报为 uncaught，
 *   测试环境需 preventDefault 避免 Vitest 把预期错误当失败。
   *
   * Code Logic（这个函数做什么）:
   *   拦截 window error / unhandledrejection 中的测试用错误消息。
   */
  const isExpectedBoundaryError = (message: string): boolean =>
    message.includes('route-boom') ||
    message.includes('broken-prompts') ||
    message.includes('flaky-once') ||
    message.includes('path-a') ||
    message.includes('overlay-boom') ||
    message.includes('prod-safe') ||
    message.includes('concurrent rendering');

  const onError = (event: ErrorEvent): void => {
    if (isExpectedBoundaryError(String(event.message ?? event.error ?? ''))) {
      event.preventDefault();
    }
  };
  const onRejection = (event: PromiseRejectionEvent): void => {
    const reason = event.reason instanceof Error ? event.reason.message : String(event.reason);
    if (isExpectedBoundaryError(reason)) {
      event.preventDefault();
    }
  };
  window.addEventListener('error', onError);
  window.addEventListener('unhandledrejection', onRejection);
  (
    throwControl as {
      fail: boolean;
      cleanupListeners?: () => void;
    }
  ).cleanupListeners = () => {
    window.removeEventListener('error', onError);
    window.removeEventListener('unhandledrejection', onRejection);
  };
});

afterEach(() => {
  cleanup();
  (
    throwControl as {
      fail: boolean;
      cleanupListeners?: () => void;
    }
  ).cleanupListeners?.();
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   boundary 依赖 Router 与 i18n，测试需稳定挂载环境。
 *
 * Code Logic（这个函数做什么）:
 *   MemoryRouter + I18nextProvider 包裹 children。
 */
function renderWithProviders(ui: ReactNode, initialPath = '/') {
  return render(
    <MemoryRouter initialEntries={[initialPath]}>
      <I18nextProvider i18n={i18n}>{ui}</I18nextProvider>
    </MemoryRouter>,
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   模拟会在 render 阶段抛错的路由叶子，触发 error boundary。
 *
 * Code Logic（这个组件做什么）:
 *   始终 throw，不渲染任何 UI。
 */
function AlwaysThrow({ message = 'route-boom' }: { message?: string }): ReactNode {
  throw new Error(message);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   验证 retry 会 remount 子树：开关打开时 throw，关闭后成功。
 *
 * Code Logic（这个组件做什么）:
 *   读取 throwControl.fail；true 则 throw，false 则渲染 recovered。
 */
function FlakyChild(): ReactNode {
  if (throwControl.fail) {
    throw new Error('flaky-once');
  }
  return <div data-testid="flaky-ok">recovered</div>;
}

describe('RouteErrorBoundary', () => {
  test('keeps outer shell and shows localized fallback when route render throws', () => {
    renderWithProviders(
      <div>
        <nav aria-label="primary">shell-sidebar</nav>
        <main>
          <RouteErrorBoundary resetKey="/prompts">
            <AlwaysThrow message="broken-prompts" />
          </RouteErrorBoundary>
        </main>
      </div>,
    );

    expect(screen.getByLabelText('primary').textContent).toContain('shell-sidebar');
    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.getByText('页面出错了')).toBeTruthy();
    expect(screen.getByRole('button', { name: '重试当前页' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '返回首页' })).toBeTruthy();
  });

  test('retry remounts the child subtree', () => {
    throwControl.fail = true;
    renderWithProviders(
      <RouteErrorBoundary
        resetKey="/workbench"
        onRetry={() => {
          throwControl.fail = false;
        }}
      >
        <FlakyChild />
      </RouteErrorBoundary>,
    );

    expect(screen.getByRole('alert')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重试当前页' }));
    expect(screen.getByTestId('flaky-ok').textContent).toContain('recovered');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  test('changing resetKey clears the error state', () => {
    const { rerender } = renderWithProviders(
      <RouteErrorBoundary resetKey="/a">
        <AlwaysThrow message="path-a" />
      </RouteErrorBoundary>,
    );

    expect(screen.getByRole('alert')).toBeTruthy();

    rerender(
      <MemoryRouter initialEntries={['/']}>
        <I18nextProvider i18n={i18n}>
          <RouteErrorBoundary resetKey="/b">
            <div data-testid="path-b-ok">ok-b</div>
          </RouteErrorBoundary>
        </I18nextProvider>
      </MemoryRouter>,
    );

    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByTestId('path-b-ok').textContent).toContain('ok-b');
  });

  test('does not render stack traces in production mode', async () => {
    vi.resetModules();
    vi.stubEnv('PROD', true);
    vi.stubEnv('DEV', false);

    const { RouteErrorBoundary: ProdBoundary } = await import('./RouteErrorBoundary');
    const stackMarker = 'UNIQUE_STACK_MARKER_ZZZ';
    const err = new Error('prod-safe');
    err.stack = `${stackMarker}\n    at Fake.tsx:1:1`;

    /**
     * Business Logic（为什么需要这个组件）:
     *   需要把带 stack 的 Error 抛给 boundary 以断言生产 DOM 不泄漏 stack。
     *
     * Code Logic（这个组件做什么）:
     *   render 时 throw 预构造 Error。
     */
    function ThrowWithStack(): ReactNode {
      throw err;
    }

    render(
      <MemoryRouter>
        <I18nextProvider i18n={i18n}>
          <ProdBoundary resetKey="/settings">
            <ThrowWithStack />
          </ProdBoundary>
        </I18nextProvider>
      </MemoryRouter>,
    );

    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.queryByText(stackMarker)).toBeNull();
    expect(document.body.textContent ?? '').not.toContain(stackMarker);
    expect(screen.queryByTestId('route-error-stack')).toBeNull();
    expect(screen.queryByTestId('route-error-message')).toBeNull();
  });

  test('overlay boundary failure is isolated from sibling main route', () => {
    renderWithProviders(
      <div>
        <RouteErrorBoundary resetKey="/main">
          <div data-testid="main-route">main-ok</div>
        </RouteErrorBoundary>
        <RouteErrorBoundary resetKey="/screenshot-overlay">
          <AlwaysThrow message="overlay-boom" />
        </RouteErrorBoundary>
      </div>,
    );

    expect(screen.getByTestId('main-route').textContent).toContain('main-ok');
    expect(screen.getByRole('alert')).toBeTruthy();
  });

  test('optional onRetry is invoked when user retries', () => {
    throwControl.fail = true;
    const onRetry = vi.fn(() => {
      throwControl.fail = false;
    });
    renderWithProviders(
      <RouteErrorBoundary resetKey="/health" onRetry={onRetry}>
        <FlakyChild />
      </RouteErrorBoundary>,
    );

    expect(screen.getByRole('alert')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重试当前页' }));
    expect(onRetry).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('flaky-ok')).toBeTruthy();
  });
});
