/**
 * 提示词三栏页面控制器。
 *
 * Business Logic（为什么需要）:
 *   按当前 agent×scope×device/project 加载原始文件到 ③，块/预览初始为空；
 *   「写入原始文件」= 三槽合成预览 → Agent 原生文件（CLAUDE.md / AGENTS.md 等）。
 *
 * Code Logic（做什么）:
 *   inspect workspace → initialThreePaneFromDisk；reparse/parseBlocksFromOriginal；
 *   requestSync 固定 blocks 基线：saveBlocks（若脏）→ preview plan → apply 写盘；
 *   dual-dirty 对话框的 original 基线仅 chooseBaseline 使用。hooks 全在 early return 前。
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
import {
  getAgentHubContextCapability,
  getAgentHubDraftIdentity,
  type AgentHubContext,
  type InstructionLane,
} from '../context/agentHubContext';
import {
  addBlock,
  appendAdaptedVariants,
  applyInstructionReviseResult,
  replaceAnalyzedParts,
  blocksFromOriginalContent,
  dtoToDraft,
  draftToDto,
  ensureModeBlock,
  findInstructionTextChangeRange,
  findBlockByMode,
  hydrateBlocksFromOriginal,
  initialThreePaneFromDisk,
  joinBlocksForTarget,
  normalizeInstructionBlocks,
  resolveInstructionSlotText,
  resolveAdaptedSlotText,
  updateBlock,
  updateOriginalText,
  type InstructionBlockDraft,
  type InstructionAiReviseFeedback,
  type InstructionBusyAction,
  type InstructionThreePaneState,
  type SyncBaseline,
} from './instructionThreePane';

export type { InstructionBusyAction } from './instructionThreePane';

/** peer 上下文稳定错误码（与 api 层常量同字面量；不 import 避免 mock 缺导出）。 */
const PEER_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE';
/** 本机 project scope 尚无三栏 V2 后端路径，必须 fail-closed。 */
const PROJECT_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE';
/** Canonical 在草稿 lease 期间变化；保存必须继续使用旧 base 并 fail-closed。 */
const CANONICAL_DRIFT = 'AGENT_HUB_CANONICAL_DRIFT';

interface InstructionDraftLease {
  contextKey: string;
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
  originalPath: string | null;
  originalText: string;
}

function instructionContextKey(context: AgentHubContext): string {
  const identity = getAgentHubDraftIdentity(context);
  return `${identity.scope}\0${identity.deviceId ?? ''}\0${identity.projectKey ?? ''}\0${identity.agent}`;
}

export interface UseInstructionThreePaneControllerArgs {
  context: AgentHubContext;
  t: TFunction<['agentHub', 'common']>;
  /**
   * Business Logic: 仅 instructions（或 adapt 需自拉）时为 true，资产 tab 禁止 inspect。
   * Code Logic: false 时不 loadWorkspace，loading=false，可 retain 已有草稿。
   */
  enabled?: boolean;
}

/** Controller 对 pure view 的返回合同。 */
export interface UseInstructionThreePaneControllerResult {
  state: InstructionThreePaneState;
  workspace: UserInstructionWorkspaceDto | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  /**
   * 任一动作或 refresh 进行中（互斥禁用）。
   * Business Logic: 禁止并发 save/analyze/sync；不等于某个具体按钮应转圈。
   */
  actionBusy: boolean;
  /**
   * 当前进行中的具体动作；view 用它把 spinner 挂到正确按钮。
   * Code Logic: refresh 不算具体动作（null），仅 actionBusy=true 禁用。
   */
  busyAction: InstructionBusyAction | null;
  /** 当前三栏是否存在未持久化草稿；用于上下文切换保护。 */
  dirty: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  /** 分析拆解将覆盖现有三槽时的显式确认（可选）。 */
  analyzeConfirmOpen: boolean;
  aiReviseOpen: boolean;
  aiReviseDirection: string;
  aiReviseError: string | null;
  aiReviseFeedback: InstructionAiReviseFeedback | null;
  aiReviseDisabled: boolean;
  openAiRevise: () => void;
  setAiReviseDirection: (value: string) => void;
  cancelAiRevise: () => void;
  confirmAiRevise: () => Promise<void>;
  previewOpen: boolean;
  plan: UserInstructionPlanDto | null;
  applyResult: UserInstructionApplyResultDto | null;
  /** 独有页：调用 Claude 把原始文件拆解并覆盖三槽。 */
  analyzeDecompose: () => void;
  confirmAnalyzeDecompose: () => void;
  cancelAnalyzeDecompose: () => void;
  /** 适配页：把当前 agent 适配正文改写并追加到其他 agent 适配槽。 */
  adaptToOtherAgents: () => Promise<void>;
  requestSync: () => Promise<void>;
  applyPlan: () => Promise<void>;
  /** 保存块文档到 canonical head（独立于 CLI 写入门禁）。 */
  saveBlocks: () => Promise<boolean>;
  closePreview: () => void;
  refresh: () => Promise<void>;
  /** 放弃当前草稿并重新读取 Canonical/原始来源；读取失败时保留旧草稿。 */
  discardAndReload: () => Promise<void>;
  updateOriginal: (text: string) => void;
  changeBlock: (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => void;
  appendBlock: () => void;
  /**
   * 按当前 instructionLane 编辑对应三槽正文。
   * 公共写 shared.common；适配写 adapted.variants[agent]；独有写 targetOnly.variants[agent]。
   */
  editCurrentSlot: (text: string) => void;
  chooseBaseline: (baseline: SyncBaseline) => void;
  cancelDualDirty: () => void;
  dismissApplyResult: () => void;
  /** 上下文切换前经用户确认后放弃当前草稿。 */
  discardDraftForContextChange: () => void;
}

function laneToMode(lane: InstructionLane): InstructionBlockDraft['mode'] {
  switch (lane) {
    case 'common':
      return 'shared';
    case 'adapted':
      return 'adapted';
    case 'exclusive':
      return 'targetOnly';
  }
}

/**
 * Business Logic: 从 workspace 抽出当前 agent 的生效路径与可展示/编辑正文。
 * Code Logic: path 优先 effective/active source，再 managedTargetPath；
 *   text 优先磁盘 source.content（原始栏真源），缺省时回退 canonical common+extension。
 */
export function originalFromWorkspace(
  workspace: UserInstructionWorkspaceDto,
  agent: AgentTarget,
): { path: string | null; text: string; contentTruncated: boolean } {
  const target = workspace.targets.find((item) => item.target === agent) ?? null;
  const effective =
    (target?.effectiveSourceId
      ? target.sources.find((source) => source.sourceId === target.effectiveSourceId)
      : null) ??
    target?.sources.find((source) => source.active) ??
    target?.sources.find((source) => source.exists && typeof source.content === 'string') ??
    null;
  const pathCandidate = effective?.path ?? target?.managedTargetPath ?? null;
  const path =
    pathCandidate && pathCandidate.trim().length > 0 ? pathCandidate : null;

  // 磁盘正文优先：打开即展示本机原始提示词，可直接编辑。
  if (typeof effective?.content === 'string') {
    return {
      path,
      text: effective.content,
      contentTruncated: Boolean(effective.contentTruncated),
    };
  }

  const common = workspace.canonical?.commonContent ?? '';
  const extension = workspace.canonical?.targetExtensions?.[agent] ?? '';
  let text = common;
  if (extension.trim().length > 0) {
    text =
      common.trim().length > 0
        ? `${common.replace(/\s+$/u, '')}\n\n${extension}`
        : extension;
  }
  return {
    path,
    text,
    contentTruncated: Boolean(workspace.canonical?.contentTruncated),
  };
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

const ALL_INSTRUCTION_TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];

/**
 * Business Logic: 按当前槽决定同步目标；公共槽同步全部 Agent，适配/独有槽同步当前 Agent；
 *   本机 external/unknown 源写回时必须 adoptExisting，否则 apply 被 OWNERSHIP_REQUIRED 挡住。
 * Code Logic: 目标逐个标为 managed，其它 target 保持 unmanaged。
 */
function buildInstructionPreviewRequest(
  workspace: UserInstructionWorkspaceDto,
  targets: AgentTarget[],
) {
  const selections = emptySelections();
  for (const agent of targets) {
    const target = workspace.targets.find((item) => item.target === agent) ?? null;
    const effective =
      (target?.effectiveSourceId
        ? target.sources.find((source) => source.sourceId === target.effectiveSourceId)
        : null) ??
      target?.sources.find((source) => source.active) ??
      null;
    const needsAdopt =
      effective != null &&
      effective.exists &&
      (effective.ownership === 'external' || effective.ownership === 'unknown');
    selections[agent] = needsAdopt
      ? {
          managementMode: 'managedActive',
          adoptExisting: true,
          manageOverride: false,
        }
      : 'managed';
  }
  return {
    // backend preview/apply 基于持久化 head InstructionDocument 投影（含 per-agent variants）；
    // 前端先 saveBlocks 推进 head，commonContent/targetExtensions 不再驱动投影。
    commonContent: '',
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
  /** 默认 true 兼容单测；页面入口必须显式传 instructionsLaneActive。 */
  const enabled = args.enabled !== false;
  const agent = context.agent;
  const instructionLane = context.instructionLane;
  const contextCapability = getAgentHubContextCapability(context);
  const contextUnavailableCode =
    contextCapability === 'remote'
      ? PEER_CONTEXT_UNAVAILABLE
      : PROJECT_CONTEXT_UNAVAILABLE;
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
  const [loading, setLoading] = useState(() => enabled);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<InstructionBusyAction | null>(null);
  const [dualDirtyOpen, setDualDirtyOpen] = useState(false);
  const [analyzeConfirmOpen, setAnalyzeConfirmOpen] = useState(false);
  const [aiReviseOpen, setAiReviseOpen] = useState(false);
  const [aiReviseDirection, setAiReviseDirection] = useState('');
  const [aiReviseError, setAiReviseError] = useState<string | null>(null);
  const [aiReviseFeedback, setAiReviseFeedback] =
    useState<InstructionAiReviseFeedback | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [plan, setPlan] = useState<UserInstructionPlanDto | null>(null);
  const [applyResult, setApplyResult] = useState<UserInstructionApplyResultDto | null>(null);

  const mountedRef = useRef(true);
  const loadSeqRef = useRef(0);
  /** context generation invalidates every inspect/save/preview/apply response. */
  const contextGenerationRef = useRef(0);
  const contextKeyRef = useRef(instructionContextKey(context));
  /** CAS lease 只在首次 hydrate、放弃草稿或成功保存后迁移。 */
  const draftLeaseRef = useRef<InstructionDraftLease | null>(null);
  /** inventory snapshot 可随成功 refresh 前进；Canonical base revision 仍冻结在 lease。 */
  const observedInventorySnapshotHashRef = useRef<string | null>(null);
  /** 用户编辑版本用于阻止旧 preview/apply 覆盖新输入，并让 save 保留期间产生的新草稿。 */
  const editVersionRef = useRef(0);
  /** 旧 operation 的 finally 不得解除新 operation 的 busy。 */
  const actionSeqRef = useRef(0);
  const blockedContextKeyRef = useRef<string | null>(null);
  const stateRef = useRef(state);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(
    null,
  );
  const planGenerationRef = useRef<number | null>(null);
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

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  const dirty = state.blocksDirty || state.originalDirty;

  /**
   * Business Logic: 用户确认放弃当前草稿后才能切换 scope/agent/project context。
   * Code Logic: 清空旧 workspace/state 并作废所有旧响应；新 context effect 随后重新 inspect。
   */
  const discardDraftForContextChange = useCallback(() => {
    const empty = initialThreePaneFromDisk(null, '');
    // Invalidate in-flight responses immediately; the context effect will advance once more
    // after the shell URL commits the new key.
    contextGenerationRef.current += 1;
    loadSeqRef.current += 1;
    actionSeqRef.current += 1;
    draftLeaseRef.current = null;
    observedInventorySnapshotHashRef.current = null;
    editVersionRef.current += 1;
    planRequestIdRef.current = null;
    planGenerationRef.current = null;
    autoReparseAfterLoadRef.current = false;
    blockedContextKeyRef.current = null;
    setBusyAction(null);
    stateRef.current = empty;
    setState(empty);
    setWorkspace(null);
    setPlan(null);
    setPreviewOpen(false);
    setApplyResult(null);
    setDualDirtyOpen(false);
    setAnalyzeConfirmOpen(false);
    setAiReviseOpen(false);
    setAiReviseDirection('');
    setAiReviseError(null);
    setAiReviseFeedback(null);
    setActionError(null);
    setError(null);
  }, []);

  /**
   * Business Logic: 从 inspect 填充 ③，块/预览保持空（除非 apply 后 auto re-parse）。
   * Code Logic: generation 防竞态；autoReparse 仅一次；peer 错误保留稳定 code。
   */
  const loadWorkspace = useCallback(
    async (
      isRefresh: boolean,
      options: { preserveDirty?: boolean; generation?: number } = {},
    ) => {
      const seq = ++loadSeqRef.current;
      const generation = options.generation ?? contextGenerationRef.current;
      const preserveDirty = options.preserveDirty ?? isRefresh;
      if (isRefresh) setRefreshing(true);
      else setLoading(true);
      setError(null);
      try {
        const next = await agentHubApi.inspectUserInstructionWorkspace(requestContext);
        if (
          !mountedRef.current ||
          seq !== loadSeqRef.current ||
          generation !== contextGenerationRef.current
        ) {
          return;
        }
        const { path, text } = originalFromWorkspace(next, agent);
        const hydrated = next.canonical?.blocks?.map(dtoToDraft) ?? null;
        let nextState = initialThreePaneFromDisk(path, text, hydrated, agent);
        if (autoReparseAfterLoadRef.current && nextState.blocks.length === 0) {
          autoReparseAfterLoadRef.current = false;
          nextState = hydrateBlocksFromOriginal(nextState, agent);
        }
        const current = stateRef.current;
        const hasDirtyDraft = current.blocksDirty || current.originalDirty;
        if (!preserveDirty || !hasDirtyDraft) {
          draftLeaseRef.current = {
            contextKey: contextKeyRef.current,
            baseRevisionId: next.canonical?.headRevisionId ?? null,
            inventorySnapshotHash: next.inventorySnapshotHash,
            originalPath: path,
            originalText: text,
          };
          observedInventorySnapshotHashRef.current = next.inventorySnapshotHash;
          stateRef.current = nextState;
          setState(nextState);
        } else {
          const lease = draftLeaseRef.current;
          const canonicalChanged =
            lease == null ||
            lease.contextKey !== contextKeyRef.current ||
            lease.baseRevisionId !== (next.canonical?.headRevisionId ?? null);
          const sourceChanged =
            lease == null || lease.originalPath !== path || lease.originalText !== text;
          const preserved = {
            ...current,
            externalDrift: current.externalDrift || canonicalChanged,
            sourceDrift: current.sourceDrift || sourceChanged,
          };
          stateRef.current = preserved;
          setState(preserved);
          observedInventorySnapshotHashRef.current = next.inventorySnapshotHash;
        }
        setWorkspace(next);
        setDualDirtyOpen(false);
      } catch (reason) {
        if (
          !mountedRef.current ||
          seq !== loadSeqRef.current ||
          generation !== contextGenerationRef.current
        ) {
          return;
        }
        const code = errorCode(reason);
        // 任何刷新/重载失败都保留现有内容；首次加载本来就是空态，无需再清空。
        setError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : reason instanceof Error
              ? reason.message
              : String(reason),
        );
      } finally {
        if (
          mountedRef.current &&
          seq === loadSeqRef.current &&
          generation === contextGenerationRef.current
        ) {
          setLoading(false);
          setRefreshing(false);
        }
      }
    },
    [agent, requestContext],
  );

  useEffect(() => {
    const nextContextKey = instructionContextKey(context);
    const contextChanged = contextKeyRef.current !== nextContextKey;
    const hasDirtyDraft = stateRef.current.blocksDirty || stateRef.current.originalDirty;
    if (contextChanged && hasDirtyDraft) {
      // 尚未确认的 context 只记录 pending key；旧 identity/lease 必须原封不动。
      blockedContextKeyRef.current = nextContextKey;
      setError('AGENT_HUB_CONTEXT_CHANGE_HAS_UNSAVED_DRAFT');
      setLoading(false);
      setRefreshing(false);
      return;
    }
    if (!contextChanged && blockedContextKeyRef.current !== null) {
      blockedContextKeyRef.current = null;
      setError(null);
    }
    if (contextChanged) {
      contextKeyRef.current = nextContextKey;
      contextGenerationRef.current += 1;
      // Invalidate an in-flight request before any new work is scheduled.
      loadSeqRef.current += 1;
      actionSeqRef.current += 1;
      draftLeaseRef.current = null;
      observedInventorySnapshotHashRef.current = null;
      planRequestIdRef.current = null;
      planGenerationRef.current = null;
      autoReparseAfterLoadRef.current = false;
      setBusyAction(null);
      setPlan(null);
      setPreviewOpen(false);
      setApplyResult(null);
      setDualDirtyOpen(false);
      setAnalyzeConfirmOpen(false);
      setAiReviseOpen(false);
      setAiReviseDirection('');
      setAiReviseError(null);
      setAiReviseFeedback(null);
      blockedContextKeyRef.current = null;
      const empty = initialThreePaneFromDisk(null, '');
      stateRef.current = empty;
      setWorkspace(null);
      setState(empty);
    }
    if (!enabled) {
      // 资产 tab：禁止 instruction inspect；不清草稿，loading=false
      // eslint-disable-next-line react-hooks/set-state-in-effect -- disable lane
      setLoading(false);
      setRefreshing(false);
      return;
    }
    if (contextCapability !== 'direct') {
      setError(contextUnavailableCode);
      setLoading(false);
      setRefreshing(false);
      return;
    }
    const generation = contextGenerationRef.current;
    const timeoutId = window.setTimeout(() => {
      void loadWorkspace(false, { preserveDirty: true, generation });
    }, 0);
    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [
    context,
    enabled,
    loadWorkspace,
    contextCapability,
    contextUnavailableCode,
  ]);

  const currentTarget = useMemo(
    () => workspace?.targets.find((item) => item.target === agent) ?? null,
    [workspace, agent],
  );

  /** 三栏直读/保存只允许本机 user；peer 必须经 Pull，project 仍未安全绑定。 */
  const directContextUnsupported = contextCapability !== 'direct';

  const syncAgents = useMemo(
    () => instructionLane === 'common'
      ? ALL_INSTRUCTION_TARGETS.filter((targetAgent) =>
          workspace?.targets.some(
            (target) =>
              target.target === targetAgent && target.capability.write === 'supported',
          ),
        )
      : [agent],
    [agent, instructionLane, workspace],
  );

  const sourceContentTruncated = useMemo(() => {
    if (!workspace) return false;
    return originalFromWorkspace(workspace, agent).contentTruncated;
  }, [workspace, agent]);

  const writeBlocked = useMemo(() => {
    if (directContextUnsupported) return true;
    if (!workspace) return true;
    if (state.externalDrift || state.sourceDrift) return true;
    if (workspace.canonical?.contentTruncated || sourceContentTruncated) return true;
    if (instructionLane === 'common') return syncAgents.length === 0;
    return !currentTarget || currentTarget.capability.write !== 'supported';
  }, [
    workspace,
    currentTarget,
    syncAgents,
    instructionLane,
    sourceContentTruncated,
    directContextUnsupported,
    state.externalDrift,
    state.sourceDrift,
  ]);

  const writeBlockedReason = useMemo(() => {
    if (!writeBlocked) return null;
    if (directContextUnsupported) {
      return t('agentHub:instructions.threePane.directContextUnsupported');
    }
    if (state.sourceDrift) {
      return t('agentHub:instructions.threePane.sourceDrift');
    }
    if (state.externalDrift) {
      return t('agentHub:instructions.threePane.canonicalDrift');
    }
    if (workspace?.canonical?.contentTruncated || sourceContentTruncated) {
      return t('agentHub:userInstructions.errors.contentTruncated');
    }
    if (currentTarget?.capability.write !== 'supported') {
      return t(
        instructionLane === 'common'
          ? 'agentHub:instructions.threePane.writeBlockedTargets'
          : 'agentHub:instructions.threePane.writeBlocked',
      );
    }
    return t(
      instructionLane === 'common'
        ? 'agentHub:instructions.threePane.writeBlockedTargets'
        : 'agentHub:instructions.threePane.writeBlocked',
    );
  }, [
    writeBlocked,
    workspace,
    currentTarget,
    sourceContentTruncated,
    directContextUnsupported,
    state.externalDrift,
    state.sourceDrift,
    instructionLane,
    t,
  ]);

  const aiReviseDisabled = useMemo(
    () =>
      directContextUnsupported ||
      state.externalDrift ||
      Boolean(workspace?.canonical?.contentTruncated) ||
      sourceContentTruncated,
    [
      directContextUnsupported,
      sourceContentTruncated,
      state.externalDrift,
      workspace?.canonical?.contentTruncated,
    ],
  );

  /**
   * Business Logic: 任一用户草稿变化都会使既有预览失效，但不能取消已开始的 Canonical Save。
   * Code Logic: 推进 edit version、清 plan/result，并同步维护 stateRef。
   */
  const updateDraft = useCallback(
    (updater: (current: InstructionThreePaneState) => InstructionThreePaneState) => {
      editVersionRef.current += 1;
      planRequestIdRef.current = null;
      planGenerationRef.current = null;
      setPlan(null);
      setPreviewOpen(false);
      setApplyResult(null);
      setAiReviseFeedback(null);
      setState((current) => {
        const next = updater(current);
        stateRef.current = next;
        return next;
      });
    },
    [],
  );

  /**
   * Business Logic: 独有页分析拆解 — 有未保存三槽时先确认，再调 Claude 拆解并覆盖三槽。
   * Code Logic: 空原文拒绝；busy 时 short-circuit。
   */
  const runAnalyzeDecompose = useCallback(async () => {
    const current = stateRef.current;
    const original = current.originalText.trim();
    if (!original) {
      setActionError(t('agentHub:instructions.threePane.errors.emptyOriginalAnalyze'));
      return;
    }
    if (busyAction !== null) return;
    const generation = contextGenerationRef.current;
    const actionSeq = ++actionSeqRef.current;
    setBusyAction('analyze');
    setActionError(null);
    setAnalyzeConfirmOpen(false);
    try {
      const parts = await agentHubApi.analyzeInstructionOriginal({
        originalMarkdown: current.originalText,
        agent,
        ...requestContext,
      });
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      updateDraft((draft) => replaceAnalyzedParts(draft, parts, agent));
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (mountedRef.current && actionSeq === actionSeqRef.current) {
        setBusyAction(null);
      }
    }
  }, [busyAction, agent, requestContext, t, updateDraft]);

  const analyzeDecompose = useCallback(() => {
    if (stateRef.current.blocksDirty) {
      setAnalyzeConfirmOpen(true);
      return;
    }
    void runAnalyzeDecompose();
  }, [runAnalyzeDecompose]);

  const confirmAnalyzeDecompose = useCallback(() => {
    void runAnalyzeDecompose();
  }, [runAnalyzeDecompose]);

  const cancelAnalyzeDecompose = useCallback(() => {
    setAnalyzeConfirmOpen(false);
  }, []);

  /**
   * Business Logic: 适配页 — 把当前 agent 适配正文改写并追加到所有其他 agent 适配槽。
   * Code Logic: 读当前 adapted 槽 → Claude adapt → appendAdaptedVariants。
   */
  const adaptToOtherAgents = useCallback(async () => {
    const current = stateRef.current;
    const adaptedBlock = findBlockByMode(current.blocks, 'adapted');
    const sourceText = resolveAdaptedSlotText(adaptedBlock, agent).trim();
    if (!sourceText) {
      setActionError(t('agentHub:instructions.threePane.errors.emptyAdaptedAdapt'));
      return;
    }
    if (busyAction !== null) return;
    const generation = contextGenerationRef.current;
    const actionSeq = ++actionSeqRef.current;
    setBusyAction('adapt');
    setActionError(null);
    try {
      const result = await agentHubApi.adaptInstructionToOtherAgents({
        sourceAgent: agent,
        adaptedMarkdown: sourceText,
        ...requestContext,
      });
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      updateDraft((draft) => appendAdaptedVariants(draft, result.variants, agent));
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      setActionError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (mountedRef.current && actionSeq === actionSeqRef.current) {
        setBusyAction(null);
      }
    }
  }, [busyAction, agent, requestContext, t, updateDraft]);

  const updateOriginal = useCallback((text: string) => {
    updateDraft((current) => updateOriginalText(current, text));
    setActionError(null);
  }, [updateDraft]);

  const changeBlock = useCallback(
    (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => {
      updateDraft((current) => updateBlock(current, id, patch, agent));
      setActionError(null);
    },
    [agent, updateDraft],
  );

  const appendBlock = useCallback(() => {
    updateDraft((current) =>
      addBlock(
        current,
        {
          id: `block-${Date.now()}`,
          // 兼容路径：新块默认只属于当前 agent
          mode: 'targetOnly',
          commonMarkdown: '',
          variants: { [agent]: '' },
          headingPath: [],
          sourceTarget: agent,
          needsAdaptation: false,
        },
        agent,
      ),
    );
    setActionError(null);
  }, [agent, updateDraft]);

  /**
   * Business Logic: 壳层 lane 驱动的三槽编辑。
   * Code Logic: ensure mode → 公共写 common；适配/独有写 variants[agent]。
   */
  const editCurrentSlot = useCallback(
    (text: string) => {
      const mode = laneToMode(context.instructionLane);
      updateDraft((current) => {
        const next = ensureModeBlock(current, mode, agent);
        const block = findBlockByMode(next.blocks, mode);
        if (!block) return next;
        if (mode === 'shared') {
          return updateBlock(next, block.id, { commonMarkdown: text }, agent);
        }
        return updateBlock(
          next,
          block.id,
          {
            variants: { ...block.variants, [agent]: text },
            sourceTarget: mode === 'targetOnly' ? (block.sourceTarget ?? agent) : block.sourceTarget,
          },
          agent,
        );
      });
      setActionError(null);
    },
    [agent, context.instructionLane, updateDraft],
  );

  /**
   * Business Logic: 用已保存的最新 head 生成一次性写入 plan（写盘受门禁）。
   * Code Logic: 调用方先 saveBlocks 推进 head，传入 refreshed workspace；preview setup/update
   *   仅作为 expected-hash/CAS 的内部准备步骤，不再打开重复预览 Dialog。
   */
  const runPreviewWithBaseline = useCallback(
    async (baseline: SyncBaseline, ws: UserInstructionWorkspaceDto) => {
      if (stateRef.current.externalDrift || stateRef.current.sourceDrift) {
        setActionError(t('agentHub:userInstructions.errors.previewStale'));
        return;
      }
      if (writeBlocked) {
        setActionError(writeBlockedReason);
        return;
      }
      const generation = contextGenerationRef.current;
      const actionSeq = ++actionSeqRef.current;
      const editVersion = editVersionRef.current;
      setBusyAction('sync');
      setActionError(null);
      setApplyResult(null);
      lastSyncBaselineRef.current = baseline;
      try {
        const request = {
          ...buildInstructionPreviewRequest(ws, syncAgents),
          ...requestContext,
        };
        const target = ws.targets.find((item) => item.target === agent) ?? null;
        const targetManaged =
          target?.managementMode === 'managedActive' ||
          target?.managementMode === 'managedPaused';
        const nextPlan =
          ws.setupState === 'configured' || targetManaged
            ? await agentHubApi.previewUserInstructionUpdate(request)
            : await agentHubApi.previewUserInstructionSetup(request);
        if (
          !mountedRef.current ||
          generation !== contextGenerationRef.current ||
          actionSeq !== actionSeqRef.current ||
          editVersion !== editVersionRef.current
        ) {
          return;
        }
        setPlan(nextPlan);
        planGenerationRef.current = generation;
        planRequestIdRef.current = {
          planToken: nextPlan.planToken,
          clientRequestId: createClientRequestId(),
        };
        setPreviewOpen(false);
        setDualDirtyOpen(false);
        return nextPlan;
      } catch (reason) {
        if (
          !mountedRef.current ||
          generation !== contextGenerationRef.current ||
          actionSeq !== actionSeqRef.current ||
          editVersion !== editVersionRef.current
        ) {
          return;
        }
        const code = errorCode(reason);
        setActionError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === PROJECT_CONTEXT_UNAVAILABLE
              ? PROJECT_CONTEXT_UNAVAILABLE
              : code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
                ? t('agentHub:userInstructions.errors.backendUnavailable')
                : reason instanceof Error
                  ? reason.message
                  : String(reason),
        );
        return null;
      } finally {
        if (
          mountedRef.current &&
          generation === contextGenerationRef.current &&
          actionSeq === actionSeqRef.current
        ) {
          setBusyAction(null);
        }
      }
    },
    [agent, requestContext, syncAgents, t, writeBlocked, writeBlockedReason],
  );

  /**
   * Business Logic: 保存块文档到 canonical head（cc-partner 内部编辑态，独立于 CLI 写入门禁）。
   * Code Logic: saveUserInstructionBlocks(baseRevisionId CAS) → rescan 拿新 head/snapshot + hydrate；
   *   返回新 workspace 供后续 preview/apply 复用最新基线。
   */
  const saveBlocks = useCallback(
    async (blocksOverride?: InstructionBlockDraft[]): Promise<UserInstructionWorkspaceDto | null> => {
      if (directContextUnsupported) {
        setActionError(contextUnavailableCode);
        return null;
      }
      if (!workspace) return null;
      const currentState = stateRef.current;
      // 普通 Save 只消费 blocksDirty。Original-only 永远 no-op，也不得清原文草稿。
      if (!blocksOverride && !currentState.blocksDirty) {
        return workspace;
      }
      if (currentState.externalDrift) {
        setActionError(CANONICAL_DRIFT);
        return null;
      }
      const lease = draftLeaseRef.current;
      if (lease == null || lease.contextKey !== contextKeyRef.current) {
        setActionError(CANONICAL_DRIFT);
        return null;
      }
      const generation = contextGenerationRef.current;
      const actionSeq = ++actionSeqRef.current;
      const editVersionAtStart = editVersionRef.current;
      setBusyAction('save');
      setActionError(null);
      try {
        const normalized = normalizeInstructionBlocks(blocksOverride ?? currentState.blocks);
        await agentHubApi.saveUserInstructionBlocks({
          blocks: normalized.map(draftToDto),
          baseRevisionId: lease.baseRevisionId,
          inventorySnapshotHash:
            observedInventorySnapshotHashRef.current ?? lease.inventorySnapshotHash,
          ...requestContext,
        });
        if (
          !mountedRef.current ||
          generation !== contextGenerationRef.current ||
          actionSeq !== actionSeqRef.current
        ) {
          return null;
        }
        const refreshed = await agentHubApi.inspectUserInstructionWorkspace(requestContext);
        if (
          !mountedRef.current ||
          generation !== contextGenerationRef.current ||
          actionSeq !== actionSeqRef.current
        ) {
          return null;
        }
        setWorkspace(refreshed);
        const { path, text } = originalFromWorkspace(refreshed, agent);
        const hydrated = refreshed.canonical?.blocks?.map(dtoToDraft) ?? null;
        const clean = initialThreePaneFromDisk(path, text, hydrated, agent);
        const liveState = stateRef.current;
        const savedWithoutConcurrentEdit = editVersionAtStart === editVersionRef.current;
        const sourceChangedDuringSave =
          lease.originalPath !== path || lease.originalText !== text;
        const nextSourceDrift = liveState.sourceDrift || sourceChangedDuringSave;
        const nextState = savedWithoutConcurrentEdit
          ? {
              ...clean,
              // Save 只消费 blocks；Original 的内容、dirty 与 drift 独立保留。
              originalPath: liveState.originalPath,
              originalText: liveState.originalText,
              originalDirty: liveState.originalDirty,
              sourceDrift: nextSourceDrift,
              externalDrift: false,
            }
          : {
              ...liveState,
              // 后端已保存 action 起点的 blocks；期间产生的新编辑仍是未保存草稿。
              blocksDirty: true,
              sourceDrift: nextSourceDrift,
              externalDrift: false,
            };
        draftLeaseRef.current = {
          contextKey: contextKeyRef.current,
          baseRevisionId: refreshed.canonical?.headRevisionId ?? null,
          inventorySnapshotHash: refreshed.inventorySnapshotHash,
          // source drift 是独立 latch；在显式 Discard/Reload 前不能把外部新内容
          // 偷偷提升为下一次 preview/apply 的可信基线。
          originalPath: nextSourceDrift ? lease.originalPath : path,
          originalText: nextSourceDrift ? lease.originalText : text,
        };
        observedInventorySnapshotHashRef.current = refreshed.inventorySnapshotHash;
        stateRef.current = nextState;
        setState(nextState);
        return refreshed;
      } catch (reason) {
        if (
          !mountedRef.current ||
          generation !== contextGenerationRef.current ||
          actionSeq !== actionSeqRef.current
        ) {
          return null;
        }
        const code = errorCode(reason);
        if (code === 'USER_INSTRUCTION_REVISION_CHANGED') {
          const drifted = { ...stateRef.current, externalDrift: true };
          stateRef.current = drifted;
          setState(drifted);
        }
        setActionError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === PROJECT_CONTEXT_UNAVAILABLE
              ? PROJECT_CONTEXT_UNAVAILABLE
              : code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
                ? t('agentHub:userInstructions.errors.backendUnavailable')
                : reason instanceof Error
                  ? reason.message
                  : String(reason),
        );
        return null;
      } finally {
        if (
          mountedRef.current &&
          generation === contextGenerationRef.current &&
          actionSeq === actionSeqRef.current
        ) {
          setBusyAction(null);
        }
      }
    },
    [
      agent,
      contextUnavailableCode,
      directContextUnsupported,
      requestContext,
      t,
      workspace,
    ],
  );

  const openAiRevise = useCallback(() => {
    if (busyAction !== null || aiReviseDisabled) return;
    setAiReviseError(null);
    setAiReviseFeedback(null);
    setAiReviseDirection('');
    setAiReviseOpen(true);
  }, [aiReviseDisabled, busyAction]);

  const cancelAiRevise = useCallback(() => {
    if (busyAction === 'revise') return;
    setAiReviseOpen(false);
    setAiReviseError(null);
  }, [busyAction]);

  /**
   * Business Logic: 按当前 lane 调用 Claude 改槽，成功后保存 Canonical。
   * Code Logic: revise 占用 busy；成功 updateDraft 后让 saveBlocks 接管 actionSeq。
   */
  const confirmAiRevise = useCallback(async () => {
    const direction = aiReviseDirection.trim();
    if (!direction) {
      setAiReviseError(t('agentHub:instructions.threePane.errors.emptyReviseDirection'));
      return;
    }
    if (busyAction !== null || aiReviseDisabled) return;
    const current = stateRef.current;
    const lane = instructionLane;
    const previousSlotText = resolveInstructionSlotText(current, lane, agent);
    const generation = contextGenerationRef.current;
    const actionSeq = ++actionSeqRef.current;
    setBusyAction('revise');
    setAiReviseError(null);
    setActionError(null);
    try {
      const shared = findBlockByMode(current.blocks, 'shared');
      const adapted = findBlockByMode(current.blocks, 'adapted');
      const exclusive = findBlockByMode(current.blocks, 'targetOnly');
      const result = await agentHubApi.reviseInstructionSlot({
        lane,
        agent,
        direction,
        commonMarkdown: shared?.commonMarkdown ?? '',
        exclusiveMarkdown: exclusive?.variants[agent] ?? '',
        adaptedVariants: {
          claude: resolveAdaptedSlotText(adapted, 'claude'),
          codex: resolveAdaptedSlotText(adapted, 'codex'),
          opencode: resolveAdaptedSlotText(adapted, 'opencode'),
        },
        ...requestContext,
      });
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      const next = applyInstructionReviseResult(stateRef.current, lane, agent, result);
      const nextSlotText = resolveInstructionSlotText(next, lane, agent);
      const selection = findInstructionTextChangeRange(previousSlotText, nextSlotText);
      const otherAdaptedSlotsChanged =
        lane === 'adapted' &&
        ALL_INSTRUCTION_TARGETS.some(
          (target) =>
            target !== agent &&
            resolveInstructionSlotText(current, lane, target) !==
              resolveInstructionSlotText(next, lane, target),
        );
      updateDraft(() => next);
      setAiReviseOpen(false);
      setAiReviseDirection('');
      const saved = await saveBlocks(next.blocks);
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current
      ) {
        return;
      }
      if (!saved) {
        setActionError(t('agentHub:instructions.threePane.errors.reviseSaveFailed'));
      } else {
        setAiReviseFeedback({
          currentSlotChanged: selection !== null,
          otherAdaptedSlotsChanged,
          selection,
        });
      }
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current
      ) {
        return;
      }
      setAiReviseError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (mountedRef.current && actionSeq === actionSeqRef.current) {
        setBusyAction(null);
      }
    }
  }, [
    agent,
    aiReviseDirection,
    aiReviseDisabled,
    busyAction,
    instructionLane,
    requestContext,
    saveBlocks,
    t,
    updateDraft,
  ]);

  const cancelDualDirty = useCallback(() => {
    setDualDirtyOpen(false);
  }, []);

  const closePreview = useCallback(() => {
    if (busyAction !== null) return;
    setPreviewOpen(false);
  }, [busyAction]);

  const applyPlan = useCallback(async (preparedPlan?: UserInstructionPlanDto) => {
    if (directContextUnsupported) {
      setActionError(contextUnavailableCode);
      return;
    }
    const selectedPlan = preparedPlan ?? plan;
    if (!selectedPlan) return;
    if (stateRef.current.externalDrift || stateRef.current.sourceDrift) {
      setPlan(null);
      setPreviewOpen(false);
      planRequestIdRef.current = null;
      planGenerationRef.current = null;
      setActionError(t('agentHub:userInstructions.errors.previewStale'));
      return;
    }
    const generation = contextGenerationRef.current;
    const actionSeq = ++actionSeqRef.current;
    const editVersion = editVersionRef.current;
    if (planGenerationRef.current !== null && planGenerationRef.current !== generation) {
      setPlan(null);
      setPreviewOpen(false);
      setActionError(t('agentHub:userInstructions.errors.previewStale'));
      return;
    }
    const existing = planRequestIdRef.current;
    const base =
      existing?.planToken === selectedPlan.planToken
        ? existing
        : { planToken: selectedPlan.planToken, clientRequestId: createClientRequestId() };
    planRequestIdRef.current = base;
    const request = { ...base, ...requestContext };
    setBusyAction('sync');
    setActionError(null);
    try {
      const result = await agentHubApi.applyUserInstructionPlan(request);
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current ||
        editVersion !== editVersionRef.current
      ) {
        return;
      }
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
        await loadWorkspace(true, { preserveDirty: true, generation });
        // load 已 hydrate 持久化 canonical 块（完整 variants）；baseline=blocks 时保留 hydrate，
        // 不再从原文 reparse（避免 adapted 块退化为 shared）。
      }
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== contextGenerationRef.current ||
        actionSeq !== actionSeqRef.current ||
        editVersion !== editVersionRef.current
      ) {
        return;
      }
      const code = errorCode(reason);
      if (
        code === 'USER_INSTRUCTION_PREVIEW_STALE' ||
        code === 'USER_INSTRUCTION_SOURCE_CHANGED' ||
        code === 'USER_INSTRUCTION_REVISION_CHANGED'
      ) {
        setPreviewOpen(false);
        setPlan(null);
        planGenerationRef.current = null;
        setActionError(t('agentHub:userInstructions.errors.previewStale'));
      } else {
        setActionError(
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === PROJECT_CONTEXT_UNAVAILABLE
              ? PROJECT_CONTEXT_UNAVAILABLE
            : code === 'USER_INSTRUCTION_V2_BACKEND_UNAVAILABLE'
              ? t('agentHub:userInstructions.errors.backendUnavailable')
              : reason instanceof Error
                ? reason.message
                : String(reason),
        );
      }
    } finally {
      if (
        mountedRef.current &&
        generation === contextGenerationRef.current &&
        actionSeq === actionSeqRef.current
      ) {
        setBusyAction(null);
      }
    }
  }, [
    contextUnavailableCode,
    directContextUnsupported,
    loadWorkspace,
    plan,
    requestContext,
    t,
  ]);

  /**
   * Business Logic: 把「合成预览」写入当前 Agent 原生文件（如 CLAUDE.md / AGENTS.md）。
   * Code Logic: 固定 blocks 基线——禁止 original 基线反写 Canonical；
   *   有未保存三槽先 saveBlocks 推进 head，再 preview+apply 投影到原生路径。
   *   本地三槽与 preview 皆空时拒绝（避免误把磁盘原文当写入源）。
   */
  const requestSync = useCallback(async () => {
    if (directContextUnsupported) {
      setActionError(contextUnavailableCode);
      return;
    }
    const composed =
      state.previewText.trim().length > 0
        ? state.previewText
        : joinBlocksForTarget(state.blocks, agent);
    if (composed.trim().length === 0 && state.blocks.length === 0) {
      setActionError(t('agentHub:instructions.threePane.errors.emptySync'));
      return;
    }
    // 先保存 canonical head（若已 clean 则 no-op 复用当前 workspace），
    // 再生成绑定 hash/revision 的一次性 plan 并立即原子应用（投影 head → 原生文件）。
    const editVersionBeforeSave = editVersionRef.current;
    const refreshed = await saveBlocks();
    if (!refreshed || editVersionBeforeSave !== editVersionRef.current) return;
    const preparedPlan = await runPreviewWithBaseline('blocks', refreshed);
    if (!preparedPlan || editVersionBeforeSave !== editVersionRef.current) return;
    await applyPlan(preparedPlan);
  }, [
    agent,
    applyPlan,
    contextUnavailableCode,
    directContextUnsupported,
    runPreviewWithBaseline,
    saveBlocks,
    state.blocks,
    state.previewText,
    t,
  ]);

  const chooseBaseline = useCallback(
    (baseline: SyncBaseline) => {
      const content =
        baseline === 'blocks'
          ? state.previewText || joinBlocksForTarget(state.blocks, agent)
          : state.originalText;
      if (!content.trim()) {
        setActionError(t('agentHub:instructions.threePane.errors.emptySync'));
        setDualDirtyOpen(false);
        return;
      }
      const originalBlocks =
        baseline === 'original' ? blocksFromOriginalContent(content) : undefined;
      const editVersionBeforeSave = editVersionRef.current;
      setDualDirtyOpen(false);
      void saveBlocks(originalBlocks).then(async (refreshed) => {
        if (!refreshed || editVersionBeforeSave !== editVersionRef.current) return;
        const preparedPlan = await runPreviewWithBaseline(baseline, refreshed);
        if (!preparedPlan || editVersionBeforeSave !== editVersionRef.current) return;
        await applyPlan(preparedPlan);
      });
    },
    [agent, applyPlan, runPreviewWithBaseline, saveBlocks, state, t],
  );

  const refresh = useCallback(async () => {
    autoReparseAfterLoadRef.current = false;
    actionSeqRef.current += 1;
    planRequestIdRef.current = null;
    planGenerationRef.current = null;
    setBusyAction(null);
    setPlan(null);
    setPreviewOpen(false);
    setApplyResult(null);
    await loadWorkspace(true);
  }, [loadWorkspace]);

  const discardAndReload = useCallback(async () => {
    autoReparseAfterLoadRef.current = false;
    actionSeqRef.current += 1;
    planRequestIdRef.current = null;
    planGenerationRef.current = null;
    setBusyAction(null);
    setPlan(null);
    setPreviewOpen(false);
    setApplyResult(null);
    setDualDirtyOpen(false);
    setAnalyzeConfirmOpen(false);
    setAiReviseOpen(false);
    setAiReviseDirection('');
    setAiReviseError(null);
    // preserveDirty=false 只在成功读取后替换；loadWorkspace 的失败分支保留旧草稿。
    await loadWorkspace(true, { preserveDirty: false });
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
    actionBusy: busyAction !== null || refreshing,
    busyAction,
    dirty,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    analyzeConfirmOpen,
    aiReviseOpen,
    aiReviseDirection,
    aiReviseError,
    aiReviseFeedback,
    aiReviseDisabled,
    openAiRevise,
    setAiReviseDirection,
    cancelAiRevise,
    confirmAiRevise,
    previewOpen,
    plan,
    applyResult,
    analyzeDecompose,
    confirmAnalyzeDecompose,
    cancelAnalyzeDecompose,
    adaptToOtherAgents,
    requestSync,
    applyPlan,
    saveBlocks: async () => {
      if (!stateRef.current.blocksDirty) return false;
      return Boolean(await saveBlocks());
    },
    closePreview,
    refresh,
    discardAndReload,
    updateOriginal,
    changeBlock,
    appendBlock,
    editCurrentSlot,
    chooseBaseline,
    cancelDualDirty,
    dismissApplyResult,
    discardDraftForContextChange,
  };
}
