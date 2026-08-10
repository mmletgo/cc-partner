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
import {
  getAgentHubContextCapability,
  getAgentHubDraftIdentity,
  type AgentHubContext,
  type InstructionLane,
} from '../context/agentHubContext';
import {
  addBlock,
  blocksFromOriginalContent,
  dtoToDraft,
  draftToDto,
  ensureModeBlock,
  findBlockByMode,
  hydrateBlocksFromOriginal,
  initialThreePaneFromDisk,
  joinBlocksForTarget,
  normalizeInstructionBlocks,
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

/** Controller 对 pure view / 预览 Dialog 的返回合同。 */
export interface UseInstructionThreePaneControllerResult {
  state: InstructionThreePaneState;
  workspace: UserInstructionWorkspaceDto | null;
  loading: boolean;
  refreshing: boolean;
  error: string | null;
  actionError: string | null;
  actionBusy: boolean;
  /** 当前三栏是否存在未持久化草稿；用于上下文切换保护。 */
  dirty: boolean;
  writeBlocked: boolean;
  writeBlockedReason: string | null;
  dualDirtyOpen: boolean;
  /** 重新解析将替换现有三槽草稿时的显式确认。 */
  reparseConfirmOpen: boolean;
  previewOpen: boolean;
  plan: UserInstructionPlanDto | null;
  applyResult: UserInstructionApplyResultDto | null;
  reparseFromOriginal: () => void;
  confirmReparseFromOriginal: () => void;
  cancelReparseFromOriginal: () => void;
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
  /** 按当前 instructionLane 编辑对应三槽正文（公共/独有；适配请用专用 API）。 */
  editCurrentSlot: (text: string) => void;
  /**
   * 适配槽：编辑 Claude 公共底稿（adapted.commonMarkdown）。
   * 与 agent 选择无关；权威为 Claude Code。
   */
  editAdaptedCommon: (text: string) => void;
  /**
   * 适配槽：编辑当前 agent 变体（adapted.variants[agent]）。
   * agent=claude 时不应调用（视图隐藏变体列）。
   */
  editAdaptedVariant: (text: string) => void;
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

/** 比较原始正文与 canonical 投影时忽略换行格式与尾随空白差异。 */
function normalizeInstructionContentForComparison(text: string): string {
  return text.replace(/\r\n/g, '\n').trimEnd();
}

function emptySelections(): Record<AgentTarget, UserInstructionTargetSelection> {
  return { claude: 'unmanaged', codex: 'unmanaged', opencode: 'unmanaged' };
}

/**
 * Business Logic: 单 agent 同步 — 只把 context.agent 标为 managed；
 *   本机 external/unknown 源写回时必须 adoptExisting，否则 apply 被 OWNERSHIP_REQUIRED 挡住。
 * Code Logic: 其它 target 一律 unmanaged，commonContent = 同步正文。
 */
function buildSingleAgentPreviewRequest(
  workspace: UserInstructionWorkspaceDto,
  agent: AgentTarget,
) {
  const selections = emptySelections();
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
  const contextCapability = getAgentHubContextCapability(context);
  const contextUnavailableCode =
    contextCapability === 'pullOnly'
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
  const [actionBusy, setActionBusy] = useState(false);
  const [dualDirtyOpen, setDualDirtyOpen] = useState(false);
  const [reparseConfirmOpen, setReparseConfirmOpen] = useState(false);
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
    setActionBusy(false);
    stateRef.current = empty;
    setState(empty);
    setWorkspace(null);
    setPlan(null);
    setPreviewOpen(false);
    setApplyResult(null);
    setDualDirtyOpen(false);
    setReparseConfirmOpen(false);
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
      setActionBusy(false);
      setPlan(null);
      setPreviewOpen(false);
      setApplyResult(null);
      setDualDirtyOpen(false);
      setReparseConfirmOpen(false);
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

  const sourceContentTruncated = useMemo(() => {
    if (!workspace) return false;
    return originalFromWorkspace(workspace, agent).contentTruncated;
  }, [workspace, agent]);

  const writeBlocked = useMemo(() => {
    if (directContextUnsupported) return true;
    if (!workspace) return true;
    if (state.externalDrift || state.sourceDrift) return true;
    if (workspace.canonical?.contentTruncated || sourceContentTruncated) return true;
    if (!currentTarget) return true;
    return currentTarget.capability.write !== 'supported';
  }, [
    workspace,
    currentTarget,
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
      return t('agentHub:instructions.threePane.writeBlocked');
    }
    return t('agentHub:instructions.threePane.writeBlocked');
  }, [
    writeBlocked,
    workspace,
    currentTarget,
    sourceContentTruncated,
    directContextUnsupported,
    state.externalDrift,
    state.sourceDrift,
    t,
  ]);

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
      setState((current) => {
        const next = updater(current);
        stateRef.current = next;
        return next;
      });
    },
    [],
  );

  const reparseFromOriginal = useCallback(() => {
    if (stateRef.current.blocksDirty) {
      setReparseConfirmOpen(true);
      return;
    }
    updateDraft((current) => parseBlocksFromOriginal(current, agent));
    setActionError(null);
  }, [agent, updateDraft]);

  const confirmReparseFromOriginal = useCallback(() => {
    updateDraft((current) => parseBlocksFromOriginal(current, agent));
    setReparseConfirmOpen(false);
    setActionError(null);
  }, [agent, updateDraft]);

  const cancelReparseFromOriginal = useCallback(() => {
    setReparseConfirmOpen(false);
  }, []);

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
   * Business Logic: 壳层 lane 驱动的三槽编辑（公共 / 独有主路径）。
   * Code Logic: ensure mode 块 → 公共写 common；独有写 variant[agent]；
   *   适配 lane 兼容路径写 common（Claude 底稿），完整双列编辑见 editAdapted*。
   */
  const editCurrentSlot = useCallback(
    (text: string) => {
      const mode = laneToMode(context.instructionLane);
      updateDraft((current) => {
        const next = ensureModeBlock(current, mode, agent);
        const block = findBlockByMode(next.blocks, mode);
        if (!block) return next;
        if (mode === 'shared' || mode === 'adapted') {
          // 适配兼容路径：写 commonMarkdown（Claude 公共底稿）
          return updateBlock(next, block.id, { commonMarkdown: text }, agent);
        }
        return updateBlock(
          next,
          block.id,
          {
            variants: { ...block.variants, [agent]: text },
            sourceTarget: block.sourceTarget ?? agent,
          },
          agent,
        );
      });
      setActionError(null);
    },
    [agent, context.instructionLane, updateDraft],
  );

  /**
   * Business Logic: 适配槽公共底稿以 Claude Code 为权威。
   * Code Logic: ensure adapted 块 → 写 commonMarkdown；preview 仍按当前 agent 合成。
   */
  const editAdaptedCommon = useCallback(
    (text: string) => {
      updateDraft((current) => {
        const next = ensureModeBlock(current, 'adapted', agent);
        const block = findBlockByMode(next.blocks, 'adapted');
        if (!block) return next;
        return updateBlock(next, block.id, { commonMarkdown: text }, agent);
      });
      setActionError(null);
    },
    [agent, updateDraft],
  );

  /**
   * Business Logic: 适配槽为非 Claude agent 写入变体。
   * Code Logic: ensure adapted → variants[agent]=text；空串保留键表示「显式空变体」。
   */
  const editAdaptedVariant = useCallback(
    (text: string) => {
      updateDraft((current) => {
        const next = ensureModeBlock(current, 'adapted', agent);
        const block = findBlockByMode(next.blocks, 'adapted');
        if (!block) return next;
        return updateBlock(
          next,
          block.id,
          {
            variants: { ...block.variants, [agent]: text },
          },
          agent,
        );
      });
      setActionError(null);
    },
    [agent, updateDraft],
  );

  /**
   * Business Logic: 用已保存的最新 head 生成单 agent 投影 plan（写盘受门禁）。
   * Code Logic: 调用方先 saveBlocks 推进 head，传入 refreshed workspace；preview setup/update。
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
      setActionBusy(true);
      setActionError(null);
      setApplyResult(null);
      lastSyncBaselineRef.current = baseline;
      try {
        const request = {
          ...buildSingleAgentPreviewRequest(ws, agent),
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
        setPreviewOpen(true);
        setDualDirtyOpen(false);
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
      } finally {
        if (
          mountedRef.current &&
          generation === contextGenerationRef.current &&
          actionSeq === actionSeqRef.current
        ) {
          setActionBusy(false);
        }
      }
    },
    [agent, requestContext, t, writeBlocked, writeBlockedReason],
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
      setActionBusy(true);
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
          setActionBusy(false);
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

  const requestSync = useCallback(async () => {
    if (directContextUnsupported) {
      setActionError(contextUnavailableCode);
      return;
    }
    const resolved = resolveSyncContent(state, agent);
    if (!resolved.ok) {
      if (resolved.reason === 'dual_dirty_conflict') {
        setDualDirtyOpen(true);
        setActionError(null);
        return;
      }
      setActionError(t('agentHub:instructions.threePane.errors.emptySync'));
      return;
    }
    // original 基线必须整篇变成唯一 shared canonical block；不能只把正文留在 preview。
    const canonicalContent = joinBlocksForTarget(state.blocks, agent);
    const originalNeedsCanonicalSave =
      resolved.baseline === 'original' &&
      (state.originalDirty ||
        state.blocks.length === 0 ||
        normalizeInstructionContentForComparison(canonicalContent) !==
          normalizeInstructionContentForComparison(resolved.content));
    const originalBlocks = originalNeedsCanonicalSave
      ? blocksFromOriginalContent(resolved.content)
      : undefined;
    // 先保存块到 canonical head（投影数据源），再用新 head preview/apply。
    // 已有 canonical blocks 且无 dirty 时直接复用 head，避免无条件推进空 head。
    const editVersionBeforeSave = editVersionRef.current;
    const refreshed = await saveBlocks(originalBlocks);
    if (!refreshed || editVersionBeforeSave !== editVersionRef.current) return;
    await runPreviewWithBaseline(resolved.baseline, refreshed);
  }, [
    agent,
    contextUnavailableCode,
    directContextUnsupported,
    runPreviewWithBaseline,
    saveBlocks,
    state,
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
      // dual-dirty 选基线后，同样先 saveBlocks 再 preview
      const editVersionBeforeSave = editVersionRef.current;
      setDualDirtyOpen(false);
      void saveBlocks(originalBlocks).then((refreshed) => {
        if (refreshed && editVersionBeforeSave === editVersionRef.current) {
          void runPreviewWithBaseline(baseline, refreshed);
        }
      });
    },
    [agent, runPreviewWithBaseline, saveBlocks, state, t],
  );

  const cancelDualDirty = useCallback(() => {
    setDualDirtyOpen(false);
  }, []);

  const closePreview = useCallback(() => {
    if (actionBusy) return;
    setPreviewOpen(false);
  }, [actionBusy]);

  const applyPlan = useCallback(async () => {
    if (directContextUnsupported) {
      setActionError(contextUnavailableCode);
      return;
    }
    if (!plan) return;
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
      existing?.planToken === plan.planToken
        ? existing
        : { planToken: plan.planToken, clientRequestId: createClientRequestId() };
    planRequestIdRef.current = base;
    const request = { ...base, ...requestContext };
    setActionBusy(true);
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
        setActionBusy(false);
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

  const refresh = useCallback(async () => {
    autoReparseAfterLoadRef.current = false;
    actionSeqRef.current += 1;
    planRequestIdRef.current = null;
    planGenerationRef.current = null;
    setActionBusy(false);
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
    setActionBusy(false);
    setPlan(null);
    setPreviewOpen(false);
    setApplyResult(null);
    setDualDirtyOpen(false);
    setReparseConfirmOpen(false);
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
    actionBusy: actionBusy || refreshing,
    dirty,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    reparseConfirmOpen,
    previewOpen,
    plan,
    applyResult,
    reparseFromOriginal,
    confirmReparseFromOriginal,
    cancelReparseFromOriginal,
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
    editAdaptedCommon,
    editAdaptedVariant,
    chooseBaseline,
    cancelDualDirty,
    dismissApplyResult,
    discardDraftForContextChange,
  };
}
