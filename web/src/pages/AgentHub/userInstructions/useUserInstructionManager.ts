/**
 * 用户级指令 V2 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   用户级指令必须以“只读发现 → 本地草稿 → 零写入预览 → 用户确认 → 逐目标结果”工作，
 *   并在 preview stale、外部文件变化或部分写入失败时保留草稿。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 workspace/draft/setup/preview/apply 状态与 request sequence；所有 API 调用集中在此，
 *   pure views 只消费 narrow props，不直接 import transport。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi } from '@/api/agentHub';
import type {
  AgentTarget,
  UserInstructionApplyResultDto,
  UserInstructionDraft,
  UserInstructionPlanDto,
  UserInstructionTargetDto,
  UserInstructionTargetSelection,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';

export type UserInstructionEditorPane = 'common' | AgentTarget;
export type UserInstructionTargetIntent =
  | 'manage'
  | 'pause'
  | 'resume'
  | 'stopManaging'
  | 'remove'
  | 'compare'
  | 'adopt'
  | 'restore';

const EMPTY_TARGET_SELECTIONS: Record<AgentTarget, UserInstructionTargetSelection> = {
  claude: 'unmanaged',
  codex: 'unmanaged',
  opencode: 'unmanaged',
};

/**
 * Business Logic（为什么需要）:
 *   首次载入和成功应用后需要把 canonical/management mode 转成用户可编辑草稿；
 *   未纳管但本机已有文件时，用磁盘正文 seed，避免编辑区空白。
 *
 * Code Logic（做什么）:
 *   common/extension 优先 canonical；否则按 target 从 active source.content 填 extension；
 *   managedActive/managedPaused 都归一为 managed 选择。
 */
export function createUserInstructionDraft(
  workspace: UserInstructionWorkspaceDto,
): UserInstructionDraft {
  const selections = { ...EMPTY_TARGET_SELECTIONS };
  const targetExtensions: Partial<Record<AgentTarget, string>> = {
    ...(workspace.canonical?.targetExtensions ?? {}),
  };
  let commonContent = workspace.canonical?.commonContent ?? '';

  for (const target of workspace.targets) {
    if (target.managementMode !== 'unmanaged') {
      selections[target.target] = 'managed';
      continue;
    }
    const activeSource =
      target.sources.find((source) => source.active) ??
      target.sources.find((source) => source.exists && typeof source.content === 'string');
    selections[target.target] = activeSource?.role === 'fallback' ? 'inherit' : 'unmanaged';

    // 无 canonical 时用磁盘正文填充，便于直接编辑（按 target extension 分栏）。
    if (
      !workspace.canonical &&
      typeof activeSource?.content === 'string' &&
      activeSource.content.length > 0
    ) {
      targetExtensions[target.target] = activeSource.content;
    }
  }

  // 仅单 target 有磁盘正文且无 canonical：放进 common（清空 extensions 避免 apply 双写）。
  if (!workspace.canonical && !commonContent.trim()) {
    const filled = (['claude', 'codex', 'opencode'] as const).filter(
      (key) => typeof targetExtensions[key] === 'string' && targetExtensions[key]!.trim().length > 0,
    );
    if (filled.length === 1) {
      const only = filled[0]!;
      commonContent = targetExtensions[only] ?? '';
      delete targetExtensions[only];
    }
  }

  return {
    commonContent,
    targetExtensions,
    targetSelections: selections,
  };
}

/**
 * Business Logic（为什么需要）:
 *   只读兼容模式和未认证 CLI 不能出现可执行自动写入按钮。
 *
 * Code Logic（做什么）:
 *   至少一个 target write=supported 才允许生成可执行 setup/update plan。
 */
export function hasWritableUserInstructionTarget(
  workspace: UserInstructionWorkspaceDto | null,
): boolean {
  return Boolean(workspace?.targets.some((target) => target.capability.write === 'supported'));
}

/**
 * Business Logic（为什么需要）:
 *   apply stale/失败必须用稳定 code 驱动恢复，不解析本地化 message。
 *
 * Code Logic（做什么）:
 *   读取 Error-like code，缺失返回 null。
 */
function errorCode(reason: unknown): string | null {
  if (!reason || typeof reason !== 'object') return null;
  const code = (reason as { code?: unknown }).code;
  return typeof code === 'string' ? code : null;
}

/**
 * Business Logic（为什么需要）:
 *   同一个 plan 的失败重试必须复用幂等键，生成新 plan 后才换键。
 *
 * Code Logic（做什么）:
 *   优先 crypto.randomUUID；旧 WebView 使用时间+随机后备。
 */
function createClientRequestId(): string {
  if (typeof globalThis.crypto?.randomUUID === 'function') {
    return globalThis.crypto.randomUUID();
  }
  return `user-instruction-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/** 用户级指令 controller 对 pure view 的返回合同。 */
export interface UseUserInstructionManagerResult {
  workspace: UserInstructionWorkspaceDto | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  draft: UserInstructionDraft;
  dirty: boolean;
  activePane: UserInstructionEditorPane;
  setActivePane: (pane: UserInstructionEditorPane) => void;
  updateDraftContent: (pane: UserInstructionEditorPane, value: string) => void;
  resetDraft: () => void;
  setupOpen: boolean;
  openSetup: (target?: AgentTarget) => void;
  closeSetup: () => void;
  setTargetSelection: (
    target: AgentTarget,
    selection: UserInstructionTargetSelection,
  ) => void;
  promoteTargetExtensionToCommon: (target: AgentTarget) => void;
  previewOpen: boolean;
  plan: UserInstructionPlanDto | null;
  closePreview: () => void;
  previewDraft: () => Promise<void>;
  applyPlan: () => Promise<void>;
  applyResult: UserInstructionApplyResultDto | null;
  dismissApplyResult: () => void;
  runTargetIntent: (
    target: UserInstructionTargetDto,
    intent: UserInstructionTargetIntent,
  ) => Promise<void>;
  openPath: (path: string) => Promise<void>;
  copyPath: (path: string) => Promise<void>;
  refresh: () => Promise<void>;
  canPreview: boolean;
  canonicalContentTruncated: boolean;
  deleteDialogOpen: boolean;
  deleteConfirmation: string;
  setDeleteConfirmation: (value: string) => void;
  openDeleteDialog: () => void;
  closeDeleteDialog: () => void;
  previewDeleteAsset: () => Promise<void>;
}

/**
 * Business Logic（为什么需要）:
 *   为 Agent Hub 默认入口提供完整的用户级指令安全编排。
 *
 * Code Logic（做什么）:
 *   inspect 有 stale guard；草稿用 ref 防刷新覆盖；preview/apply 共享 plan 与稳定幂等键。
 */
/**
 * Business Logic: 旧 V2 用户指令编排；主 UI 已切三栏，默认不 auto-load。
 * Code Logic: 类型与 refresh 保留供测试/F6 cleanup；mount 不发 inspect。
 */
export function useUserInstructionManager(
  t: TFunction<['agentHub', 'common']>,
): UseUserInstructionManagerResult {
  const [workspace, setWorkspace] = useState<UserInstructionWorkspaceDto | null>(null);
  // PR1 按需：默认不 auto-load，loading 初值 false
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [draft, setDraft] = useState<UserInstructionDraft>({
    commonContent: '',
    targetExtensions: {},
    targetSelections: { ...EMPTY_TARGET_SELECTIONS },
  });
  const [dirty, setDirty] = useState(false);
  const [activePane, setActivePane] = useState<UserInstructionEditorPane>('common');
  const [setupOpen, setSetupOpen] = useState(false);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [plan, setPlan] = useState<UserInstructionPlanDto | null>(null);
  const [applyResult, setApplyResult] = useState<UserInstructionApplyResultDto | null>(null);
  const [deleteDialogOpen, setDeleteDialogOpen] = useState(false);
  const [deleteConfirmation, setDeleteConfirmation] = useState('');

  const mountedRef = useRef(true);
  const loadSeqRef = useRef(0);
  const previewSeqRef = useRef(0);
  const draftRef = useRef(draft);
  const dirtyRef = useRef(dirty);
  const baselineDraftRef = useRef(draft);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(null);

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

  useEffect(() => {
    dirtyRef.current = dirty;
  }, [dirty]);

  /** 加载 inventory，用户已有草稿时只刷新事实，不覆盖草稿。 */
  const loadWorkspace = useCallback(async (isRefresh: boolean) => {
    const seq = ++loadSeqRef.current;
    if (isRefresh) setRefreshing(true);
    else setLoading(true);
    setError(null);
    try {
      const next = await agentHubApi.inspectUserInstructionWorkspace();
      if (!mountedRef.current || seq !== loadSeqRef.current) return;
      setWorkspace(next);
      if (!dirtyRef.current) {
        const nextDraft = createUserInstructionDraft(next);
        setDraft(nextDraft);
        baselineDraftRef.current = nextDraft;
      }
    } catch (reason) {
      if (!mountedRef.current || seq !== loadSeqRef.current) return;
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (mountedRef.current && seq === loadSeqRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    // 主路径由 three-pane 负责 inspect；V2 manager 仅保留手动 refresh 能力
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /** 更新公共或专属正文，只修改本地草稿。 */
  const updateDraftContent = useCallback((pane: UserInstructionEditorPane, value: string) => {
    setDraft((current) => {
      if (pane === 'common') return { ...current, commonContent: value };
      return {
        ...current,
        targetExtensions: { ...current.targetExtensions, [pane]: value },
      };
    });
    setDirty(true);
    setActionError(null);
  }, []);

  /** 放弃本地修改并恢复最后一次成功 inventory 的 canonical 草稿。 */
  const resetDraft = useCallback(() => {
    setDraft(baselineDraftRef.current);
    setDirty(false);
    setActionError(null);
  }, []);

  /** 打开首次设置；从 target 卡进入时只预选该 target，不触发 mutation。 */
  const openSetup = useCallback((target?: AgentTarget) => {
    if (target) {
      setDraft((current) => ({
        ...current,
        targetSelections: { ...current.targetSelections, [target]: 'managed' },
      }));
      setDirty(true);
    }
    setSetupOpen(true);
    setActionError(null);
  }, []);

  /** 关闭向导但保留草稿。 */
  const closeSetup = useCallback(() => {
    if (actionBusy) return;
    setSetupOpen(false);
  }, [actionBusy]);

  /** 修改 target 选择，只更新本地草稿。 */
  const setTargetSelection = useCallback(
    (target: AgentTarget, selection: UserInstructionTargetSelection) => {
      setDraft((current) => ({
        ...current,
        targetSelections: { ...current.targetSelections, [target]: selection },
      }));
      setDirty(true);
      setActionError(null);
    },
    [],
  );

  /** 把单一来源专属草稿显式提升为公共规则，不做语义改写。 */
  const promoteTargetExtensionToCommon = useCallback((target: AgentTarget) => {
    setDraft((current) => {
      const source = current.targetExtensions[target] ?? '';
      if (!source.trim()) return current;
      return {
        ...current,
        commonContent: source,
        targetExtensions: { ...current.targetExtensions, [target]: '' },
      };
    });
    setDirty(true);
    setActivePane('common');
    setActionError(null);
  }, []);

  /** 关闭预览但保留草稿和 setup 选择。 */
  const closePreview = useCallback(() => {
    if (actionBusy) return;
    setPreviewOpen(false);
  }, [actionBusy]);

  /** 统一执行零写入 setup/update preview。 */
  const previewDraft = useCallback(async () => {
    if (!workspace) return;
    if (workspace.canonical?.contentTruncated) {
      setActionError(t('agentHub:userInstructions.errors.contentTruncated'));
      return;
    }
    const seq = ++previewSeqRef.current;
    setActionBusy(true);
    setActionError(null);
    setApplyResult(null);
    try {
      const request = {
        ...draftRef.current,
        baseRevisionId: workspace.canonical?.headRevisionId ?? null,
        inventorySnapshotHash: workspace.inventorySnapshotHash,
      };
      const nextPlan =
        workspace.setupState === 'configured'
          ? await agentHubApi.previewUserInstructionUpdate(request)
          : await agentHubApi.previewUserInstructionSetup(request);
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setPlan(nextPlan);
      planRequestIdRef.current = {
        planToken: nextPlan.planToken,
        clientRequestId: createClientRequestId(),
      };
      setPreviewOpen(true);
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      const code = errorCode(reason);
      setActionError(
        code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
          ? t('agentHub:userInstructions.errors.backendUnavailable')
          : reason instanceof Error
            ? reason.message
            : String(reason),
      );
    } finally {
      if (mountedRef.current && seq === previewSeqRef.current) setActionBusy(false);
    }
  }, [t, workspace]);

  /** 应用当前 plan；stale/partial/failed 时保留草稿。 */
  const applyPlan = useCallback(async () => {
    if (!plan) return;
    const existing = planRequestIdRef.current;
    const request =
      existing?.planToken === plan.planToken
        ? existing
        : { planToken: plan.planToken, clientRequestId: createClientRequestId() };
    planRequestIdRef.current = request;
    setActionBusy(true);
    setActionError(null);
    try {
      const result = await agentHubApi.applyUserInstructionPlan(request);
      if (!mountedRef.current) return;
      setApplyResult(result);
      setPreviewOpen(false);
      setSetupOpen(false);
      const hasIncomplete = result.targets.some(
        (target) =>
          target.status === 'stalePreview' ||
          target.status === 'blocked' ||
          target.status === 'conflict' ||
          target.status === 'failed',
      );
      if (!hasIncomplete) {
        baselineDraftRef.current = draftRef.current;
        setDirty(false);
      }
      await loadWorkspace(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      const code = errorCode(reason);
      if (
        code === 'USER_INSTRUCTION_PREVIEW_STALE' ||
        code === 'USER_INSTRUCTION_SOURCE_CHANGED' ||
        code === 'USER_INSTRUCTION_REVISION_CHANGED'
      ) {
        setPreviewOpen(false);
        setPlan(null);
        setActionError(t('agentHub:userInstructions.errors.previewStale'));
      } else {
        setActionError(
          code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
            ? t('agentHub:userInstructions.errors.backendUnavailable')
            : reason instanceof Error
              ? reason.message
              : String(reason),
        );
      }
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [loadWorkspace, plan, t]);

  /** target 卡片动作先生成 plan；manage/resume/restore 回到统一草稿预览。 */
  const runTargetIntent = useCallback(
    async (target: UserInstructionTargetDto, intent: UserInstructionTargetIntent) => {
      if (!workspace) return;
      if (intent === 'manage' || intent === 'resume' || intent === 'restore') {
        openSetup(target.target);
        return;
      }
      if (intent === 'compare') {
        openSetup(target.target);
        return;
      }
      setActionBusy(true);
      setActionError(null);
      try {
        const request = {
          target: target.target,
          baseRevisionId: workspace.canonical?.headRevisionId ?? null,
          inventorySnapshotHash: workspace.inventorySnapshotHash,
        };
        let nextPlan: UserInstructionPlanDto;
        if (intent === 'pause') {
          nextPlan = await agentHubApi.previewPauseUserInstructionTarget(request);
        } else if (intent === 'stopManaging') {
          nextPlan = await agentHubApi.previewStopManagingUserInstructionTarget(request);
        } else if (intent === 'remove') {
          nextPlan = await agentHubApi.previewRemoveUserInstructionTarget(request);
        } else {
          const activeSource = target.sources.find((source) => source.active) ?? target.sources[0];
          if (!activeSource) {
            setActionError(t('agentHub:userInstructions.errors.sourceRequired'));
            return;
          }
          nextPlan = await agentHubApi.previewAdoptUserInstructionSource({
            ...request,
            sourceId: activeSource.sourceId,
            mode: 'targetExtension',
          });
        }
        if (!mountedRef.current) return;
        setPlan(nextPlan);
        planRequestIdRef.current = {
          planToken: nextPlan.planToken,
          clientRequestId: createClientRequestId(),
        };
        setPreviewOpen(true);
      } catch (reason) {
        if (!mountedRef.current) return;
        setActionError(
          errorCode(reason) === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
            ? t('agentHub:userInstructions.errors.backendUnavailable')
            : reason instanceof Error
              ? reason.message
              : String(reason),
        );
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [openSetup, t, workspace],
  );

  /** 打开 adapter 解析出的路径。 */
  const openPath = useCallback(async (path: string) => {
    setActionError(null);
    try {
      await agentHubApi.openUserInstructionPath(path);
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  /** 复制 adapter 解析出的路径。 */
  const copyPath = useCallback(async (path: string) => {
    setActionError(null);
    try {
      await navigator.clipboard.writeText(path);
    } catch (reason) {
      setActionError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  /** 打开危险区删除确认；真实删除仍必须先走 preview。 */
  const openDeleteDialog = useCallback(() => {
    setDeleteConfirmation('');
    setDeleteDialogOpen(true);
    setActionError(null);
  }, []);

  /** 关闭危险删除确认。 */
  const closeDeleteDialog = useCallback(() => {
    if (actionBusy) return;
    setDeleteDialogOpen(false);
  }, [actionBusy]);

  /** 删除 canonical/managed files 前生成路径级 plan。 */
  const previewDeleteAsset = useCallback(async () => {
    if (!workspace?.canonical) return;
    setActionBusy(true);
    setActionError(null);
    try {
      const nextPlan = await agentHubApi.previewDeleteUserInstructionAsset({
        baseRevisionId: workspace.canonical.headRevisionId,
        inventorySnapshotHash: workspace.inventorySnapshotHash,
      });
      if (!mountedRef.current) return;
      setPlan(nextPlan);
      planRequestIdRef.current = {
        planToken: nextPlan.planToken,
        clientRequestId: createClientRequestId(),
      };
      setDeleteDialogOpen(false);
      setPreviewOpen(true);
    } catch (reason) {
      if (!mountedRef.current) return;
      setActionError(
        errorCode(reason) === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
          ? t('agentHub:userInstructions.errors.backendUnavailable')
          : reason instanceof Error
            ? reason.message
            : String(reason),
      );
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [t, workspace]);

  const canPreview = useMemo(() => {
    if (!workspace || workspace.canonical?.contentTruncated) return false;
    return workspace.targets.some(
      (target) =>
        draft.targetSelections[target.target] === 'managed' &&
        target.capability.write === 'supported',
    );
  }, [draft.targetSelections, workspace]);

  return {
    workspace,
    loading,
    refreshing,
    error,
    actionError,
    actionBusy,
    draft,
    dirty,
    activePane,
    setActivePane,
    updateDraftContent,
    resetDraft,
    setupOpen,
    openSetup,
    closeSetup,
    setTargetSelection,
    promoteTargetExtensionToCommon,
    previewOpen,
    plan,
    closePreview,
    previewDraft,
    applyPlan,
    applyResult,
    dismissApplyResult: () => setApplyResult(null),
    runTargetIntent,
    openPath,
    copyPath,
    refresh: () => loadWorkspace(true),
    canPreview,
    canonicalContentTruncated: Boolean(workspace?.canonical?.contentTruncated),
    deleteDialogOpen,
    deleteConfirmation,
    setDeleteConfirmation,
    openDeleteDialog,
    closeDeleteDialog,
    previewDeleteAsset,
  };
}
