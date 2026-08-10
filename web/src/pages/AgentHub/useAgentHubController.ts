/**
 * Agent Hub 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent Hub 持有 status/assets/选中详情/预览/冲突与块抽屉状态；
 *   按需加载：默认只跑当前 tab lane，不在 mount 全量 listAssets/portable inspect。
 *
 * Code Logic（这个 hook 做什么）:
 *   lane 激活布尔 + status/legacy 懒加载；scope/kind 过滤；stale sequence 防切换覆盖；
 *   暴露 preview/enable/resolve/update/pair/binding/presence/restore/everywhere 动作。
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  agentHubApi,
  type AgentHubPairInstructionVariantsArgs,
  type AgentHubResolveConflictArgs,
  type AgentHubSetTargetBindingArgs,
  type AgentHubSetTargetEnabledArgs,
  type AgentHubSetTargetPresenceArgs,
  type AgentHubUpdateInstructionArgs,
  type AgentHubUpdateInstructionBlockArgs,
} from '@/api/agentHub';
import { devicesApi } from '@/api/devices';
import { workbenchApi } from '@/api/workbench';
import type {
  AgentHubAdoptionPreview,
  AgentHubAssetDetail,
  AgentHubAssetSummary,
  AgentHubConfirmGitImportOutcome,
  AgentHubGitImportPreview,
  AgentHubGitLaneInspectReport,
  AgentHubLanPushPreview,
  AgentHubMultiTargetPushReport,
  AgentHubProjectPreview,
  AgentHubPushSelectionMode,
  AgentHubResolvedProjectMapping,
  AgentHubStatus,
  AgentTarget,
  DesiredPresence,
  PluginPackageReport,
} from '@/lib/types/agentHub';
import type { LanPushPeerOption } from './LanPushDialog';
import {
  useUserInstructionManager,
  type UseUserInstructionManagerResult,
} from './userInstructions/useUserInstructionManager';
import { portableAssetApi } from '@/api/portableInventory';
import type {
  ApplyPortableAssetActionRequest,
  PortableAssetActionKind,
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableInventoryItemDto,
  PreviewPortableAssetActionRequest,
} from '@/lib/types/portableInventory';
import {
  DEFAULT_PORTABLE_INVENTORY_FILTERS,
  usePortableInventoryController,
  usePortablePullController,
  type PortableInventoryFilters,
  type UsePortableInventoryControllerResult,
  type UsePortablePullControllerResult,
} from './portableAssets';
import {
  parseAgentHubContext,
  writeAgentHubContext,
  mapLegacySection,
  DEFAULT_AGENT_HUB_CONTEXT,
  isAssetKindTab,
  type AgentHubContext,
  type AgentHubTab,
  type AgentHubScope,
} from './context/agentHubContext';

export {
  parseAgentHubContext,
  writeAgentHubContext,
  mapLegacySection,
  DEFAULT_AGENT_HUB_CONTEXT,
  isAssetKindTab,
  type AgentHubContext,
  type AgentHubTab,
  type AgentHubScope,
};

/** Agent Hub 一级工作区。 */
export type AgentHubSection =
  | 'userInstructions'
  | 'projectInstructions'
  | 'assets'
  | 'syncImport'
  | 'diagnostics';

/**
 * Business Logic: 将壳层 context 映射到旧五段 section，双路径内容区暂不炸裂。
 * Code Logic: instructions×user/project → 对应指令区；资产 tab → assets。
 */
export function mapContextToSection(ctx: AgentHubContext): AgentHubSection {
  if (ctx.tab === 'instructions') {
    return ctx.scope === 'project' ? 'projectInstructions' : 'userInstructions';
  }
  return 'assets';
}

/**
 * Business Logic: 冷 URL / deep link 时把 tab 与 section 对齐，避免 ?tab=skill 空主体。
 * Code Logic: asset/conflict/preview 优先；section 次之；最后 mapContextToSection(hubContext)。
 */
export function resolveInitialSection(
  params: {
    assetId?: string | null;
    conflictId?: string | null;
    preview?: string | null;
    projectId?: string | null;
    bridge?: string | null;
    section?: string | null;
    inventoryItemId?: string | null;
  },
  hubContext: AgentHubContext,
): AgentHubSection {
  if (params.assetId || params.conflictId || params.inventoryItemId) return 'assets';
  if (params.section === 'assets' || params.section === 'portableAssets') return 'assets';
  // 旧 project/preview/diagnostics/syncImport 入口只做 URL 迁移，不再恢复 writer UI。
  return mapContextToSection(hubContext);
}

/**
 * Business Logic: portable 资产 tab 或需要库存的 deep link / Pull。
 * Code Logic: skill|command|mcp|plugin 或 inventory/asset deep link 或 pull 打开。
 */
export function computePortableLaneActive(
  hubContext: AgentHubContext,
  opts: {
    inventoryItemId?: string | null;
    assetId?: string | null;
    conflictId?: string | null;
    portablePullOpen?: boolean;
  },
): boolean {
  if (isAssetKindTab(hubContext.tab)) return true;
  if (opts.inventoryItemId || opts.assetId || opts.conflictId) return true;
  if (opts.portablePullOpen) return true;
  return false;
}

/**
 * Business Logic: 提示词 tab 才跑三栏 inspect；adapt 且无父级 markdown 时可自拉。
 * Code Logic: tab===instructions 或 adaptView。
 */
export function computeInstructionsLaneActive(
  hubContext: AgentHubContext,
  opts?: { hasAdaptMarkdown?: boolean },
): boolean {
  if (hubContext.tab === 'instructions') return true;
  if (hubContext.adaptView && !opts?.hasAdaptMarkdown) return true;
  return false;
}

/**
 * Business Logic: 旧 section 写回壳层 Partial context（工具入口/测试双路径）。
 * Code Logic: 仅覆盖 scope/tab；不碰 agent/device。
 */
export function mapSectionToContextPatch(section: AgentHubSection): Partial<AgentHubContext> {
  switch (section) {
    case 'userInstructions':
      return { scope: 'user', tab: 'instructions' };
    case 'projectInstructions':
      return { scope: 'project', tab: 'instructions' };
    case 'assets':
      return { tab: 'skill' };
    default:
      return {};
  }
}

/** URL 中 section 合法值（含 legacy portableAssets 别名）。 */
const SECTION_VALUES = new Set<string>([
  'userInstructions',
  'projectInstructions',
  'assets',
  'portableAssets',
  'syncImport',
  'diagnostics',
]);

const AGENT_TARGETS = new Set(['claude', 'codex', 'opencode']);
const ASSET_KINDS = new Set(['skill', 'command', 'plugin', 'mcp']);
const SCOPES = new Set(['user', 'project']);
const ACTUAL_STATES = new Set(['all', 'enabled', 'disabled', 'problem']);
const MANAGEMENT_STATES = new Set([
  'all',
  'unmanaged',
  'hubManaged',
  'drifted',
  'externalCollision',
  'unsupported',
]);

/**
 * Business Logic: URL section 归一；legacy portableAssets → assets。
 * Code Logic: 未知值回落 fallback。
 */
export function normalizeAgentHubSection(
  raw: string | null | undefined,
  fallback: AgentHubSection = 'userInstructions',
): AgentHubSection {
  if (!raw || !SECTION_VALUES.has(raw)) return fallback;
  if (raw === 'portableAssets') return 'assets';
  return raw as AgentHubSection;
}

/**
 * Business Logic: 从 search params 解析 portable inventory 筛选。
 * Code Logic: 非法枚举忽略，不发明默认以外的值。
 */
export function parsePortableFiltersFromSearchParams(
  params: URLSearchParams,
): Partial<PortableInventoryFilters> {
  const patch: Partial<PortableInventoryFilters> = {};
  const target = params.get('target');
  if (target === 'all' || (target && AGENT_TARGETS.has(target))) {
    patch.target = target as PortableInventoryFilters['target'];
  }
  const kind = params.get('kind');
  if (kind && ASSET_KINDS.has(kind)) {
    patch.kind = kind as PortableInventoryFilters['kind'];
  }
  // `scope` belongs to the shell context (user|project); portable's local
  // inventory filter must use its own key so filter changes never move the shell.
  const scope = params.get('inventoryScope');
  if (scope === 'all' || (scope && SCOPES.has(scope))) {
    patch.scope = scope as PortableInventoryFilters['scope'];
  }
  const state = params.get('state');
  if (state && ACTUAL_STATES.has(state)) {
    patch.actualState = state as PortableInventoryFilters['actualState'];
  }
  const management = params.get('management');
  if (management && MANAGEMENT_STATES.has(management)) {
    patch.management = management as PortableInventoryFilters['management'];
  }
  return patch;
}

/**
 * Business Logic: 把资产页次级筛选写回 URL；Agent/kind/scope 由 Shell 独占。
 * Code Logic: 只写 state/management/selection，并清除 legacy 导航与范围键。
 */
export function writePortableFiltersToSearchParams(
  params: URLSearchParams,
  filters: PortableInventoryFilters,
  inventoryItemId: string | null,
): URLSearchParams {
  const next = new URLSearchParams(params);
  if (next.get('section') === 'assets' || next.get('section') === 'portableAssets') {
    next.delete('section');
  }
  next.delete('target');
  next.delete('kind');
  next.delete('inventoryScope');
  if (filters.actualState === 'all') next.delete('state');
  else next.set('state', filters.actualState);
  if (filters.management === 'all') next.delete('management');
  else next.set('management', filters.management);
  if (inventoryItemId) next.set('inventoryItemId', inventoryItemId);
  else next.delete('inventoryItemId');
  return next;
}

/** portable 库存筛选 URL 键（离开 assets 时必须清掉，否则 parse 会把 kind 当 tab）。 */
const PORTABLE_FILTER_URL_KEYS = [
  'kind',
  'target',
  'state',
  'management',
  'inventoryItemId',
  'inventoryScope',
] as const;

/**
 * Business Logic: 离开 portable 资产区时清掉会干扰壳层 tab 的 filter query。
 * Code Logic: 删 kind/target/state/management/inventoryItemId；仅当 section=assets 时删 section
 *   （保留 diagnostics/syncImport 等非资产 section 深链）。
 */
export function clearPortableFilterSearchParams(params: URLSearchParams): URLSearchParams {
  const next = new URLSearchParams(params);
  for (const key of PORTABLE_FILTER_URL_KEYS) {
    next.delete(key);
  }
  if (next.get('section') === 'assets') {
    next.delete('section');
  }
  return next;
}

/**
 * Controller 返回值。
 *
 * Business Logic: 纯视图只消费本接口，禁止 import @/api/*。
 * Code Logic: 聚合 loading/error/filters/drawers 与 actions。
 */
export interface UseAgentHubControllerResult {
  t: TFunction<['agentHub', 'common']>;
  activeSection: AgentHubSection;
  setActiveSection: (section: AgentHubSection) => void;
  /** 新 IA 壳层上下文（URL 权威）。 */
  hubContext: AgentHubContext;
  /** 旧/不支持上下文被规范化后的单次可见说明。 */
  contextMigrationNotice: string | null;
  /** 壳层 patch → URL write + 双路径 activeSection 同步。 */
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  /** 壳层可选择的局域网设备。 */
  shellPeers: Array<{ deviceId: string; name: string; online: boolean }>;
  /** 壳层可选择的本机/远端 Workbench 项目。 */
  shellProjects: Array<{
    key: string;
    label: string;
    remote: boolean;
    deviceId: string | null;
  }>;
  userInstructions: UseUserInstructionManagerResult;
  /** F2 inventory controller（URL 同步后的包装）。 */
  portableInventory: UsePortableInventoryControllerResult;
  /** F3 详情/动作编排。 */
  portableDetailsOpen: boolean;
  portableSelectedItem: PortableInventoryItemDto | null;
  closePortableDetails: () => void;
  requestPortableAction: (itemId: string, action: PortableAssetActionKind) => void;
  portableActionOpen: boolean;
  portableActionKind: PortableAssetActionKind | null;
  portableActionPlan: PortableAssetActionPlanDto | null;
  portableActionResult: PortableAssetActionResultDto | null;
  portableActionBusy: boolean;
  portableActionError: string | null;
  portableActionClientRequestId: string | null;
  previewPortableAction: (request: PreviewPortableAssetActionRequest) => Promise<void>;
  confirmPortableAction: (planToken: string, clientRequestId: string) => Promise<void>;
  reconcilePortableAction: (clientRequestId: string) => Promise<void>;
  closePortableAction: () => void;
  /** F4 same-agent Pull。 */
  portablePullOpen: boolean;
  openPortablePull: () => void;
  closePortablePull: () => void;
  portablePull: UsePortablePullControllerResult;
  /** 当前 lane 是否忙碌（header refresh 绑定）；legacy 未加载时为 false。 */
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  status: AgentHubStatus | null;
  /** status lane 加载中（diagnostics）。 */
  statusLoading: boolean;
  /** legacy matrix 是否至少成功加载过一次。 */
  legacyLoadedOnce: boolean;
  /** 用户展开或 deep link 强制的 legacy 矩阵。 */
  legacyMatrixExpanded: boolean;
  expandLegacyMatrix: () => void;
  assets: AgentHubAssetSummary[];
  filteredAssets: AgentHubAssetSummary[];
  /** 三栏 / 页面入口用：是否激活 L-instructions。 */
  instructionsLaneActive: boolean;
  /** portable controller 用：是否激活 L-portable。 */
  portableLaneActive: boolean;
  /**
   * Business Logic: 页面把 three-pane.refresh 注入后，header reload 在 instructions 可刷新。
   * Code Logic: AgentHub 入口 setInstructionRefresh。
   */
  setInstructionRefresh: (fn: (() => Promise<void>) | null) => void;
  scopeFilter: string;
  kindFilter: string;
  setScopeFilter: (value: string) => void;
  setKindFilter: (value: string) => void;
  selectedAssetId: string | null;
  selectedAsset: AgentHubAssetDetail | null;
  selectAsset: (assetId: string | null) => void;
  preview: AgentHubProjectPreview | null;
  previewOpen: boolean;
  previewProjectId: string;
  setPreviewProjectId: (value: string) => void;
  openPreviewDialog: () => void;
  closePreviewDialog: () => void;
  runPreviewProject: () => Promise<void>;
  runEnableProject: () => Promise<void>;
  conflictDrawerOpen: boolean;
  openConflictDrawer: () => void;
  closeConflictDrawer: () => void;
  blocksDrawerOpen: boolean;
  openBlocksDrawer: () => void;
  closeBlocksDrawer: () => void;
  pluginDrawerOpen: boolean;
  pluginReport: import('@/lib/types/agentHub').PluginPackageReport | null;
  /** 当前插件 report 对应的 canonical/legacy asset id，避免跨资产复用旧矩阵。 */
  pluginReportAssetId?: string | null;
  openPluginDrawer: (assetId?: string) => void;
  closePluginDrawer: () => void;
  loadPluginReport: (assetId: string) => Promise<void>;
  adoptionOpen: boolean;
  adoptionPreview: AgentHubAdoptionPreview | null;
  openAdoptionPreview: (asset: AgentHubAssetSummary, target: AgentTarget) => void;
  closeAdoptionDialog: () => void;
  deleteEverywhereOpen: boolean;
  deleteEverywhereAssetId: string | null;
  openDeleteEverywhere: (assetId: string) => void;
  closeDeleteEverywhere: () => void;
  confirmDeleteEverywhere: () => Promise<void>;
  deepLinkConflictId: string | null;
  /** OpenCode bridge deep link 相对路径（仅展示，不写盘）。 */
  deepLinkBridgePath: string | null;
  reload: () => Promise<void>;
  resolveConflict: (args: Omit<AgentHubResolveConflictArgs, 'assetId'>) => Promise<void>;
  updateInstruction: (args: Omit<AgentHubUpdateInstructionArgs, 'assetId'>) => Promise<void>;
  updateInstructionBlock: (
    args: Omit<AgentHubUpdateInstructionBlockArgs, 'assetId'>,
  ) => Promise<void>;
  pairInstructionVariants: (
    args: Omit<AgentHubPairInstructionVariantsArgs, 'assetId'>,
  ) => Promise<void>;
  setTargetBinding: (
    args: Omit<AgentHubSetTargetBindingArgs, 'assetId'> & { assetId?: string },
  ) => Promise<void>;
  setTargetEnabled: (
    args: Omit<AgentHubSetTargetEnabledArgs, 'assetId'> & { assetId?: string },
  ) => Promise<void>;
  setTargetPresence: (
    args: Omit<AgentHubSetTargetPresenceArgs, 'assetId'> & { assetId?: string },
  ) => Promise<void>;
  restoreDetachedTarget: (args: { assetId?: string; target: AgentTarget }) => Promise<void>;
  removeTarget: (args: { assetId?: string; target: AgentTarget }) => Promise<void>;
  // Gate C replication surfaces
  lanPushOpen: boolean;
  openLanPushDialog: () => void;
  closeLanPushDialog: () => void;
  lanPeers: LanPushPeerOption[];
  lanSelectedPeerIds: string[];
  toggleLanPeer: (deviceId: string) => void;
  lanMode: AgentHubPushSelectionMode;
  setLanMode: (mode: AgentHubPushSelectionMode) => void;
  lanAssetIdsText: string;
  setLanAssetIdsText: (value: string) => void;
  lanHubProjectIdsText: string;
  setLanHubProjectIdsText: (value: string) => void;
  lanPreview: AgentHubLanPushPreview | null;
  lanReport: AgentHubMultiTargetPushReport | null;
  runLanPreview: () => Promise<void>;
  runLanStart: () => Promise<void>;
  gitImportOpen: boolean;
  openGitImportDrawer: () => void;
  closeGitImportDrawer: () => void;
  gitInspectReport: AgentHubGitLaneInspectReport | null;
  gitSelectedLaneDeviceId: string | null;
  selectGitLane: (laneDeviceId: string) => void;
  gitPreview: AgentHubGitImportPreview | null;
  gitSelectedAssetIds: string[];
  /** true 表示用户已显式编辑资产集合；空数组因此代表显式空集而非“全部”。 */
  gitAssetSelectionExplicit: boolean;
  toggleGitAsset: (assetId: string) => void;
  gitMappingDrafts: Record<string, string>;
  setGitMappingDraft: (hubProjectId: string, localProjectId: string) => void;
  gitConfirmOutcome: AgentHubConfirmGitImportOutcome | null;
  gitLastMapping: AgentHubResolvedProjectMapping | null;
  runGitInspect: () => Promise<void>;
  runGitPreview: () => Promise<void>;
  runGitConfirmMapping: (hubProjectId: string) => Promise<void>;
  runGitConfirmImport: () => Promise<void>;
  writeBlocked: boolean;
  upgradeRequired: boolean;
}

/**
 * Business Logic: 把 unknown reject 转成可读短消息。
 * Code Logic: Error.message 或 String。
 */
function toErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message) return reason.message;
  if (typeof reason === 'string') return reason;
  return String(reason);
}

/**
 * Business Logic: preview 只对生成它的精确请求有效。
 * Code Logic: Agent Hub request DTO 已固定字段顺序，JSON 作为本地指纹即可防输入漂移。
 */
function requestFingerprint(value: unknown): string {
  return JSON.stringify(value);
}

/**
 * Business Logic: remote Workbench shortcut 使用 `remote:<deviceId>:<hash>`，Pull/Push 需恢复 owner。
 * Code Logic: 只解析已规范化前缀；损坏/本机 project ref 返回 null。
 */
export function remoteProjectDeviceId(projectRef: string | null): string | null {
  const value = projectRef?.trim() ?? '';
  if (!value.startsWith('remote:')) return null;
  const [, deviceId] = value.split(':', 3);
  return deviceId?.trim() || null;
}

/**
 * Business Logic: 页面挂载即持有全部 Agent Hub 编排状态。
 * Code Logic: hooks 全在 early return 前；refreshSeq 丢弃过期响应。
 */
export function useAgentHubController(): UseAgentHubControllerResult {
  const { t } = useTranslation(['agentHub', 'common']);
  const [searchParams, setSearchParams] = useSearchParams();
  const deepLinkAssetId = searchParams.get('assetId');
  const deepLinkConflictId = searchParams.get('conflictId');
  const deepLinkPreview = searchParams.get('preview');
  const deepLinkProjectId = searchParams.get('projectId');
  const deepLinkBridge = searchParams.get('bridge');
  const deepLinkSection = searchParams.get('section');
  const deepLinkInventoryItemId = searchParams.get('inventoryItemId');
  /**
   * Business Logic: URL 权威的壳层上下文，须在子 controller 之前解析以便透传 device/project。
   * Code Logic: 每次 searchParams 变化 re-parse。
   */
  const hubContext = useMemo(
    () => parseAgentHubContext(searchParams),
    [searchParams],
  );
  /** portable / instruction inspect 用的 device|project 上下文。 */
  const inventoryRequestContext = useMemo(
    () =>
      hubContext.scope === 'project'
        ? { deviceId: null as string | null, projectRef: hubContext.projectKey }
        : { deviceId: hubContext.deviceId, projectRef: null as string | null },
    [hubContext.scope, hubContext.deviceId, hubContext.projectKey],
  );
  const userInstructions = useUserInstructionManager(t);
  const [portablePullOpen, setPortablePullOpen] = useState(false);
  /**
   * Business Logic: 仅资产 tab / deep link / Pull 打开时拉 portable inventory。
   * Code Logic: 见 computePortableLaneActive。
   */
  const portableLaneActive = useMemo(
    () =>
      computePortableLaneActive(hubContext, {
        inventoryItemId: deepLinkInventoryItemId,
        assetId: deepLinkAssetId,
        conflictId: deepLinkConflictId,
        portablePullOpen,
      }),
    [
      hubContext,
      deepLinkInventoryItemId,
      deepLinkAssetId,
      deepLinkConflictId,
      portablePullOpen,
    ],
  );
  /**
   * Business Logic: 仅提示词 tab（或 adapt 需自拉）时 inspect 指令。
   * Code Logic: 资产 tab 切换 agent 不得触发 instruction inspect。
   */
  const instructionsLaneActive = useMemo(
    () => computeInstructionsLaneActive(hubContext),
    [hubContext],
  );
  const portableInventoryBase = usePortableInventoryController({
    ...inventoryRequestContext,
    enabled:
      portableLaneActive &&
      hubContext.deviceId === null &&
      (hubContext.scope !== 'project' || hubContext.projectKey !== null),
    initialFilters: {
      target: hubContext.agent,
      kind: isAssetKindTab(hubContext.tab)
        ? (hubContext.tab as PortableInventoryFilters['kind'])
        : DEFAULT_PORTABLE_INVENTORY_FILTERS.kind,
      scope: hubContext.scope,
    },
  });
  const clearPortablePendingAction = portableInventoryBase.clearPendingAction;
  /**
   * Business Logic: 壳层工具栏 Pull 预填当前 peer（deviceId）与当前 Agent（same-agent）。
   * Code Logic: hubContext 变化在抽屉 open 时由 pull controller 应用。
   */
  const portablePull = usePortablePullController({
    open: portablePullOpen,
    initialSourceDeviceId:
      hubContext.deviceId ?? remoteProjectDeviceId(hubContext.projectKey),
    initialSourceTarget: hubContext.agent,
    sourceProjectRef:
      hubContext.scope === 'project' && hubContext.projectKey?.startsWith('remote:')
        ? hubContext.projectKey
        : null,
    destinationLocalProjectId:
      hubContext.scope === 'project' &&
      hubContext.projectKey &&
      !hubContext.projectKey.startsWith('remote:')
        ? hubContext.projectKey
        : null,
  });
  const [activeSection, setActiveSectionState] = useState<AgentHubSection>(() =>
    resolveInitialSection(
      {
        assetId: deepLinkAssetId,
        conflictId: deepLinkConflictId,
        preview: deepLinkPreview,
        projectId: deepLinkProjectId,
        bridge: deepLinkBridge,
        section: deepLinkSection,
        inventoryItemId: deepLinkInventoryItemId,
      },
      hubContext,
    ),
  );
  const [portableActionPlan, setPortableActionPlan] = useState<PortableAssetActionPlanDto | null>(
    null,
  );
  const [portableActionResult, setPortableActionResult] =
    useState<PortableAssetActionResultDto | null>(null);
  const [portableActionBusy, setPortableActionBusy] = useState(false);
  const [portableActionError, setPortableActionError] = useState<string | null>(null);
  const [portableActionClientRequestId, setPortableActionClientRequestId] = useState<string | null>(
    null,
  );
  /** 同步 busy 门闩：防止 re-render 前双击启动两次 preview/apply。 */
  const portableActionBusyRef = useRef(false);
  /** 任一 action session/context 变化都会使旧 preview/apply/reconcile 响应失效。 */
  const portableActionSeqRef = useRef(0);
  /** plan 只可在生成它的 item/action/shell context 内确认。 */
  const portableActionPlanContextRef = useRef<{
    planToken: string;
    clientRequestId: string;
    fingerprint: string;
  } | null>(null);
  const portableActionContextFingerprint = [
    hubContext.scope,
    hubContext.deviceId ?? '',
    hubContext.projectKey ?? '',
    hubContext.agent,
    hubContext.tab,
    portableInventoryBase.requestContext.deviceId ?? '',
    portableInventoryBase.requestContext.projectRef ?? '',
    portableInventoryBase.inventoryQuery.target ?? '',
    portableInventoryBase.inventoryQuery.kind ?? '',
    portableInventoryBase.inventoryQuery.scopeKind ?? '',
    portableInventoryBase.inventoryQuery.localProjectId ?? '',
    portableInventoryBase.pendingAction?.itemId ?? '',
    portableInventoryBase.pendingAction?.action ?? '',
  ].join('\0');
  /** layout commit 时同步最新值，使 history 切换后的旧 Promise 无法提交。 */
  const portableActionContextFingerprintRef = useRef(portableActionContextFingerprint);
  useLayoutEffect(() => {
    if (portableActionContextFingerprintRef.current === portableActionContextFingerprint) return;
    portableActionContextFingerprintRef.current = portableActionContextFingerprint;
    portableActionSeqRef.current += 1;
  }, [portableActionContextFingerprint]);
  /** 最近一次已 hydrate 的资产 URL 状态；history/back 可再次应用旧指纹。 */
  const portableUrlHydrationFingerprintRef = useRef<string | null>(null);
  /** URL→state 正在提交的目标；阻止同一 commit 中的旧 state 反向覆盖 history URL。 */
  const portableUrlHydrationTargetRef = useRef<{
    filters: Partial<PortableInventoryFilters>;
    selectedItemId: string | null;
    awaitingRequestReset: boolean;
  } | null>(null);
  const legacyAssetMigrationRef = useRef<string | null>(null);

  // 按需加载：legacy/status 初值 false；不再 mount 全量 loadCore
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [statusLoading, setStatusLoading] = useState(false);
  const [legacyLoadedOnce, setLegacyLoadedOnce] = useState(false);
  const [legacyMatrixExpanded, setLegacyMatrixExpanded] = useState(false);
  const [contextMigrationNotice, setContextMigrationNotice] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const instructionRefreshRef = useRef<(() => Promise<void>) | null>(null);
  const setInstructionRefresh = useCallback((fn: (() => Promise<void>) | null) => {
    instructionRefreshRef.current = fn;
  }, []);
  // Gate C LAN push / Git import UI state
  const [lanPushOpen, setLanPushOpen] = useState(false);
  const [lanPeers, setLanPeers] = useState<LanPushPeerOption[]>([]);
  const [lanSelectedPeerIds, setLanSelectedPeerIdsState] = useState<string[]>([]);
  const [lanMode, setLanModeState] = useState<AgentHubPushSelectionMode>('fullHub');
  const [lanAssetIdsText, setLanAssetIdsTextState] = useState('');
  const [lanHubProjectIdsText, setLanHubProjectIdsTextState] = useState('');
  const [lanPreview, setLanPreview] = useState<AgentHubLanPushPreview | null>(null);
  const [lanPreviewFingerprint, setLanPreviewFingerprint] = useState<string | null>(null);
  const [lanReport, setLanReport] = useState<AgentHubMultiTargetPushReport | null>(null);
  const [shellPeers, setShellPeers] = useState<
    Array<{ deviceId: string; name: string; online: boolean }>
  >([]);
  const [shellProjects, setShellProjects] = useState<
    Array<{ key: string; label: string; remote: boolean; deviceId: string | null }>
  >([]);
  const [gitImportOpen, setGitImportOpen] = useState(false);
  const [gitInspectReport, setGitInspectReport] = useState<AgentHubGitLaneInspectReport | null>(null);
  const [gitSelectedLaneDeviceId, setGitSelectedLaneDeviceId] = useState<string | null>(null);
  const [gitPreview, setGitPreview] = useState<AgentHubGitImportPreview | null>(null);
  const [gitSelectedAssetIds, setGitSelectedAssetIds] = useState<string[]>([]);
  const [gitAssetSelectionExplicit, setGitAssetSelectionExplicit] = useState(false);
  const [gitMappingDrafts, setGitMappingDrafts] = useState<Record<string, string>>({});
  const [gitConfirmOutcome, setGitConfirmOutcome] = useState<AgentHubConfirmGitImportOutcome | null>(null);
  const [gitLastMapping, setGitLastMapping] = useState<AgentHubResolvedProjectMapping | null>(null);
  const [status, setStatus] = useState<AgentHubStatus | null>(null);
  const [assets, setAssets] = useState<AgentHubAssetSummary[]>([]);
  const [scopeFilter, setScopeFilter] = useState('');
  const [kindFilter, setKindFilter] = useState('');
  // deep link 初值在 useState 中完成，避免 effect 同步 setState 级联渲染
  // legacy asset/project deep links are translation inputs only; never hydrate retired writers.
  const [selectedAssetId, setSelectedAssetId] = useState<string | null>(null);
  const [selectedAsset, setSelectedAsset] = useState<AgentHubAssetDetail | null>(null);
  const [preview, setPreview] = useState<AgentHubProjectPreview | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewProjectId, setPreviewProjectIdState] = useState(() =>
    hubContext.scope === 'project' && hubContext.projectKey
      ? hubContext.projectKey
      : '',
  );
  const [projectPreviewFingerprint, setProjectPreviewFingerprint] = useState<string | null>(null);
  const [conflictDrawerOpen, setConflictDrawerOpen] = useState(false);
  const [blocksDrawerOpen, setBlocksDrawerOpen] = useState(false);
  const [pluginDrawerOpen, setPluginDrawerOpen] = useState(false);
  const [pluginReport, setPluginReport] = useState<PluginPackageReport | null>(null);
  const [pluginReportAssetId, setPluginReportAssetId] = useState<string | null>(null);
  const [adoptionOpen, setAdoptionOpen] = useState(false);
  const [adoptionPreview, setAdoptionPreview] = useState<AgentHubAdoptionPreview | null>(null);
  const [deleteEverywhereOpen, setDeleteEverywhereOpen] = useState(false);
  const [deleteEverywhereAssetId, setDeleteEverywhereAssetId] = useState<string | null>(null);

  const refreshSeqRef = useRef(0);
  const detailSeqRef = useRef(0);
  const scopeCursorRef = useRef(0);
  const lanInputVersionRef = useRef(0);
  const lanPreviewInputVersionRef = useRef<number | null>(null);
  const projectInputVersionRef = useRef(0);
  const projectPreviewInputVersionRef = useRef<number | null>(null);
  const mountedRef = useRef(true);
  const filtersBootstrappedRef = useRef(false);
  const hubContextKeyRef = useRef(
    `${hubContext.scope}\0${hubContext.deviceId ?? ''}\0${hubContext.projectKey ?? ''}\0${hubContext.agent}\0${hubContext.tab}`,
  );
  const scopeFilterRef = useRef(scopeFilter);
  const kindFilterRef = useRef(kindFilter);
  useEffect(() => {
    scopeFilterRef.current = scopeFilter;
    kindFilterRef.current = kindFilter;
  }, [scopeFilter, kindFilter]);

  useEffect(() => {
    const nextKey =
      `${hubContext.scope}\0${hubContext.deviceId ?? ''}\0${hubContext.projectKey ?? ''}\0${hubContext.agent}\0${hubContext.tab}`;
    if (hubContextKeyRef.current === nextKey) return;
    hubContextKeyRef.current = nextKey;
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    projectInputVersionRef.current += 1;
    projectPreviewInputVersionRef.current = null;
    // Preview/apply actions are scoped to the previous shell context.
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    setActionBusy(false);
    setPreview(null);
    setProjectPreviewFingerprint(null);
    setGitPreview(null);
    setGitSelectedAssetIds([]);
    setGitAssetSelectionExplicit(false);
    portableActionSeqRef.current += 1;
    portableActionPlanContextRef.current = null;
    portableActionBusyRef.current = false;
    clearPortablePendingAction();
    setPortableActionPlan(null);
    setPortableActionResult(null);
    setPortableActionClientRequestId(null);
    setPortableActionError(null);
    setPortableActionBusy(false);
    const selectedProjectId =
      hubContext.scope === 'project' && hubContext.projectKey
        ? hubContext.projectKey
        : '';
    setPreviewProjectIdState(selectedProjectId);
  }, [clearPortablePendingAction, hubContext]);

  /**
   * Business Logic: 仅 diagnostics / 显式需要时拉 status。
   * Code Logic: 独立 statusLoading；不碰 assets。
   */
  const loadStatus = useCallback(async (_isRefresh = false) => {
    void _isRefresh;
    setStatusLoading(true);
    try {
      const nextStatus = await agentHubApi.getStatus();
      if (!mountedRef.current) return;
      setStatus(nextStatus);
    } catch (reason) {
      if (!mountedRef.current) return;
      setError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setStatusLoading(false);
    }
  }, []);

  /**
   * Business Logic: 懒加载 legacy canonical matrix（N+1 重，禁止默认 mount）。
   * Code Logic: refreshSeq + scopeCursor；成功后 legacyLoadedOnce。
   */
  const loadLegacyAssets = useCallback(async (isRefresh: boolean) => {
    const seq = ++refreshSeqRef.current;
    const scopeCursor = ++scopeCursorRef.current;
    const scopeAtRequest = scopeFilterRef.current;
    const kindAtRequest = kindFilterRef.current;
    if (isRefresh && legacyLoadedOnce) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    setError(null);
    try {
      const nextAssets = await agentHubApi.listAssets({
        scopeId: scopeAtRequest.trim() || null,
        kind: kindAtRequest.trim() || null,
      });
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      if (scopeCursor !== scopeCursorRef.current) return;
      if (
        scopeAtRequest !== scopeFilterRef.current ||
        kindAtRequest !== kindFilterRef.current
      ) {
        return;
      }
      setAssets(nextAssets);
      setLegacyLoadedOnce(true);
      setStale(false);
    } catch (reason) {
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      if (scopeCursor !== scopeCursorRef.current) return;
      setError(toErrorMessage(reason));
      setStale((prev) => prev || assets.length > 0);
    } finally {
      if (mountedRef.current && seq === refreshSeqRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [assets.length, legacyLoadedOnce]);

  /**
   * Business Logic: legacy mutation 后刷新 matrix；冷路径无 matrix 时也拉一次以便后续一致。
   * Code Logic: 始终 loadLegacyAssets；仅当已有 status 时刷新 L-status。
   */
  const invalidateLegacyLanes = useCallback(async () => {
    const tasks: Promise<void>[] = [loadLegacyAssets(true)];
    if (status !== null) {
      tasks.push(loadStatus(true));
    }
    await Promise.all(tasks);
  }, [loadLegacyAssets, loadStatus, status]);

  const expandLegacyMatrix = useCallback(() => {
    setLegacyMatrixExpanded(true);
    void loadLegacyAssets(legacyLoadedOnce);
  }, [legacyLoadedOnce, loadLegacyAssets]);

  /**
   * Business Logic: 选中资产后加载详情。
   * Code Logic: detailSeq 防旧响应覆盖。
   */
  const loadAssetDetail = useCallback(async (assetId: string) => {
    const seq = ++detailSeqRef.current;
    try {
      const detail = await agentHubApi.getAsset(assetId);
      if (!mountedRef.current || seq !== detailSeqRef.current) return;
      setSelectedAsset(detail);
    } catch (reason) {
      if (!mountedRef.current || seq !== detailSeqRef.current) return;
      setActionError(toErrorMessage(reason));
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    void devicesApi
      .list()
      .then((devices) => {
        if (!mountedRef.current) return;
        setShellPeers(
          devices.map((device) => ({
            deviceId: device.id,
            name: device.name,
            online: device.status === 'online',
          })),
        );
      })
      .catch(() => {
        // 设备发现为 best-effort；失败不阻断本机 Agent Hub。
      });
    void workbenchApi.projects
      .list()
      .then((projects) => {
        if (!mountedRef.current) return;
        setShellProjects(
          projects.map((project) => ({
            key: project.id,
            label:
              project.kind === 'remote' && project.deviceName
                ? `${project.name} · ${project.deviceName}`
                : project.name,
            remote: project.kind === 'remote',
            deviceId: project.kind === 'remote' ? project.deviceId : null,
          })),
        );
      })
      .catch(() => {
        // 项目列表为 best-effort；URL 中已选择的 identity 仍保留。
      });
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const legacySection = searchParams.get('section');
    const unsupportedOwner = searchParams.has('inventoryScope');
    const retiredView =
      legacySection === 'syncImport' ||
      legacySection === 'diagnostics' ||
      searchParams.has('preview') ||
      searchParams.has('projectId') ||
      searchParams.has('bridge');
    const legacyNavigation =
      legacySection !== null || searchParams.has('target') || searchParams.has('kind');

    const next = writeAgentHubContext(searchParams, hubContext);
    next.delete('inventoryScope');
    next.delete('preview');
    next.delete('projectId');
    next.delete('bridge');

    if (unsupportedOwner || retiredView) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- URL migration exposes one visible, non-blocking notice.
      setContextMigrationNotice(t('agentHub:shell.unsupportedContextMigrated'));
    } else if (legacyNavigation) {
      setContextMigrationNotice(t('agentHub:shell.legacyUrlMigrated'));
    }
    if (next.toString() !== searchParams.toString()) {
      setSearchParams(next, { replace: true });
    }
  }, [hubContext, searchParams, setSearchParams, t]);

  useEffect(() => {
    // 跳过 mount 首轮；仅 legacy 已加载时 filter 变化才 listAssets
    if (!filtersBootstrappedRef.current) {
      filtersBootstrappedRef.current = true;
      return;
    }
    if (!legacyLoadedOnce && !legacyMatrixExpanded) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- filter change external fetch
    void loadLegacyAssets(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeFilter, kindFilter]);

  // hubContext.tab → activeSection（仅 URL/context 变化时同步）
  // 注意：不得把 activeSection 放进 deps——onContextChange 会乐观 setActiveSection，
  // 若此时 searchParams 尚未刷成新 tab，effect 会用旧 hubContext 把 section 盖回 assets，
  // 随后 filters→URL 再写 kind/section，表现为 skill/command/mcp/plugin 后无法切回提示词。
  useEffect(() => {
    if (hubContext.adaptView) return;
    const mapped = mapContextToSection(hubContext);
    if (isAssetKindTab(hubContext.tab) || hubContext.tab === 'instructions') {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- tab→section dual-path
      setActiveSectionState(mapped);
    }
    // 仅跟随 hubContext / deepLinkSection；勿读 activeSection 以免乐观更新被 stale URL 回滚
  }, [hubContext]);

  // diagnostics section 懒加载 status
  useEffect(() => {
    if (activeSection !== 'diagnostics') return;
    if (status !== null || statusLoading) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- diagnostics lane
    void loadStatus(false);
  }, [activeSection, status, statusLoading, loadStatus]);

  const filteredAssets = useMemo(() => {
    const scope = scopeFilter.trim().toLowerCase();
    const kind = kindFilter.trim().toLowerCase();
    return assets.filter((asset) => {
      if (scope && !asset.scopeId.toLowerCase().includes(scope)) return false;
      if (kind && !asset.kind.toLowerCase().includes(kind)) return false;
      return true;
    });
  }, [assets, kindFilter, scopeFilter]);

  const selectAsset = useCallback(
    (assetId: string | null) => {
      setSelectedAssetId(assetId);
      setSelectedAsset(null);
      setActionError(null);
      if (assetId) {
        void loadAssetDetail(assetId);
      }
    },
    [loadAssetDetail],
  );

  const openPreviewDialog = useCallback(() => {
    setPreviewOpen(true);
    setActionError(null);
  }, []);

  const setPreviewProjectId = useCallback((value: string) => {
    projectInputVersionRef.current += 1;
    projectPreviewInputVersionRef.current = null;
    setPreviewProjectIdState(value);
    setPreview(null);
    setProjectPreviewFingerprint(null);
    setActionBusy(false);
  }, []);

  const closePreviewDialog = useCallback(() => {
    if (actionBusy) return;
    setPreviewOpen(false);
  }, [actionBusy]);

  const runPreviewProject = useCallback(async () => {
    const projectId = previewProjectId.trim();
    if (!projectId) {
      setActionError(t('agentHub:errors.projectIdRequired'));
      return;
    }
    const inputVersion = projectInputVersionRef.current;
    const fingerprint = requestFingerprint({ projectId });
    setActionBusy(true);
    setActionError(null);
    try {
      const next = await agentHubApi.previewProject(projectId);
      if (!mountedRef.current || inputVersion !== projectInputVersionRef.current) return;
      setPreview(next);
      setProjectPreviewFingerprint(fingerprint);
      projectPreviewInputVersionRef.current = inputVersion;
    } catch (reason) {
      if (!mountedRef.current || inputVersion !== projectInputVersionRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current && inputVersion === projectInputVersionRef.current) {
        setActionBusy(false);
      }
    }
  }, [previewProjectId, t]);

  const runEnableProject = useCallback(async () => {
    const projectId = previewProjectId.trim();
    if (!projectId) {
      setActionError(t('agentHub:errors.projectIdRequired'));
      return;
    }
    if (
      !preview ||
      projectPreviewFingerprint !== requestFingerprint({ projectId }) ||
      projectPreviewInputVersionRef.current !== projectInputVersionRef.current
    ) {
      setActionError(t('agentHub:errors.previewRequired'));
      return;
    }
    setActionBusy(true);
    setActionError(null);
    try {
      const enabled = await agentHubApi.enableProject(projectId);
      if (!mountedRef.current) return;
      setPreview((current) => ({
        ...current,
        ...enabled,
        projectId,
        optedIn: true,
      }));
      setPreviewOpen(false);
      await invalidateLegacyLanes();
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [invalidateLegacyLanes, preview, previewProjectId, projectPreviewFingerprint, t]);

  const openConflictDrawer = useCallback(() => setConflictDrawerOpen(true), []);
  const closeConflictDrawer = useCallback(() => {
    if (actionBusy) return;
    setConflictDrawerOpen(false);
  }, [actionBusy]);

  const openBlocksDrawer = useCallback(() => setBlocksDrawerOpen(true), []);
  const closeBlocksDrawer = useCallback(() => {
    if (actionBusy) return;
    setBlocksDrawerOpen(false);
  }, [actionBusy]);

  /**
   * Business Logic: 打开 Plugin 组件矩阵并加载 delete preview（tombstone/preserve）。
   * Code Logic: getPluginPackageReport（或 detail fallback）后始终调用 previewPluginDelete 合并 deletePreview。
   */
  const loadPluginReport = useCallback(
    async (assetId: string) => {
      setActionBusy(true);
      setActionError(null);
      try {
        let base: PluginPackageReport | null = null;
        if (selectedAsset?.assetId === assetId && selectedAsset.pluginReport) {
          base = selectedAsset.pluginReport;
        } else {
          try {
            base = await agentHubApi.getPluginPackageReport(assetId);
          } catch {
            if (selectedAsset?.assetId === assetId && selectedAsset.pluginReport) {
              base = selectedAsset.pluginReport;
            }
          }
        }

        // 生产路径必须调用 previewPluginDelete；不得只依赖 fixture 嵌入的 deletePreview。
        try {
          const deleteReport = await agentHubApi.previewPluginDelete(assetId);
          if (!mountedRef.current) return;
          if (!base) {
            base = deleteReport;
          } else {
            base = {
              ...base,
              deletePreview: deleteReport.deletePreview ?? base.deletePreview ?? null,
            };
          }
        } catch {
          // delete preview 失败时仍可展示 package matrix；drawer 在无 deletePreview 时隐藏 delete 区。
        }

        if (!mountedRef.current) return;
        if (!base) {
          throw new Error(t('agentHub:plugin.loadFailed'));
        }
        setPluginReport(base);
        setPluginReportAssetId(assetId);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
        setPluginReport(null);
        setPluginReportAssetId(null);
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [selectedAsset, t],
  );

  const openPluginDrawer = useCallback(
    (assetId?: string) => {
      const id = assetId ?? selectedAssetId;
      setPluginDrawerOpen(true);
      setPluginReport(null);
      setPluginReportAssetId(null);
      if (id) {
        void loadPluginReport(id);
      }
    },
    [loadPluginReport, selectedAssetId],
  );

  const closePluginDrawer = useCallback(() => {
    if (actionBusy) return;
    setPluginDrawerOpen(false);
  }, [actionBusy]);

  const openAdoptionPreview = useCallback(
    (asset: AgentHubAssetSummary, target: AgentTarget) => {
      const cell = asset.targets.find((item) => item.target === target) ?? null;
      const diagnostics: string[] = [];
      if (cell?.lastError) diagnostics.push(cell.lastError);
      if (cell?.materializationStatus) {
        diagnostics.push(`materialization:${cell.materializationStatus}`);
      }
      diagnostics.push(`aggregate:${asset.aggregateStatus}`);
      setAdoptionPreview({
        assetId: asset.assetId,
        displayName: asset.displayName,
        logicalKey: asset.logicalKey,
        originNamespace: asset.originNamespace,
        target,
        diagnostics,
        aggregateStatus: asset.aggregateStatus,
      });
      setAdoptionOpen(true);
      setActionError(null);
    },
    [],
  );

  const closeAdoptionDialog = useCallback(() => {
    if (actionBusy) return;
    setAdoptionOpen(false);
  }, [actionBusy]);

  const openDeleteEverywhere = useCallback((assetId: string) => {
    setDeleteEverywhereAssetId(assetId);
    setDeleteEverywhereOpen(true);
    setActionError(null);
  }, []);

  const closeDeleteEverywhere = useCallback(() => {
    if (actionBusy) return;
    setDeleteEverywhereOpen(false);
    setDeleteEverywhereAssetId(null);
  }, [actionBusy]);

  /**
   * Business Logic: header 刷新只打当前 lane，禁止无脑 listAssets。
   * Code Logic: diagnostics→status；instructions→three-pane；assets→portable（+ legacy 若已展开）。
   */
  const reload = useCallback(async () => {
    if (hubContext.adaptView) return;
    if (activeSection === 'diagnostics') {
      await loadStatus(true);
      return;
    }
    if (hubContext.tab === 'instructions') {
      const refreshInstructions = instructionRefreshRef.current;
      if (refreshInstructions) await refreshInstructions();
      return;
    }
    if (isAssetKindTab(hubContext.tab) || activeSection === 'assets') {
      await portableInventoryBase.refresh();
      if (legacyMatrixExpanded || legacyLoadedOnce || selectedAssetId) {
        await loadLegacyAssets(true);
        if (selectedAssetId) await loadAssetDetail(selectedAssetId);
      }
      return;
    }
    if (activeSection === 'syncImport') return;
  }, [
    activeSection,
    hubContext.adaptView,
    hubContext.tab,
    legacyLoadedOnce,
    legacyMatrixExpanded,
    loadAssetDetail,
    loadLegacyAssets,
    loadStatus,
    portableInventoryBase,
    selectedAssetId,
  ]);

  const requireSelectedAssetId = useCallback((): string | null => {
    if (!selectedAssetId) {
      setActionError(t('agentHub:errors.assetRequired'));
      return null;
    }
    return selectedAssetId;
  }, [selectedAssetId, t]);

  const resolveConflict = useCallback(
    async (args: Omit<AgentHubResolveConflictArgs, 'assetId'>) => {
      const assetId = requireSelectedAssetId();
      if (!assetId) return;
      setActionBusy(true);
      setActionError(null);
      try {
        const detail = await agentHubApi.resolveConflict({ ...args, assetId });
        if (!mountedRef.current) return;
        setSelectedAsset(detail);
        await invalidateLegacyLanes();
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [invalidateLegacyLanes, requireSelectedAssetId],
  );

  /**
   * Business Logic: UI mutation 必须带详情页 head CAS，避免并发静默丢写。
   * Code Logic: 优先用 selectedAsset.currentRevisionId；缺失则 undefined 由后端 fail-closed。
   */
  const expectedRevisionFromSelection = useCallback((): string | null => {
    const rev = selectedAsset?.currentRevisionId?.trim();
    return rev && rev.length > 0 ? rev : null;
  }, [selectedAsset]);

  const updateInstruction = useCallback(
    async (args: Omit<AgentHubUpdateInstructionArgs, 'assetId'>) => {
      const assetId = requireSelectedAssetId();
      if (!assetId) return;
      setActionBusy(true);
      setActionError(null);
      try {
        const detail = await agentHubApi.updateInstruction({
          ...args,
          assetId,
          expectedRevisionId: args.expectedRevisionId ?? expectedRevisionFromSelection(),
        });
        if (!mountedRef.current) return;
        setSelectedAsset(detail);
        await invalidateLegacyLanes();
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, invalidateLegacyLanes, requireSelectedAssetId],
  );

  const updateInstructionBlock = useCallback(
    async (args: Omit<AgentHubUpdateInstructionBlockArgs, 'assetId'>) => {
      const assetId = requireSelectedAssetId();
      if (!assetId) return;
      setActionBusy(true);
      setActionError(null);
      try {
        await agentHubApi.updateInstructionBlock({
          ...args,
          assetId,
          expectedRevisionId: args.expectedRevisionId ?? expectedRevisionFromSelection(),
        });
        if (!mountedRef.current) return;
        await loadAssetDetail(assetId);
        await invalidateLegacyLanes();
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, loadAssetDetail, invalidateLegacyLanes, requireSelectedAssetId],
  );

  const pairInstructionVariants = useCallback(
    async (args: Omit<AgentHubPairInstructionVariantsArgs, 'assetId'>) => {
      const assetId = requireSelectedAssetId();
      if (!assetId) return;
      setActionBusy(true);
      setActionError(null);
      try {
        const detail = await agentHubApi.pairInstructionVariants({
          ...args,
          assetId,
          expectedRevisionId: args.expectedRevisionId ?? expectedRevisionFromSelection(),
        });
        if (!mountedRef.current) return;
        setSelectedAsset(detail);
        await invalidateLegacyLanes();
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, invalidateLegacyLanes, requireSelectedAssetId],
  );

  const applySummaryMutation = useCallback(
    async (assetId: string, mutate: () => Promise<AgentHubAssetSummary>) => {
      setActionBusy(true);
      setActionError(null);
      const cursorBefore = scopeCursorRef.current;
      try {
        await mutate();
        if (!mountedRef.current) return;
        if (cursorBefore !== scopeCursorRef.current) return;
        await invalidateLegacyLanes();
        if (selectedAssetId === assetId) {
          await loadAssetDetail(assetId);
        }
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [loadAssetDetail, invalidateLegacyLanes, selectedAssetId],
  );

  const setTargetBinding = useCallback(
    async (args: Omit<AgentHubSetTargetBindingArgs, 'assetId'> & { assetId?: string }) => {
      const assetId = args.assetId ?? requireSelectedAssetId();
      if (!assetId) return;
      await applySummaryMutation(assetId, () =>
        agentHubApi.setTargetBinding({
          assetId,
          target: args.target,
          desiredPresence: args.desiredPresence,
          desiredEnabled: args.desiredEnabled,
        }),
      );
    },
    [applySummaryMutation, requireSelectedAssetId],
  );

  const setTargetEnabled = useCallback(
    async (args: Omit<AgentHubSetTargetEnabledArgs, 'assetId'> & { assetId?: string }) => {
      const assetId = args.assetId ?? requireSelectedAssetId();
      if (!assetId) return;
      await applySummaryMutation(assetId, () =>
        agentHubApi.setTargetEnabled({
          assetId,
          target: args.target,
          desiredEnabled: args.desiredEnabled,
        }),
      );
    },
    [applySummaryMutation, requireSelectedAssetId],
  );

  const setTargetPresence = useCallback(
    async (args: Omit<AgentHubSetTargetPresenceArgs, 'assetId'> & { assetId?: string }) => {
      const assetId = args.assetId ?? requireSelectedAssetId();
      if (!assetId) return;
      await applySummaryMutation(assetId, () =>
        agentHubApi.setTargetPresence({
          assetId,
          target: args.target,
          desiredPresence: args.desiredPresence,
        }),
      );
    },
    [applySummaryMutation, requireSelectedAssetId],
  );

  const restoreDetachedTarget = useCallback(
    async (args: { assetId?: string; target: AgentTarget }) => {
      const assetId = args.assetId ?? requireSelectedAssetId();
      if (!assetId) return;
      await applySummaryMutation(assetId, () =>
        agentHubApi.restoreDetachedTarget({ assetId, target: args.target }),
      );
    },
    [applySummaryMutation, requireSelectedAssetId],
  );

  const removeTarget = useCallback(
    async (args: { assetId?: string; target: AgentTarget }) => {
      const assetId = args.assetId ?? requireSelectedAssetId();
      if (!assetId) return;
      await applySummaryMutation(assetId, () =>
        agentHubApi.setTargetPresence({
          assetId,
          target: args.target,
          desiredPresence: 'absent',
        }),
      );
    },
    [applySummaryMutation, requireSelectedAssetId],
  );

  const confirmDeleteEverywhere = useCallback(async () => {
    const assetId = deleteEverywhereAssetId ?? requireSelectedAssetId();
    if (!assetId) return;
    setActionBusy(true);
    setActionError(null);
    const cursorBefore = scopeCursorRef.current;
    try {
      await agentHubApi.deleteAssetEverywhere({ assetId });
      if (!mountedRef.current) return;
      if (cursorBefore !== scopeCursorRef.current) return;
      setDeleteEverywhereOpen(false);
      setDeleteEverywhereAssetId(null);
      if (selectedAssetId === assetId) {
        setSelectedAssetId(null);
        setSelectedAsset(null);
      }
      await invalidateLegacyLanes();
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [deleteEverywhereAssetId, invalidateLegacyLanes, requireSelectedAssetId, selectedAssetId]);


  /**
   * Business Logic: 壳层工具栏 Push 打开 LAN 对话框，按当前 scope 预填 mode/project。
   * Code Logic: project scope → mode=project + hub project id；user → userScope。
   */
  const openLanPushDialog = useCallback(() => {
    // A reopened dialog is a new request context; invalidate an earlier preview.
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    setLanPushOpen(true);
    setActionError(null);
    setActionBusy(false);
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    const selectedPeerId =
      hubContext.deviceId ?? remoteProjectDeviceId(hubContext.projectKey);
    setLanSelectedPeerIdsState(selectedPeerId ? [selectedPeerId] : []);
    if (hubContext.scope === 'project') {
      setLanModeState('project');
      const enabledHubProjectId =
        previewProjectId === hubContext.projectKey && preview?.optedIn === true
          ? preview.hubProjectId
          : null;
      setLanHubProjectIdsTextState(enabledHubProjectId ?? '');
      setLanAssetIdsTextState('');
    } else {
      setLanModeState('userScope');
      setLanHubProjectIdsTextState('');
      setLanAssetIdsTextState('');
    }
    void devicesApi.list().then((list) => {
      if (!mountedRef.current) return;
      setLanPeers(
        list
          .filter((d) => d.status === 'online')
          .map((d) => ({ deviceId: d.id, name: d.name })),
      );
    }).catch((reason) => {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    });
  }, [
    hubContext.scope,
    hubContext.projectKey,
    hubContext.deviceId,
    preview,
    previewProjectId,
  ]);

  const closeLanPushDialog = useCallback(() => {
    if (actionBusy) return;
    setLanPushOpen(false);
  }, [actionBusy]);

  const toggleLanPeer = useCallback((deviceId: string) => {
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    setLanSelectedPeerIdsState((prev) =>
      prev.includes(deviceId) ? prev.filter((id) => id !== deviceId) : [...prev, deviceId],
    );
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    setActionBusy(false);
  }, []);

  const setLanMode = useCallback((mode: AgentHubPushSelectionMode) => {
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    setLanModeState(mode);
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    setActionBusy(false);
  }, []);

  const setLanAssetIdsText = useCallback((value: string) => {
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    setLanAssetIdsTextState(value);
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    setActionBusy(false);
  }, []);

  const setLanHubProjectIdsText = useCallback((value: string) => {
    lanInputVersionRef.current += 1;
    lanPreviewInputVersionRef.current = null;
    setLanHubProjectIdsTextState(value);
    setLanPreview(null);
    setLanPreviewFingerprint(null);
    setLanReport(null);
    setActionBusy(false);
  }, []);

  const buildLanRequest = useCallback(() => {
    const assetIds = lanAssetIdsText
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    const hubProjectIds = lanHubProjectIdsText
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean);
    return {
      peerDeviceIds: lanSelectedPeerIds,
      mode: lanMode,
      scopeIds: [],
      assetIds,
      hubProjectIds,
      includeHistory: true,
    };
  }, [lanAssetIdsText, lanHubProjectIdsText, lanMode, lanSelectedPeerIds]);

  const runLanPreview = useCallback(async () => {
    const request = buildLanRequest();
    const fingerprint = requestFingerprint(request);
    const inputVersion = lanInputVersionRef.current;
    setActionBusy(true);
    setActionError(null);
    try {
      const previewResult = await agentHubApi.previewLanPush(request);
      if (!mountedRef.current || inputVersion !== lanInputVersionRef.current) return;
      setLanPreview(previewResult);
      setLanPreviewFingerprint(fingerprint);
      lanPreviewInputVersionRef.current = inputVersion;
    } catch (reason) {
      if (!mountedRef.current || inputVersion !== lanInputVersionRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current && inputVersion === lanInputVersionRef.current) {
        setActionBusy(false);
      }
    }
  }, [buildLanRequest]);

  const runLanStart = useCallback(async () => {
    const request = buildLanRequest();
    const fingerprint = requestFingerprint(request);
    if (
      !lanPreview ||
      lanPreviewFingerprint !== fingerprint ||
      lanPreviewInputVersionRef.current !== lanInputVersionRef.current
    ) {
      setActionError(t('agentHub:errors.previewRequired'));
      return;
    }
    setActionBusy(true);
    setActionError(null);
    try {
      const report = await agentHubApi.startLanPush({
        ...request,
        previewToken: lanPreview.previewToken,
      });
      if (!mountedRef.current) return;
      setLanReport(report);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [buildLanRequest, lanPreview, lanPreviewFingerprint, t]);

  const openGitImportDrawer = useCallback(() => {
    setGitImportOpen(true);
    setActionError(null);
    setGitInspectReport(null);
    setGitSelectedLaneDeviceId(null);
    setGitPreview(null);
    setGitSelectedAssetIds([]);
    setGitAssetSelectionExplicit(false);
    setGitMappingDrafts({});
    setGitConfirmOutcome(null);
    setGitLastMapping(null);
  }, []);

  const closeGitImportDrawer = useCallback(() => {
    if (actionBusy) return;
    setGitImportOpen(false);
  }, [actionBusy]);

  const selectGitLane = useCallback((laneDeviceId: string) => {
    setGitSelectedLaneDeviceId(laneDeviceId);
    setGitPreview(null);
    setGitSelectedAssetIds([]);
    setGitAssetSelectionExplicit(false);
    setGitConfirmOutcome(null);
  }, []);

  const runGitInspect = useCallback(async () => {
    setActionBusy(true);
    setActionError(null);
    try {
      const report = await agentHubApi.inspectGitLanes();
      if (!mountedRef.current) return;
      setGitInspectReport(report);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, []);

  const runGitPreview = useCallback(async () => {
    if (!gitSelectedLaneDeviceId) return;
    setActionBusy(true);
    setActionError(null);
    try {
      const previewResult = await agentHubApi.previewGitImport(gitSelectedLaneDeviceId);
      if (!mountedRef.current) return;
      setGitPreview(previewResult);
      setGitSelectedAssetIds([]);
      setGitAssetSelectionExplicit(false);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [gitSelectedLaneDeviceId]);

  const toggleGitAsset = useCallback((assetId: string) => {
    setGitSelectedAssetIds((prev) => {
      // First toggle materializes the implicit all-set; subsequent toggles can
      // intentionally reach an empty explicit set (which disables confirm).
      if (!gitAssetSelectionExplicit && gitPreview) {
        setGitAssetSelectionExplicit(true);
        const all = gitPreview.assets.map((a) => a.assetId);
        return all.filter((id) => id !== assetId);
      }
      return prev.includes(assetId) ? prev.filter((id) => id !== assetId) : [...prev, assetId];
    });
  }, [gitAssetSelectionExplicit, gitPreview]);

  const setGitMappingDraft = useCallback((hubProjectId: string, localProjectId: string) => {
    setGitMappingDrafts((prev) => ({ ...prev, [hubProjectId]: localProjectId }));
  }, []);

  const runGitConfirmMapping = useCallback(async (hubProjectId: string) => {
    const local = (gitMappingDrafts[hubProjectId] ?? '').trim();
    if (!local) return;
    setActionBusy(true);
    setActionError(null);
    try {
      const mapping = await agentHubApi.confirmProjectMapping({
        hubProjectId,
        localWorkbenchProjectId: local,
        optedIn: false,
      });
      if (!mountedRef.current) return;
      setGitLastMapping(mapping);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [gitMappingDrafts]);

  const runGitConfirmImport = useCallback(async () => {
    if (!gitPreview) return;
    if (gitAssetSelectionExplicit && gitSelectedAssetIds.length === 0) {
      setActionError(t('agentHub:errors.selectionRequired'));
      return;
    }
    setActionBusy(true);
    setActionError(null);
    try {
      const projectMappings = Object.entries(gitMappingDrafts)
        .filter(([, v]) => v.trim())
        .map(([hubProjectId, localWorkbenchProjectId]) => ({
          hubProjectId,
          localWorkbenchProjectId: localWorkbenchProjectId.trim(),
          optedIn: false,
        }));
      const outcome = await agentHubApi.confirmGitImport({
        laneDeviceId: gitPreview.laneDeviceId,
        snapshotHash: gitPreview.snapshotHash,
        selectedAssetIds: gitAssetSelectionExplicit ? gitSelectedAssetIds : undefined,
        projectMappings,
        importUnmappedProjects: true,
      });
      if (!mountedRef.current) return;
      setGitConfirmOutcome(outcome);
      await invalidateLegacyLanes();
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [
    gitAssetSelectionExplicit,
    gitMappingDrafts,
    gitPreview,
    gitSelectedAssetIds,
    invalidateLegacyLanes,
    t,
  ]);

  /**
   * Business Logic: 壳层 patch 写回 URL，并同步旧 activeSection 双路径内容。
   * Code Logic: merge + scope 互斥 → writeAgentHubContext；离开资产 tab 时清 portable filter keys，
   *   防止 filters→URL 残留 kind/section 经 parse 把 tab 拉回 skill/command；资产 tab 粗同步 filters。
   */
  const onContextChange = useCallback(
    (patch: Partial<AgentHubContext>) => {
      setSearchParams((prev) => {
        const current = parseAgentHubContext(prev);
        const next: AgentHubContext = { ...current, ...patch };
        if (next.scope === 'user') {
          next.projectKey = null;
        } else {
          next.deviceId = null;
        }
        // lane 仅 instructions 有意义
        if (next.tab !== 'instructions') {
          next.instructionLane = 'common';
        }
        let written = writeAgentHubContext(prev, next);
        // 离开 portable 资产 tab 时必须清掉 kind/section=assets 等，否则 re-parse 会盖回资产 tab
        if (!isAssetKindTab(next.tab)) {
          written = clearPortableFilterSearchParams(written);
        }
        return written;
      }, { replace: true });

      const merged: AgentHubContext = {
        ...parseAgentHubContext(searchParams),
        ...patch,
      };
      if (merged.scope === 'user') {
        merged.projectKey = null;
      } else {
        merged.deviceId = null;
      }
      if (merged.tab !== 'instructions') {
        merged.instructionLane = 'common';
      }

      // dual path: keep legacy section content in sync with shell
      if (merged.adaptView) {
        // adapt 全页后续任务；暂不改 section
      } else {
        setActiveSectionState(mapContextToSection(merged));
      }

      // 粗映射：资产 tab → portable kind/target 筛选
      if (isAssetKindTab(merged.tab)) {
      portableInventoryBase.setFilters({
        kind: merged.tab as PortableInventoryFilters['kind'],
        target: merged.agent,
        scope: merged.scope,
      });
      }
    },
    [portableInventoryBase, searchParams, setSearchParams],
  );

  /**
   * Business Logic: 兼容调用方只能落到现代 Instructions 或 Assets；退役 section 不复活旧 writer。
   * Code Logic: 直接写 agent/tab context，清 legacy/deep-link mutation keys，全程 replace。
   */
  const setActiveSection = useCallback(
    (section: AgentHubSection) => {
      const nextSection: AgentHubSection = section === 'assets' ? 'assets' : 'userInstructions';
      setActiveSectionState(nextSection);
      if (section !== 'assets' && section !== 'userInstructions') {
        setContextMigrationNotice(t('agentHub:shell.unsupportedContextMigrated'));
      }
      setSearchParams((prev) => {
        const current = parseAgentHubContext(prev);
        const ctx: AgentHubContext = {
          ...current,
          scope: 'user',
          deviceId: null,
          projectKey: null,
          tab:
            nextSection === 'assets'
              ? isAssetKindTab(current.tab)
                ? current.tab
                : 'skill'
              : 'instructions',
          instructionLane:
            nextSection === 'assets' ? 'common' : current.instructionLane,
          adaptView: false,
        };
        let next = writeAgentHubContext(prev, ctx);
        next.delete('assetId');
        next.delete('conflictId');
        next.delete('preview');
        next.delete('projectId');
        next.delete('bridge');
        if (nextSection !== 'assets') {
          next = clearPortableFilterSearchParams(next);
        }
        return next;
      }, { replace: true });
    },
    [setSearchParams, t],
  );

  const portableInventory: UsePortableInventoryControllerResult = portableInventoryBase;

  // URL → filters/selection。指纹既允许 history/back 重放，也吸收自身 replace 回声。
  useEffect(() => {
    const portableUrlActive =
      deepLinkSection === 'assets' ||
      deepLinkSection === 'portableAssets' ||
      Boolean(deepLinkInventoryItemId || deepLinkAssetId || deepLinkConflictId) ||
      isAssetKindTab(hubContext.tab);
    if (!portableUrlActive) return;

    const parsed = parsePortableFiltersFromSearchParams(searchParams);
    const fingerprint = JSON.stringify({
      agent: hubContext.agent,
      tab: hubContext.tab,
      section: deepLinkSection,
      actualState: parsed.actualState ?? 'all',
      management: parsed.management ?? 'all',
      inventoryItemId: deepLinkInventoryItemId,
      assetId: deepLinkAssetId,
      conflictId: deepLinkConflictId,
    });
    const sameFingerprint = portableUrlHydrationFingerprintRef.current === fingerprint;
    if (sameFingerprint && portableUrlHydrationTargetRef.current === null) return;
    portableUrlHydrationFingerprintRef.current = fingerprint;

    const desired: Partial<PortableInventoryFilters> = {
      target: hubContext.agent,
      kind: isAssetKindTab(hubContext.tab)
        ? (hubContext.tab as PortableInventoryFilters['kind'])
        : DEFAULT_PORTABLE_INVENTORY_FILTERS.kind,
      scope: hubContext.scope,
      actualState: parsed.actualState ?? 'all',
      management: parsed.management ?? 'all',
    };
    const current = portableInventoryBase.filters;
    const filtersNeedUpdate =
      current.target !== desired.target ||
      current.kind !== desired.kind ||
      current.scope !== desired.scope ||
      current.actualState !== desired.actualState ||
      current.management !== desired.management;
    if (!sameFingerprint) {
      portableUrlHydrationTargetRef.current = {
        filters: desired,
        selectedItemId: deepLinkInventoryItemId,
        // requestKey 变化会在 portable controller effect 中清 selection；先等该
        // reset 发生，再恢复 URL selection，避免它被同一轮请求启动覆盖。
        awaitingRequestReset: filtersNeedUpdate,
      };
    }
    if (filtersNeedUpdate) {
      portableInventoryBase.setFilters(desired);
    }
    const hydrationTarget = portableUrlHydrationTargetRef.current;
    if (hydrationTarget?.awaitingRequestReset && !filtersNeedUpdate) {
      hydrationTarget.awaitingRequestReset = false;
    }
    if (
      !hydrationTarget?.awaitingRequestReset &&
      portableInventoryBase.selectedItemId !== deepLinkInventoryItemId
    ) {
      portableInventoryBase.selectItem(deepLinkInventoryItemId);
    }
    if (
      !filtersNeedUpdate &&
      hydrationTarget &&
      !hydrationTarget.awaitingRequestReset &&
      portableInventoryBase.selectedItemId === deepLinkInventoryItemId
    ) {
      // URL 目标已完整落入 state 后立刻释放 echo guard；多留一个 render 会把
      // 列表首次可见后的用户选择误当作旧 state，再清回 URL 的 null selection。
      portableUrlHydrationTargetRef.current = null;
    }
    setActiveSectionState('assets');
  }, [
    deepLinkAssetId,
    deepLinkConflictId,
    deepLinkInventoryItemId,
    deepLinkSection,
    hubContext.agent,
    hubContext.scope,
    hubContext.tab,
    portableInventoryBase,
    searchParams,
  ]);

  // legacy asset/conflict 只翻译到 portable inventory；绝不调用旧 list/get/writer。
  useEffect(() => {
    if (!deepLinkAssetId && !deepLinkConflictId) return;
    if (portableInventoryBase.loading || portableInventoryBase.refreshing) return;
    // 首次扫描失败时保留 legacy identity，让 Retry 仍有机会在成功库存中完成翻译。
    // 只有拿到一次可信 snapshot 后，才能把“未匹配”判定为真正 unavailable。
    if (!portableInventoryBase.snapshot) return;
    const migrationKey = `${deepLinkAssetId ?? ''}\0${deepLinkConflictId ?? ''}`;
    if (legacyAssetMigrationRef.current === migrationKey) return;
    legacyAssetMigrationRef.current = migrationKey;

    const matched = deepLinkAssetId
      ? portableInventoryBase.snapshot?.items.find(
          (item) =>
            item.inventoryItemId === deepLinkAssetId ||
            item.canonicalAssetId === deepLinkAssetId ||
            item.nativeId === deepLinkAssetId,
        ) ?? null
      : null;
    setSearchParams((prev) => {
      const nextContext: AgentHubContext = matched
        ? {
            ...parseAgentHubContext(prev),
            agent: matched.target,
            tab: matched.kind,
            scope: 'user',
            deviceId: null,
            projectKey: null,
            instructionLane: 'common',
            adaptView: false,
          }
        : {
            ...parseAgentHubContext(prev),
            tab: isAssetKindTab(parseAgentHubContext(prev).tab)
              ? parseAgentHubContext(prev).tab
              : 'skill',
            scope: 'user',
            deviceId: null,
            projectKey: null,
            instructionLane: 'common',
            adaptView: false,
          };
      const next = writeAgentHubContext(prev, nextContext);
      next.delete('assetId');
      next.delete('conflictId');
      if (matched) next.set('inventoryItemId', matched.inventoryItemId);
      else next.delete('inventoryItemId');
      return next;
    }, { replace: true });
    setContextMigrationNotice(
      t(
        matched
          ? 'agentHub:shell.legacyAssetMigrated'
          : 'agentHub:shell.legacyAssetUnavailable',
      ),
    );
  }, [
    deepLinkAssetId,
    deepLinkConflictId,
    portableInventoryBase.error,
    portableInventoryBase.loading,
    portableInventoryBase.refreshing,
    portableInventoryBase.snapshot,
    setSearchParams,
    t,
  ]);

  // filters/selection → URL while on portable asset tabs only
  useEffect(() => {
    // 双路径：section 与壳层 tab 都必须在资产区，避免切回提示词后 stale filters 回写 kind/section
    if (activeSection !== 'assets' || !isAssetKindTab(hubContext.tab)) return;
    if (portableUrlHydrationTargetRef.current) return;
    setSearchParams((prev) => {
      // 若现代 tab 已离开资产（竞态帧），禁止写回 legacy 导航键
      if (!isAssetKindTab(parseAgentHubContext(prev).tab)) return prev;
      const modernContext = writeAgentHubContext(prev, hubContext);
      const desired = writePortableFiltersToSearchParams(
        modernContext,
        portableInventoryBase.filters,
        portableInventoryBase.selectedItemId,
      );
      // 避免无变化 set 触发环
      if (desired.toString() === prev.toString()) return prev;
      return desired;
    }, { replace: true });
  }, [
    activeSection,
    hubContext,
    portableInventoryBase.filters,
    portableInventoryBase.selectedItemId,
    setSearchParams,
  ]);

  const portableSelectedItem = useMemo(() => {
    const id = portableInventoryBase.selectedItemId;
    if (!id || !portableInventoryBase.snapshot) return null;
    return (
      portableInventoryBase.snapshot.items.find((item) => item.inventoryItemId === id) ?? null
    );
  }, [portableInventoryBase.selectedItemId, portableInventoryBase.snapshot]);

  const closePortableDetails = useCallback(() => {
    portableInventoryBase.selectItem(null);
  }, [portableInventoryBase]);

  const requestPortableAction = useCallback(
    (itemId: string, action: PortableAssetActionKind) => {
      portableActionSeqRef.current += 1;
      portableActionPlanContextRef.current = null;
      portableActionBusyRef.current = false;
      setPortableActionBusy(false);
      setPortableActionPlan(null);
      setPortableActionResult(null);
      setPortableActionError(null);
      setPortableActionClientRequestId(null);
      portableInventoryBase.openAction(itemId, action);
    },
    [portableInventoryBase],
  );

  const portableActionOpen = Boolean(portableInventoryBase.pendingAction);
  const portableActionKind = portableInventoryBase.pendingAction?.action ?? null;

  const mintClientRequestId = useCallback(() => {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID();
    }
    return `portable-action-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  }, []);

  const previewPortableAction = useCallback(
    async (request: PreviewPortableAssetActionRequest) => {
      // 同步 busy 门闩：双击在 re-render 前不得启动第二次 preview。
      if (portableActionBusyRef.current) return;
      // stale 禁止 mutation：preview 也不得在 mutationBlocked 时发出。
      if (portableInventoryBase.mutationBlocked || portableInventoryBase.stale) {
        setPortableActionError(t('agentHub:portable.actionDialog.mutationBlocked'));
        return;
      }
      const pendingAction = portableInventoryBase.pendingAction;
      if (
        !pendingAction ||
        request.inventoryItemIds.length !== 1 ||
        request.inventoryItemIds[0] !== pendingAction.itemId ||
        request.action !== pendingAction.action ||
        request.inventorySnapshotHash !==
          portableInventoryBase.snapshot?.inventorySnapshotHash
      ) {
        setPortableActionError(t('agentHub:portable.actionDialog.contextChanged'));
        return;
      }
      portableActionBusyRef.current = true;
      const actionSeq = ++portableActionSeqRef.current;
      const contextFingerprint = portableActionContextFingerprintRef.current;
      setPortableActionBusy(true);
      setPortableActionError(null);
      setPortableActionResult(null);
      try {
        // 透传壳层 device/project，peer 路径 API fail-closed 不静默本机写
        const plan = await portableAssetApi.previewAction({
          ...request,
          inventoryQuery: portableInventoryBase.inventoryQuery,
          ...portableInventoryBase.requestContext,
        });
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        const clientRequestId = mintClientRequestId();
        setPortableActionPlan(plan);
        portableActionPlanContextRef.current = {
          planToken: plan.planToken,
          clientRequestId,
          fingerprint: contextFingerprint,
        };
        setPortableActionClientRequestId(clientRequestId);
      } catch (reason) {
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        setPortableActionError(toErrorMessage(reason));
      } finally {
        if (
          mountedRef.current &&
          actionSeq === portableActionSeqRef.current &&
          contextFingerprint === portableActionContextFingerprintRef.current
        ) {
          portableActionBusyRef.current = false;
          setPortableActionBusy(false);
        }
      }
    },
    [
      mintClientRequestId,
      portableInventoryBase.mutationBlocked,
      portableInventoryBase.inventoryQuery,
      portableInventoryBase.pendingAction,
      portableInventoryBase.requestContext,
      portableInventoryBase.snapshot?.inventorySnapshotHash,
      portableInventoryBase.stale,
      t,
    ],
  );

  const confirmPortableAction = useCallback(
    async (planToken: string, clientRequestId: string) => {
      // 同步 busy 门闩：confirm 在已 busy 时直接拒绝。
      if (portableActionBusyRef.current) return;
      // H1: preview 成功后 inventory 变 stale 时禁止 apply。
      if (portableInventoryBase.mutationBlocked || portableInventoryBase.stale) {
        setPortableActionError(t('agentHub:portable.actionDialog.mutationBlocked'));
        return;
      }
      const contextFingerprint = portableActionContextFingerprintRef.current;
      const planContext = portableActionPlanContextRef.current;
      if (
        portableActionPlan?.planToken !== planToken ||
        planContext?.planToken !== planToken ||
        planContext.clientRequestId !== clientRequestId ||
        planContext.fingerprint !== contextFingerprint ||
        portableActionPlan.inventorySnapshotHash !==
          portableInventoryBase.snapshot?.inventorySnapshotHash
      ) {
        setPortableActionError(t('agentHub:portable.actionDialog.contextChanged'));
        return;
      }
      const itemId = portableInventoryBase.pendingAction?.itemId;
      portableActionBusyRef.current = true;
      const actionSeq = ++portableActionSeqRef.current;
      setPortableActionBusy(true);
      setPortableActionError(null);
      if (itemId) portableInventoryBase.setItemLocked(itemId, true);
      try {
        const applyRequest: ApplyPortableAssetActionRequest = {
          planToken,
          clientRequestId,
          ...portableInventoryBase.requestContext,
        };
        const result = await portableAssetApi.applyAction(applyRequest);
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        setPortableActionResult(result);
        setPortableActionClientRequestId(clientRequestId);
        await portableInventoryBase.refresh();
      } catch (reason) {
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        setPortableActionError(toErrorMessage(reason));
      } finally {
        if (
          mountedRef.current &&
          actionSeq === portableActionSeqRef.current &&
          contextFingerprint === portableActionContextFingerprintRef.current
        ) {
          if (itemId) portableInventoryBase.setItemLocked(itemId, false);
          portableActionBusyRef.current = false;
          setPortableActionBusy(false);
        }
      }
    },
    [portableActionPlan, portableInventoryBase, t],
  );

  const reconcilePortableAction = useCallback(
    async (clientRequestId: string) => {
      // reconcile 是 getAction 对账（非 apply mutation），但 busy 仍需同步门闩。
      if (portableActionBusyRef.current) return;
      const planContext = portableActionPlanContextRef.current;
      if (
        !planContext ||
        planContext.clientRequestId !== clientRequestId ||
        planContext.fingerprint !== portableActionContextFingerprintRef.current
      ) {
        setPortableActionError(t('agentHub:portable.actionDialog.contextChanged'));
        return;
      }
      portableActionBusyRef.current = true;
      const actionSeq = ++portableActionSeqRef.current;
      const contextFingerprint = portableActionContextFingerprintRef.current;
      setPortableActionBusy(true);
      setPortableActionError(null);
      try {
        const result = await portableAssetApi.getAction(
          clientRequestId,
          portableInventoryBase.requestContext,
        );
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        setPortableActionResult(result);
        await portableInventoryBase.refresh();
      } catch (reason) {
        if (
          !mountedRef.current ||
          actionSeq !== portableActionSeqRef.current ||
          contextFingerprint !== portableActionContextFingerprintRef.current
        ) {
          return;
        }
        setPortableActionError(toErrorMessage(reason));
      } finally {
        if (
          mountedRef.current &&
          actionSeq === portableActionSeqRef.current &&
          contextFingerprint === portableActionContextFingerprintRef.current
        ) {
          portableActionBusyRef.current = false;
          setPortableActionBusy(false);
        }
      }
    },
    [portableInventoryBase, t],
  );

  const closePortableAction = useCallback(() => {
    if (portableActionBusy || portableActionBusyRef.current) return;
    portableActionSeqRef.current += 1;
    portableActionPlanContextRef.current = null;
    portableInventoryBase.clearPendingAction();
    setPortableActionPlan(null);
    setPortableActionResult(null);
    setPortableActionError(null);
    setPortableActionClientRequestId(null);
  }, [portableActionBusy, portableInventoryBase]);

  const openPortablePull = useCallback(() => {
    setPortablePullOpen(true);
  }, []);

  const closePortablePull = useCallback(() => {
    if (portablePull.busy) return;
    setPortablePullOpen(false);
  }, [portablePull.busy]);

  const writeBlocked = Boolean(status && !status.writeCompatible);
  const upgradeRequired = writeBlocked;

  return {
    t,
    activeSection,
    setActiveSection,
    hubContext,
    contextMigrationNotice,
    onContextChange,
    shellPeers,
    shellProjects,
    userInstructions,
    portableInventory,
    portableDetailsOpen: Boolean(portableInventoryBase.selectedItemId),
    portableSelectedItem,
    closePortableDetails,
    requestPortableAction,
    portableActionOpen,
    portableActionKind,
    portableActionPlan,
    portableActionResult,
    portableActionBusy,
    portableActionError,
    portableActionClientRequestId,
    previewPortableAction,
    confirmPortableAction,
    reconcilePortableAction,
    closePortableAction,
    portablePullOpen,
    openPortablePull,
    closePortablePull,
    portablePull,
    loading,
    refreshing,
    stale,
    error,
    actionError,
    actionBusy,
    status,
    statusLoading,
    legacyLoadedOnce,
    legacyMatrixExpanded,
    expandLegacyMatrix,
    assets,
    filteredAssets,
    instructionsLaneActive,
    portableLaneActive,
    setInstructionRefresh,
    scopeFilter,
    kindFilter,
    setScopeFilter,
    setKindFilter,
    selectedAssetId,
    selectedAsset,
    selectAsset,
    preview,
    previewOpen,
    previewProjectId,
    setPreviewProjectId,
    openPreviewDialog,
    closePreviewDialog,
    runPreviewProject,
    runEnableProject,
    conflictDrawerOpen,
    openConflictDrawer,
    closeConflictDrawer,
    blocksDrawerOpen,
    openBlocksDrawer,
    closeBlocksDrawer,
    pluginDrawerOpen,
    pluginReport,
    openPluginDrawer,
    closePluginDrawer,
    loadPluginReport,
    pluginReportAssetId,
    adoptionOpen,
    adoptionPreview,
    openAdoptionPreview,
    closeAdoptionDialog,
    deleteEverywhereOpen,
    deleteEverywhereAssetId,
    openDeleteEverywhere,
    closeDeleteEverywhere,
    confirmDeleteEverywhere,
    deepLinkConflictId,
    deepLinkBridgePath: deepLinkBridge,
    reload,
    resolveConflict,
    updateInstruction,
    updateInstructionBlock,
    pairInstructionVariants,
    setTargetBinding,
    setTargetEnabled,
    setTargetPresence,
    restoreDetachedTarget,
    removeTarget,
    lanPushOpen,
    openLanPushDialog,
    closeLanPushDialog,
    lanPeers,
    lanSelectedPeerIds,
    toggleLanPeer,
    lanMode,
    setLanMode,
    lanAssetIdsText,
    setLanAssetIdsText,
    lanHubProjectIdsText,
    setLanHubProjectIdsText,
    lanPreview,
    lanReport,
    runLanPreview,
    runLanStart,
    gitImportOpen,
    openGitImportDrawer,
    closeGitImportDrawer,
    gitInspectReport,
    gitSelectedLaneDeviceId,
    selectGitLane,
    gitPreview,
    gitSelectedAssetIds,
    gitAssetSelectionExplicit,
    toggleGitAsset,
    gitMappingDrafts,
    setGitMappingDraft,
    gitConfirmOutcome,
    gitLastMapping,
    runGitInspect,
    runGitPreview,
    runGitConfirmMapping,
    runGitConfirmImport,
    writeBlocked,
    upgradeRequired,
  };
}

export type { AgentTarget, DesiredPresence };
