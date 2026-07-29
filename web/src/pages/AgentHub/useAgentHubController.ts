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
import type {
  AgentHubAdoptionPreview,
  AgentHubAssetDetail,
  AgentHubAssetSummary,
  AgentHubProjectPreview,
  AgentHubStatus,
  AgentTarget,
  DesiredPresence,
} from '@/lib/types/agentHub';

/**
 * Controller 返回值。
 *
 * Business Logic: 纯视图只消费本接口，禁止 import @/api/*。
 * Code Logic: 聚合 loading/error/filters/drawers 与 actions。
 */
export interface UseAgentHubControllerResult {
  t: TFunction<['agentHub', 'common']>;
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
  const [searchParams] = useSearchParams();

  const deepLinkAssetId = searchParams.get('assetId');
  const deepLinkConflictId = searchParams.get('conflictId');

  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [stale, setStale] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [status, setStatus] = useState<AgentHubStatus | null>(null);
  const [assets, setAssets] = useState<AgentHubAssetSummary[]>([]);
  const [scopeFilter, setScopeFilter] = useState('');
  const [kindFilter, setKindFilter] = useState('');
  // deep link 初值在 useState 中完成，避免 effect 同步 setState 级联渲染
  const [selectedAssetId, setSelectedAssetId] = useState<string | null>(deepLinkAssetId);
  const [selectedAsset, setSelectedAsset] = useState<AgentHubAssetDetail | null>(null);
  const [preview, setPreview] = useState<AgentHubProjectPreview | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewProjectId, setPreviewProjectId] = useState('');
  const [conflictDrawerOpen, setConflictDrawerOpen] = useState(Boolean(deepLinkConflictId));
  const [blocksDrawerOpen, setBlocksDrawerOpen] = useState(false);
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
  const scopeFilterRef = useRef(scopeFilter);
  const kindFilterRef = useRef(kindFilter);
  scopeFilterRef.current = scopeFilter;
  kindFilterRef.current = kindFilter;

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

  const writeBlocked = Boolean(status && !status.writeCompatible);
  const upgradeRequired = writeBlocked;

  return {
    t,
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
    writeBlocked,
    upgradeRequired,
  };
}

export type { AgentTarget, DesiredPresence };
