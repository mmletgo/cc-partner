// @vitest-environment jsdom
/**
 * useTheme / bootstrapTheme 合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   移动端独立入口依赖 bootstrapTheme 在 React 挂载前写入 data-theme；
 *   useTheme 切换必须持久化 localStorage 并派发 cp-theme-change。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 bootstrap 读 storage/系统偏好，以及 setTheme 同步 document + storage + 事件。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import {
  THEME_CHANGE_EVENT,
  THEME_STORAGE_KEY,
  bootstrapTheme,
  useTheme,
} from './useTheme';

describe('bootstrapTheme', () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  afterEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
    vi.restoreAllMocks();
  });

  test('applies stored dark theme before react mount', () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, 'dark');
    expect(bootstrapTheme()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  test('falls back to system preference when storage empty', () => {
    const matchMedia = vi.fn().mockReturnValue({ matches: true });
    Object.defineProperty(window, 'matchMedia', {
      writable: true,
      value: matchMedia,
    });

    expect(bootstrapTheme()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(matchMedia).toHaveBeenCalledWith('(prefers-color-scheme: dark)');
  });
});

describe('useTheme', () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  afterEach(() => {
    cleanup();
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  test('toggleTheme persists light/dark and dispatches change event', () => {
    window.localStorage.setItem(THEME_STORAGE_KEY, 'light');
    const events: string[] = [];
    const handler = (event: Event) => {
      events.push((event as CustomEvent<string>).detail);
    };
    window.addEventListener(THEME_CHANGE_EVENT, handler);

    const { result } = renderHook(() => useTheme());
    expect(result.current.theme).toBe('light');

    act(() => {
      result.current.toggleTheme();
    });

    expect(result.current.theme).toBe('dark');
    expect(window.localStorage.getItem(THEME_STORAGE_KEY)).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(events).toEqual(['dark']);

    window.removeEventListener(THEME_CHANGE_EVENT, handler);
  });
});
