/**
 * i18next 初始化
 *
 * Business Logic（为什么需要这个文件）:
 *   应用需要中英文双语切换;语言偏好存 localStorage,首次按系统
 *   语言推断(navigator.language 以 zh 开头→中文),其余回退英文。
 *
 * Code Logic（这个文件做什么）:
 *   - 同步 import 全部 namespace 的 en/zh JSON 资源
 *   - detectLanguage:localStorage['cp-lang'] > navigator.language > 'en'
 *   - 配置 fallbackLng='en'、defaultNS='common'
 *   - declare module 让 react-i18next 的 t() 在编译期校验 key
 */
import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

import enCommon from './locales/en/common.json';
import enNav from './locales/en/nav.json';
import enHome from './locales/en/home.json';
import enPrompts from './locales/en/prompts.json';
import enTransfer from './locales/en/transfer.json';
import enDevices from './locales/en/devices.json';
import enScratchpad from './locales/en/scratchpad.json';
import enClaudeMd from './locales/en/claudeMd.json';
import enWelcome from './locales/en/welcome.json';
import enSettings from './locales/en/settings.json';
import enCcHistory from './locales/en/ccHistory.json';
import enSsh from './locales/en/ssh.json';

import zhCommon from './locales/zh/common.json';
import zhNav from './locales/zh/nav.json';
import zhHome from './locales/zh/home.json';
import zhPrompts from './locales/zh/prompts.json';
import zhTransfer from './locales/zh/transfer.json';
import zhDevices from './locales/zh/devices.json';
import zhScratchpad from './locales/zh/scratchpad.json';
import zhClaudeMd from './locales/zh/claudeMd.json';
import zhWelcome from './locales/zh/welcome.json';
import zhSettings from './locales/zh/settings.json';
import zhCcHistory from './locales/zh/ccHistory.json';
import zhSsh from './locales/zh/ssh.json';

export type AppLanguage = 'en' | 'zh';
export const LANGUAGE_STORAGE_KEY = 'cp-lang';

/**
 * 检测初始语言
 *
 * Business Logic（为什么需要这个函数）:
 *   首次进入应用时,按用户已保存的偏好或系统语言决定初始显示语言,
 *   避免每次刷新都回到默认英文。
 *
 * Code Logic（这个函数做什么）:
 *   读取 localStorage['cp-lang'],有效则用;否则按 navigator.language
 *   是否以 zh 开头推断;都不满足回退 'en'。
 */
export function detectLanguage(): AppLanguage {
  if (typeof window === 'undefined') return 'en';
  const stored = window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
  if (stored === 'en' || stored === 'zh') return stored;
  const nav = window.navigator.language?.toLowerCase() ?? '';
  return nav.startsWith('zh') ? 'zh' : 'en';
}

export const resources = {
  en: {
    common: enCommon,
    nav: enNav,
    home: enHome,
    prompts: enPrompts,
    transfer: enTransfer,
    devices: enDevices,
    scratchpad: enScratchpad,
    claudeMd: enClaudeMd,
    welcome: enWelcome,
    settings: enSettings,
    ccHistory: enCcHistory,
    ssh: enSsh,
  },
  zh: {
    common: zhCommon,
    nav: zhNav,
    home: zhHome,
    prompts: zhPrompts,
    transfer: zhTransfer,
    devices: zhDevices,
    scratchpad: zhScratchpad,
    claudeMd: zhClaudeMd,
    welcome: zhWelcome,
    settings: zhSettings,
    ccHistory: zhCcHistory,
    ssh: zhSsh,
  },
} as const;

// 让 t('common:xxx') 在编译期校验 key,拼错即 tsc 报错
declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'common';
    resources: (typeof resources)['en'];
  }
}

void i18n.use(initReactI18next).init({
  resources,
  lng: detectLanguage(),
  fallbackLng: 'en',
  defaultNS: 'common',
  interpolation: {
    escapeValue: false, // React 已转义,无需再 escape
  },
});

export default i18n;
