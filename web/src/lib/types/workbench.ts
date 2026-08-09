/**
 * Workbench 域类型（项目/终端/文件/Git/浏览器预览）。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台 DTO 体量最大，按域拆分后便于 controller 与 mobile transport 精准引用，而不拖入 settings/orchestrator 杂项。
 *
 * Code Logic（这个模块做什么）:
 *   导出 Workbench 项目、worktree、session、文件工作区、合并进度、浏览器预览与 Claude session 搜索相关类型。
 */

/** 工作台项目来源类型：本机项目或局域网远端项目。 */
export type WorkbenchProjectKind = 'local' | 'remote' | string;

/**
 * Workbench 远端可浏览根目录。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户需要在局域网设备上直接选择项目文件夹，前端先展示远端声明的可浏览入口。
 *
 * Code Logic（字段说明）:
 *   label 是 UI 展示名；path 是远端设备上的绝对路径；kind 用于区分 home/volume 等后端来源。
 */
export interface WorkbenchRemoteRoot {
  label: string;
  path: string;
  kind: string;
}

/**
 * Workbench 远端目录项。
 *
 * Business Logic（为什么需要这个类型）:
 *   远端项目选择器需要浏览局域网设备目录，并提示哪些目录看起来是 Git 项目。
 *
 * Code Logic（字段说明）:
 *   kind 标识文件/目录；modifiedAt 为远端时间戳或 null；isGitRepo 表示该目录是否包含 Git 仓库特征。
 */
export interface WorkbenchRemoteDirectoryEntry {
  name: string;
  path: string;
  kind: 'dir' | 'file' | string;
  modifiedAt: string | null;
  isGitRepo: boolean;
}

/**
 * Workbench 远端路径信息。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户选择远端目录后，前端需要知道路径是否可读、是否项目目录，以及建议的项目名。
 *
 * Code Logic（字段说明）:
 *   readable/isGitRepo 由后端探测；suggestedProjectName 用于打开远端项目后命名。
 */
export interface WorkbenchRemotePathInfo {
  name: string;
  path: string;
  kind: 'dir' | 'file' | string;
  readable: boolean;
  isGitRepo: boolean;
  suggestedProjectName: string;
}

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

/**
 * 不确定传输类别（与 asyncState/mutationOutcome 对齐）。
 *
 * Business Logic（为什么需要这个类型）:
 *   timeout/network 无法区分 not-started 与已执行，前端必须标 unknown 并对账。
 *
 * Code Logic（联合形态）:
 *   wire 用小写 token：timeout | network。
 */
export type MutationTransportClass = 'timeout' | 'network';

/**
 * Workbench mutation 成功通道 envelope（与 Rust WorkbenchMutationEnvelopeDto 对齐）。
 *
 * Business Logic（为什么需要这个类型）:
 *   commit/push/merge/remove 在 uncertain transport 下不能猜成失败或成功，只能 succeeded | unknown；
 *   本机 commit/push 因 pre-commit/pre-push 钩子失败时走 failedHook，让前端展示「让 AI 修复并重试」。
 *
 * Code Logic（联合形态）:
 *   succeeded 带权威 value；unknown 仅带 clientOperationId 与可选 transportClass；
 *   failedHook 携带结构化 hook 输出（仅本机 commit/push 产生，远端/P2P 不产生）。
 */
export type WorkbenchMutationEnvelope<T> =
  | { kind: 'succeeded'; value: T; clientOperationId: string }
  | {
      kind: 'unknown';
      clientOperationId: string;
      transportClass?: MutationTransportClass;
    }
  | {
      kind: 'failedHook';
      clientOperationId: string;
      hookFailure: WorkbenchHookFailure;
    };

/** pre-commit / pre-push 钩子阶段（与 Rust WorkbenchHookStage 对齐）。 */
export type WorkbenchHookStage = 'preCommit' | 'prePush';

/**
 * 结构化的 hook 钩子失败（failedHook envelope 载荷）。
 *
 * Business Logic（为什么需要这个类型）:
 *   把钩子原始 stdout/stderr/退出码交给前端展示与修复 agent；禁止靠文案匹配判业务。
 */
export interface WorkbenchHookFailure {
  stage: WorkbenchHookStage;
  stdout: string;
  stderr: string;
  exitCode?: number;
}

/**
 * 启动 hook 修复 agent 的返回值（与 Rust RepairHookFailureDto 对齐）。
 *
 * Business Logic（为什么需要这个类型）:
 *   failedHook 之后「让 AI 修复」按钮调 repair_worktree_hook_failure；返回 agent/terminal id
 *   供前端聚焦终端并展示「重试」入口；projectId 隔离便于 cross-project 调用。
 */
export interface WorkbenchRepairHookFailureDto {
  agentSessionId: string;
  terminalSessionId: string;
  worktreeId: string;
  projectId: string;
}

/** mutation 种类（ledger / wire 小写 token）。 */
export type MutationKind = 'commit' | 'push' | 'merge' | 'remove';

/** ledger 状态。 */
export type MutationState = 'claimed' | 'running' | 'succeeded' | 'failed';

/**
 * reconciliation intent（执行前捕获，与 Rust MutationIntent camelCase 对齐）。
 *
 * Business Logic（为什么需要这个类型）:
 *   unknown 后必须按精确后置条件确认，不能用 message 代替 tree/ref/identity。
 *
 * Code Logic（联合形态）:
 *   tag=kind 的 camelCase 联合。
 */
export type MutationIntent =
  | {
      kind: 'commit';
      projectId: string;
      worktreeId: string;
      beforeHead: string | null;
      expectedTree: string;
    }
  | {
      kind: 'push';
      projectId: string;
      worktreeId: string;
      localRef: string;
      remoteRef: string;
      localHead: string;
    }
  | {
      kind: 'merge';
      projectId: string;
      sourceWorktreeId: string;
      sourceHead: string;
      mainHead: string;
    }
  | {
      kind: 'remove';
      projectId: string;
      worktreeId: string;
      path: string;
      branch: string | null;
    };

/**
 * ledger 中的一条 operation 记录（对齐 Rust WorkbenchMutationOperationDto）。
 *
 * Business Logic（为什么需要这个类型）:
 *   unknown 后前端按 clientOperationId 查询 owning sidecar ledger，取得 intent/state。
 *
 * Code Logic（字段说明）:
 *   outcome 为成功时的权威 value JSON；失败时为 null。
 */
export type WorkbenchMutationOperation = {
  clientOperationId: string;
  kind: MutationKind;
  payloadHash: string;
  intent: MutationIntent;
  state: MutationState;
  outcome: unknown | null;
  errorMessage: string | null;
  projectId: string | null;
  worktreeId: string | null;
  createdAt: string;
  updatedAt: string;
};

/**
 * 权威 Git/worktree 状态快照，供纯 confirm 矩阵使用。
 *
 * Business Logic（为什么需要这个类型）:
 *   对账只看精确后置条件，不猜 message 或列表文案。
 *
 * Code Logic（字段说明）:
 *   字段均可空；缺失时 confirm 返回 unknown。
 */
export type MutationAuthoritySnapshot = {
  head?: string | null;
  headTree?: string | null;
  headParent?: string | null;
  remoteRefHead?: string | null;
  mainContainsSourceHead?: boolean | null;
  sourceWorktreePresent?: boolean | null;
  worktreeIdentityPresent?: boolean | null;
};

/** Workbench 浏览器预览候选来源。 */
export type WorkbenchBrowserTargetSource =
  | 'remembered'
  | 'terminalOutput'
  | 'projectConfig'
  | 'portProbe'
  | 'manual';

/**
 * Workbench 浏览器预览目标候选。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户打开浏览器预览时，需要看到从历史选择、终端输出、项目配置或端口探测得到的 dev server 候选。
 *
 * Code Logic（字段说明）:
 *   url 是后端规范化后的可代理目标；displayUrl/source/reachable 用于前端展示与排序提示；label 仅是兼容字段里的稳定 key，不作为用户可见文案来源。
 */
export interface WorkbenchBrowserTarget {
  id: string;
  url: string;
  displayUrl: string;
  source: WorkbenchBrowserTargetSource;
  label: string;
  reachable: boolean;
}

/**
 * Workbench 浏览器预览发现结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   浏览器工作区挂载时需要一次性获取当前项目/worktree 下的候选目标和默认选择。
 *
 * Code Logic（字段说明）:
 *   targets 已由后端排序；selectedTargetId 为空时前端可回退到首个候选。
 */
export interface WorkbenchBrowserDiscovery {
  projectId: string;
  worktreeId: string | null;
  targets: WorkbenchBrowserTarget[];
  selectedTargetId: string | null;
}

/**
 * Workbench 浏览器预览代理会话。
 *
 * Business Logic（为什么需要这个类型）:
 *   桌面端和手机端访问同一个 dev server 时，需要分别使用 loopback 绝对 URL 和移动端同源 path。
 *
 * Code Logic（字段说明）:
 *   previewId 标识后端 registry session；desktopProxyUrl/mobileProxyPath 分别供 desktop/mobile iframe 使用。
 */
export interface WorkbenchBrowserPreview {
  previewId: string;
  projectId: string;
  worktreeId: string | null;
  targetUrl: string;
  desktopProxyUrl: string;
  mobileProxyPath: string;
  expiresAtMs: number;
}

/** 浏览器验证会话状态。 */
export type BrowserVerificationState =
  | 'queued'
  | 'running'
  | 'succeeded'
  | 'failed'
  | 'canceled';

/**
 * 浏览器验证会话 DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   一键验证需要展示 run 状态与错误码；不包含 target URL 作为能力。
 *
 * Code Logic（字段说明）:
 *   id/previewId/state 与后端 camelCase 对齐。
 */
export interface BrowserVerificationSession {
  id: string;
  projectId: string;
  worktreeId?: string | null;
  previewId: string;
  ownerInstanceId: string;
  state: BrowserVerificationState;
  createdAt: string;
  lastActivityAt: string;
  expiresAt: string;
  errorCode?: string | null;
  errorMessage?: string | null;
}

/**
 * 浏览器验证 evidence 摘要。
 *
 * Business Logic（为什么需要这个类型）:
 *   UI 展示 a11y/console/screenshot 摘要，不含 fill value。
 *
 * Code Logic（字段说明）:
 *   screenshotId 用于拉取 artifact；consoleErrors 已脱敏。
 */
export interface BrowserVerificationEvidence {
  sessionId: string;
  urlPath: string;
  pageTitle?: string | null;
  assertions: Array<{ name: string; passed: boolean; detail?: string | null }>;
  consoleErrors: Array<{ sequence: number; level: string; text: string; timestampMs: number }>;
  screenshotId?: string | null;
  truncated: boolean;
  capturedAt: string;
}

/**
 * 浏览器验证 run DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   start/get/cancel 返回完整 run。
 *
 * Code Logic（字段说明）:
 *   session + 可选 evidence；commandResults 为结构化结果。
 */
export interface BrowserVerificationRun {
  session: BrowserVerificationSession;
  evidence?: BrowserVerificationEvidence | null;
  commandResults?: unknown[];
}

/**
 * 浏览器验证 artifact DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   展示截图 PNG（base64）。
 *
 * Code Logic（字段说明）:
 *   base64 为 PNG 字节；无 cookie/value。
 */
export interface BrowserVerificationArtifact {
  runId: string;
  artifactId: string;
  contentType: string;
  byteLen: number;
  base64: string;
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
  /** 名称来源：default | auto | manual；缺省兼容旧后端。 */
  nameSource?: 'default' | 'auto' | 'manual' | string;
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

/**
 * 工作台终端最近输出 replay DTO。
 *
 * Business Logic（为什么需要这个类型）:
 *   移动端首次打开远端终端时，需要先拉取最近输出，再订阅增量事件。
 *
 * Code Logic（字段说明）:
 *   buffer 是后端按 Unicode char 边界保留的最近输出；lastSeq 用于前端衔接后续 terminal-output 事件；
 *   ownerInstanceId 为 cutover 权威（可选，缺失时不得重置已绑定 authority）。
 */
export interface WorkbenchSessionReplay {
  sessionId: string;
  buffer: string;
  truncated: boolean;
  lastSeq: number;
  /**
   * Business Logic（为什么需要这个字段）:
   *   baseline/overflow replay 必须绑定 stream owner，迟到的旧 owner 快照不得 clobber 新 authority。
   *
   * Code Logic（这个字段做什么）:
   *   可选 ownerInstanceId；缺失时 applyCutover 不得覆盖已绑定 authority。
   */
  ownerInstanceId?: string;
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

/** HTML 预览资源内联数据。 */
export interface WorkbenchHtmlAsset {
  path: string;
  mime: string;
  size: number;
  dataUrl: string;
  text: string | null;
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
  /**
   * Business Logic（为什么需要这个字段）:
   *   owner 重启后 terminal reader seq 从 0 起算；前端 cutover 必须按 authority 分代。
   *
   * Code Logic（字段说明）:
   *   GUI relay 注入的 ownerInstanceId；缺省时保持既有 lastSeq 比较基线。
   */
  ownerInstanceId?: string;
}

/** 工作台终端状态事件 payload（listen('workbench:terminal-status')）。 */
export interface WorkbenchTerminalStatusEvent {
  sessionId: string;
  status: WorkbenchSessionStatus;
  exitCode: number | null;
  ts: number;
}

/**
 * 工作台 session 元数据更新事件（listen('workbench:session-updated')）。
 *
 * Business Logic（为什么需要这个类型）:
 *   agent 自动标题 / 用户 rename 后后端主动推送完整 session DTO，前端无需等下一次 list。
 *
 * Code Logic（字段说明）:
 *   对齐 WorkbenchSession（camelCase）；nameSource 可选以兼容旧后端。
 */
export type WorkbenchSessionUpdatedEvent = WorkbenchSession;

/**
 * Workbench HTTP Agent runtime 事件 payload。
 *
 * Business Logic（为什么需要这个类型）:
 *   Mobile 需要与桌面同一份 phase 投影（capability workbench.agent-runtime.v1）。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust WorkbenchAgentRuntimePayload：仅 agentSession DTO。
 */
export interface WorkbenchAgentRuntimeHttpPayload {
  agentSession: import('./agentRuntime').AgentSessionRuntimeDto;
}

/**
 * Workbench HTTP terminalResync 事件 payload（Gap resync 权威回放）。
 *
 * Business Logic（为什么需要这个类型）:
 *   bridge Gap resync 后 Mobile 必须收到权威 buffer/lastSeq/owner 才能 store.reset；
 *   仅桌面 Tauri `workbench:terminal-resync` 不够。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust WorkbenchTerminalResyncPayload / WorkbenchSessionReplayDto 字段子集。
 */
export interface WorkbenchTerminalResyncHttpPayload {
  sessionId: string;
  buffer: string;
  truncated: boolean;
  lastSeq: number;
  ownerInstanceId?: string;
}

/**
 * Workbench HTTP NDJSON 事件。
 *
 * Business Logic（为什么需要这个类型）:
 *   移动端普通浏览器无法使用 Tauri event，需要通过 `/api/workbench/events` 一条长连接接收多类 Workbench 事件。
 *
 * Code Logic（类型说明）:
 *   对齐 Rust `#[serde(tag="type", content="payload", rename_all="camelCase")]`；
 *   terminalOutput/status 复用桌面事件 payload，mergeProgress 复用已有阶段进度 payload，
 *   agentRuntime 为 A1 投影，terminalResync 为 Gap 权威 cutover（R37 H2）。
 */
export type WorkbenchHttpEvent =
  | { type: 'terminalOutput'; payload: WorkbenchTerminalOutputEvent }
  | { type: 'terminalStatus'; payload: WorkbenchTerminalStatusEvent }
  | { type: 'mergeProgress'; payload: WorkbenchMergeProgressEvent }
  | { type: 'agentRuntime'; payload: WorkbenchAgentRuntimeHttpPayload }
  | { type: 'terminalResync'; payload: WorkbenchTerminalResyncHttpPayload };

/**
 * Claude session 搜索命中结果（对齐后端 SessionSearchHitDto，camelCase）。
 * 由 search_claude_sessions 命令返回，供 WorkbenchSessionSearch Command Palette 渲染结果列表。
 */
export interface SessionSearchHit {
  /** Claude session id（jsonl 文件名 stem，resume 时用） */
  sessionId: string;
  /** 标题（lastPrompt，无则回退第一条 user 文本） */
  title: string;
  /** 标题字段是否命中搜索关键词 */
  titleHit: boolean;
  /** user 文本字段是否命中搜索关键词 */
  userHit: boolean;
  /** assistant 文本字段是否命中搜索关键词 */
  assistantHit: boolean;
  /** 首次活动时间（ISO） */
  firstActivityAt: string;
  /** 最近活动时间（ISO） */
  lastActivityAt: string;
  /** 消息总数 */
  messageCount: number;
  /** 命中上下文片段（命中位置前后各约 30 字符），最多 3 段 */
  previewSnippets: string[];
}

/**
 * Claude session 搜索索引诊断（对齐后端 SessionSearchDiagnosticsDto，camelCase）。
 * 说明扫描是否因预算截断、以及文件/字节消耗，供 UI 展示非阻塞提示。
 */
export interface SessionSearchDiagnostics {
  /**
   * 诊断状态：ok | truncated | unavailable（或其它前向兼容字符串）
   */
  status: 'ok' | 'truncated' | 'unavailable' | string;
  /**
   * 截断原因 token 列表，如 max_files / max_file_bytes / max_jsonl_line_bytes /
   * max_total_bytes / max_session_chars
   */
  reasons: string[];
  /** 扫描阶段考虑过的文件数 */
  filesConsidered: number;
  /** 实际进入索引的文件数 */
  filesIndexed: number;
  /** 累计读取字节数 */
  bytesRead: number;
}

/**
 * Claude session 有界搜索结果（对齐后端 SessionSearchResultDto，camelCase）。
 * 由 search_claude_sessions 返回：items 为命中列表，truncated/diagnostics 描述预算截断。
 */
export interface SessionSearchResult {
  /** 命中条目（最多 50 条） */
  items: SessionSearchHit[];
  /** 是否因预算截断（与 diagnostics.status 可能叠加） */
  truncated: boolean;
  /** 索引/扫描诊断 */
  diagnostics: SessionSearchDiagnostics;
}

/**
 * Claude session preview 单条消息（对齐后端 SessionPreviewMessageDto）。
 */
export interface SessionPreviewMessage {
  /** 角色：user 或 assistant */
  role: 'user' | 'assistant';
  /** 已过滤 thinking/tool_use 后的纯文本 */
  text: string;
  /** 消息时间（ISO） */
  timestamp: string;
}

/**
 * Claude session preview 完整数据（对齐后端 SessionPreviewDto，camelCase）。
 * 由 get_claude_session_preview 命令返回，供 preview 面板渲染最近对话。
 */
export interface SessionPreview {
  /** Claude session id */
  sessionId: string;
  /** 标题 */
  title: string;
  /** 记录时的工作目录，可能为 null */
  cwd: string | null;
  /** 记录时的 git 分支，可能为 null */
  gitBranch: string | null;
  /** 首次活动时间（ISO） */
  firstActivityAt: string;
  /** 最近活动时间（ISO） */
  lastActivityAt: string;
  /** 消息总数 */
  messageCount: number;
  /** 最近 20 条对话（user/assistant 交替） */
  recentMessages: SessionPreviewMessage[];
}

/**
 * resume Claude session 结果（对齐后端 ResumeClaudeSessionResultDto，camelCase）。
 * 由 resume_claude_session 命令返回，前端据此刷新 sessions 并切到新建的 window。
 */
export interface ResumeClaudeSessionResult {
  /** 是否成功创建并注入命令 */
  ok: boolean;
  /** 新建 terminal window 的 session id */
  sessionId: string;
}

/* ---------------------------------------------------------------------------
 * Workbench launch summary（继续工作启动摘要 wire DTO）
 * ------------------------------------------------------------------------- */

/** 启动摘要：最近项目条目。 */
export interface WorkbenchLaunchProject {
  id: string;
  name: string;
  kind: string;
  deviceId: string;
  deviceName: string;
  path: string;
  lastOpenedAt: string;
}

/** 启动摘要：活跃会话条目。 */
export interface WorkbenchLaunchSession {
  id: string;
  projectId: string;
  projectName: string;
  worktreeId?: string | null;
  name: string;
  status: string;
  startedAt: string;
}

/** 启动摘要：Orchestrator 任务条目。 */
export interface WorkbenchLaunchTask {
  id: string;
  projectId: string;
  projectName?: string | null;
  title: string;
  status: string;
  workflowState: string;
  runState: string;
  updatedAt: string;
}

/** 启动摘要：传输任务条目。 */
export interface WorkbenchLaunchTransfer {
  id: string;
  filename: string;
  status: string;
  direction: string;
  progress?: number | null;
  size?: number | null;
  updatedAt?: string | null;
  createdAt?: string | null;
}

/** 启动摘要：设备条目。 */
export interface WorkbenchLaunchDevice {
  id: string;
  name: string;
  online: boolean;
  lastSeen?: string | null;
  address?: string | null;
}

/**
 * 后端 launch section wire：ready 带 value 数组；error 仅 message。
 * 单 section 失败不拖垮其余 section。
 */
export type WorkbenchLaunchSectionWire<T> =
  | { kind: 'ready'; value: T[] }
  | { kind: 'error'; message: string };

/** 后端 `get_workbench_launch_summary` 完整 wire。 */
export interface WorkbenchLaunchSummaryWire {
  projects: WorkbenchLaunchSectionWire<WorkbenchLaunchProject>;
  sessions: WorkbenchLaunchSectionWire<WorkbenchLaunchSession>;
  tasks: WorkbenchLaunchSectionWire<WorkbenchLaunchTask>;
  transfers: WorkbenchLaunchSectionWire<WorkbenchLaunchTransfer>;
  devices: WorkbenchLaunchSectionWire<WorkbenchLaunchDevice>;
  generatedAt: string;
}
