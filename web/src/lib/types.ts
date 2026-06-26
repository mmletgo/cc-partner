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
  promptOptimizerHotkey: string;
  promptOptimizerFillLanguage: PromptOptimizerFillLanguage;
  httpPort: number;
}

export type PromptOptimizerFillLanguage = 'zh' | 'en';

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
 *   Workbench 的真实 window/pane 体验依赖 tmux，前端需要展示检测、安装、失败和重检状态。
 *
 * Code Logic（字段说明）:
 *   对齐后端 dependency manager DTO；installCommandPreview 是只读预览，不代表前端可直接执行命令。
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

/** 工作台项目来源类型：本期仅 local，后续扩展局域网设备项目。 */
export type WorkbenchProjectKind = 'local' | string;

/**
 * 工作台项目 DTO（对齐 Rust WorkbenchProjectDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   工作台需要展示用户添加过的项目文件夹，并把 projectId 传给终端与文件树命令。
 *
 * Code Logic（字段说明）:
 *   path 是本机或已挂载局域网目录的绝对路径；lastOpenedAt 用于最近项目排序。
 */
export interface WorkbenchProject {
  id: string;
  name: string;
  kind: WorkbenchProjectKind;
  deviceId: string;
  deviceName: string;
  path: string;
  lastOpenedAt: string;
  createdAt: string;
  updatedAt: string;
}

/**
 * 工作台 Git 状态摘要。
 *
 * Business Logic（为什么需要这个类型）:
 *   Workbench worktree 管理层需要让用户快速判断当前工作区是否干净、是否领先/落后远端以及是否有冲突。
 *
 * Code Logic（字段说明）:
 *   changed/conflicts 是本地 `git status --porcelain --branch` 的摘要计数；clean/canPush 为后端派生布尔值。
 */
export interface WorkbenchGitStatus {
  branch: string | null;
  changed: number;
  ahead: number;
  behind: number;
  conflicts: number;
  clean: boolean;
  canPush: boolean;
}

/**
 * 工作台 Git worktree DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   一个项目下可以有主工作区和多个功能 worktree，顶部 worktree strip 负责切换它们。
 *
 * Code Logic（字段说明）:
 *   path 是该 worktree 的绝对路径；status 是运行期 Git 状态，不代表落库字段。
 */
export interface WorkbenchWorktree {
  id: string;
  projectId: string;
  name: string;
  branch: string | null;
  baseBranch: string | null;
  path: string;
  isMain: boolean;
  status: WorkbenchGitStatus;
  createdAt: string;
  updatedAt: string;
}

/** Workbench 一键合并阶段状态。 */
export type WorkbenchMergeStageStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped';

/** Workbench 一键合并阶段 id。 */
export type WorkbenchMergeStageId =
  | 'checkSource'
  | 'closeSessions'
  | 'mergeMain'
  | 'resolveConflicts'
  | 'cleanup';

/**
 * Workbench 一键合并阶段 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   一键合并会跨越检查、关闭终端、合并、AI 解冲突和清理，前端需要逐阶段展示进度。
 *
 * Code Logic（字段说明）:
 *   id/status 由后端阶段机产生；message 是后端给用户看的当前阶段说明或失败原因。
 */
export interface WorkbenchMergeStage {
  id: WorkbenchMergeStageId;
  status: WorkbenchMergeStageStatus;
  message: string;
}

/**
 * Workbench 一键合并结果 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   合并完成后前端需要知道 worktree 已被合并清理，并展示完整阶段结果。
 *
 * Code Logic（字段说明）:
 *   stages 按后端实际执行结果返回；前端 helper 会补齐缺失阶段用于稳定渲染。
 */
export interface WorkbenchMergeResult {
  ok: boolean;
  worktreeId: string;
  stages: WorkbenchMergeStage[];
}

/**
 * Workbench 一键合并进度事件 payload。
 *
 * Business Logic（为什么需要这个类型）:
 *   多项目或多窗口同时存在时，前端只能展示当前项目的 merge 进度，不能被其他项目事件串台。
 *
 * Code Logic（字段说明）:
 *   projectId/worktreeId 用于事件过滤；stage 是当前阶段的最新状态。
 */
export interface WorkbenchMergeProgressEvent {
  projectId: string;
  worktreeId: string;
  stage: WorkbenchMergeStage;
}

/**
 * 工作台 Git 引用类型。
 *
 * Business Logic（为什么需要这个类型）:
 *   Git 历史树需要区分本地分支、远端分支和 tag，让用户知道云端与本地位置。
 *
 * Code Logic（字段说明）:
 *   与 Rust enum camelCase 序列化结果保持一致。
 */
export type WorkbenchGitRefKind = 'local' | 'remote' | 'tag' | 'head' | 'other';

/**
 * 工作台 Git 引用标签。
 *
 * Business Logic（为什么需要这个类型）:
 *   提交旁需要展示 main、origin/main、tag 等标签，并标识当前 HEAD。
 *
 * Code Logic（字段说明）:
 *   remote 仅远端分支有值；isHead 表示当前 worktree HEAD 指向该 ref。
 */
export interface WorkbenchGitRef {
  name: string;
  fullName: string;
  kind: WorkbenchGitRefKind;
  remote: string | null;
  isHead: boolean;
}

/**
 * 工作台 Git 提交历史项。
 *
 * Business Logic（为什么需要这个类型）:
 *   右侧 Git 历史 tab 需要展示当前 active worktree 的最近提交和 Git 树。
 *
 * Code Logic（字段说明）:
 *   parentHashes 用于 graph lane 计算；refs 用于标识本地/远端/tag；authoredAt 为 ISO 字符串。
 */
export interface WorkbenchGitCommit {
  hash: string;
  shortHash: string;
  parentHashes: string[];
  authorName: string;
  authorEmail: string;
  authoredAt: string;
  summary: string;
  refs: WorkbenchGitRef[];
}

/** 工作台终端会话状态。 */
export type WorkbenchSessionStatus = 'running' | 'exited' | 'disconnected' | string;

/**
 * 工作台项目 terminal window DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   一个项目可开启多个 terminal window，tmux backend 下 window 内 pane 由 tmux 管理。
 *
 * Code Logic（字段说明）:
 *   window 元数据由后端持久化；paneCount 来自后端 tmux 查询或 raw PTY 兜底；终端输出通过 workbench:terminal-output 事件增量推送。
 */
export interface WorkbenchSession {
  id: string;
  projectId: string;
  worktreeId: string | null;
  name: string;
  command: string;
  cwd: string;
  status: WorkbenchSessionStatus;
  cols: number;
  rows: number;
  startedAt: string;
  exitedAt: string | null;
  exitCode: number | null;
  supportsPanes: boolean;
  paneCount: number;
}

/** 工作台文件节点类型：文件或文件夹。 */
export type WorkbenchPathKind = 'file' | 'dir' | string;

/**
 * 工作台文件树节点 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   右侧检查器本期展示可交互项目文件夹，后续文件预览会基于同一节点模型扩展。
 *
 * Code Logic（字段说明）:
 *   path 是相对项目根的路径，children 为 null/undefined 表示尚未加载或非目录。
 */
export interface WorkbenchFileNode {
  name: string;
  path: string;
  kind: WorkbenchPathKind;
  size: number | null;
  modifiedAt: string | null;
  children?: WorkbenchFileNode[] | null;
}

/**
 * 工作台单路径信息 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   创建、重命名、选中路径后，前端需要最新元信息刷新文件树和检查器详情。
 *
 * Code Logic（字段说明）:
 *   与 WorkbenchFileNode 去掉 children 后一致，表示单个路径的 metadata。
 */
export interface WorkbenchPathInfo {
  name: string;
  path: string;
  kind: WorkbenchPathKind;
  size: number | null;
  modifiedAt: string | null;
}

/** Workbench 文件内容检测类型，和 Rust WorkbenchDetectedFileType 对齐，HTML 独立于普通 code 类型。 */
export type WorkbenchDetectedFileType =
  | 'image'
  | 'markdown'
  | 'html'
  | 'code'
  | 'json'
  | 'toml'
  | 'yaml'
  | 'csv'
  | 'sqlite'
  | 'text'
  | 'binary'
  | 'unsupported';

/** Workbench 文件工作区显示模式，和 Rust WorkbenchFileMode 对齐。 */
export type WorkbenchFileMode = 'viewer' | 'editor' | 'wysiwyg' | 'source' | 'split';

/** 文件可执行操作能力，用于控制预览、编辑、格式化和默认打开模式。 */
export interface WorkbenchFileCapabilities {
  canPreview: boolean;
  canEdit: boolean;
  canFormat: boolean;
  mustValidateBeforeSave: boolean;
  defaultMode: WorkbenchFileMode;
  availableModes: WorkbenchFileMode[];
}

/** 可编辑文本文件内容与保存基线。 */
export interface WorkbenchTextContent {
  content: string;
  baseHash: string;
  baseModifiedAt: string | null;
}

/** 图片只读预览数据。 */
export interface WorkbenchImagePreview {
  dataUrl: string;
  mime: string;
  width: number | null;
  height: number | null;
}

/** CSV/TSV 只读表格预览数据。 */
export interface WorkbenchCsvPreview {
  columns: string[];
  rows: string[][];
  truncated: boolean;
}

/** SQLite 只读表格预览数据。 */
export interface WorkbenchSqlitePreview {
  tables: string[];
  selectedTable: string | null;
  columns: string[];
  rows: string[][];
  truncated: boolean;
}

/** 打开文件响应：包含 metadata、类型能力与某一种内容/预览载荷。 */
export interface WorkbenchOpenFile {
  metadata: WorkbenchPathInfo;
  detectedType: WorkbenchDetectedFileType;
  capabilities: WorkbenchFileCapabilities;
  text: WorkbenchTextContent | null;
  image: WorkbenchImagePreview | null;
  csv: WorkbenchCsvPreview | null;
  sqlite: WorkbenchSqlitePreview | null;
  truncated: boolean;
  notice: string | null;
}

/** 保存文本文件后的最新 metadata 与下一次保存基线。 */
export interface WorkbenchSaveTextResult {
  metadata: WorkbenchPathInfo;
  baseHash: string;
  baseModifiedAt: string | null;
}

/** JSON/TOML 格式化结果。 */
export interface WorkbenchFormatResult {
  formatted: string;
}

/** 工作台终端输出事件 payload（listen('workbench:terminal-output')）。 */
export interface WorkbenchTerminalOutputEvent {
  sessionId: string;
  chunk: string;
  seq: number;
  ts: number;
}

/** 工作台终端状态事件 payload（listen('workbench:terminal-status')）。 */
export interface WorkbenchTerminalStatusEvent {
  sessionId: string;
  status: WorkbenchSessionStatus;
  exitCode: number | null;
  ts: number;
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
