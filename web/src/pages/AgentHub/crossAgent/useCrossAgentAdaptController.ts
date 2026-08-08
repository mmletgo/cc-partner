/**
 * Cross-agent selective adapt page controller.
 *
 * Business Logic（为什么需要）:
 *   独立全页编排：源=当前 agent、多选目标（不含源）、scope 确认、内容加载、
 *   preview → apply；peer 设备上下文整页 blocked（同机 only）。
 *
 * Code Logic（做什么）:
 *   持有 destinations/scopeConfirmed/markdown/preview/apply/busy；
 *   调用 agentHubApi.preview/applyCrossAgentInstruction；pure view 不 import transport。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi } from '@/api/agentHub';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import { originalFromWorkspace } from '../instructions';
import {
  canRunCrossAgentApply,
  canRunCrossAgentPreview,
  countApplicableDestinations,
  defaultDestinationsForSource,
  destinationCandidates,
  isPeerContextBlocked,
  parseCrossAgentApplyResults,
  parseCrossAgentPreview,
  sanitizeDestinations,
  toggleDestinationSelection,
  type CrossAgentApplyResult,
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
  source: AgentTarget;
  destinations: AgentTarget[];
  destinationOptions: AgentTarget[];
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
  applicableCount: number;
  canPreview: boolean;
  canApply: boolean;
  previewBlockedReason: string | null;
  applyBlockedReason: string | null;
  /** project scope 但未选项目时提示启用/选择。 */
  projectOptInNeeded: boolean;
  toggleDestination: (target: AgentTarget) => void;
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
 * Business Logic: 为 Agent Hub 适配全页提供选择性编排状态机。
 * Code Logic: source 跟随 context.agent；peer blocked 时禁止 preview/apply。
 */
export function useCrossAgentAdaptController(
  args: UseCrossAgentAdaptControllerArgs,
): UseCrossAgentAdaptControllerResult {
  const { context, t, initialSourceMarkdown } = args;
  const source = context.agent;
  const peerBlocked = isPeerContextBlocked(context.deviceId);

  const [destinations, setDestinations] = useState<AgentTarget[]>(() =>
    defaultDestinationsForSource(source),
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
    setPreview(null);
    setApplyResults(null);
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

  const setScopeConfirmed = useCallback((value: boolean) => {
    setScopeConfirmedState(value);
    setPreview(null);
    setApplyResults(null);
    clientRequestIdRef.current = null;
  }, []);

  const setSourceMarkdown = useCallback((value: string) => {
    setSourceMarkdownState(value);
    setPreview(null);
    setApplyResults(null);
    clientRequestIdRef.current = null;
  }, []);

  const toggleDestination = useCallback(
    (target: AgentTarget) => {
      if (target === source) return;
      setDestinations((prev) => toggleDestinationSelection(source, prev, target));
      setPreview(null);
      setApplyResults(null);
      clientRequestIdRef.current = null;
    },
    [source],
  );

  const clearPreview = useCallback(() => {
    setPreview(null);
    setApplyResults(null);
    clientRequestIdRef.current = null;
    setError(null);
  }, []);

  const previewGate = useMemo(
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

  const applyGate = useMemo(
    () =>
      canRunCrossAgentApply({
        deviceId: context.deviceId,
        preview,
        busy,
      }),
    [busy, context.deviceId, preview],
  );

  const previewBlockedReason = useMemo(() => {
    if (previewGate.ok) return null;
    switch (previewGate.reason) {
      case 'peerBlocked':
        return t('agentHub:crossAgent.errors.peerBlocked');
      case 'emptyMarkdown':
        return t('agentHub:crossAgent.errors.emptyMarkdown');
      case 'emptyDestinations':
        return t('agentHub:crossAgent.errors.emptyDestinations');
      case 'sourceInDestinations':
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
    try {
      const raw = await agentHubApi.previewCrossAgentInstruction({
        source,
        destinations,
        sourceMarkdown: sourceMarkdown.trim(),
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
      clientRequestIdRef.current = createClientRequestId();
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setError(formatError(reason));
      setPreview(null);
    } finally {
      if (mountedRef.current && seq === previewSeqRef.current) {
        setBusy(false);
      }
    }
  }, [
    destinations,
    previewBlockedReason,
    previewGate.ok,
    source,
    sourceMarkdown,
    t,
  ]);

  const runApply = useCallback(async () => {
    if (!applyGate.ok) {
      setError(applyBlockedReason ?? t('agentHub:crossAgent.errors.previewRequired'));
      return;
    }
    const applicable = applyGate.applicableDestinations;
    const seq = ++applySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const clientRequestId =
        clientRequestIdRef.current ?? createClientRequestId();
      clientRequestIdRef.current = clientRequestId;
      const raw = await agentHubApi.applyCrossAgentInstruction({
        source,
        destinations: applicable,
        sourceMarkdown: sourceMarkdown.trim(),
        destinationPaths: {},
        clientRequestId,
      });
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setApplyResults(parseCrossAgentApplyResults(raw));
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [applyBlockedReason, applyGate, source, sourceMarkdown, t]);

  const projectOptInNeeded =
    context.scope === 'project' &&
    !(context.projectKey && context.projectKey.trim().length > 0);

  return {
    source,
    destinations,
    destinationOptions: destinationCandidates(source),
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
    applicableCount: countApplicableDestinations(preview),
    canPreview: previewGate.ok && !busy,
    canApply: applyGate.ok && !busy,
    previewBlockedReason,
    applyBlockedReason,
    projectOptInNeeded,
    toggleDestination,
    runPreview,
    runApply,
    refreshSourceContent,
    clearPreview,
  };
}
