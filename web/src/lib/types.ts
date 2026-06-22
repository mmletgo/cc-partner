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
}

export type PermissionType = 'screenCapture' | 'inputMonitoring';

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
