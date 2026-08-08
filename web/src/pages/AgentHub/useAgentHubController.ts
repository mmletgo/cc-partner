/**
 * Agent Hub 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent Hub 持有 status/assets/选中详情/预览/冲突与块抽屉状态；
 *   把 IPC 与 request sequence 从纯视图拆出，保证 hooks 在 early return 前。
 *
 * Code Logic（这个 hook 做什么）:
 *   首屏加载 status+assets；scope/kind 过滤；stale sequence 防切换覆盖；
 *   暴露 preview/enable/resolve/update/pair/binding/presence/restore/everywhere 动作。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
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
  type AgentHubContext,
  type AgentHubTab,
  type AgentHubScope,
} from './context/agentHubContext';

export {
  parseAgentHubContext,
  writeAgentHubContext,
  mapLegacySection,
  DEFAULT_AGENT_HUB_CONTEXT,
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
  const scope = params.get('scope');
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
 * Business Logic: 把 assets 筛选写回 URL，保留无关 deep link 参数。
 * Code Logic: 默认值删除 query key，避免噪声。
 */
export function writePortableFiltersToSearchParams(
  params: URLSearchParams,
  filters: PortableInventoryFilters,
  inventoryItemId: string | null,
): URLSearchParams {
  const next = new URLSearchParams(params);
  next.set('section', 'assets');
  if (filters.target === 'all') next.delete('target');
  else next.set('target', filters.target);
  if (filters.kind === DEFAULT_PORTABLE_INVENTORY_FILTERS.kind) next.delete('kind');
  else next.set('kind', filters.kind);
  if (filters.scope === 'all') next.delete('scope');
  else next.set('scope', filters.scope);
  if (filters.actualState === 'all') next.delete('state');
  else next.set('state', filters.actualState);
  if (filters.management === 'all') next.delete('management');
  else next.set('management', filters.management);
  if (inventoryItemId) next.set('inventoryItemId', inventoryItemId);
  else next.delete('inventoryItemId');
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
  /** 壳层 patch → URL write + 双路径 activeSection 同步。 */
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  /** 壳层 peer 列表（空数组 stub；T7 填实）。 */
  shellPeers: Array<{ deviceId: string; name: string; online: boolean }>;
  /** 壳层项目列表（空数组 stub；T7 填实）。 */
  shellProjects: Array<{ key: string; label: string; remote: boolean }>;
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
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  status: AgentHubStatus | null;
  assets: AgentHubAssetSummary[];
  filteredAssets: AgentHubAssetSummary[];
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
  const userInstructions = useUserInstructionManager(t);
  const portableInventoryBase = usePortableInventoryController();
  const [portablePullOpen, setPortablePullOpen] = useState(false);
  const portablePull = usePortablePullController({ open: portablePullOpen });
  const [activeSection, setActiveSectionState] = useState<AgentHubSection>(() => {
    if (deepLinkAssetId || deepLinkConflictId) return 'assets';
    if (deepLinkPreview || deepLinkProjectId || deepLinkBridge) return 'projectInstructions';
    return normalizeAgentHubSection(deepLinkSection, 'userInstructions');
  });
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
  const portableFiltersBootRef = useRef(false);
  const portableUrlSyncSkipRef = useRef(true);

  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  // Gate C LAN push / Git import UI state
  const [lanPushOpen, setLanPushOpen] = useState(false);
  const [lanPeers, setLanPeers] = useState<LanPushPeerOption[]>([]);
  const [lanSelectedPeerIds, setLanSelectedPeerIds] = useState<string[]>([]);
  const [lanMode, setLanMode] = useState<AgentHubPushSelectionMode>('fullHub');
  const [lanAssetIdsText, setLanAssetIdsText] = useState('');
  const [lanHubProjectIdsText, setLanHubProjectIdsText] = useState('');
  const [lanPreview, setLanPreview] = useState<AgentHubLanPushPreview | null>(null);
  const [lanReport, setLanReport] = useState<AgentHubMultiTargetPushReport | null>(null);
  const [gitImportOpen, setGitImportOpen] = useState(false);
  const [gitInspectReport, setGitInspectReport] = useState<AgentHubGitLaneInspectReport | null>(null);
  const [gitSelectedLaneDeviceId, setGitSelectedLaneDeviceId] = useState<string | null>(null);
  const [gitPreview, setGitPreview] = useState<AgentHubGitImportPreview | null>(null);
  const [gitSelectedAssetIds, setGitSelectedAssetIds] = useState<string[]>([]);
  const [gitMappingDrafts, setGitMappingDrafts] = useState<Record<string, string>>({});
  const [gitConfirmOutcome, setGitConfirmOutcome] = useState<AgentHubConfirmGitImportOutcome | null>(null);
  const [gitLastMapping, setGitLastMapping] = useState<AgentHubResolvedProjectMapping | null>(null);
  const [status, setStatus] = useState<AgentHubStatus | null>(null);
  const [assets, setAssets] = useState<AgentHubAssetSummary[]>([]);
  const [scopeFilter, setScopeFilter] = useState('');
  const [kindFilter, setKindFilter] = useState('');
  // deep link 初值在 useState 中完成，避免 effect 同步 setState 级联渲染
  const [selectedAssetId, setSelectedAssetId] = useState<string | null>(deepLinkAssetId);
  const [selectedAsset, setSelectedAsset] = useState<AgentHubAssetDetail | null>(null);
  const [preview, setPreview] = useState<AgentHubProjectPreview | null>(null);
  const [previewOpen, setPreviewOpen] = useState(
    deepLinkPreview === '1' || deepLinkPreview === 'true',
  );
  const [previewProjectId, setPreviewProjectId] = useState(deepLinkProjectId?.trim() ?? '');
  const [conflictDrawerOpen, setConflictDrawerOpen] = useState(Boolean(deepLinkConflictId));
  const [blocksDrawerOpen, setBlocksDrawerOpen] = useState(false);
  const [pluginDrawerOpen, setPluginDrawerOpen] = useState(false);
  const [pluginReport, setPluginReport] = useState<PluginPackageReport | null>(null);
  const [adoptionOpen, setAdoptionOpen] = useState(false);
  const [adoptionPreview, setAdoptionPreview] = useState<AgentHubAdoptionPreview | null>(null);
  const [deleteEverywhereOpen, setDeleteEverywhereOpen] = useState(false);
  const [deleteEverywhereAssetId, setDeleteEverywhereAssetId] = useState<string | null>(null);

  const refreshSeqRef = useRef(0);
  const detailSeqRef = useRef(0);
  const scopeCursorRef = useRef(0);
  const mountedRef = useRef(true);
  const filtersBootstrappedRef = useRef(false);
  const appliedDeepLinkRef = useRef<string | null>(null);
  const appliedPreviewDeepLinkRef = useRef<string | null>(null);
  const scopeFilterRef = useRef(scopeFilter);
  const kindFilterRef = useRef(kindFilter);
  useEffect(() => {
    scopeFilterRef.current = scopeFilter;
    kindFilterRef.current = kindFilter;
  }, [scopeFilter, kindFilter]);

  /**
   * Business Logic: 首屏与手动刷新加载 status + assets。
   * Code Logic: 递增 refreshSeq + scopeCursor；过期/错 scope 响应不写入。
   */
  const loadCore = useCallback(async (isRefresh: boolean) => {
    const seq = ++refreshSeqRef.current;
    const scopeCursor = ++scopeCursorRef.current;
    const scopeAtRequest = scopeFilterRef.current;
    const kindAtRequest = kindFilterRef.current;
    if (isRefresh) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    setError(null);
    try {
      const [nextStatus, nextAssets] = await Promise.all([
        agentHubApi.getStatus(),
        agentHubApi.listAssets({
          scopeId: scopeAtRequest.trim() || null,
          kind: kindAtRequest.trim() || null,
        }),
      ]);
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      // 快速切换 scope/kind 时，旧响应不得覆盖新 filter 的列表
      if (scopeCursor !== scopeCursorRef.current) return;
      if (
        scopeAtRequest !== scopeFilterRef.current ||
        kindAtRequest !== kindFilterRef.current
      ) {
        return;
      }
      setStatus(nextStatus);
      setAssets(nextAssets);
      setStale(false);
    } catch (reason) {
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      if (scopeCursor !== scopeCursorRef.current) return;
      setError(toErrorMessage(reason));
      // 有旧数据时标 stale，保留列表
      setStale((prev) => prev || status !== null || assets.length > 0);
    }
    if (!mountedRef.current || seq !== refreshSeqRef.current) return;
    setLoading(false);
    setRefreshing(false);
  }, [assets.length, status]);

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
    // 挂载拉取外部 status/assets；异步路径内 setState，非同步级联渲染
    // eslint-disable-next-line react-hooks/set-state-in-effect -- mount external fetch
    void loadCore(false);
    // 首屏 deep link：仅异步拉详情，selected/conflict 已由 useState 初值设置
    if (deepLinkAssetId) {
      appliedDeepLinkRef.current = `${deepLinkAssetId}|${deepLinkConflictId ?? ''}`;
      void loadAssetDetail(deepLinkAssetId);
    }
    return () => {
      mountedRef.current = false;
    };
    // 仅挂载首载；过滤变化与后续 deep link 走下方 effect
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    // 跳过 mount 首轮（由挂载 effect 负责）
    if (!filtersBootstrappedRef.current) {
      filtersBootstrappedRef.current = true;
      return;
    }
    // 过滤变化触发外部 list 刷新
    // eslint-disable-next-line react-hooks/set-state-in-effect -- filter change external fetch
    void loadCore(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeFilter, kindFilter]);

  useEffect(() => {
    // URL deep link 后续变化：异步拉详情；仅当 key 变化时同步 selected（事件驱动，非首轮 mount）
    if (!deepLinkAssetId && !deepLinkConflictId) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
    setActiveSectionState('assets');
    if (!deepLinkAssetId) return;
    const key = `${deepLinkAssetId}|${deepLinkConflictId ?? ''}`;
    if (appliedDeepLinkRef.current === key) return;
    appliedDeepLinkRef.current = key;
    // searchParams 变化是外部导航事件，同步选中资产与冲突抽屉
    // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
    setSelectedAssetId(deepLinkAssetId);
    if (deepLinkConflictId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
      setConflictDrawerOpen(true);
    }
    void loadAssetDetail(deepLinkAssetId);
  }, [deepLinkAssetId, deepLinkConflictId, loadAssetDetail]);

  useEffect(() => {
    // OpenCode bridge / project preview deep link：打开既有 preview dialog，不 enable。
    const wantsPreview = deepLinkPreview === '1' || deepLinkPreview === 'true';
    if (!wantsPreview && !deepLinkBridge) return;
    const key = `preview|${deepLinkPreview ?? ''}|${deepLinkProjectId ?? ''}|${deepLinkBridge ?? ''}`;
    if (appliedPreviewDeepLinkRef.current === key) return;
    appliedPreviewDeepLinkRef.current = key;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
    setActiveSectionState('projectInstructions');
    // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
    setPreviewOpen(true);
    if (deepLinkProjectId?.trim()) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
      setPreviewProjectId(deepLinkProjectId.trim());
    }
  }, [deepLinkBridge, deepLinkPreview, deepLinkProjectId]);

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
    setActionBusy(true);
    setActionError(null);
    try {
      const next = await agentHubApi.previewProject(projectId);
      if (!mountedRef.current) return;
      setPreview(next);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [previewProjectId, t]);

  const runEnableProject = useCallback(async () => {
    const projectId = previewProjectId.trim();
    if (!projectId) {
      setActionError(t('agentHub:errors.projectIdRequired'));
      return;
    }
    setActionBusy(true);
    setActionError(null);
    try {
      await agentHubApi.enableProject(projectId);
      if (!mountedRef.current) return;
      setPreviewOpen(false);
      await loadCore(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [loadCore, previewProjectId, t]);

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
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
        setPluginReport(null);
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

  const reload = useCallback(async () => {
    await loadCore(true);
    if (selectedAssetId) {
      await loadAssetDetail(selectedAssetId);
    }
  }, [loadAssetDetail, loadCore, selectedAssetId]);

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
        await loadCore(true);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [loadCore, requireSelectedAssetId],
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
        await loadCore(true);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, loadCore, requireSelectedAssetId],
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
        await loadCore(true);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, loadAssetDetail, loadCore, requireSelectedAssetId],
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
        await loadCore(true);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(toErrorMessage(reason));
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [expectedRevisionFromSelection, loadCore, requireSelectedAssetId],
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
        await loadCore(true);
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
    [loadAssetDetail, loadCore, selectedAssetId],
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
      await loadCore(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [deleteEverywhereAssetId, loadCore, requireSelectedAssetId, selectedAssetId]);


  const openLanPushDialog = useCallback(() => {
    setLanPushOpen(true);
    setActionError(null);
    setLanPreview(null);
    setLanReport(null);
    void devicesApi.list().then((list) => {
      if (!mountedRef.current) return;
      setLanPeers(list.map((d) => ({ deviceId: d.id, name: d.name })));
    }).catch((reason) => {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    });
  }, []);

  const closeLanPushDialog = useCallback(() => {
    if (actionBusy) return;
    setLanPushOpen(false);
  }, [actionBusy]);

  const toggleLanPeer = useCallback((deviceId: string) => {
    setLanSelectedPeerIds((prev) =>
      prev.includes(deviceId) ? prev.filter((id) => id !== deviceId) : [...prev, deviceId],
    );
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
    setActionBusy(true);
    setActionError(null);
    try {
      const previewResult = await agentHubApi.previewLanPush(buildLanRequest());
      if (!mountedRef.current) return;
      setLanPreview(previewResult);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [buildLanRequest]);

  const runLanStart = useCallback(async () => {
    setActionBusy(true);
    setActionError(null);
    try {
      const report = await agentHubApi.startLanPush(buildLanRequest());
      if (!mountedRef.current) return;
      setLanReport(report);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [buildLanRequest]);

  const openGitImportDrawer = useCallback(() => {
    setGitImportOpen(true);
    setActionError(null);
    setGitInspectReport(null);
    setGitSelectedLaneDeviceId(null);
    setGitPreview(null);
    setGitSelectedAssetIds([]);
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
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [gitSelectedLaneDeviceId]);

  const toggleGitAsset = useCallback((assetId: string) => {
    setGitSelectedAssetIds((prev) => {
      // empty means "all"; first toggle materializes all then removes/adds
      if (prev.length === 0 && gitPreview) {
        const all = gitPreview.assets.map((a) => a.assetId);
        return all.filter((id) => id !== assetId);
      }
      return prev.includes(assetId) ? prev.filter((id) => id !== assetId) : [...prev, assetId];
    });
  }, [gitPreview]);

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
        selectedAssetIds: gitSelectedAssetIds,
        projectMappings,
        importUnmappedProjects: true,
      });
      if (!mountedRef.current) return;
      setGitConfirmOutcome(outcome);
      await loadCore(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(toErrorMessage(reason));
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [gitMappingDrafts, gitPreview, gitSelectedAssetIds, loadCore]);

  /**
   * Business Logic: URL 权威的壳层上下文。
   * Code Logic: 每次 searchParams 变化 re-parse。
   */
  const hubContext = useMemo(
    () => parseAgentHubContext(searchParams),
    [searchParams],
  );

  /**
   * Business Logic: 壳层 patch 写回 URL，并同步旧 activeSection 双路径内容。
   * Code Logic: merge + scope 互斥 → writeAgentHubContext；资产 tab 顺带粗同步 kind/target filter。
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
        return writeAgentHubContext(prev, next);
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

      // dual path: keep legacy section content in sync with shell
      if (merged.adaptView) {
        // adapt 全页后续任务；暂不改 section
      } else {
        setActiveSectionState(mapContextToSection(merged));
      }

      // 粗映射：资产 tab → portable kind/target 筛选
      if (
        merged.tab === 'skill' ||
        merged.tab === 'command' ||
        merged.tab === 'mcp' ||
        merged.tab === 'plugin'
      ) {
        portableInventoryBase.setFilters({
          kind: merged.tab,
          target: merged.agent,
          scope: merged.scope,
        });
      }
    },
    [portableInventoryBase, searchParams, setSearchParams],
  );

  /**
   * Business Logic: 一级 section 切换写 URL，离开 assets 时清库存 deep-link 参数。
   * Code Logic: replace 避免堆 history；保留 conflictId/assetId 等无关参数；
   *   同时 patch 新 IA context 键，避免壳层与 section 分叉。
   */
  const setActiveSection = useCallback(
    (section: AgentHubSection) => {
      setActiveSectionState(section);
      setSearchParams((prev) => {
        const next = new URLSearchParams(prev);
        if (section === 'userInstructions') {
          next.delete('section');
        } else {
          next.set('section', section);
        }
        if (section !== 'assets') {
          next.delete('kind');
          next.delete('target');
          next.delete('state');
          next.delete('management');
          next.delete('inventoryItemId');
        }
        // 双路径：section → context keys；write 会剥离 legacy section，再写回以保 deep link 测试契约
        const patch = mapSectionToContextPatch(section);
        if (Object.keys(patch).length > 0) {
          const ctx = { ...parseAgentHubContext(next), ...patch };
          if (ctx.scope === 'user') ctx.projectKey = null;
          else ctx.deviceId = null;
          const written = writeAgentHubContext(next, ctx);
          if (section === 'userInstructions') {
            written.delete('section');
          } else {
            written.set('section', section);
          }
          return written;
        }
        return next;
      }, { replace: true });
    },
    [setSearchParams],
  );

  /** 壳层 peers stub：复用 lanPeers 名；online 暂 true（T7 接真状态）。 */
  const shellPeers = useMemo(
    () =>
      lanPeers.map((peer) => ({
        deviceId: peer.deviceId,
        name: peer.name,
        online: true,
      })),
    [lanPeers],
  );

  /** 壳层项目 stub：T7 接 workbench 本机/远端项目。 */
  const shellProjects = useMemo(
    () => [] as Array<{ key: string; label: string; remote: boolean }>,
    [],
  );

  const portableInventory: UsePortableInventoryControllerResult = portableInventoryBase;

  // URL → initial filters/selection once
  useEffect(() => {
    if (portableFiltersBootRef.current) return;
    portableFiltersBootRef.current = true;
    const patch = parsePortableFiltersFromSearchParams(searchParams);
    if (Object.keys(patch).length > 0) {
      portableInventoryBase.setFilters(patch);
    }
    if (deepLinkInventoryItemId) {
      portableInventoryBase.selectItem(deepLinkInventoryItemId);
    }
    if (normalizeAgentHubSection(deepLinkSection) === 'assets' || deepLinkInventoryItemId) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot deep-link bootstrap
      setActiveSectionState('assets');
    }
    // 首帧后允许 filters → URL 同步
    portableUrlSyncSkipRef.current = false;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-shot URL bootstrap
  }, []);

  // section deep link later changes
  useEffect(() => {
    if (!deepLinkSection) return;
    const next = normalizeAgentHubSection(deepLinkSection, activeSection);
    if (next !== activeSection) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- deep-link navigation
      setActiveSectionState(next);
    }
  }, [deepLinkSection, activeSection]);

  // filters/selection → URL while on assets
  useEffect(() => {
    if (portableUrlSyncSkipRef.current) return;
    if (activeSection !== 'assets') return;
    setSearchParams((prev) => {
      const desired = writePortableFiltersToSearchParams(
        prev,
        portableInventoryBase.filters,
        portableInventoryBase.selectedItemId,
      );
      // 避免无变化 set 触发环
      if (desired.toString() === prev.toString()) return prev;
      return desired;
    }, { replace: true });
  }, [
    activeSection,
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
      portableActionBusyRef.current = true;
      setPortableActionBusy(true);
      setPortableActionError(null);
      setPortableActionResult(null);
      try {
        const plan = await portableAssetApi.previewAction(request);
        if (!mountedRef.current) return;
        setPortableActionPlan(plan);
        setPortableActionClientRequestId(mintClientRequestId());
      } catch (reason) {
        if (!mountedRef.current) return;
        setPortableActionError(toErrorMessage(reason));
      } finally {
        portableActionBusyRef.current = false;
        if (mountedRef.current) setPortableActionBusy(false);
      }
    },
    [mintClientRequestId, portableInventoryBase.mutationBlocked, portableInventoryBase.stale, t],
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
      const itemId = portableInventoryBase.pendingAction?.itemId;
      portableActionBusyRef.current = true;
      setPortableActionBusy(true);
      setPortableActionError(null);
      if (itemId) portableInventoryBase.setItemLocked(itemId, true);
      try {
        const applyRequest: ApplyPortableAssetActionRequest = { planToken, clientRequestId };
        const result = await portableAssetApi.applyAction(applyRequest);
        if (!mountedRef.current) return;
        setPortableActionResult(result);
        setPortableActionClientRequestId(clientRequestId);
        await portableInventoryBase.refresh();
      } catch (reason) {
        if (!mountedRef.current) return;
        setPortableActionError(toErrorMessage(reason));
      } finally {
        if (itemId) portableInventoryBase.setItemLocked(itemId, false);
        portableActionBusyRef.current = false;
        if (mountedRef.current) setPortableActionBusy(false);
      }
    },
    [portableInventoryBase, t],
  );

  const reconcilePortableAction = useCallback(
    async (clientRequestId: string) => {
      // reconcile 是 getAction 对账（非 apply mutation），但 busy 仍需同步门闩。
      if (portableActionBusyRef.current) return;
      portableActionBusyRef.current = true;
      setPortableActionBusy(true);
      setPortableActionError(null);
      try {
        const result = await portableAssetApi.getAction(clientRequestId);
        if (!mountedRef.current) return;
        setPortableActionResult(result);
        await portableInventoryBase.refresh();
      } catch (reason) {
        if (!mountedRef.current) return;
        setPortableActionError(toErrorMessage(reason));
      } finally {
        portableActionBusyRef.current = false;
        if (mountedRef.current) setPortableActionBusy(false);
      }
    },
    [portableInventoryBase],
  );

  const closePortableAction = useCallback(() => {
    if (portableActionBusy || portableActionBusyRef.current) return;
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
    assets,
    filteredAssets,
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
