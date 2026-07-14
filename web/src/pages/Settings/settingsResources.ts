/**
 * Settings 资源分组加载器
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 同时依赖 11 个配置/版本端点；任一失败不应拖垮整页，
 *   且「恢复默认」失败时仍允许编辑当前值，业务 tab 失败只在对应 panel 重试。
 *
 * Code Logic（这个模块做什么）:
 *   用一次 Promise.allSettled 按稳定下标映射 11 个端点结果；
 *   分组为 core/defaults/version 与 4 个 current+defaults 业务组；
 *   提供单组 retry，不静默用 defaults 顶替 current。
 */
import type {
  AppConfig,
  CloudSyncConfig,
  GithubTrendingConfig,
  HealthConfig,
  VersionInfo,
} from '@/lib/types';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';

/**
 * 单个资源的判别联合结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   调用方必须显式处理 ready/error，避免把失败当成功或静默吞掉。
 *
 * Code Logic（这个类型做什么）:
 *   ready 携带值；error 携带 Error 实例。
 */
export type ResourceResult<T> =
  | { status: 'ready'; value: T }
  | { status: 'error'; error: Error };

/**
 * current + defaults 成对资源结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   业务 tab 需要各自的当前值与默认值；二者失败语义不同（当前失败阻断 panel，默认失败只禁 reset）。
 *
 * Code Logic（这个类型做什么）:
 *   分别保存 current/defaults 的 ResourceResult。
 */
export interface PairResourceResult<T> {
  current: ResourceResult<T>;
  defaults: ResourceResult<T>;
}

/** Settings 资源分组 id */
export type SettingsResourceGroup =
  | 'core'
  | 'defaults'
  | 'version'
  | 'cloudSync'
  | 'githubTrending'
  | 'health'
  | 'automation';

/**
 * Settings 页面一次加载的全部分组结果。
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层与各 panel 需要按组读取 ready/error，而不是整页成功/失败。
 *
 * Code Logic（这个接口做什么）:
 *   core/defaults/version 为单结果；业务组为 PairResourceResult。
 */
export interface SettingsResourceResults {
  core: ResourceResult<AppConfig>;
  defaults: ResourceResult<AppConfig>;
  version: ResourceResult<VersionInfo>;
  cloudSync: PairResourceResult<CloudSyncConfig>;
  githubTrending: PairResourceResult<GithubTrendingConfig>;
  health: PairResourceResult<HealthConfig>;
  automation: PairResourceResult<OrchestratorAutomationConfig>;
}

/**
 * Settings 资源加载依赖的 API 面（可注入，便于单测）。
 *
 * Business Logic（为什么需要这个接口）:
 *   生产走真实 config/health/github/orchestrator API；测试注入 fake 端点。
 *
 * Code Logic（这个接口做什么）:
 *   11 个 Promise 工厂，顺序与 SETTINGS_RESOURCE_ENDPOINT_ORDER 对齐。
 */
export interface SettingsResourceApi {
  getConfig: () => Promise<AppConfig>;
  getDefaults: () => Promise<AppConfig>;
  getVersion: () => Promise<VersionInfo>;
  getCloudSyncConfig: () => Promise<CloudSyncConfig>;
  getDefaultCloudSyncConfig: () => Promise<CloudSyncConfig>;
  getGithubTrendingConfig: () => Promise<GithubTrendingConfig>;
  getDefaultGithubTrendingConfig: () => Promise<GithubTrendingConfig>;
  getHealthConfig: () => Promise<HealthConfig>;
  getDefaultHealthConfig: () => Promise<HealthConfig>;
  getAutomationConfig: () => Promise<OrchestratorAutomationConfig>;
  getDefaultAutomationConfig: () => Promise<OrchestratorAutomationConfig>;
}

/**
 * 11 个端点的稳定下标顺序（allSettled 映射真源）。
 *
 * Business Logic（为什么需要这个常量）:
 *   下标映射必须与 load 实现一致，避免重排导致串结果。
 *
 * Code Logic（这个常量做什么）:
 *   文档化 0..10 端点语义，供测试断言。
 */
export const SETTINGS_RESOURCE_ENDPOINT_ORDER = [
  'core.current',
  'defaults.current',
  'version.current',
  'cloudSync.current',
  'cloudSync.defaults',
  'githubTrending.current',
  'githubTrending.defaults',
  'health.current',
  'health.defaults',
  'automation.current',
  'automation.defaults',
] as const;

/**
 * 把 unknown reject 规范为 Error。
 *
 * Business Logic（为什么需要这个函数）:
 *   allSettled 的 reason 可能是任意类型，UI 需要稳定的 Error.message。
 *
 * Code Logic（这个函数做什么）:
 *   Error 原样返回；其余包装为 Error(String(reason))。
 */
function toError(reason: unknown): Error {
  if (reason instanceof Error) return reason;
  return new Error(String(reason));
}

/**
 * 把 PromiseSettledResult 映射为 ResourceResult。
 *
 * Business Logic（为什么需要这个函数）:
 *   统一 fulfilled/rejected 形态，供分组聚合。
 *
 * Code Logic（这个函数做什么）:
 *   fulfilled → ready；rejected → error。
 */
function mapSettled<T>(settled: PromiseSettledResult<T>): ResourceResult<T> {
  if (settled.status === 'fulfilled') {
    return { status: 'ready', value: settled.value };
  }
  return { status: 'error', error: toError(settled.reason) };
}

/**
 * 判断资源是否 ready。
 *
 * Business Logic（为什么需要这个函数）:
 *   Settings 应用结果时需要类型收窄。
 *
 * Code Logic（这个函数做什么）:
 *   status === 'ready' 的 type guard。
 */
export function isResourceReady<T>(
  result: ResourceResult<T>,
): result is { status: 'ready'; value: T } {
  return result.status === 'ready';
}

/**
 * 成对资源是否允许「恢复默认」。
 *
 * Business Logic（为什么需要这个函数）:
 *   defaults 失败时仍可编辑 current，但必须禁用 reset，避免把 pending/空默认写进表单。
 *
 * Code Logic（这个函数做什么）:
 *   仅当 defaults 为 ready 时返回 true。
 */
export function canResetFromPair<T>(pair: PairResourceResult<T>): boolean {
  return pair.defaults.status === 'ready';
}

/**
 * 成对资源的 panel 级错误（仅 current 失败阻断 panel）。
 *
 * Business Logic（为什么需要这个函数）:
 *   业务 tab 失败只展示 current 错误；defaults 失败不整 panel 报错。
 *
 * Code Logic（这个函数做什么）:
 *   current 为 error 时返回 error，否则 null。
 */
export function pairCurrentError<T>(pair: PairResourceResult<T>): Error | null {
  return pair.current.status === 'error' ? pair.current.error : null;
}

/**
 * 加载 Settings 全部资源分组。
 *
 * Business Logic（为什么需要这个函数）:
 *   进入设置页时并行拉取 11 端点，任一失败不得 Promise.all 整页拒绝。
 *
 * Code Logic（这个函数做什么）:
 *   固定顺序 Promise.allSettled，再按稳定下标组装 SettingsResourceResults。
 *
 * @param api 可注入的 11 端点 API
 * @returns 分组判别结果
 */
export async function loadSettingsResources(
  api: SettingsResourceApi,
): Promise<SettingsResourceResults> {
  const settled = await Promise.allSettled([
    api.getConfig(),
    api.getDefaults(),
    api.getVersion(),
    api.getCloudSyncConfig(),
    api.getDefaultCloudSyncConfig(),
    api.getGithubTrendingConfig(),
    api.getDefaultGithubTrendingConfig(),
    api.getHealthConfig(),
    api.getDefaultHealthConfig(),
    api.getAutomationConfig(),
    api.getDefaultAutomationConfig(),
  ]);

  return {
    core: mapSettled(settled[0]),
    defaults: mapSettled(settled[1]),
    version: mapSettled(settled[2]),
    cloudSync: {
      current: mapSettled(settled[3]),
      defaults: mapSettled(settled[4]),
    },
    githubTrending: {
      current: mapSettled(settled[5]),
      defaults: mapSettled(settled[6]),
    },
    health: {
      current: mapSettled(settled[7]),
      defaults: mapSettled(settled[8]),
    },
    automation: {
      current: mapSettled(settled[9]),
      defaults: mapSettled(settled[10]),
    },
  };
}

/**
 * 仅重试一个资源分组。
 *
 * Business Logic（为什么需要这个函数）:
 *   panel 局部 retry 只请求失败分组，不得重置其他 tab 未保存草稿。
 *
 * Code Logic（这个函数做什么）:
 *   按 group 调用 1 或 2 个端点，allSettled 后返回该组结果切片。
 *
 * @param api 可注入 API
 * @param group 要重试的分组
 * @returns 该分组的最新 ResourceResult / PairResourceResult
 */
export async function retrySettingsResource(
  api: SettingsResourceApi,
  group: SettingsResourceGroup,
): Promise<
  | ResourceResult<AppConfig>
  | ResourceResult<VersionInfo>
  | PairResourceResult<CloudSyncConfig>
  | PairResourceResult<GithubTrendingConfig>
  | PairResourceResult<HealthConfig>
  | PairResourceResult<OrchestratorAutomationConfig>
> {
  switch (group) {
    case 'core':
      return mapSettled(await settleOne(api.getConfig()));
    case 'defaults':
      return mapSettled(await settleOne(api.getDefaults()));
    case 'version':
      return mapSettled(await settleOne(api.getVersion()));
    case 'cloudSync': {
      const [current, defaults] = await Promise.allSettled([
        api.getCloudSyncConfig(),
        api.getDefaultCloudSyncConfig(),
      ]);
      return { current: mapSettled(current), defaults: mapSettled(defaults) };
    }
    case 'githubTrending': {
      const [current, defaults] = await Promise.allSettled([
        api.getGithubTrendingConfig(),
        api.getDefaultGithubTrendingConfig(),
      ]);
      return { current: mapSettled(current), defaults: mapSettled(defaults) };
    }
    case 'health': {
      const [current, defaults] = await Promise.allSettled([
        api.getHealthConfig(),
        api.getDefaultHealthConfig(),
      ]);
      return { current: mapSettled(current), defaults: mapSettled(defaults) };
    }
    case 'automation': {
      const [current, defaults] = await Promise.allSettled([
        api.getAutomationConfig(),
        api.getDefaultAutomationConfig(),
      ]);
      return { current: mapSettled(current), defaults: mapSettled(defaults) };
    }
    default: {
      const _exhaustive: never = group;
      throw new Error(`Unknown settings resource group: ${String(_exhaustive)}`);
    }
  }
}

/**
 * 把单次 Promise 包成 allSettled 单项（复用 mapSettled）。
 *
 * Business Logic（为什么需要这个函数）:
 *   单端点 retry 与 11 端点 load 共用同一 settled→ResourceResult 映射。
 *
 * Code Logic（这个函数做什么）:
 *   Promise.allSettled([promise]) 后取 [0]。
 */
async function settleOne<T>(promise: Promise<T>): Promise<PromiseSettledResult<T>> {
  const [settled] = await Promise.allSettled([promise]);
  return settled;
}

/**
 * 用生产 API 模块构造 SettingsResourceApi。
 *
 * Business Logic（为什么需要这个函数）:
 *   Settings 页面需要真实 invoke 端点，同时保持 loadSettingsResources 可测。
 *
 * Code Logic（这个函数做什么）:
 *   绑定 configApi / githubTrendingApi / healthApi / orchestratorConfigApi。
 */
export function createSettingsResourceApi(deps: {
  configApi: {
    get: () => Promise<AppConfig>;
    getDefaults: () => Promise<AppConfig>;
    version: () => Promise<VersionInfo>;
    getCloudSyncConfig: () => Promise<CloudSyncConfig>;
    getDefaultCloudSyncConfig: () => Promise<CloudSyncConfig>;
  };
  githubTrendingApi: {
    getConfig: () => Promise<GithubTrendingConfig>;
    getDefaultConfig: () => Promise<GithubTrendingConfig>;
  };
  healthApi: {
    getConfig: () => Promise<HealthConfig>;
    getDefaultConfig: () => Promise<HealthConfig>;
  };
  orchestratorConfigApi: {
    get: () => Promise<OrchestratorAutomationConfig>;
    getDefaults: () => Promise<OrchestratorAutomationConfig>;
  };
}): SettingsResourceApi {
  return {
    getConfig: () => deps.configApi.get(),
    getDefaults: () => deps.configApi.getDefaults(),
    getVersion: () => deps.configApi.version(),
    getCloudSyncConfig: () => deps.configApi.getCloudSyncConfig(),
    getDefaultCloudSyncConfig: () => deps.configApi.getDefaultCloudSyncConfig(),
    getGithubTrendingConfig: () => deps.githubTrendingApi.getConfig(),
    getDefaultGithubTrendingConfig: () => deps.githubTrendingApi.getDefaultConfig(),
    getHealthConfig: () => deps.healthApi.getConfig(),
    getDefaultHealthConfig: () => deps.healthApi.getDefaultConfig(),
    getAutomationConfig: () => deps.orchestratorConfigApi.get(),
    getDefaultAutomationConfig: () => deps.orchestratorConfigApi.getDefaults(),
  };
}
