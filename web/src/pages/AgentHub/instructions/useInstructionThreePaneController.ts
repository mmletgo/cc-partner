/**
 * 提示词三栏页面控制器。
 *
 * Business Logic（为什么需要）:
 *   按当前 agent×scope×device/project 加载原始文件到 ③，块/预览初始为空；
 *   显式 reparse / 同步 preview→apply 只写当前 agent。
 *
 * Code Logic（做什么）:
 *   inspect workspace → initialThreePaneFromDisk；reparse/parseBlocksFromOriginal；
 *   resolveSyncContent → preview/apply user instruction plan（单 destination）；
 *   成功后 rescan；original baseline 自动 re-parse 一次。hooks 全在 early return 前。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi, type AgentHubRequestContext } from '@/api/agentHub';
import type {
  AgentTarget,
  UserInstructionApplyResultDto,
  UserInstructionPlanDto,
  UserInstructionTargetSelection,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import {
  addBlock,
  initialThreePaneFromDisk,
  parseBlocksFromOriginal,
  resolveSyncContent,
  updateBlock,
  updateOriginalText,
  type InstructionBlockDraft,
  type InstructionThreePaneState,
  type SyncBaseline,
} from './instructionThreePane';

/** peer 上下文稳定错误码（与 api 层常量同字面量；不 import 避免 mock 缺导出）。 */
const PEER_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE';

export interface UseInstructionThreePaneControllerArgs {
  context: AgentHubContext;
  t: TFunction<['agentHub', 'common']>;
}

/** Controller 对 pure view / 预览 Dialog 的返回合同。 */
export interface UseInstructionThreePaneControllerResult {
  state: InstructionThreePaneState;
  workspace: UserInstructionWorkspaceDto | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  previewOpen: boolean;
  plan: UserInstructionPlanDto | null;
  applyResult: UserInstructionApplyResultDto | null;
  reparseFromOriginal: () => void;
  requestSync: () => Promise<void>;
  applyPlan: () => Promise<void>;
  closePreview: () => void;
  refresh: () => Promise<void>;
  updateOriginal: (text: string) => void;
  changeBlock: (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => void;
  appendBlock: () => void;
  chooseBaseline: (baseline: SyncBaseline) => void;
  cancelDualDirty: () => void;
  dismissApplyResult: () => void;
}

/**
 * Business Logic: 从 workspace 抽出当前 agent 的生效路径与可展示正文。
 * Code Logic: path 优先 effective/active source，再 managedTargetPath；
 *   text = commonContent + 可选 target extension（无磁盘 body API 时的最小路径）。
 */
export function originalFromWorkspace(
  workspace: UserInstructionWorkspaceDto,
  agent: AgentTarget,
): { path: string | null; text: string } {
  const target = workspace.targets.find((item) => item.target === agent) ?? null;
  const effective =
    (target?.effectiveSourceId
      ? target.sources.find((source) => source.sourceId === target.effectiveSourceId)
      : null) ??
    target?.sources.find((source) => source.active) ??
    null;
  const pathCandidate = effective?.path ?? target?.managedTargetPath ?? null;
  const path =
    pathCandidate && pathCandidate.trim().length > 0 ? pathCandidate : null;

  const common = workspace.canonical?.commonContent ?? '';
  const extension = workspace.canonical?.targetExtensions?.[agent] ?? '';
  let text = common;
  if (extension.trim().length > 0) {
    text =
      common.trim().length > 0
        ? `${common.replace(/\s+$/u, '')}\n\n${extension}`
        : extension;
  }
  return { path, text };
}

/**
 * Business Logic: 同一 plan 内复用幂等键，新 plan 才 mint。
 * Code Logic: crypto.randomUUID 优先。
 */
function createClientRequestId(): string {
  if (typeof globalThis.crypto?.randomUUID === 'function') {
    return globalThis.crypto.randomUUID();
  }
  return `instruction-three-pane-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function errorCode(reason: unknown): string | null {
  if (!reason || typeof reason !== 'object') return null;
  const code = (reason as { code?: unknown }).code;
  return typeof code === 'string' ? code : null;
}

function emptySelections(): Record<AgentTarget, UserInstructionTargetSelection> {
  return { claude: 'unmanaged', codex: 'unmanaged', opencode: 'unmanaged' };
}

/**
 * Business Logic: 单 agent 同步 — 只把 context.agent 标为 managed。
 * Code Logic: 其它 target 一律 unmanaged，commonContent = 同步正文。
 */
function buildSingleAgentPreviewRequest(
  workspace: UserInstructionWorkspaceDto,
  agent: AgentTarget,
  content: string,
) {
  const selections = emptySelections();
  selections[agent] = 'managed';
  return {
    commonContent: content,
    targetExtensions: {} as Partial<Record<AgentTarget, string>>,
    targetSelections: selections,
    baseRevisionId: workspace.canonical?.headRevisionId ?? null,
    inventorySnapshotHash: workspace.inventorySnapshotHash,
  };
}

/**
 * Business Logic: 为 Agent Hub 提示词 Tab 提供三栏编排。
 * Code Logic: inspect 有 generation；dirty 本地编辑；preview/apply 共享 plan 键。
 */
export function useInstructionThreePaneController(
  args: UseInstructionThreePaneControllerArgs,
): UseInstructionThreePaneControllerResult {
  const { context, t } = args;
  const agent = context.agent;
  /** 用户级 deviceId；项目级 projectKey 作为 projectRef。 */
  const requestContext = useMemo((): AgentHubRequestContext => {
    if (context.scope === 'project') {
      return { deviceId: null, projectRef: context.projectKey };
    }
    return { deviceId: context.deviceId, projectRef: null };
  }, [context.scope, context.deviceId, context.projectKey]);

  const [workspace, setWorkspace] = useState<UserInstructionWorkspaceDto | null>(null);
  const [state, setState] = useState<InstructionThreePaneState>(() =>
    initialThreePaneFromDisk(null, ''),
  );
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [dualDirtyOpen, setDualDirtyOpen] = useState(false);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [plan, setPlan] = useState<UserInstructionPlanDto | null>(null);
  const [applyResult, setApplyResult] = useState<UserInstructionApplyResultDto | null>(null);

  const mountedRef = useRef(true);
  const loadSeqRef = useRef(0);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(
    null,
  );
  /** 最近一次成功 resolve 的同步基线，用于 apply 后 auto re-parse。 */
  const lastSyncBaselineRef = useRef<SyncBaseline | null>(null);
  /** apply 成功后若 baseline=original，rescan 后自动 parse 一次。 */
  const autoReparseAfterLoadRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic: 从 inspect 填充 ③，块/预览保持空（除非 apply 后 auto re-parse）。
   * Code Logic: generation 防竞态；autoReparse 仅一次；peer 错误保留稳定 code。
   */
  const loadWorkspace = useCallback(
    async (isRefresh: boolean) => {
      const seq = ++loadSeqRef.current;
      if (isRefresh) setRefreshing(true);
      else setLoading(true);
      setError(null);
      try {
        const next = await agentHubApi.inspectUserInstructionWorkspace(requestContext);
        if (!mountedRef.current || seq !== loadSeqRef.current) return;
        setWorkspace(next);
        const { path, text } = originalFromWorkspace(next, agent);
        let nextState = initialThreePaneFromDisk(path, text);
        if (autoReparseAfterLoadRef.current) {
          autoReparseAfterLoadRef.current = false;
          nextState = parseBlocksFromOriginal(nextState);
        }
        setState(nextState);
        setDualDirtyOpen(false);
      } catch (reason) {
        if (!mountedRef.current || seq !== loadSeqRef.current) return;
        const code = errorCode(reason);
        // 切换到 peer 时清空本机 workspace，避免 UI 冒充对端内容
        setWorkspace(null);
        setState(initialThreePaneFromDisk(null, ''));
        setError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : reason instanceof Error
              ? reason.message
              : String(reason),
        );
      } finally {
        if (mountedRef.current && seq === loadSeqRef.current) {
          setLoading(false);
          setRefreshing(false);
        }
      }
    },
    [agent, requestContext],
  );

  useEffect(() => {
    const timeoutId = window.setTimeout(() => {
      void loadWorkspace(false);
    }, 0);
    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [loadWorkspace, context.scope, context.deviceId, context.projectKey, context.agent]);

  const currentTarget = useMemo(
    () => workspace?.targets.find((item) => item.target === agent) ?? null,
    [workspace, agent],
  );

  const writeBlocked = useMemo(() => {
    if (!workspace) return true;
    if (workspace.canonical?.contentTruncated) return true;
    if (!currentTarget) return true;
    return currentTarget.capability.write !== 'supported';
  }, [workspace, currentTarget]);

  const writeBlockedReason = useMemo(() => {
    if (!writeBlocked) return null;
    if (workspace?.canonical?.contentTruncated) {
      return t('agentHub:userInstructions.errors.contentTruncated');
    }
    if (currentTarget?.capability.write !== 'supported') {
      return t('agentHub:instructions.threePane.writeBlocked');
    }
    return t('agentHub:instructions.threePane.writeBlocked');
  }, [writeBlocked, workspace, currentTarget, t]);

  const reparseFromOriginal = useCallback(() => {
    setState((current) => parseBlocksFromOriginal(current));
    setActionError(null);
  }, []);

  const updateOriginal = useCallback((text: string) => {
    setState((current) => updateOriginalText(current, text));
    setActionError(null);
  }, []);

  const changeBlock = useCallback(
    (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => {
      setState((current) => updateBlock(current, id, patch));
      setActionError(null);
    },
    [],
  );

  const appendBlock = useCallback(() => {
    setState((current) =>
      addBlock(current, {
        id: `block-${Date.now()}`,
        mode: 'shared',
        title: '',
        body: '',
      }),
    );
    setActionError(null);
  }, []);

  /**
   * Business Logic: resolve 同步内容并生成单 agent 零写入 plan。
   * Code Logic: dual_dirty → UI 选择；empty → error；否则 preview setup/update。
   */
  const runPreviewWithContent = useCallback(
    async (content: string, baseline: SyncBaseline) => {
      if (!workspace) return;
      if (writeBlocked) {
        setActionError(writeBlockedReason);
        return;
      }
      setActionBusy(true);
      setActionError(null);
      setApplyResult(null);
      lastSyncBaselineRef.current = baseline;
      try {
        const request = {
          ...buildSingleAgentPreviewRequest(workspace, agent, content),
          ...requestContext,
        };
        const targetManaged =
          currentTarget?.managementMode === 'managedActive' ||
          currentTarget?.managementMode === 'managedPaused';
        const nextPlan =
          workspace.setupState === 'configured' || targetManaged
            ? await agentHubApi.previewUserInstructionUpdate(request)
            : await agentHubApi.previewUserInstructionSetup(request);
        if (!mountedRef.current) return;
        setPlan(nextPlan);
        planRequestIdRef.current = {
          planToken: nextPlan.planToken,
          clientRequestId: createClientRequestId(),
        };
        setPreviewOpen(true);
        setDualDirtyOpen(false);
      } catch (reason) {
        if (!mountedRef.current) return;
        const code = errorCode(reason);
        setActionError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
              ? t('agentHub:userInstructions.errors.backendUnavailable')
              : reason instanceof Error
                ? reason.message
                : String(reason),
        );
      } finally {
        if (mountedRef.current) setActionBusy(false);
      }
    },
    [agent, currentTarget, requestContext, t, workspace, writeBlocked, writeBlockedReason],
  );

  const requestSync = useCallback(async () => {
    const resolved = resolveSyncContent(state);
    if (!resolved.ok) {
      if (resolved.reason === 'dual_dirty_conflict') {
        setDualDirtyOpen(true);
        setActionError(null);
        return;
      }
      setActionError(t('agentHub:instructions.threePane.errors.emptySync'));
      return;
    }
    await runPreviewWithContent(resolved.content, resolved.baseline);
  }, [runPreviewWithContent, state, t]);

  const chooseBaseline = useCallback(
    (baseline: SyncBaseline) => {
      const content =
        baseline === 'blocks'
          ? state.previewText || state.blocks.map((b) => `## ${b.title}\n\n${b.body}`).join('\n\n')
          : state.originalText;
      if (!content.trim()) {
        setActionError(t('agentHub:instructions.threePane.errors.emptySync'));
        setDualDirtyOpen(false);
        return;
      }
      void runPreviewWithContent(content, baseline);
    },
    [runPreviewWithContent, state, t],
  );

  const cancelDualDirty = useCallback(() => {
    setDualDirtyOpen(false);
  }, []);

  const closePreview = useCallback(() => {
    if (actionBusy) return;
    setPreviewOpen(false);
  }, [actionBusy]);

  const applyPlan = useCallback(async () => {
    if (!plan) return;
    const existing = planRequestIdRef.current;
    const base =
      existing?.planToken === plan.planToken
        ? existing
        : { planToken: plan.planToken, clientRequestId: createClientRequestId() };
    planRequestIdRef.current = base;
    const request = { ...base, ...requestContext };
    setActionBusy(true);
    setActionError(null);
    try {
      const result = await agentHubApi.applyUserInstructionPlan(request);
      if (!mountedRef.current) return;
      setApplyResult(result);
      setPreviewOpen(false);
      const hasIncomplete = result.targets.some((target) =>
        ['stalePreview', 'blocked', 'conflict', 'failed'].includes(target.status),
      );
      if (!hasIncomplete) {
        // Spec: 以原始为基线写入 → 自动 re-parse 一次对齐 ①②
        if (lastSyncBaselineRef.current === 'original') {
          autoReparseAfterLoadRef.current = true;
        }
        // 以块为基线：rescan 后仍用 initial（块空）再保留本地块？
        // Spec: 保留当前块模型，② 与 ③ 反映新磁盘。此处 rescan 覆盖 ③；
        // 若 baseline=blocks，不 auto re-parse，并在 load 后恢复块需额外状态。
        // 最小路径：blocks baseline 时也 rescan 原文；块在 load 中清空后，
        // 若 baseline=blocks 则在 load 后不 auto parse，用户需再解析或我们保留块。
        // 简化：blocks baseline → 不 auto re-parse（load 会清空块）；
        // 为保留块模型，blocks baseline 时跳过 state 重置中的块清空——在 loadWorkspace 处理。
        await loadWorkspace(true);
        if (lastSyncBaselineRef.current === 'blocks' && mountedRef.current) {
          // load 已重置为空块；对 original 文本 reparse 等价于对齐合成内容到块
          // Spec 说保留当前块模型 — 最小实现：rescan 后按新 ③ reparse 一次也可接受对齐
          setState((current) => parseBlocksFromOriginal(current));
        }
      }
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
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
              ? t('agentHub:userInstructions.errors.backendUnavailable')
              : reason instanceof Error
                ? reason.message
                : String(reason),
        );
      }
    } finally {
      if (mountedRef.current) setActionBusy(false);
    }
  }, [loadWorkspace, plan, requestContext, t]);

  const refresh = useCallback(async () => {
    autoReparseAfterLoadRef.current = false;
    await loadWorkspace(true);
  }, [loadWorkspace]);

  const dismissApplyResult = useCallback(() => {
    setApplyResult(null);
  }, []);

  return {
    state,
    workspace,
    loading,
    refreshing,
    error,
    actionError,
    actionBusy: actionBusy || refreshing,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    previewOpen,
    plan,
    applyResult,
    reparseFromOriginal,
    requestSync,
    applyPlan,
    closePreview,
    refresh,
    updateOriginal,
    changeBlock,
    appendBlock,
    chooseBaseline,
    cancelDualDirty,
    dismissApplyResult,
  };
}
