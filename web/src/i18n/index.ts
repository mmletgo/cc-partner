/**
 * i18next 初始化
 *
 * Business Logic（为什么需要这个文件）:
 *   应用需要中英文双语切换;语言偏好存 localStorage,首次按系统
 *   语言推断(navigator.language 以 zh 开头→中文),其余回退英文。
 *
 * Code Logic（这个文件做什么）:
 *   - 同步 import 非传输 namespace 的 en/zh JSON；transfer 由 registerTransferLocale 在 lazy 页加载
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
import enWorkbench from './locales/en/workbench.json';
import type enTransfer from './locales/en/transfer.json';
import enScratchpad from './locales/en/scratchpad.json';
import enPromptOptimizer from './locales/en/promptOptimizer.json';
import enClaudeMd from './locales/en/claudeMd.json';
import enWelcome from './locales/en/welcome.json';
import enSettings from './locales/en/settings.json';
import enCcHistory from './locales/en/ccHistory.json';
import enHealth from './locales/en/health.json';
import enOrchestrator from './locales/en/orchestrator.json';
import enAttention from './locales/en/attention.json';
import enAgentHub from './locales/en/agentHub.json';
import enProviderManager from './locales/en/providerManager.json';
import enWordgame from './locales/en/wordgame.json';
import enBattery from './locales/en/battery.json';
import enTokenStats from './locales/en/tokenStats.json';

import zhCommon from './locales/zh/common.json';
import zhNav from './locales/zh/nav.json';
import zhHome from './locales/zh/home.json';
import zhPrompts from './locales/zh/prompts.json';
import zhWorkbench from './locales/zh/workbench.json';
import zhScratchpad from './locales/zh/scratchpad.json';
import zhPromptOptimizer from './locales/zh/promptOptimizer.json';
import zhClaudeMd from './locales/zh/claudeMd.json';
import zhWelcome from './locales/zh/welcome.json';
import zhSettings from './locales/zh/settings.json';
import zhCcHistory from './locales/zh/ccHistory.json';
import zhHealth from './locales/zh/health.json';
import zhOrchestrator from './locales/zh/orchestrator.json';
import zhAttention from './locales/zh/attention.json';
import zhAgentHub from './locales/zh/agentHub.json';
import zhProviderManager from './locales/zh/providerManager.json';
import zhWordgame from './locales/zh/wordgame.json';
import zhBattery from './locales/zh/battery.json';
import zhTokenStats from './locales/zh/tokenStats.json';

export type AppLanguage = 'en' | 'zh';
export const LANGUAGE_STORAGE_KEY = 'cp-lang';

/**
 * 把任意语言码归一成应用支持的界面语种。
 *
 * Business Logic（为什么需要）:
 *   Prompt 优化等能力应跟随当前界面语种，不能另存一套中英开关。
 *
 * Code Logic（做什么）:
 *   仅 `zh` 视为中文，其余一律英文。
 */
export function resolveAppLanguage(raw: string | null | undefined): AppLanguage {
  return raw === 'zh' ? 'zh' : 'en';
}

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
    workbench: enWorkbench,
    transfer: {} as typeof enTransfer,
    scratchpad: enScratchpad,
    promptOptimizer: enPromptOptimizer,
    claudeMd: enClaudeMd,
    welcome: enWelcome,
    settings: enSettings,
    ccHistory: enCcHistory,
    health: enHealth,
    orchestrator: enOrchestrator,
    attention: enAttention,
    agentHub: enAgentHub,
    providerManager: enProviderManager,
    wordgame: enWordgame,
    battery: enBattery,
    tokenStats: enTokenStats,
  },
  zh: {
    common: zhCommon,
    nav: zhNav,
    home: zhHome,
    prompts: zhPrompts,
    workbench: zhWorkbench,
    transfer: {} as typeof enTransfer,
    scratchpad: zhScratchpad,
    promptOptimizer: zhPromptOptimizer,
    claudeMd: zhClaudeMd,
    welcome: zhWelcome,
    settings: zhSettings,
    ccHistory: zhCcHistory,
    health: zhHealth,
    orchestrator: zhOrchestrator,
    attention: zhAttention,
    agentHub: zhAgentHub,
    providerManager: zhProviderManager,
    wordgame: zhWordgame,
    battery: zhBattery,
    tokenStats: zhTokenStats,
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
