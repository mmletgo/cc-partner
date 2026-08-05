/**
 * useTheme Hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   应用需要支持浅色/深色两种主题切换，并在所有组件之间同步。
 *   主题持久化到 localStorage，并在初始化时优先读取系统偏好，
 *   同时通过自定义事件 'cp-theme-change' 通知订阅者。
 *
 * Code Logic（这个 hook 做什么）:
 *   - 暴露当前 theme（'light' | 'dark'）与 toggleTheme
 *   - 维护 document.documentElement 的 data-theme 属性
 *   - 订阅自定义事件 'cp-theme-change'，让多个组件能即时同步
 *   - 首次挂载时调用一次 syncDocument 应用当前主题
 */
import { useCallback, useEffect, useState } from 'react';

export type Theme = 'light' | 'dark';
export const THEME_STORAGE_KEY = 'cp-theme';
export const THEME_CHANGE_EVENT = 'cp-theme-change';

function readInitialTheme(): Theme {
  if (typeof window === 'undefined') return 'light';
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  if (stored === 'light' || stored === 'dark') return stored;
  const prefersDark =
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(prefers-color-scheme: dark)').matches;
  return prefersDark ? 'dark' : 'light';
}

function syncDocument(theme: Theme): void {
  if (typeof document === 'undefined') return;
  document.documentElement.setAttribute('data-theme', theme);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `/mobile` 独立入口在任何 React 组件挂载前也需要正确的 data-theme，
 *   否则首屏会以默认浅色 token 闪一下再跳到存储/系统偏好。
 *
 * Code Logic（这个函数做什么）:
 *   读取 localStorage / prefers-color-scheme，写入 documentElement.data-theme，并返回主题。
 */
export function bootstrapTheme(): Theme {
  const theme = readInitialTheme();
  syncDocument(theme);
  return theme;
}

export interface UseThemeResult {
  theme: Theme;
  toggleTheme: () => void;
  setTheme: (next: Theme) => void;
}

export function useTheme(): UseThemeResult {
  const [theme, setThemeState] = useState<Theme>(readInitialTheme);

  // 初始化：把当前 theme 同步到 document（避免 SSR 不一致，本项目 SPA 不会有 SSR）
  useEffect(() => {
    syncDocument(theme);
  }, [theme]);

  // 监听其他实例派发的切换事件
  useEffect(() => {
    const handler = (e: Event) => {
      const ce = e as CustomEvent<Theme>;
      if (ce.detail === 'light' || ce.detail === 'dark') {
        setThemeState(ce.detail);
      }
    };
    window.addEventListener(THEME_CHANGE_EVENT, handler);
    return () => window.removeEventListener(THEME_CHANGE_EVENT, handler);
  }, []);

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next);
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
      syncDocument(next);
      window.dispatchEvent(new CustomEvent<Theme>(THEME_CHANGE_EVENT, { detail: next }));
    }
  }, []);

  const toggleTheme = useCallback(() => {
    setTheme(theme === 'dark' ? 'light' : 'dark');
  }, [theme, setTheme]);

  return { theme, toggleTheme, setTheme };
}
