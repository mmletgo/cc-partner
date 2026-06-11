/**
 * useLanguage Hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   应用需要中英文切换并在所有组件、标签页、多个 QWebEngineView 窗口
 *   之间即时同步。语言偏好持久化到 localStorage,复用 useTheme 的事件
 *   同步范式。
 *
 * Code Logic（这个 hook 做什么）:
 *   - 暴露当前 language('en'|'zh')、setLanguage、toggleLanguage
 *   - setLanguage:i18next.changeLanguage + 写 localStorage + 派发
 *     'cp-lang-change' 自定义事件
 *   - 监听 'cp-lang-change' 与 'storage' 事件,跨标签页/窗口同步
 */
import { useCallback, useEffect, useState } from 'react';
import i18n, { LANGUAGE_STORAGE_KEY, type AppLanguage } from '../i18n';

export const LANGUAGE_CHANGE_EVENT = 'cp-lang-change';

function readInitialLanguage(): AppLanguage {
  return i18n.language === 'zh' ? 'zh' : 'en';
}

export interface UseLanguageResult {
  language: AppLanguage;
  setLanguage: (next: AppLanguage) => void;
  toggleLanguage: () => void;
}

export function useLanguage(): UseLanguageResult {
  const [language, setLanguageState] = useState<AppLanguage>(readInitialLanguage);

  // 监听其他实例派发的切换事件 + 跨标签 storage 事件
  useEffect(() => {
    const changeHandler = (e: Event) => {
      const ce = e as CustomEvent<AppLanguage>;
      if (ce.detail === 'en' || ce.detail === 'zh') {
        setLanguageState(ce.detail);
      }
    };
    const storageHandler = (e: StorageEvent) => {
      if (
        e.key === LANGUAGE_STORAGE_KEY &&
        (e.newValue === 'en' || e.newValue === 'zh')
      ) {
        void i18n.changeLanguage(e.newValue);
        setLanguageState(e.newValue);
      }
    };
    window.addEventListener(LANGUAGE_CHANGE_EVENT, changeHandler);
    window.addEventListener('storage', storageHandler);
    return () => {
      window.removeEventListener(LANGUAGE_CHANGE_EVENT, changeHandler);
      window.removeEventListener('storage', storageHandler);
    };
  }, []);

  const setLanguage = useCallback((next: AppLanguage) => {
    setLanguageState(next);
    void i18n.changeLanguage(next);
    if (typeof window !== 'undefined') {
      window.localStorage.setItem(LANGUAGE_STORAGE_KEY, next);
      window.dispatchEvent(
        new CustomEvent<AppLanguage>(LANGUAGE_CHANGE_EVENT, { detail: next })
      );
    }
  }, []);

  const toggleLanguage = useCallback(() => {
    setLanguage(language === 'zh' ? 'en' : 'zh');
  }, [language, setLanguage]);

  return { language, setLanguage, toggleLanguage };
}
