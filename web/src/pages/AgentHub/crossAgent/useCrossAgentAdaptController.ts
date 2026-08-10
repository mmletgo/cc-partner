/**
 * Cross-agent adapt page controller (selective + full-volume).
 *
 * Business Logic（为什么需要）:
 *   独立全页编排：源=当前 agent；selective 多选目标指令适配；full 单目标五类清单
 *   强制 preview 后 apply；peer 设备上下文整页 blocked（同机 only）。
 *
 * Code Logic（做什么）:
 *   mode 切换清理 plan/preview；selective 走 preview/applyCrossAgentInstruction；
 *   full 走 preview/applyCrossAgentFull + 项 include 勾选。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi } from '@/api/agentHub';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import { originalFromWorkspace } from '../instructions';
import {
  canRunCrossAgentApply,
  canRunCrossAgentFullApply,
  canRunCrossAgentFullPreview,
  canRunCrossAgentPreview,
  countApplicableDestinations,
  countApplicableFullItems,
  defaultDestinationsForSource,
  defaultFullDestination,
  destinationCandidates,
  isPeerContextBlocked,
  parseCrossAgentApplyResults,
  parseCrossAgentFullApplyResults,
  parseCrossAgentFullPlan,
  parseCrossAgentPreview,
  sanitizeDestinations,
  toggleDestinationSelection,
  toggleFullPlanItemIncluded,
  type CrossAgentAdaptVolumeMode,
  type CrossAgentApplyResult,
  type CrossAgentFullApplyItemResult,
  type CrossAgentFullPlan,
  type CrossAgentPreviewReport,
} from './crossAgentPresentation';

export interface UseCrossAgentAdaptControllerArgs {
  context: AgentHubContext;
  t: TFunction<['agentHub', 'common']>;
  /**
   * 可选：三栏 original/preview 正文；缺省时 inspect workspace 加载。
   * 非空时优先使用（用户在进入页面前已编辑的内容）。
   */
  initialSourceMarkdown?: string | null;
}

export interface UseCrossAgentAdaptControllerResult {
  mode: CrossAgentAdaptVolumeMode;
  setMode: (mode: CrossAgentAdaptVolumeMode) => void;
  source: AgentTarget;
  destinations: AgentTarget[];
  destinationOptions: AgentTarget[];
  /** full 模式单目标 */
  fullDestination: AgentTarget | null;
  setFullDestination: (target: AgentTarget) => void;
  scope: AgentHubContext['scope'];
  projectKey: string | null;
  scopeConfirmed: boolean;
  setScopeConfirmed: (value: boolean) => void;
  sourceMarkdown: string;
  setSourceMarkdown: (value: string) => void;
  contentLoading: boolean;
  contentError: string | null;
  peerBlocked: boolean;
  busy: boolean;
  error: string | null;
  preview: CrossAgentPreviewReport | null;
  applyResults: CrossAgentApplyResult[] | null;
  fullPlan: CrossAgentFullPlan | null;
  fullApplyResults: CrossAgentFullApplyItemResult[] | null;
  applicableCount: number;
  canPreview: boolean;
  canApply: boolean;
  previewBlockedReason: string | null;
  applyBlockedReason: string | null;
  /** project scope 但未选项目时提示启用/选择。 */
  projectOptInNeeded: boolean;
  toggleDestination: (target: AgentTarget) => void;
  toggleFullItemIncluded: (logicalKey: string) => void;
  runPreview: () => Promise<void>;
  runApply: () => Promise<void>;
  refreshSourceContent: () => Promise<void>;
  clearPreview: () => void;
}

/**
 * Business Logic: apply 幂等键；同一 preview 会话内复用直至新 preview。
 * Code Logic: crypto.randomUUID 优先。
 */
function createClientRequestId(): string {
  if (typeof globalThis.crypto?.randomUUID === 'function') {
    return globalThis.crypto.randomUUID();
  }
  return `cross-agent-adapt-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function formatError(reason: unknown): string {
  if (!reason) return 'unknown_error';
  if (reason instanceof Error) {
    const code = (reason as { code?: unknown }).code;
    if (typeof code === 'string' && code.length > 0) {
      return `${code}: ${reason.message}`;
    }
    return reason.message || 'unknown_error';
  }
  return String(reason);
}

/**
 * Business Logic: 为 Agent Hub 适配全页提供 selective + full 编排状态机。
 * Code Logic: source 跟随 context.agent；peer blocked 时禁止 preview/apply。
 */
export function useCrossAgentAdaptController(
  args: UseCrossAgentAdaptControllerArgs,
): UseCrossAgentAdaptControllerResult {
  const { context, t, initialSourceMarkdown } = args;
  const source = context.agent;
  const peerBlocked = isPeerContextBlocked(context.deviceId);

  const [mode, setModeState] = useState<CrossAgentAdaptVolumeMode>('selective');
  const [destinations, setDestinations] = useState<AgentTarget[]>(() =>
    defaultDestinationsForSource(source),
  );
  const [fullDestination, setFullDestinationState] = useState<AgentTarget | null>(() =>
    defaultFullDestination(source),
  );
  const [scopeConfirmed, setScopeConfirmedState] = useState(false);
  const [sourceMarkdown, setSourceMarkdownState] = useState(() =>
    (initialSourceMarkdown ?? '').trim().length > 0
      ? String(initialSourceMarkdown)
      : '',
  );
  const [contentLoading, setContentLoading] = useState(false);
  const [contentError, setContentError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<CrossAgentPreviewReport | null>(null);
  const [applyResults, setApplyResults] = useState<CrossAgentApplyResult[] | null>(null);
  const [fullPlan, setFullPlan] = useState<CrossAgentFullPlan | null>(null);
  const [fullApplyResults, setFullApplyResults] = useState<
    CrossAgentFullApplyItemResult[] | null
  >(null);

  const mountedRef = useRef(true);
  const contentSeqRef = useRef(0);
  const previewSeqRef = useRef(0);
  const applySeqRef = useRef(0);
  const clientRequestIdRef = useRef<string | null>(null);
  const initialMarkdownAppliedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // 源 agent 变化：剔除非法 destination 并作废 preview
  useEffect(() => {
    setDestinations((prev) => sanitizeDestinations(source, prev));
    setFullDestinationState(defaultFullDestination(source));
    setPreview(null);
    setApplyResults(null);
    setFullPlan(null);
    setFullApplyResults(null);
    clientRequestIdRef.current = null;
  }, [source]);

  // initialSourceMarkdown 仅在首次有值时注入（避免父组件重渲覆盖用户编辑）
  useEffect(() => {
    if (initialMarkdownAppliedRef.current) return;
    const initial = (initialSourceMarkdown ?? '').trim();
    if (initial.length === 0) return;
    initialMarkdownAppliedRef.current = true;
    setSourceMarkdownState(initialSourceMarkdown ?? '');
  }, [initialSourceMarkdown]);

  /**
   * Business Logic: 无父级正文时从 inspect workspace 拉当前 agent 指令 markdown。
   * Code Logic: sequence guard；peer 时不调用本机 inspect。
   */
  const refreshSourceContent = useCallback(async () => {
    if (peerBlocked) {
      setContentError(t('agentHub:crossAgent.errors.peerBlocked'));
      return;
    }
    const seq = ++contentSeqRef.current;
    setContentLoading(true);
    setContentError(null);
    try {
      const workspace = await agentHubApi.inspectUserInstructionWorkspace();
      if (!mountedRef.current || seq !== contentSeqRef.current) return;
      const { text } = originalFromWorkspace(workspace, source);
      setSourceMarkdownState(text);
      setPreview(null);
      setApplyResults(null);
      setFullPlan(null);
      setFullApplyResults(null);
      clientRequestIdRef.current = null;
    } catch (reason) {
      if (!mountedRef.current || seq !== contentSeqRef.current) return;
      setContentError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === contentSeqRef.current) {
        setContentLoading(false);
      }
    }
  }, [peerBlocked, source, t]);

  // 进入页：无 initial 正文则自动 inspect
  useEffect(() => {
    if (peerBlocked) return;
    if ((initialSourceMarkdown ?? '').trim().length > 0) return;
    if (sourceMarkdown.trim().length > 0) return;
    const timeoutId = window.setTimeout(() => {
      void refreshSourceContent();
    }, 0);
    return () => {
      window.clearTimeout(timeoutId);
    };
    // 仅挂载/源切换时尝试加载；不把 sourceMarkdown 放 deps 避免循环
    // eslint-disable-next-line react-hooks/exhaustive-deps -- intentional mount/source load
  }, [peerBlocked, source, refreshSourceContent]);

  const invalidatePlans = useCallback(() => {
    setPreview(null);
    setApplyResults(null);
    setFullPlan(null);
    setFullApplyResults(null);
    clientRequestIdRef.current = null;
  }, []);

  const setMode = useCallback(
    (next: CrossAgentAdaptVolumeMode) => {
      setModeState(next);
      invalidatePlans();
      setError(null);
    },
    [invalidatePlans],
  );

  const setScopeConfirmed = useCallback(
    (value: boolean) => {
      setScopeConfirmedState(value);
      invalidatePlans();
    },
    [invalidatePlans],
  );

  const setSourceMarkdown = useCallback(
    (value: string) => {
      setSourceMarkdownState(value);
      invalidatePlans();
    },
    [invalidatePlans],
  );

  const setFullDestination = useCallback(
    (target: AgentTarget) => {
      if (target === source) return;
      setFullDestinationState(target);
      invalidatePlans();
    },
    [invalidatePlans, source],
  );

  const toggleDestination = useCallback(
    (target: AgentTarget) => {
      if (target === source) return;
      setDestinations((prev) => toggleDestinationSelection(source, prev, target));
      invalidatePlans();
    },
    [invalidatePlans, source],
  );

  const toggleFullItemIncluded = useCallback((logicalKey: string) => {
    setFullPlan((prev) => (prev ? toggleFullPlanItemIncluded(prev, logicalKey) : prev));
    setFullApplyResults(null);
  }, []);

  const clearPreview = useCallback(() => {
    invalidatePlans();
    setError(null);
  }, [invalidatePlans]);

  const scopeWire =
    context.scope === 'project' && context.projectKey
      ? context.projectKey
      : 'user';

  const selectivePreviewGate = useMemo(
    () =>
      canRunCrossAgentPreview({
        deviceId: context.deviceId,
        source,
        destinations,
        sourceMarkdown,
        busy,
        scope: context.scope,
        projectKey: context.projectKey,
        scopeConfirmed,
      }),
    [
      busy,
      context.deviceId,
      context.projectKey,
      context.scope,
      destinations,
      scopeConfirmed,
      source,
      sourceMarkdown,
    ],
  );

  const fullPreviewGate = useMemo(
    () =>
      canRunCrossAgentFullPreview({
        deviceId: context.deviceId,
        source,
        destination: fullDestination,
        sourceMarkdown,
        busy,
        scope: context.scope,
        projectKey: context.projectKey,
        scopeConfirmed,
      }),
    [
      busy,
      context.deviceId,
      context.projectKey,
      context.scope,
      fullDestination,
      scopeConfirmed,
      source,
      sourceMarkdown,
    ],
  );

  const selectiveApplyGate = useMemo(
    () =>
      canRunCrossAgentApply({
        deviceId: context.deviceId,
        preview,
        busy,
      }),
    [busy, context.deviceId, preview],
  );

  const fullApplyGate = useMemo(
    () =>
      canRunCrossAgentFullApply({
        deviceId: context.deviceId,
        plan: fullPlan,
        busy,
      }),
    [busy, context.deviceId, fullPlan],
  );

  const previewGate = mode === 'full' ? fullPreviewGate : selectivePreviewGate;
  const applyGate = mode === 'full' ? fullApplyGate : selectiveApplyGate;

  const previewBlockedReason = useMemo(() => {
    if (previewGate.ok) return null;
    switch (previewGate.reason) {
      case 'peerBlocked':
        return t('agentHub:crossAgent.errors.peerBlocked');
      case 'emptyMarkdown':
        return t('agentHub:crossAgent.errors.emptyMarkdown');
      case 'emptyDestinations':
      case 'emptyDestination':
        return t('agentHub:crossAgent.errors.emptyDestinations');
      case 'sourceInDestinations':
      case 'sourceEqualsDestination':
        return t('agentHub:crossAgent.errors.sourceInDestinations');
      case 'scopeUnconfirmed':
        return t('agentHub:crossAgent.errors.scopeUnconfirmed');
      case 'projectKeyRequired':
        return t('agentHub:crossAgent.errors.projectKeyRequired');
      case 'busy':
        return null;
      default:
        return null;
    }
  }, [previewGate, t]);

  const applyBlockedReason = useMemo(() => {
    if (applyGate.ok) return null;
    switch (applyGate.reason) {
      case 'peerBlocked':
        return t('agentHub:crossAgent.errors.peerBlocked');
      case 'missingPreview':
      case 'emptyPlanHash':
        return t('agentHub:crossAgent.errors.previewRequired');
      case 'noApplicable':
        return t('agentHub:crossAgent.errors.noApplicable');
      case 'busy':
        return null;
      default:
        return null;
    }
  }, [applyGate, t]);

  const runPreview = useCallback(async () => {
    if (!previewGate.ok) {
      setError(previewBlockedReason);
      return;
    }
    const seq = ++previewSeqRef.current;
    setBusy(true);
    setError(null);
    setApplyResults(null);
    setFullApplyResults(null);
    try {
      if (mode === 'full') {
        if (!fullDestination) {
          setError(t('agentHub:crossAgent.errors.emptyDestinations'));
          return;
        }
        const raw = await agentHubApi.previewCrossAgentFull({
          source,
          destination: fullDestination,
          scope: scopeWire,
          sourceMarkdown: sourceMarkdown.trim(),
          deviceId: context.deviceId,
        });
        if (!mountedRef.current || seq !== previewSeqRef.current) return;
        const parsed = parseCrossAgentFullPlan(raw);
        if (!parsed) {
          setError(t('agentHub:crossAgent.errors.invalidPreview'));
          setFullPlan(null);
          return;
        }
        setFullPlan(parsed);
        setPreview(null);
        clientRequestIdRef.current = createClientRequestId();
      } else {
        const raw = await agentHubApi.previewCrossAgentInstruction({
          source,
          destinations,
          sourceMarkdown: sourceMarkdown.trim(),
          scope: scopeWire,
          destinationPaths: {},
        });
        if (!mountedRef.current || seq !== previewSeqRef.current) return;
        const parsed = parseCrossAgentPreview(raw);
        if (!parsed) {
          setError(t('agentHub:crossAgent.errors.invalidPreview'));
          setPreview(null);
          return;
        }
        setPreview(parsed);
        setFullPlan(null);
        clientRequestIdRef.current = createClientRequestId();
      }
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setError(formatError(reason));
      setPreview(null);
      setFullPlan(null);
    } finally {
      if (mountedRef.current && seq === previewSeqRef.current) {
        setBusy(false);
      }
    }
  }, [
    context.deviceId,
    destinations,
    fullDestination,
    mode,
    previewBlockedReason,
    previewGate.ok,
    scopeWire,
    source,
    sourceMarkdown,
    t,
  ]);

  const runApply = useCallback(async () => {
    if (!applyGate.ok) {
      setError(applyBlockedReason ?? t('agentHub:crossAgent.errors.previewRequired'));
      return;
    }
    const seq = ++applySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const clientRequestId =
        clientRequestIdRef.current ?? createClientRequestId();
      clientRequestIdRef.current = clientRequestId;

      if (mode === 'full') {
        if (!fullPlan || !fullDestination) {
          setError(t('agentHub:crossAgent.errors.previewRequired'));
          return;
        }
        const raw = await agentHubApi.applyCrossAgentFull({
          source,
          destination: fullDestination,
          scope: scopeWire,
          sourceMarkdown: sourceMarkdown.trim(),
          planHash: fullPlan.planHash,
          clientRequestId,
          items: fullPlan.items.map((item) => ({
            logicalKey: item.logicalKey,
            included: item.included,
          })),
          deviceId: context.deviceId,
        });
        if (!mountedRef.current || seq !== applySeqRef.current) return;
        setFullApplyResults(parseCrossAgentFullApplyResults(raw));
        setApplyResults(null);
      } else {
        const applicable =
          'applicableDestinations' in applyGate
            ? applyGate.applicableDestinations
            : [];
        const raw = await agentHubApi.applyCrossAgentInstruction({
          source,
          destinations: applicable,
          sourceMarkdown: sourceMarkdown.trim(),
          scope: scopeWire,
          destinationPaths: {},
          planHash: preview?.planHash ?? '',
          clientRequestId,
        });
        if (!mountedRef.current || seq !== applySeqRef.current) return;
        setApplyResults(parseCrossAgentApplyResults(raw));
        setFullApplyResults(null);
      }
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [
    applyBlockedReason,
    applyGate,
    context.deviceId,
    fullDestination,
    fullPlan,
    mode,
    scopeWire,
    source,
    sourceMarkdown,
    t,
  ]);

  const projectOptInNeeded =
    context.scope === 'project' &&
    !(context.projectKey && context.projectKey.trim().length > 0);

  const applicableCount =
    mode === 'full'
      ? countApplicableFullItems(fullPlan)
      : countApplicableDestinations(preview);

  return {
    mode,
    setMode,
    source,
    destinations,
    destinationOptions: destinationCandidates(source),
    fullDestination,
    setFullDestination,
    scope: context.scope,
    projectKey: context.projectKey,
    scopeConfirmed,
    setScopeConfirmed,
    sourceMarkdown,
    setSourceMarkdown,
    contentLoading,
    contentError,
    peerBlocked,
    busy,
    error,
    preview,
    applyResults,
    fullPlan,
    fullApplyResults,
    applicableCount,
    canPreview: previewGate.ok && !busy,
    canApply: applyGate.ok && !busy,
    previewBlockedReason,
    applyBlockedReason,
    projectOptInNeeded,
    toggleDestination,
    toggleFullItemIncluded,
    runPreview,
    runApply,
    refreshSourceContent,
    clearPreview,
  };
}
