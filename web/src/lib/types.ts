/**
 * 前端业务类型定义 - 与后端 models/*.py 对应
 */

export interface Prompt {
  id: string;
  title: string;
  content: string;
  tags: string[];
  /** @deprecated 使用 tags 字段代替 */
  tag?: string;
  updatedAt: string;
  vectorClock?: Record<string, number>;
}

/**
 * 速记本页面完整内容（对齐 Rust ScratchpadPage）。
 */
export interface ScratchpadPage {
  id: string;
  title: string;
  content: string;
  createdAt: string;
  updatedAt: string;
  deviceId: string;
  vectorClock: Record<string, number>;
  deleted: boolean;
}

/**
 * 速记本页面列表摘要（对齐 Rust ScratchpadPageSummary）。
 */
export interface ScratchpadPageSummary {
  id: string;
  title: string;
  updatedAt: string;
  deviceId: string;
  deleted: boolean;
}

/**
 * 速记本页面删除结果（对齐 Rust ScratchpadDeleteResult）。
 */
export interface ScratchpadDeleteResult {
  ok: boolean;
  pageId: string;
}

/**
 * 局域网同步结果（对齐 Rust SyncResult）。
 */
export interface LanSyncResult {
  accepted: boolean;
  synced: number;
  note: string;
}

export interface Device {
  id: string;
  name: string;
  address: string;
  port: number;
  status: 'online' | 'offline';
  lastSeen?: string;
}

export type TransferDirection = 'send' | 'receive';
export type TransferStatus = 'pending' | 'transferring' | 'completed' | 'failed' | 'cancelled';

export interface TransferTask {
  id: string;
  fileName: string;
  filePath: string;
  fileSize: number;
  direction: TransferDirection;
  status: TransferStatus;
  progress: number;
  peerDeviceId?: string;
  peerDeviceName?: string;
  speed?: number;
  errorMessage?: string;
  startedAt: string;
  completedAt?: string;
}

export interface AppConfig {
  deviceId: string;
  deviceName: string;
  receiveDir: string;
  screenshotHotkey: string;
  httpPort: number;
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
 * Prompt 优化响应（对齐 Rust optimize_prompt 返回）。
 */
export interface PromptOptimizeResponse {
  optimizedZh: string;
  optimizedEn: string;
}

export interface VersionInfo {
  version: string;
  buildDate: string;
}

export interface UpdateCheckResult {
  hasUpdate: boolean;
  version?: string;
  body?: string;
  /** 当前平台安装包的浏览器下载地址，无匹配资源时为空 */
  downloadUrl?: string;
  /** 当前平台安装包文件名，无匹配资源时为空 */
  filename?: string;
  /** 安装包字节数，无匹配资源时为 0 */
  size?: number;
  error?: string;
}

/** 更新下载状态机状态值 */
export type UpdateDownloadStatusValue =
  | 'idle'
  | 'downloading'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface UpdateDownloadStatus {
  status: UpdateDownloadStatusValue;
  /** 下载进度 0.0 ~ 1.0 */
  progress: number;
  error: string;
  filePath: string;
  url: string;
  filename: string;
  size: number;
}

export interface PermissionsStatus {
  screenCapture: { granted: boolean };
  inputMonitoring: { granted: boolean };
  accessibility: { granted: boolean };
  /** 通知权限（前端 JS API 检测合并；后端 check_permissions 不含此字段） */
  notification: { granted: boolean };
}

export type PermissionType = 'screenCapture' | 'inputMonitoring' | 'accessibility' | 'notification';

export interface PermissionRequestResult {
  ok: boolean;
  /** 是否触发了系统授权弹窗（仅 screenCapture 且首次可能为 true） */
  requested: boolean;
  /** 是否成功打开了系统设置面板 */
  opened: boolean;
  error?: string;
}

/**
 * Claude 历史采集——按 cwd 聚合的项目分组
 * 字段与 Rust 后端 list_cc_projects 命令返回对齐（camelCase）。
 */
export interface CcProject {
  /** 项目绝对路径（cwd），作为分组主键 */
  projectPath: string;
  /** 项目名（cwd 末段目录名） */
  projectName: string;
  /** 该项目下的用户输入 prompt 条数 */
  count: number;
  /** 最近一次采集时间（ISO） */
  lastOccurredAt: string;
}

/**
 * Claude 历史采集——单条用户输入 prompt
 * 字段与 Rust 后端 list_cc_prompts / get_cc_prompt 命令返回对齐（camelCase）。
 */
export interface CcHistoryItem {
  /** 主键 id */
  id: string;
  /** 来源项目绝对路径（cwd） */
  projectPath: string;
  /** 项目名（cwd 末段目录名） */
  projectName: string;
  /** Claude 会话 id */
  sessionId: string;
  /** 用户输入的 prompt 正文 */
  content: string;
  /** 采集时的 git 分支（可能为空） */
  gitBranch?: string;
  /** 采集时的 Claude Code 版本（可能为空） */
  ccVersion?: string;
  /** prompt 发生时间（ISO） */
  occurredAt: string;
  /** 采集设备 id（向量时钟用） */
  deviceId: string;
  /** 入库时间（ISO） */
  createdAt: string;
  /** 软删除标记 */
  deleted: boolean;
}

/**
 * SSH 连接目标配置（对齐后端 SshTargetDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   SSH 页为每个连接目标（局域网设备 IP 或手填 IP）保存用户名/端口，前端需消费后端
 *   list_ssh_targets / upsert_ssh_target 返回的 camelCase DTO。
 *
 * Code Logic（字段说明）:
 *   host 为主键（IP 或 hostname）；port 默认 22；username 空串表示用本机默认用户名；
 *   label 为可选备注；updatedAt 为最近更新时间（ISO，同步合并 LWW 依据）。
 */
export interface SshTarget {
  /** 主机 IP 或 hostname */
  host: string;
  /** SSH 端口，默认 22 */
  port: number;
  /** SSH 用户名（空串 = 用本机默认用户名） */
  username: string;
  /** 可选备注 */
  label?: string;
  /** 更新时间（ISO） */
  updatedAt: string;
}

/**
 * 本机操作系统信息（对齐后端 get_os_info 返回）。
 *
 * Business Logic（为什么需要这个类型）:
 *   SSH 页配置指南区需按本机系统渲染连接端用法，platform 由后端归一化后返回。
 *
 * Code Logic（字段说明）:
 *   platform 归一化为 mac/windows/ubuntu；raw 为 std::env::consts::OS 原始值（macos/windows/linux 等）。
 */
export interface OsInfo {
  /** 归一化平台：mac / windows / ubuntu */
  platform: 'mac' | 'windows' | 'ubuntu';
  /** 原始 OS 字符串 */
  raw: string;
}

/** Claude Code 资产类型：个人 skills / commands / plugins / user-scope MCP */
export type ClaudeCodeAssetKind = 'skill' | 'command' | 'plugin' | 'mcp';

/** Claude Code 资产展示 DTO（对齐后端 ClaudeCodeAsset，camelCase）。 */
export interface ClaudeCodeAsset {
  kind: ClaudeCodeAssetKind;
  id: string;
  name: string;
  scope: string;
  enabled: boolean;
  source: string;
  version?: string | null;
  description?: string | null;
  path?: string | null;
  sizeBytes?: number | null;
  updatedAt?: string | null;
  canEnable: boolean;
  canUninstall: boolean;
  canExport: boolean;
  warnings: string[];
}

/** Claude Code 资产选择器：局域网拉取只传用户勾选的项。 */
export interface ClaudeCodeAssetSelector {
  kind: ClaudeCodeAssetKind;
  id: string;
}

/** Claude Code 本地安装来源。 */
export interface ClaudeCodeInstallSource {
  kind: ClaudeCodeAssetKind;
  path?: string | null;
  name?: string | null;
  config?: unknown;
  overwrite: boolean;
}

/** Claude Code 资产安装/拉取的单项结果。 */
export interface ClaudeCodeAssetInstallItem {
  kind: ClaudeCodeAssetKind;
  id: string;
  name: string;
  status: 'installed' | 'skipped' | 'failed' | string;
  message: string;
}

/** Claude Code 资产安装/拉取结果。 */
export interface ClaudeCodeAssetInstallReport {
  ok: boolean;
  installed: number;
  skipped: number;
  failed: number;
  note: string;
  items: ClaudeCodeAssetInstallItem[];
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
  /** 是否开启喝水提醒 */
  waterEnabled: boolean;
  /** 喝水提醒间隔（秒） */
  waterIntervalSeconds: number;
  /** 是否开启全屏遮罩提醒（触发久坐提醒时每屏弹透明置顶遮罩） */
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
