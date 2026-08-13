/**
 * useTheme Hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   应用需要支持浅色/深色两种主题切换，并在所有组件、标签页、
 *   以及主窗/卫星窗等多个 WebView 之间同步。主题持久化到
 *   localStorage，初始化时优先读取存储，否则回落系统偏好。
 *
 * Code Logic（这个 hook 做什么）:
 *   - 暴露当前 theme（'light' | 'dark'）与 toggleTheme / setTheme
 *   - 维护 document.documentElement 的 data-theme 属性
 *   - 订阅 'cp-theme-change' 与 'storage'，跨组件/跨窗即时同步
 *   - 桌面多 WebView 再经 Tauri event 广播（storage 事件在 WKWebView 间不可靠）
 *   - 首次挂载时调用一次 syncDocument 应用当前主题
 */
import { useCallback, useEffect, useState } from 'react';

export type Theme = 'light' | 'dark';
export const THEME_STORAGE_KEY = 'cp-theme';
export const THEME_CHANGE_EVENT = 'cp-theme-change';

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: { transformCallback?: unknown };
};

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器与 /mobile 没有 Tauri internals，不能注册跨窗 event。
 *
 * Code Logic（这个函数做什么）:
 *   检测 transformCallback 是否为函数。
 */
function canUseTauriEvents(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   主窗切换主题后，已打开的卫星窗必须立刻跟上；WKWebView 之间
 *   往往不派发 window.storage，需要走 Tauri 广播。
 *
 * Code Logic（这个函数做什么）:
 *   有 Tauri internals 时动态 import emit，把主题发给其它窗口。
 */
function emitThemeToOtherWindows(theme: Theme): void {
  if (!canUseTauriEvents()) return;
  void import('@tauri-apps/api/event')
    .then(({ emit }) => emit(THEME_CHANGE_EVENT, theme))
    .catch(() => undefined);
}

function isTheme(value: unknown): value is Theme {
  return value === 'light' || value === 'dark';
}

function readInitialTheme(): Theme {
  if (typeof window === 'undefined') return 'light';
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  if (isTheme(stored)) return stored;
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
 *   桌面主窗/卫星窗与 `/mobile` 独立入口在任何 React 组件挂载前也需要正确的 data-theme，
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

  // 监听同窗自定义事件 + 跨窗 storage；桌面再听 Tauri 广播
  useEffect(() => {
    const applyIncoming = (next: Theme): void => {
      setThemeState(next);
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
      syncDocument(next);
    };
    const changeHandler = (e: Event) => {
      const ce = e as CustomEvent<Theme>;
      if (isTheme(ce.detail)) applyIncoming(ce.detail);
    };
    const storageHandler = (e: StorageEvent) => {
      if (e.key === THEME_STORAGE_KEY && isTheme(e.newValue)) {
        applyIncoming(e.newValue);
      }
    };
    window.addEventListener(THEME_CHANGE_EVENT, changeHandler);
    window.addEventListener('storage', storageHandler);

    let disposed = false;
    let unlistenTauri: (() => void) | undefined;
    if (canUseTauriEvents()) {
      void import('@tauri-apps/api/event')
        .then(({ listen }) => {
          if (disposed) return undefined;
          return listen<Theme>(THEME_CHANGE_EVENT, (event) => {
            if (isTheme(event.payload)) applyIncoming(event.payload);
          });
        })
        .then((fn) => {
          if (!fn) return;
          if (disposed) {
            fn();
            return;
          }
          unlistenTauri = fn;
        })
        .catch(() => undefined);
    }

    return () => {
      disposed = true;
      window.removeEventListener(THEME_CHANGE_EVENT, changeHandler);
      window.removeEventListener('storage', storageHandler);
      unlistenTauri?.();
    };
  }, []);

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next);
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
      syncDocument(next);
      window.dispatchEvent(new CustomEvent<Theme>(THEME_CHANGE_EVENT, { detail: next }));
      emitThemeToOtherWindows(next);
    }
  }, []);

  const toggleTheme = useCallback(() => {
    setTheme(theme === 'dark' ? 'light' : 'dark');
  }, [theme, setTheme]);

  return { theme, toggleTheme, setTheme };
}
