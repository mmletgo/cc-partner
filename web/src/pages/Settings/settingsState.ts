import type {
  AppConfig,
  CloudSyncConfig,
  GithubTrendingConfig,
  HealthConfig,
  UpdateDownloadStatus,
  UpdateDownloadStatusValue,
} from '../../lib/types';
import {
  cloneHealthReminders,
  createDefaultHealthReminders,
  resetBuiltinHealthReminders,
  withResolvedTemplateCredits,
} from '../../lib/healthReminders';
import { getDefaultShortcutValue } from './shortcutRecorder';

/** 快捷键字段 id（与 shortcut 录制控件 / buildConfigUpdate 映射一一对应）。 */
export type ShortcutId = 'screenshot' | 'promptOptimizer' | 'promptQuickInput';

/**
 * 三个快捷键 label 的完整 i18n 子路径（不含 `settings:` 前缀）。
 *
 * Business Logic（为什么用字面量联合）:
 *   渲染层用 `t(\`settings:${labelKey}\`)` 拼接；字面量联合让 TS 展开模板为已知 key 联合，
 *   既保留 i18next 强类型校验，又支持三个分属不同命名空间的 label。
 */
export type ShortcutLabelKey =
  | 'shortcut.screenshot.label'
  | 'promptOptimizerSettings.hotkey.label'
  | 'promptQuickInputSettings.hotkey.label';

/** 三个快捷键 helper 的完整 i18n 子路径（不含 `settings:` 前缀），同 ShortcutLabelKey 思路。 */
export type ShortcutHelperKey =
  | 'shortcut.screenshot.helper'
  | 'promptOptimizerSettings.hotkey.helper'
  | 'promptQuickInputSettings.hotkey.helper';

/**
 * 单个快捷键字段定义。
 *
 * Business Logic（为什么 labelKey/helperKey 存完整 i18n 子路径）:
 *   screenshot 走 `shortcut.*` 命名空间，prompt 两个 hotkey 沿用各自设置块的既有文案
 *   （`promptOptimizerSettings.hotkey.*` / `promptQuickInputSettings.hotkey.*`），三者不同构，
 *   不能再用单一前缀拼接；存完整子路径让渲染层直接 `t('settings:' + key)`。
 */
export interface ShortcutField {
  id: ShortcutId;
  /** label 的完整 i18n 子路径（不含 `settings:` 前缀），如 `shortcut.screenshot.label`。 */
  labelKey: ShortcutLabelKey;
  /** helper 的完整 i18n 子路径（不含 `settings:` 前缀），如 `shortcut.screenshot.helper`。 */
  helperKey: ShortcutHelperKey;
  value: string;
}

/** Settings 页面整体表单状态 */
export interface SettingsState {
  deviceName: string;
  receiveDir: string;
  gamePluginDir: string;
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

export type HealthForm = HealthConfig;

/** Settings 页内子 tab id（与 Settings.tsx SETTINGS_TABS 对齐）。 */
export type SettingsTabId =
  | 'general'
  | 'dependencies'
  | 'health'
  | 'activity'
  | 'sync'
  | 'ai'
  | 'experimental'
  | 'about';

/** 内测功能页内嵌套功能 id。 */
export type ExperimentalFeatureId =
  | 'battery'
  | 'game'
  | 'browser'
  | 'automation'
  | 'cloudSync';

/** 合法 Settings tab 集合。 */
export const SETTINGS_TAB_IDS: readonly SettingsTabId[] = [
  'general',
  'dependencies',
  'health',
  'activity',
  'sync',
  'ai',
  'experimental',
  'about',
] as const;

const LEGACY_TAB_TO_FEATURE: Record<string, ExperimentalFeatureId> = {
  battery: 'battery',
  automation: 'automation',
};

/** 内测功能嵌套设置 tab 顺序（有独立设置的功能；网页浏览仅总开关）。 */
export const EXPERIMENTAL_SETTINGS_TAB_IDS: readonly ExperimentalFeatureId[] = [
  'battery',
  'game',
  'automation',
  'cloudSync',
];

/** 内测功能总开关顺序（充电 / 游戏 / 网页浏览 / 自动化 / 云同步）。 */
export const EXPERIMENTAL_FEATURE_IDS: readonly ExperimentalFeatureId[] = [
  'battery',
  'game',
  'browser',
  'automation',
  'cloudSync',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 深链与 Attention 跳转依赖 ?tab= 参数；未知值必须回退 general，避免空白面板。
 *   旧 `battery` / `automation` tab 已迁入内测功能页，仍解析为 experimental。
 *
 * Code Logic（这个函数做什么）:
 *   校验 raw 是否为已知 SettingsTabId 或遗留实验 tab，否则返回 fallback（默认 general）。
 */
export function resolveSettingsTabId(
  raw: string | null | undefined,
  fallback: SettingsTabId = 'general',
): SettingsTabId {
  if (!raw) return fallback;
  if (raw in LEGACY_TAB_TO_FEATURE) return 'experimental';
  return (SETTINGS_TAB_IDS as readonly string[]).includes(raw)
    ? (raw as SettingsTabId)
    : fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   内测功能页需要定位到充电/游戏/自动化/云同步区块；旧 `?tab=battery|automation` 必须落到对应功能。
 *
 * Code Logic（这个函数做什么）:
 *   优先读 `feature=`；否则把遗留 tab 名映射为 ExperimentalFeatureId。
 */
export function parseExperimentalFeatureFromSearch(search: string): ExperimentalFeatureId | null {
  const params = new URLSearchParams(
    search === '' || search.startsWith('?') ? search : `?${search}`,
  );
  const feature = params.get('feature');
  if (feature && (EXPERIMENTAL_FEATURE_IDS as readonly string[]).includes(feature)) {
    return feature as ExperimentalFeatureId;
  }
  const tab = params.get('tab');
  if (tab && tab in LEGACY_TAB_TO_FEATURE) {
    return LEGACY_TAB_TO_FEATURE[tab] ?? null;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   内测设置 tab 只列出已开启功能；深链 `feature=` 必须落在仍开启的功能上，否则回退第一个已开启项。
 *
 * Code Logic（这个函数做什么）:
 *   按 EXPERIMENTAL_SETTINGS_TAB_IDS 过滤已开启且带设置的功能；requested 仍开启则用之，否则取首个。
 */
export function resolveExperimentalSettingsTab(
  features: Record<ExperimentalFeatureId, boolean>,
  requested: ExperimentalFeatureId | null,
): ExperimentalFeatureId | null {
  const enabled = EXPERIMENTAL_SETTINGS_TAB_IDS.filter((id) => features[id]);
  if (enabled.length === 0) return null;
  if (
    requested &&
    features[requested] &&
    (EXPERIMENTAL_SETTINGS_TAB_IDS as readonly string[]).includes(requested)
  ) {
    return requested;
  }
  return enabled[0] ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 已挂载时 location.search 变化（含 Attention 跳转）必须同步 activeTab，不能只读初始值。
 *
 * Code Logic（这个函数做什么）:
 *   从 search 字符串读取 tab 并 resolve；供 effect / 测试复用。
 */
export function parseSettingsTabFromSearch(
  search: string,
  fallback: SettingsTabId = 'general',
): SettingsTabId {
  const params = new URLSearchParams(
    search === '' || search.startsWith('?') ? search : `?${search}`,
  );
  return resolveSettingsTabId(params.get('tab'), fallback);
}


/**
 * 可提交到 update_config 的 Settings 字段。
 *
 * Business Logic（为什么扩展 prompt hotkey）:
 *   Prompt 优化 / 收藏快捷输入的两个快捷键已并入常规 tab 的 shortcuts 数组，
 *   常规「保存」必须能把它们写到后端 `prompt_optimizer_hotkey` / `prompt_quick_input_hotkey`，
 *   避免双数据源或只有 AI tab 独立 apply 才生效的回归。
 */
export type SettingsConfigUpdate = Partial<
  Pick<
    AppConfig,
    | 'deviceName'
    | 'receiveDir'
    | 'gamePluginDir'
    | 'screenshotHotkey'
    | 'promptOptimizerHotkey'
    | 'promptQuickInputHotkey'
  >
>;

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
  reminders: createDefaultHealthReminders(),
};

/**
 * 快捷键字段定义（值由运行平台或后端配置决定，文案走 t）。
 *
 * Business Logic（为什么 labelKey/helperKey 是完整子路径）:
 *   三个快捷键分属不同 i18n 命名空间（shortcut.* / promptOptimizerSettings.hotkey.* /
 *   promptQuickInputSettings.hotkey.*），存完整子路径让渲染层用 `t('settings:' + key)` 统一解析。
 */
const SHORTCUT_FIELDS: Pick<ShortcutField, 'id' | 'labelKey' | 'helperKey'>[] = [
  {
    id: 'screenshot',
    labelKey: 'shortcut.screenshot.label',
    helperKey: 'shortcut.screenshot.helper',
  },
  {
    id: 'promptOptimizer',
    labelKey: 'promptOptimizerSettings.hotkey.label',
    helperKey: 'promptOptimizerSettings.hotkey.helper',
  },
  {
    id: 'promptQuickInput',
    labelKey: 'promptQuickInputSettings.hotkey.label',
    helperKey: 'promptQuickInputSettings.hotkey.helper',
  },
];

/** shortcut id → 后端 AppConfig 字段名映射，供 buildConfigUpdate 输出。 */
const SHORTCUT_CONFIG_KEY: Record<ShortcutId, keyof SettingsConfigUpdate> = {
  screenshot: 'screenshotHotkey',
  promptOptimizer: 'promptOptimizerHotkey',
  promptQuickInput: 'promptQuickInputHotkey',
};

/** Prompt 优化快捷键的前端兜底默认值（后端默认值相同）。 */
const DEFAULT_PROMPT_OPTIMIZER_HOTKEY = '<ctrl>';

/** 收藏快捷输入快捷键的前端兜底默认值（后端默认值相同）。 */
const DEFAULT_PROMPT_QUICK_INPUT_HOTKEY = '<ctrl>+/';

/**
 * 生成快捷键字段
 *
 * Business Logic（为什么需要）:
 *   设置页加载、恢复默认和初始占位都需要生成新快捷键对象，避免复用数组对象导致状态污染。
 *   Prompt 优化 / 收藏快捷输入的快捷键已从 AI tab 的 secondary 表单迁入常规 shortcuts 数组，
 *   这里统一从 AppConfig 的三个 hotkey 字段生成。
 *
 * Code Logic（做什么）:
 *   接收三个可选 hotkey；screenshot 未提供时按平台兜底，prompt 两个未提供时各自默认
 *   `<ctrl>` / `<ctrl>+/`；返回 SettingsState 可直接使用的字段数组。
 */
function createShortcutFields(opts: {
  screenshotHotkey?: string;
  promptOptimizerHotkey?: string;
  promptQuickInputHotkey?: string;
}): ShortcutField[] {
  return SHORTCUT_FIELDS.map((s) => {
    let value: string;
    if (s.id === 'screenshot') {
      value = opts.screenshotHotkey || getDefaultShortcutValue();
    } else if (s.id === 'promptOptimizer') {
      value = opts.promptOptimizerHotkey || DEFAULT_PROMPT_OPTIMIZER_HOTKEY;
    } else {
      value = opts.promptQuickInputHotkey || DEFAULT_PROMPT_QUICK_INPUT_HOTKEY;
    }
    return { ...s, value };
  });
}

/**
 * 生成加载前的占位状态
 *
 * Business Logic（为什么需要）:
 *   Settings 页在后端配置返回前需要一个受控输入占位状态；该状态只用于 loading 期间，
 *   不能作为“恢复默认”的真实默认值。
 *
 * Code Logic（做什么）:
 *   基础字段保持空字符串，快捷键用平台/各自兜底默认值，保证 React 输入始终受控。
 */
export function createPendingSettingsState(): SettingsState {
  return {
    deviceName: '',
    receiveDir: '',
    gamePluginDir: '',
    shortcuts: createShortcutFields({}),
  };
}

/**
 * 将后端 AppConfig 映射为 Settings 表单状态
 *
 * Business Logic（为什么需要）:
 *   后端配置是设备名、接收目录和三个快捷键（截图 / Prompt 优化 / 收藏快捷输入）的权威来源；
 *   前端保存快捷键时必须保留已加载的基础设置。
 *
 * Code Logic（做什么）:
 *   拷贝 deviceName/receiveDir，并把三个 hotkey 字段映射到 shortcuts 数组；缺失时各自走兜底默认值。
 */
export function settingsStateFromConfig(config: AppConfig): SettingsState {
  return {
    deviceName: config.deviceName,
    receiveDir: config.receiveDir,
    gamePluginDir: config.gamePluginDir,
    shortcuts: createShortcutFields({
      screenshotHotkey: config.screenshotHotkey,
      promptOptimizerHotkey: config.promptOptimizerHotkey,
      promptQuickInputHotkey: config.promptQuickInputHotkey,
    }),
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
 *   空 reminders 按旧 work/water 标量 seed 三条出厂模板。
 */
export function healthConfigToForm(config: HealthConfig | null): HealthForm {
  const source = config ?? PENDING_HEALTH_FORM;
  const reminders = source.reminders?.length
    ? cloneHealthReminders(source.reminders).map(withResolvedTemplateCredits)
    : createDefaultHealthReminders({
        workWindowSeconds: source.workWindowSeconds,
        waterIntervalSeconds: source.waterIntervalSeconds,
      });
  return {
    ...source,
    reminders,
    waterEnabled: true,
    reminderFullscreen: true,
  };
}

/**
 * 组装健康提醒 tab 提交 payload：只覆盖提醒切片，活动统计字段取已应用值。
 *
 * Business Logic（为什么需要）:
 *   update_health_config 是整包覆盖。健康提醒 tab 保存不得把活动统计 tab 的未保存草稿
 *   或过期值写进 recordWindowTitle / retainDays。
 *
 * Code Logic（做什么）:
 *   以 draft 为底，把活动统计两字段替换为 applied（无 applied 时回退 draft），并归一固定开关。
 */
export function mergeHealthReminderSlice(
  applied: HealthForm | null,
  draft: HealthForm,
): HealthForm {
  const activitySource = applied ?? draft;
  return {
    ...draft,
    reminders: cloneHealthReminders(draft.reminders),
    recordWindowTitle: activitySource.recordWindowTitle,
    retainDays: activitySource.retainDays,
    waterEnabled: true,
    reminderFullscreen: true,
  };
}

/**
 * 组装活动统计 tab 提交 payload：只覆盖统计切片，提醒字段取已应用值。
 *
 * Business Logic（为什么需要）:
 *   活动统计 tab 保存不得覆盖工作窗口、免打扰、通知等健康提醒字段。
 *
 * Code Logic（做什么）:
 *   以 applied（无则 draft）为底，只写入 draft 的 recordWindowTitle / retainDays，并归一固定开关。
 */
export function mergeActivityStatsSlice(
  applied: HealthForm | null,
  draft: HealthForm,
): HealthForm {
  const reminderSource = applied ?? draft;
  return {
    ...reminderSource,
    reminders: cloneHealthReminders(reminderSource.reminders),
    recordWindowTitle: draft.recordWindowTitle,
    retainDays: draft.retainDays,
    waterEnabled: true,
    reminderFullscreen: true,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   「恢复默认」只重置三条内置，不能把用户加过的自定义提醒清掉。
 *
 * Code Logic（这个函数做什么）:
 *   用出厂 reminders 替换内置三项，再按切片规则保留活动统计字段。
 */
export function resetHealthReminderDefaults(
  applied: HealthForm | null,
  draft: HealthForm,
  factory: HealthForm,
): HealthForm {
  return mergeHealthReminderSlice(applied, {
    ...factory,
    reminders: resetBuiltinHealthReminders(draft.reminders, factory.reminders),
  });
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
 * 读取指定快捷键值
 *
 * Business Logic（为什么需要）:
 *   buildConfigUpdate 需要按 shortcut id 比较 current/baseline 的值，决定是否写入对应后端字段。
 *
 * Code Logic（做什么）:
 *   从 shortcuts 数组查找对应 id 项，找不到时返回 undefined，让调用方按 patch 语义跳过。
 */
function shortcutValueFromState(state: SettingsState, id: ShortcutId): string | undefined {
  return state.shortcuts.find((s) => s.id === id)?.value;
}

/**
 * 生成 update_config patch
 *
 * Business Logic（为什么需要）:
 *   用户只修改快捷键时，保存不应夹带未改变的 deviceName/receiveDir，避免把异常空占位值写入基础设置。
 *   Prompt 优化 / 收藏快捷输入的快捷键已并入常规 shortcuts，必须随常规保存持久化到后端。
 *
 * Code Logic（做什么）:
 *   对比当前状态与最近已保存快照，仅把实际变化的字段放入 payload；三个快捷键按后端字段名
 *   screenshotHotkey / promptOptimizerHotkey / promptQuickInputHotkey 输出。
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
  if (current.gamePluginDir !== baseline.gamePluginDir) {
    update.gamePluginDir = current.gamePluginDir;
  }

  for (const field of SHORTCUT_FIELDS) {
    const currentHotkey = shortcutValueFromState(current, field.id);
    const baselineHotkey = shortcutValueFromState(baseline, field.id);
    if (currentHotkey !== baselineHotkey && currentHotkey !== undefined) {
      const configKey = SHORTCUT_CONFIG_KEY[field.id];
      update[configKey] = currentHotkey;
    }
  }
  return update;
}

/**
 * 更新 UI 阶段：后端 IPC 状态，或前端本地乐观 checking/installing。
 * local-* 用于后端尚未回写 status 时的按钮禁用与文案。
 */
export type UpdateUiPhase = UpdateDownloadStatusValue | 'local-checking' | 'local-installing';

/**
 * Business Logic（为什么需要这个函数）:
 *   检查更新与安装进行中时，用户不应再点“检查更新”，否则会并发触发冲突或掩盖当前阶段。
 *
 * Code Logic（这个函数做什么）:
 *   checkingUpdate 为 true，或 downloadStatus.status 为 checking/installing 时返回 true。
 */
export function isUpdateCheckDisabled(opts: {
  checkingUpdate: boolean;
  downloadStatus: UpdateDownloadStatus | null;
}): boolean {
  if (opts.checkingUpdate) return true;
  const status = opts.downloadStatus?.status;
  return status === 'checking' || status === 'installing';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检查/安装/下载进行中时禁止启动下载，避免与后端状态机冲突或重复下载。
 *
 * Code Logic（这个函数做什么）:
 *   checkingUpdate 为 true，或 status 属于 checking/installing/downloading 时返回 true。
 */
export function isUpdateDownloadDisabled(opts: {
  checkingUpdate: boolean;
  downloadStatus: UpdateDownloadStatus | null;
}): boolean {
  if (opts.checkingUpdate) return true;
  const status = opts.downloadStatus?.status;
  return status === 'checking' || status === 'installing' || status === 'downloading';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   安装失败后后端保留 completed + 非空 error（字节仍可重试安装）；UI 需与“下载成功可安装”区分。
 *
 * Code Logic（这个函数做什么）:
 *   仅当 status===completed 且 error.trim() 非空时返回 true；普通 completed 与 failed 下载为 false。
 */
export function shouldShowInstallRetry(status: UpdateDownloadStatus | null): boolean {
  if (!status || status.status !== 'completed') return false;
  return status.error.trim().length > 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前端需在检查/下载/安装阶段轮询 getDownloadStatus，终态停止，避免空转。
 *
 * Code Logic（这个函数做什么）:
 *   status 为 checking|downloading|installing 时返回 true。
 */
export function shouldPollUpdateStatus(status: UpdateDownloadStatus | null): boolean {
  const s = status?.status;
  return s === 'checking' || s === 'downloading' || s === 'installing';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   安装按钮文案在“安装并重启 / 安装中 / 重试安装”间切换，安装失败可重试且不与下载失败混淆。
 *
 * Code Logic（这个函数做什么）:
 *   installing 或 status=installing → installing；completed+error → retryInstall；否则 install。
 */
export function installButtonMode(opts: {
  installing: boolean;
  downloadStatus: UpdateDownloadStatus | null;
}): 'install' | 'installing' | 'retryInstall' {
  if (opts.installing || opts.downloadStatus?.status === 'installing') {
    return 'installing';
  }
  if (shouldShowInstallRetry(opts.downloadStatus)) {
    return 'retryInstall';
  }
  return 'install';
}
