/**
 * 前端核心共享类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   Prompt/设备/传输/后端生命周期/权限/更新等跨页面共享 DTO 需要稳定边界，
 *   供 settings/workbench/orchestrator/attention 域只依赖 upstream core，避免回指兼容 barrel。
 *
 * Code Logic（这个模块做什么）:
 *   导出与后端 camelCase DTO 对齐的核心业务类型；不含 settings/workbench/orchestrator/attention 专属类型。
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
 * 内容版本 kind：history 为普通历史，conflict 为同步冲突副本。
 *
 * Business Logic（为什么需要这个类型）:
 *   Prompt/Scratchpad 版本历史 UI 需要区分冲突副本与普通历史，以展示非阻塞 Pill 与标签。
 *
 * Code Logic（这个类型做什么）:
 *   对齐后端 content_versions.kind 的 camelCase 枚举。
 */
export type ContentVersionKind = 'history' | 'conflict';

/**
 * 内容版本摘要 DTO（对齐 list_prompt_versions / list_scratchpad_versions）。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户需要查看版本摘要、恢复为新版本、复制冲突内容，前端不能只依赖 active 行。
 *
 * Code Logic（这个类型做什么）:
 *   承载版本 id、来源设备、hash、时间、kind 与可选标题/预览/全文；content 可选，供复制时优先使用。
 */
export interface ContentVersion {
  id: string;
  sourceDevice: string;
  contentHash: string;
  createdAt: string;
  kind: ContentVersionKind;
  title?: string | null;
  contentPreview?: string | null;
  /** 可选全文；后端若只返回预览则前端复制预览 */
  content?: string | null;
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
  /**
   * 对端能力清单提示（mDNS 非权威；可能被 TXT 长度裁剪）。
   * 用于 UI 预筛 resume 等动作；最终以后端 health/capability 校验为准。
   */
  capabilities?: string[];
  /** 协议版本提示（mDNS 非权威）。 */
  protoVersion?: number;
}

export type TransferDirection = 'send' | 'receive';
export type TransferStatus = 'pending' | 'transferring' | 'completed' | 'failed' | 'cancelled';

/**
 * 传输细粒度阶段（对齐 Rust TransferPhase camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   UI 动作矩阵与历史分区依赖 queued/connecting/transferring/finalizing 等阶段，
 *   不能只靠 coarse status 判断是否允许 cancel/retry/resume。
 *
 * Code Logic（这个类型做什么）:
 *   闭集字符串联合；未知后端值必须在 decoder 层 reject，不得映射 failed。
 */
export type TransferPhase =
  | 'queued'
  | 'connecting'
  | 'transferring'
  | 'finalizing'
  | 'completed'
  | 'cancelled'
  | 'failed';

/**
 * 失败发生阶段（对齐 Rust TransferFailureStage）。
 *
 * Business Logic（为什么需要这个类型）:
 *   retry/resume 决策依赖失败发生在 connect/transfer/finalize/source 等哪一步。
 *
 * Code Logic（这个类型做什么）:
 *   闭集字符串联合；未知 stage 由 decoder reject。
 */
export type TransferFailureStage =
  | 'connect'
  | 'transfer'
  | 'finalize'
  | 'source'
  | 'protocol'
  | 'local'
  | 'unknown';

/**
 * 结构化失败信息（对齐 Rust TransferFailure）。
 *
 * Business Logic（为什么需要这个类型）:
 *   失败卡需要稳定 code、是否可重试与用户可读 message，不能只靠 errorMessage 字符串。
 *
 * Code Logic（字段说明）:
 *   stage 为闭集；code/message 为字符串；retryable 决定是否渲染 Retry/Resume。
 */
export interface TransferFailure {
  stage: TransferFailureStage;
  code: string;
  retryable: boolean;
  message: string;
}

/**
 * 发送端 clientOperationId 对账结果（对齐 Rust TransferOperationStatus）。
 *
 * Business Logic（为什么需要这个类型）:
 *   timeout/断线后 UI 不得盲重试；必须先按 clientOperationId 查询 notFound/pending/succeeded/failed。
 *
 * Code Logic（这个类型做什么）:
 *   tag=`status` 判别联合；succeeded 带 taskId，failed 带稳定 code。
 */
export type TransferOperationStatus =
  | { status: 'notFound' }
  | { status: 'pending' }
  | { status: 'succeeded'; taskId: string }
  | { status: 'failed'; code: string };

/**
 * 传输任务 DTO（对齐 Rust TransferTaskDto camelCase + recovery 字段）。
 *
 * Business Logic（为什么需要这个类型）:
 *   列表与 retry/resume 返回值共享；UI 需要 phase/failure/attempt 身份字段驱动动作矩阵。
 *
 * Code Logic（字段说明）:
 *   保留 coarse status；N5 增补 phase/failure/attempt 与 logical/attempt/protocol/clientOperation 身份；
 *   可选字段兼容 mock/旧 fixture（后端 list/retry/resume 会填 recovery）。
 */
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
  /** 已传输字节数（进度数字展示；旧 fixture 可缺） */
  transferredBytes?: number;
  /** 细粒度阶段；旧 fixture 可缺，后端 list 会由 status 推导后填入 */
  phase?: TransferPhase;
  /** 结构化失败；非失败任务为 null/缺省 */
  failure?: TransferFailure | null;
  /** 同一 logical transfer 下的 attempt 序号 */
  attempt?: number;
  /** 逻辑传输 ID（跨 retry 稳定） */
  logicalTransferId?: string;
  /** 本 attempt 身份 */
  attemptId?: string;
  /** 协议层 transfer id（resume 复用） */
  protocolTransferId?: string;
  /** 发送端全局幂等键 */
  clientOperationId?: string | null;
  /** 操作 payload 规范哈希 */
  operationPayloadHash?: string | null;
}

/**
 * 发起传输后后端立即返回的受理结果（对齐 Rust send_transfer JSON）。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端 send 按钮需要确认任务已被后端 spawn，并用 id 定位新任务；
 *   不能再把返回值误当成完整 TransferTask。
 *
 * Code Logic（字段说明）:
 *   accepted 恒为 true；deviceId/filePath 回显请求；id 为新 transfer_id。
 */
export interface SendTransferResult {
  accepted: true;
  deviceId: string;
  filePath: string;
  id: string;
}

/**
 * 取消传输后后端返回的确认结果（对齐 Rust cancel_transfer JSON）。
 *
 * Business Logic（为什么需要这个类型）:
 *   取消是逐任务 busy 动作，前端需用 id 确认哪条任务取消成功。
 *
 * Code Logic（字段说明）:
 *   ok 恒为 true；id 为被取消的 taskId。任务不存在时后端 reject，不返回本结构。
 */
export interface CancelTransferResult {
  ok: true;
  id: string;
}

export type BackendStatusKind = 'running' | 'stopped' | 'stale' | 'error';

/**
 * 独立后端控制文件信息（对齐 Rust BackendControlFile）。
 *
 * Business Logic（为什么需要这个类型）:
 *   GUI 需要展示和管理后台 sidecar，状态查询必须携带 pid/port 等控制文件信息。
 *
 * Code Logic（字段说明）:
 *   camelCase 字段来自后端控制 JSON；controlToken 仅供本机 Tauri 命令内部管理，不在 UI 中展示。
 */
export interface BackendControlFile {
  pid: number;
  port: number;
  deviceId: string;
  deviceName: string;
  startedAt: string;
  controlToken: string;
}

/**
 * 独立后端生命周期状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   GUI 启动、关闭选择弹窗和调试入口都需要知道 sidecar 是 running/stopped/stale/error。
 *
 * Code Logic（字段说明）:
 *   kind 是固定状态枚举；control 在有控制文件时存在；error 保存状态读取或健康检查错误。
 */
export interface BackendStatus {
  kind: BackendStatusKind;
  control: BackendControlFile | null;
  error: string | null;
}

/**
 * 移动端局域网访问入口信息。
 *
 * Business Logic（为什么需要这个接口）:
 *   桌面端需要把当前设备可供手机浏览器访问的 `/mobile` URL 展示给用户或二维码组件。
 *
 * Code Logic（字段说明）:
 *   deviceName/port 来自后端当前配置和实际 HTTP 端口；urls 是过滤 loopback 后的同源访问地址列表。
 */
export interface MobileAccessInfo {
  deviceName: string;
  port: number;
  urls: string[];
}

export type PromptOptimizerFillLanguage = 'zh' | 'en';

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

/**
 * 更新下载/安装状态机状态值（对齐后端 get_download_status）。
 * checking：检查更新中；downloading：下载中；completed：下载完成可安装（若 error 非空表示安装失败可重试）；
 * installing：安装中；failed/cancelled：下载失败/取消；idle：空闲。
 */
export type UpdateDownloadStatusValue =
  | 'idle'
  | 'checking'
  | 'downloading'
  | 'completed'
  | 'installing'
  | 'failed'
  | 'cancelled';

export interface UpdateDownloadStatus {
  status: UpdateDownloadStatusValue;
  /** 下载进度 0.0 ~ 1.0；installing 阶段不应伪造进度条 */
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
  /** 通知权限（后端 UNUserNotificationCenter，按 Bundle 身份独立；非 plugin stub） */
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
