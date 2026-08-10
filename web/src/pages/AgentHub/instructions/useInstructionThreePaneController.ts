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
import type { InstructionLane } from '../context/agentHubContext';
import {
  addBlock,
  blocksFromOriginalContent,
  dtoToDraft,
  draftToDto,
  ensureModeBlock,
  findBlockByMode,
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
  previewOpen: boolean;
  plan: UserInstructionPlanDto | null;
  applyResult: UserInstructionApplyResultDto | null;
  reparseFromOriginal: () => void;
  requestSync: () => Promise<void>;
  applyPlan: () => Promise<void>;
  /** 保存块文档到 canonical head（独立于 CLI 写入门禁）。 */
  saveBlocks: () => Promise<void>;
  closePreview: () => void;
  refresh: () => Promise<void>;
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
  const [previewOpen, setPreviewOpen] = useState(false);
  const [plan, setPlan] = useState<UserInstructionPlanDto | null>(null);
  const [applyResult, setApplyResult] = useState<UserInstructionApplyResultDto | null>(null);

  const mountedRef = useRef(true);
  const loadSeqRef = useRef(0);
  /** context generation invalidates every inspect/save/preview/apply response. */
  const contextGenerationRef = useRef(0);
  const contextKeyRef = useRef(
    `${context.scope}\0${context.deviceId ?? ''}\0${context.projectKey ?? ''}\0${agent}`,
  );
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
        setWorkspace(next);
        const { path, text } = originalFromWorkspace(next, agent);
        const hydrated = next.canonical?.blocks?.map(dtoToDraft) ?? null;
        let nextState = initialThreePaneFromDisk(path, text, hydrated, agent);
        if (autoReparseAfterLoadRef.current && nextState.blocks.length === 0) {
          autoReparseAfterLoadRef.current = false;
          nextState = parseBlocksFromOriginal(nextState, agent);
        }
        if (!preserveDirty || !stateRef.current.blocksDirty && !stateRef.current.originalDirty) {
          setState(nextState);
        }
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
        // 同 context 刷新失败时保留 dirty 草稿；context 切换由显式确认先清理。
        if (!preserveDirty || (!stateRef.current.blocksDirty && !stateRef.current.originalDirty)) {
          setWorkspace(null);
          setState(initialThreePaneFromDisk(null, ''));
        }
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
    const nextContextKey =
      `${context.scope}\0${context.deviceId ?? ''}\0${context.projectKey ?? ''}\0${agent}`;
    const contextChanged = contextKeyRef.current !== nextContextKey;
    if (
      blockedContextKeyRef.current === nextContextKey &&
      (stateRef.current.blocksDirty || stateRef.current.originalDirty)
    ) {
      setError('AGENT_HUB_CONTEXT_CHANGE_HAS_UNSAVED_DRAFT');
      setLoading(false);
      setRefreshing(false);
      return;
    }
    if (contextChanged) {
      contextKeyRef.current = nextContextKey;
      contextGenerationRef.current += 1;
      // Invalidate an in-flight request before any new work is scheduled.
      loadSeqRef.current += 1;
      planRequestIdRef.current = null;
      planGenerationRef.current = null;
      autoReparseAfterLoadRef.current = false;
      setActionBusy(false);
      setPlan(null);
      setPreviewOpen(false);
      setApplyResult(null);
      setDualDirtyOpen(false);
      if (stateRef.current.blocksDirty || stateRef.current.originalDirty) {
        // Real shell context changes are guarded by AgentHub; direct callers must resolve
        // this explicit stop before a new context can replace the draft.
        blockedContextKeyRef.current = nextContextKey;
        setError('AGENT_HUB_CONTEXT_CHANGE_HAS_UNSAVED_DRAFT');
        setLoading(false);
        setRefreshing(false);
        return;
      }
      blockedContextKeyRef.current = null;
      setWorkspace(null);
      setState(initialThreePaneFromDisk(null, ''));
    }
    if (!enabled) {
      // 资产 tab：禁止 instruction inspect；不清草稿，loading=false
      // eslint-disable-next-line react-hooks/set-state-in-effect -- disable lane
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
    enabled,
    loadWorkspace,
    context.scope,
    context.deviceId,
    context.projectKey,
    agent,
  ]);

  const currentTarget = useMemo(
    () => workspace?.targets.find((item) => item.target === agent) ?? null,
    [workspace, agent],
  );

  /** 当前后端没有本机 projectRef 的三栏写入路径，禁止回落用户级命令。 */
  const projectScopeUnsupported = context.scope === 'project';

  const sourceContentTruncated = useMemo(() => {
    if (!workspace) return false;
    return originalFromWorkspace(workspace, agent).contentTruncated;
  }, [workspace, agent]);

  const writeBlocked = useMemo(() => {
    if (projectScopeUnsupported) return true;
    if (!workspace) return true;
    if (workspace.canonical?.contentTruncated || sourceContentTruncated) return true;
    if (!currentTarget) return true;
    return currentTarget.capability.write !== 'supported';
  }, [workspace, currentTarget, sourceContentTruncated, projectScopeUnsupported]);

  const writeBlockedReason = useMemo(() => {
    if (!writeBlocked) return null;
    if (projectScopeUnsupported) {
      return t('agentHub:instructions.threePane.projectScopeUnsupported');
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
    projectScopeUnsupported,
    t,
  ]);

  const reparseFromOriginal = useCallback(() => {
    setState((current) => parseBlocksFromOriginal(current, agent));
    setActionError(null);
  }, [agent]);

  const updateOriginal = useCallback((text: string) => {
    setState((current) => updateOriginalText(current, text));
    setActionError(null);
  }, []);

  const changeBlock = useCallback(
    (id: string, patch: Partial<Omit<InstructionBlockDraft, 'id'>>) => {
      setState((current) => updateBlock(current, id, patch, agent));
      setActionError(null);
    },
    [agent],
  );

  const appendBlock = useCallback(() => {
    setState((current) =>
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
  }, [agent]);

  /**
   * Business Logic: 壳层 lane 驱动的三槽编辑（公共 / 独有主路径）。
   * Code Logic: ensure mode 块 → 公共写 common；独有写 variant[agent]；
   *   适配 lane 兼容路径写 common（Claude 底稿），完整双列编辑见 editAdapted*。
   */
  const editCurrentSlot = useCallback(
    (text: string) => {
      const mode = laneToMode(context.instructionLane);
      setState((current) => {
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
    [agent, context.instructionLane],
  );

  /**
   * Business Logic: 适配槽公共底稿以 Claude Code 为权威。
   * Code Logic: ensure adapted 块 → 写 commonMarkdown；preview 仍按当前 agent 合成。
   */
  const editAdaptedCommon = useCallback(
    (text: string) => {
      setState((current) => {
        const next = ensureModeBlock(current, 'adapted', agent);
        const block = findBlockByMode(next.blocks, 'adapted');
        if (!block) return next;
        return updateBlock(next, block.id, { commonMarkdown: text }, agent);
      });
      setActionError(null);
    },
    [agent],
  );

  /**
   * Business Logic: 适配槽为非 Claude agent 写入变体。
   * Code Logic: ensure adapted → variants[agent]=text；空串保留键表示「显式空变体」。
   */
  const editAdaptedVariant = useCallback(
    (text: string) => {
      setState((current) => {
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
    [agent],
  );

  /**
   * Business Logic: 用已保存的最新 head 生成单 agent 投影 plan（写盘受门禁）。
   * Code Logic: 调用方先 saveBlocks 推进 head，传入 refreshed workspace；preview setup/update。
   */
  const runPreviewWithBaseline = useCallback(
    async (baseline: SyncBaseline, ws: UserInstructionWorkspaceDto) => {
      if (writeBlocked) {
        setActionError(writeBlockedReason);
        return;
      }
      const generation = contextGenerationRef.current;
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
          generation !== contextGenerationRef.current
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
        if (!mountedRef.current || generation !== contextGenerationRef.current) return;
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
        if (mountedRef.current && generation === contextGenerationRef.current) {
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
      if (projectScopeUnsupported) {
        setActionError(PROJECT_CONTEXT_UNAVAILABLE);
        return null;
      }
      if (!workspace) return null;
      // A refresh or sync with no edits must not advance an empty canonical head.
      if (!blocksOverride && !state.blocksDirty && !state.originalDirty) {
        return workspace;
      }
      const generation = contextGenerationRef.current;
      setActionBusy(true);
      setActionError(null);
      try {
        const normalized = normalizeInstructionBlocks(blocksOverride ?? state.blocks);
        await agentHubApi.saveUserInstructionBlocks({
          blocks: normalized.map(draftToDto),
          baseRevisionId: workspace.canonical?.headRevisionId ?? null,
          inventorySnapshotHash: workspace.inventorySnapshotHash,
          ...requestContext,
        });
        if (!mountedRef.current || generation !== contextGenerationRef.current) {
          return null;
        }
        const refreshed = await agentHubApi.inspectUserInstructionWorkspace(requestContext);
        if (!mountedRef.current || generation !== contextGenerationRef.current) {
          return null;
        }
        setWorkspace(refreshed);
        const { path, text } = originalFromWorkspace(refreshed, agent);
        const hydrated = refreshed.canonical?.blocks?.map(dtoToDraft) ?? null;
        setState(initialThreePaneFromDisk(path, text, hydrated, agent));
        return refreshed;
      } catch (reason) {
        if (!mountedRef.current || generation !== contextGenerationRef.current) return null;
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
        if (mountedRef.current && generation === contextGenerationRef.current) {
          setActionBusy(false);
        }
      }
    },
    [agent, projectScopeUnsupported, requestContext, state, t, workspace],
  );

  const requestSync = useCallback(async () => {
    if (projectScopeUnsupported) {
      setActionError(PROJECT_CONTEXT_UNAVAILABLE);
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
    const refreshed = await saveBlocks(originalBlocks);
    if (!refreshed) return;
    await runPreviewWithBaseline(resolved.baseline, refreshed);
  }, [agent, projectScopeUnsupported, runPreviewWithBaseline, saveBlocks, state, t]);

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
      void saveBlocks(originalBlocks).then((refreshed) => {
        if (refreshed) void runPreviewWithBaseline(baseline, refreshed);
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
    if (projectScopeUnsupported) {
      setActionError(PROJECT_CONTEXT_UNAVAILABLE);
      return;
    }
    if (!plan) return;
    const generation = contextGenerationRef.current;
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
      if (!mountedRef.current || generation !== contextGenerationRef.current) return;
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
        await loadWorkspace(true, { preserveDirty: false, generation });
        // load 已 hydrate 持久化 canonical 块（完整 variants）；baseline=blocks 时保留 hydrate，
        // 不再从原文 reparse（避免 adapted 块退化为 shared）。
      }
    } catch (reason) {
      if (!mountedRef.current || generation !== contextGenerationRef.current) return;
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
      if (mountedRef.current && generation === contextGenerationRef.current) {
        setActionBusy(false);
      }
    }
  }, [loadWorkspace, plan, projectScopeUnsupported, requestContext, t]);

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
    dirty,
    writeBlocked,
    writeBlockedReason,
    dualDirtyOpen,
    previewOpen,
    plan,
    applyResult,
    reparseFromOriginal,
    requestSync,
    applyPlan,
    saveBlocks: async () => {
      await saveBlocks();
    },
    closePreview,
    refresh,
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
