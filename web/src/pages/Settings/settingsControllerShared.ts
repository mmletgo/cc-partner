/**
 * Settings 控制器共享常量与纯 helper
 *
 * Business Logic（为什么需要这个模块）:
 *   tab 定义、更新提示与时间/大小格式化被 composer 与子 hook 共用；
 *   抽到独立模块可避免 useSettingsController ↔ controllers/* 循环依赖。
 *
 * Code Logic（这个模块做什么）:
 *   导出 SETTINGS_TABS、PROMPT_OPTIMIZER_SHORTCUT_ID、buildUpdateHint、formatTime、formatSize。
 */
import type { TFunction } from 'i18next';
import type { UpdateCheckResult } from '@/lib/types';
import type { SettingsTabId } from './settingsState';

/** Settings 页内子 tab 定义 */
export interface SettingsTab {
  id: SettingsTabId;
  labelKey: SettingsTabId;
}

/** Settings 页内子 tab 顺序：按用户查看任务组织，而不是按底层配置来源组织 */
export const SETTINGS_TABS: SettingsTab[] = [
  { id: 'general', labelKey: 'general' },
  { id: 'dependencies', labelKey: 'dependencies' },
  { id: 'health', labelKey: 'health' },
  { id: 'sync', labelKey: 'sync' },
  { id: 'ai', labelKey: 'ai' },
  { id: 'automation', labelKey: 'automation' },
  { id: 'about', labelKey: 'about' },
];

/** Workbench Prompt 优化快捷键录制控件 id */
export const PROMPT_OPTIMIZER_SHORTCUT_ID = 'promptOptimizer';

/**
 * 计算更新检查结果的提示文本
 *
 * Business Logic（为什么需要这个函数）:
 *   关于 tab 需要根据检查进度与结果展示统一提示，避免 JSX 内嵌多分支文案。
 *
 * Code Logic（这个函数做什么）:
 *   优先 checking；无结果为 upToDate；有 error 显示 error；hasUpdate 插值版本；否则 upToDate。
 *
 * @param updateResult 更新检查结果
 * @param checkingUpdate 是否正在检查
 * @param t i18next 翻译函数（settings ns）
 * @returns 当前应展示的提示文本
 */
export function buildUpdateHint(
  updateResult: UpdateCheckResult | null,
  checkingUpdate: boolean,
  t: TFunction<'settings'>,
): string {
  if (checkingUpdate) return t('about.checkingHint');
  if (!updateResult) return t('about.upToDate');
  if (updateResult.error) return updateResult.error;
  if (updateResult.hasUpdate) return t('about.newVersionFound', { version: updateResult.version });
  return t('about.upToDate');
}

/**
 * 把 Date 格式化为 "HH:MM:SS" 字符串
 *
 * Business Logic（为什么需要这个函数）:
 *   常规 tab 保存成功后需在页脚展示本地保存时间。
 *
 * Code Logic（这个函数做什么）:
 *   从 Date 取本地时分秒并零填充。
 *
 * @param d Date 实例
 * @returns 时间字符串
 */
export function formatTime(d: Date): string {
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 把字节数格式化为人类可读的大小字符串（B/KB/MB/GB）
 *
 * Business Logic（为什么需要这个函数）:
 *   关于 tab 下载按钮需展示更新包体积。
 *
 * Code Logic（这个函数做什么）:
 *   按 1024 进制递降单位，保留合适小数位。
 *
 * @param bytes 字节数
 * @returns 形如 "12.3 MB" 的字符串
 */
export function formatSize(bytes: number): string {
  if (!bytes) return '';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 100 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

/**
 * 分组重试水合选项。
 *
 * Business Logic（为什么需要这个类型）:
 *   失败组重试成功应允许写 form；已 ready 组重试需 dirty 保护。
 *
 * Code Logic（这个类型做什么）:
 *   allowRewriteForm=true 时无视 dirty 覆盖 draft。
 */
export interface ApplyGroupOptions {
  allowRewriteForm?: boolean;
}
