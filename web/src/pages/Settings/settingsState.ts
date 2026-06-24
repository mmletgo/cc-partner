import type { AppConfig, CloudSyncConfig, GithubTrendingConfig, HealthConfig } from '../../lib/types';
import { getDefaultShortcutValue } from './shortcutRecorder';

/** 单个快捷键字段定义（label/helper 在渲染时按 i18n 解析，这里只存可本地化的 id） */
export interface ShortcutField {
  id: string;
  /** label/helper 的 i18n 子键，对应 shortcut.<key>.{label,helper} */
  labelKey: 'screenshot';
  value: string;
}

/** Settings 页面整体表单状态 */
export interface SettingsState {
  deviceName: string;
  receiveDir: string;
  shortcuts: ShortcutField[];
}

/** 云端同步 Card 的可编辑表单值（受控输入，与已应用配置分离） */
export interface CloudSyncForm {
  repoUrl: string;
  branch: string;
  enabled: boolean;
  auto: boolean;
  intervalSecs: number;
}

/** AI tab 中 GitHub 解说开关与共用 Claude CLI 配置的受控表单值 */
export interface GithubTrendingForm {
  aiEnabled: boolean;
  claudeCliPath: string;
  claudeModel: string;
  cacheTtlHours: number;
}

/** 健康提醒 tab 的受控表单值;与 HealthConfig 同构,直接整体提交给 update_health_config。 */
export type HealthForm = HealthConfig;

/** 可提交到 update_config 的 Settings 字段。 */
export type SettingsConfigUpdate = Partial<Pick<AppConfig, 'deviceName' | 'receiveDir' | 'screenshotHotkey'>>;

/** 云端同步表单提交 payload；空 repoUrl/branch 用空字符串表示“清空”。 */
export interface CloudSyncFormUpdate {
  repoUrl: string;
  enabled: boolean;
  auto: boolean;
  intervalSecs: number;
  branch: string;
}

/** 云端同步表单加载前占位值；真实默认值由后端 get_default_cloud_sync_config 覆盖。 */
export const PENDING_CLOUD_SYNC_FORM: CloudSyncForm = {
  repoUrl: '',
  branch: '',
  enabled: false,
  auto: false,
  intervalSecs: 600,
};

/** AI 表单加载前占位值；真实默认值由后端 get_default_github_trending_config 覆盖。 */
export const PENDING_GITHUB_TRENDING_FORM: GithubTrendingForm = {
  aiEnabled: true,
  claudeCliPath: 'claude',
  claudeModel: 'sonnet',
  cacheTtlHours: 24,
};

/** 健康表单加载前占位值;真实值由后端 get_health_config / get_default_health_config 覆盖。 */
export const PENDING_HEALTH_FORM: HealthForm = {
  enabled: true,
  workWindowSeconds: 45 * 60,
  breakSeconds: 5 * 60,
  recordWindowTitle: true,
  retainDays: 90,
  notifyEnabled: true,
  dndStart: null,
  dndEnd: null,
  waterEnabled: true,
  waterIntervalSeconds: 60 * 60,
  reminderFullscreen: true,
};

/** 快捷键字段定义（值由运行平台或后端配置决定，文案走 t） */
const SHORTCUT_FIELDS: Pick<ShortcutField, 'id' | 'labelKey'>[] = [
  { id: 'screenshot', labelKey: 'screenshot' },
];

/**
 * 生成快捷键字段
 *
 * Business Logic（为什么需要）:
 *   设置页加载、恢复默认和初始占位都需要生成新快捷键对象，避免复用数组对象导致状态污染。
 *
 * Code Logic（做什么）:
 *   接收可选截图快捷键；未提供时按当前平台生成前端兜底默认值，返回 SettingsState 可直接使用的字段数组。
 */
function createShortcutFields(screenshotHotkey?: string): ShortcutField[] {
  return SHORTCUT_FIELDS.map((s) => ({
    ...s,
    value: s.id === 'screenshot' ? (screenshotHotkey || getDefaultShortcutValue()) : '',
  }));
}

/**
 * 生成加载前的占位状态
 *
 * Business Logic（为什么需要）:
 *   Settings 页在后端配置返回前需要一个受控输入占位状态；该状态只用于 loading 期间，
 *   不能作为“恢复默认”的真实默认值。
 *
 * Code Logic（做什么）:
 *   基础字段保持空字符串，快捷键用平台兜底默认值，保证 React 输入始终受控。
 */
export function createPendingSettingsState(): SettingsState {
  return {
    deviceName: '',
    receiveDir: '',
    shortcuts: createShortcutFields(),
  };
}

/**
 * 将后端 AppConfig 映射为 Settings 表单状态
 *
 * Business Logic（为什么需要）:
 *   后端配置是设备名、接收目录和截图快捷键的权威来源；前端保存快捷键时必须保留已加载的基础设置。
 *
 * Code Logic（做什么）:
 *   拷贝 deviceName/receiveDir，并把 screenshotHotkey 映射到快捷键字段；快捷键缺失时使用平台兜底值。
 */
export function settingsStateFromConfig(config: AppConfig): SettingsState {
  return {
    deviceName: config.deviceName,
    receiveDir: config.receiveDir,
    shortcuts: createShortcutFields(config.screenshotHotkey),
  };
}

/**
 * 将后端返回的 CloudSyncConfig 映射为受控表单值
 *
 * Business Logic（为什么需要）:
 *   同步 tab 需要同时支持当前配置和后端默认配置两种来源；表单层必须把 null URL/分支显示为空文本。
 *
 * Code Logic（做什么）:
 *   复制布尔开关和间隔秒数，`repoUrl` / `branch` 的 null 归一为空字符串。
 */
export function cloudSyncConfigToForm(config: CloudSyncConfig | null): CloudSyncForm {
  if (!config) return { ...PENDING_CLOUD_SYNC_FORM };
  return {
    repoUrl: config.repoUrl ?? '',
    branch: config.branch ?? '',
    enabled: config.enabled,
    auto: config.auto,
    intervalSecs: config.intervalSecs,
  };
}

/**
 * 将云端同步表单映射为 update_cloud_sync_config payload
 *
 * Business Logic（为什么需要）:
 *   用户恢复默认或手动清空仓库/分支后，保存必须真的清掉旧配置；Tauri 的 null 会被 Rust
 *   `Option<String>` 当成“字段未传”，不能表达清空。
 *
 * Code Logic（做什么）:
 *   字符串字段 trim 后保留空字符串，由后端 update_cloud_sync_config 统一把空字符串归一为 None。
 */
export function cloudSyncFormToUpdate(form: CloudSyncForm): CloudSyncFormUpdate {
  return {
    repoUrl: form.repoUrl.trim(),
    enabled: form.enabled,
    auto: form.auto,
    intervalSecs: form.intervalSecs,
    branch: form.branch.trim(),
  };
}

/**
 * 将后端返回的 GithubTrendingConfig 映射为受控表单值
 *
 * Business Logic（为什么需要）:
 *   AI tab 需要用同一套映射处理当前配置和恢复默认配置，避免按钮逻辑和加载逻辑分叉。
 *
 * Code Logic（做什么）:
 *   对 CLI 路径和模型做空值兜底，其他字段按后端 DTO 原样进入表单。
 */
export function githubTrendingConfigToForm(config: GithubTrendingConfig | null): GithubTrendingForm {
  if (!config) return { ...PENDING_GITHUB_TRENDING_FORM };
  return {
    aiEnabled: config.aiEnabled,
    claudeCliPath: config.claudeCliPath || 'claude',
    claudeModel: config.claudeModel || 'sonnet',
    cacheTtlHours: config.cacheTtlHours,
  };
}

/**
 * 将后端 HealthConfig 映射为健康 tab 受控表单值
 *
 * Business Logic（为什么需要）:
 *   健康 tab 需用同一套映射处理当前配置和恢复默认配置,与其他 tab 的 *ConfigToForm 模式对齐。
 *   喝水提醒和全屏遮罩现在随健康监测固定启用,旧配置中的 false 不能继续进入表单或提交。
 *
 * Code Logic（做什么）:
 *   null 返回占位默认的新拷贝;非 null 返回字段拷贝,并把 waterEnabled/reminderFullscreen 归一为 true。
 */
export function healthConfigToForm(config: HealthConfig | null): HealthForm {
  const source = config ?? PENDING_HEALTH_FORM;
  return {
    ...source,
    waterEnabled: true,
    reminderFullscreen: true,
  };
}

/**
 * 判断 Settings 表单是否有未保存改动
 *
 * Business Logic（为什么需要）:
 *   页脚状态文案和保存按钮需要基于当前表单与最近已保存快照比较，而不是基于单个字段猜测。
 *
 * Code Logic（做什么）:
 *   当前状态字段量很小，直接 JSON 序列化比较即可保持实现简单且确定。
 */
export function isSettingsStateDirty(current: SettingsState, baseline: SettingsState): boolean {
  return JSON.stringify(current) !== JSON.stringify(baseline);
}

/**
 * 读取截图快捷键值
 *
 * Business Logic（为什么需要）:
 *   保存时只需要提交后端认识的 screenshotHotkey 字段，页面内部则以 shortcuts 数组渲染。
 *
 * Code Logic（做什么）:
 *   从 shortcuts 数组查找 screenshot 项，找不到时返回 undefined，让调用方按 patch 语义跳过。
 */
function screenshotHotkeyFromState(state: SettingsState): string | undefined {
  return state.shortcuts.find((s) => s.id === 'screenshot')?.value;
}

/**
 * 生成 update_config patch
 *
 * Business Logic（为什么需要）:
 *   用户只修改快捷键时，保存不应夹带未改变的 deviceName/receiveDir，避免把异常空占位值写入基础设置。
 *
 * Code Logic（做什么）:
 *   对比当前状态与最近已保存快照，仅把实际变化的字段放入 payload；快捷键按后端字段名 screenshotHotkey 输出。
 */
export function buildConfigUpdate(
  current: SettingsState,
  baseline: SettingsState,
): SettingsConfigUpdate {
  const update: SettingsConfigUpdate = {};
  if (current.deviceName !== baseline.deviceName) {
    update.deviceName = current.deviceName;
  }
  if (current.receiveDir !== baseline.receiveDir) {
    update.receiveDir = current.receiveDir;
  }

  const currentHotkey = screenshotHotkeyFromState(current);
  const baselineHotkey = screenshotHotkeyFromState(baseline);
  if (currentHotkey !== baselineHotkey && currentHotkey !== undefined) {
    update.screenshotHotkey = currentHotkey;
  }
  return update;
}
