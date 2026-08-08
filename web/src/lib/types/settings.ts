/**
 * Settings 与依赖/健康/云同步相关类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   设置页、依赖环境、健康提醒与云同步配置需要独立类型边界，避免塞回巨型 types monolith。
 *
 * Code Logic（这个模块做什么）:
 *   导出 AppConfig、依赖状态、防火墙指引、云同步、GitHub Trending 与 Health DTO；
 *   仅从 ./core 引入 upstream 类型（如 PromptOptimizerFillLanguage）。
 */

import type { PromptOptimizerFillLanguage } from './core';

export interface AppConfig {
  deviceId: string;
  deviceName: string;
  receiveDir: string;
  screenshotHotkey: string;
  promptOptimizerHotkey: string;
  promptOptimizerFillLanguage: PromptOptimizerFillLanguage;
  httpPort: number;
}

export type WorkbenchDependencyState =
  | 'checking'
  | 'ready'
  | 'missing'
  | 'installing'
  | 'installedNeedsRecheck'
  | 'unsupported'
  | 'failed';

export type WorkbenchDependencyBackend = 'native' | 'wsl' | string;

/**
 * 工作台运行时依赖状态（tmux）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Workbench 的真实 window/pane 体验依赖 tmux，前端需要展示检测、安装、失败和重检状态；
 *   Attention 还需要稳定的状态变更时间作为 environment 条目 updatedAt。
 *
 * Code Logic（字段说明）:
 *   对齐后端 dependency manager DTO；installCommandPreview 是只读预览，不代表前端可直接执行命令；
 *   statusChangedAt 是进程内语义状态最近变化时间（RFC3339），重复轮询不会重置。
 */
export interface WorkbenchDependencyStatus {
  status: WorkbenchDependencyState;
  available: boolean;
  version: string | null;
  backend: WorkbenchDependencyBackend;
  path: string | null;
  installable: boolean;
  installCommandPreview: string[];
  error: string | null;
  output: string[];
  statusChangedAt: string;
}

export type LanFirewallPlatform = 'macos' | 'windows' | 'linux' | 'unsupported' | string;

/**
 * 局域网防火墙依赖检测项。
 *
 * Business Logic（为什么需要这个类型）:
 *   Settings 依赖环境页需要直接展示 HTTP/LAN 基础状态和 TCP/mDNS 防火墙是否已开放。
 *
 * Code Logic（字段说明）:
 *   ok=true/false 表示后端按当前系统可读取信息给出的明确检测结果。
 */
export interface LanFirewallCheck {
  id: 'httpListener' | 'lanIp' | 'tcpFirewall' | 'mdnsFirewall' | string;
  ok: boolean;
  detail: string;
}

/**
 * 局域网防火墙方法步骤。
 *
 * Business Logic（为什么需要这个类型）:
 *   不同系统的放行方法需要前端用 i18n 文案渲染，后端只返回稳定 key。
 *
 * Code Logic（字段说明）:
 *   labelKey 是完整 i18n key，组件通过 t(labelKey) 转成当前语言文案。
 */
export interface LanFirewallStep {
  labelKey: string;
}

/**
 * 局域网防火墙可复制命令。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户需要按当前系统复制命令手动放行端口，但应用不应自动 sudo 或修改防火墙。
 *
 * Code Logic（字段说明）:
 *   labelKey 为命令说明的 i18n key；command 是后端按当前端口生成的只读命令字符串。
 */
export interface LanFirewallCommand {
  labelKey: string;
  command: string;
}

/**
 * 局域网防火墙平台指引。
 *
 * Business Logic（为什么需要这个类型）:
 *   Settings 依赖环境页需要展示系统方法、步骤和命令块。
 *
 * Code Logic（字段说明）:
 *   summaryKey/steps 的可见文案均走 i18n；commands 保留系统命令原文。
 */
export interface LanFirewallGuidance {
  summaryKey: string;
  steps: LanFirewallStep[];
  commands: LanFirewallCommand[];
}

/**
 * 局域网防火墙依赖状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   局域网互联访问项目需要本机 HTTP/P2P TCP 端口与 mDNS UDP 5353 被允许入站。
 *
 * Code Logic（字段说明）:
 *   对齐后端 check_lan_firewall_dependency camelCase DTO；checks 中含端口开放状态和系统放行指引。
 */
export interface LanFirewallDependencyStatus {
  platform: LanFirewallPlatform;
  platformLabel: string;
  lanIp: string | null;
  httpPort: number;
  mdnsPort: number;
  appPath: string | null;
  checks: LanFirewallCheck[];
  guidance: LanFirewallGuidance;
}

/**
 * GitHub 私有仓库云端同步配置
 * 字段与 Rust 后端 get_cloud_sync_config / update_cloud_sync_config 命令返回对齐（camelCase）。
 */
export interface CloudSyncConfig {
  /** 仓库地址，如 git@github.com:user/repo.git 或 https URL；未配置时为 null */
  repoUrl: string | null;
  /** 是否启用云端同步 */
  enabled: boolean;
  /** 是否自动定时同步 */
  auto: boolean;
  /** 自动同步间隔（秒） */
  intervalSecs: number;
  /** 同步分支；留空（null）用仓库默认分支 */
  branch: string | null;
}

/**
 * 触发一次云端同步的结果
 * 字段与 Rust 后端 trigger_cloud_sync_cmd 命令返回对齐（camelCase）。
 */
export interface CloudSyncResult {
  /** 同步是否成功 */
  ok: boolean;
  /** 本次拉取条数 */
  pulled: number;
  /** 本次推送条数 */
  pushed: number;
  /** 备注（成功/失败说明） */
  note: string;
  /** 同步完成时间（ISO） */
  syncedAt: string;
}

/**
 * 云端同步连通性测试结果
 * 字段与 Rust 后端 test_cloud_sync 命令返回对齐（camelCase）。
 */
export interface TestCloudSyncResult {
  /** 测试是否通过 */
  ok: boolean;
  /** 本机 git 版本（获取失败时为 null） */
  gitVersion: string | null;
  /** 仓库默认分支（获取失败时为 null） */
  defaultBranch: string | null;
  /** 失败原因（成功时为 null） */
  error: string | null;
}

/**
 * GitHub 周热门仓库卡片数据（对齐后端 list_github_trending_repos 返回）。
 */
export interface GithubTrendingRepo {
  rank: number;
  owner: string;
  name: string;
  fullName: string;
  url: string;
  description: string;
  language?: string | null;
  stars: number;
  forks: number;
  starsThisWeek: number;
  explanationZh: string;
  explanationEn: string;
}

export type GithubTrendingAiStatus = 'ready' | 'disabled' | 'failed';

/**
 * GitHub 周热门首页响应。
 */
export interface GithubTrendingResponse {
  repos: GithubTrendingRepo[];
  fetchedAt: string;
  expiresAt: string;
  fromCache: boolean;
  stale: boolean;
  aiStatus: GithubTrendingAiStatus;
  aiError?: string | null;
}

/**
 * GitHub Trending / Claude CLI 解说配置。
 */
export interface GithubTrendingConfig {
  aiEnabled: boolean;
  claudeCliPath: string;
  claudeModel: string;
  cacheTtlHours: number;
}

/**
 * Claude CLI 可用性测试结果。
 */
export interface ClaudeCliTestResult {
  ok: boolean;
  version?: string | null;
  error?: string | null;
}

/**
 * cc-partner 内部 Claude 调用所用 provider 覆盖配置。
 *
 * Business Logic:
 *   commit/merge/prompt 优化/GitHub 解说/verifier 等内部 headless Claude 调用可选使用一个
 *   不等于 OS 默认的 cc-switch provider。`providerId` 为 cc-switch claude provider id，
 *   `null` 表示沿用 OS 默认。本配置只持久化 id（不含凭据），后端运行时从 cc-switch 读取并写入
 *   隔离 CLAUDE_CONFIG_DIR，不改写 `~/.claude/settings.json`。
 */
export interface InternalClaudeConfig {
  providerId: string | null;
}

/**
 * 健康提醒配置（与后端 config.rs::HealthConfig 对齐，camelCase）。
 * 整体覆盖式回写（update_health_config 接收完整对象）。
 */
export interface HealthConfig {
  /** 是否开启久坐监测 */
  enabled: boolean;
  /** 连续工作多久触发提醒（秒） */
  workWindowSeconds: number;
  /** 停歇多久判定为休息、关闭工作窗口（秒） */
  breakSeconds: number;
  /** 是否记录前台窗口标题（统计用） */
  recordWindowTitle: boolean;
  /** 活动明细保留天数 */
  retainDays: number;
  /** 是否在提醒时弹系统通知 */
  notifyEnabled: boolean;
  /** 免打扰开始 "HH:MM"，null 表示不限制 */
  dndStart: string | null;
  /** 免打扰结束 "HH:MM"，null 表示不限制 */
  dndEnd: string | null;
  /** 喝水提醒历史开关；业务上随健康监测固定启用，不再展示独立设置项 */
  waterEnabled: boolean;
  /** 喝水提醒间隔（秒） */
  waterIntervalSeconds: number;
  /** 全屏遮罩历史开关；业务上随健康监测固定启用，不再展示独立设置项 */
  reminderFullscreen: boolean;
}

/** 健康提醒运行时状态相位 */
export type HealthPhase = 'idle' | 'working' | 'resting';

/**
 * 健康提醒运行时状态（get_health_status 返回，camelCase）。
 * 派生自状态机 + 配置 + 内存标记，非落盘数据。
 */
export interface HealthStatus {
  /** 是否开启监测 */
  enabled: boolean;
  /** 是否手动暂停 */
  paused: boolean;
  /** 当前相位 */
  phase: HealthPhase;
  /** 当前工作窗口开始时间戳（秒），null 表示无活动窗口 */
  windowStartTs: number | null;
  /** 工作窗口阈值（秒，来自配置） */
  workWindowSeconds: number;
  /** 休息判定阈值（秒，来自配置） */
  breakSeconds: number;
  /** 贪睡到期时间戳（秒），null 表示未贪睡 */
  snoozeUntil: number | null;
  /** 「开始休息」遮罩倒计时结束时间戳（秒），null 表示未在遮罩休息；多屏共享同一权威值 */
  overlayRestEndTs: number | null;
}

/**
 * 活动统计（get_activity_stats 返回，camelCase）。
 * 由 activity_records 表 SUM 聚合得出。
 */
export interface ActivityStats {
  /** 活跃分钟数 */
  activeMinutes: number;
  /** 闲置分钟数 */
  idleMinutes: number;
}

/**
 * 单个 app 的活跃分钟数排行项（get_activity_detail 返回，camelCase）。
 */
export interface AppUsageItem {
  /** 进程名 */
  name: string;
  /** 活跃分钟数 */
  minutes: number;
}

/**
 * 活动明细统计（get_activity_detail 返回，camelCase）。
 * app 使用时长排行 + 24 小时活跃分布，供 StatsChart 图表渲染。
 */
export interface ActivityDetail {
  /** 按活跃分钟倒序的 app 使用时长排行 */
  appUsage: AppUsageItem[];
  /** 长度恒为 24 的数组，下标为 UTC 小时（0-23），值为该小时活跃分钟数 */
  hourly: number[];
}

/** 习惯统计(饮水 + 休息)后端返回,对应 HabitStatsDto。 */
export interface HabitStats {
  /** 今日饮水次数。 */
  todayWaterCount: number;
  /** 近 N 天每日饮水次数,索引 0 = N-1 天前,末位 = 今日。 */
  waterDailyCounts: number[];
  /** 距今最近一次饮水时间戳(Unix 秒),无记录为 null/undefined。 */
  lastWaterTs?: number | null;
  /** 今日完成休息次数。 */
  todayRestCount: number;
  /** 今日完成休息总时长秒数。 */
  todayRestTotalSeconds: number;
  /** 今日久坐提醒触发次数。 */
  todayReminderCount: number;
  /** 近 N 天每日完成休息次数。 */
  restDailyCounts: number[];
}
