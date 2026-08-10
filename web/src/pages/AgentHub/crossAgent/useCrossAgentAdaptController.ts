/**
 * Cross-agent selective preview controller.
 *
 * Business Logic（为什么需要）:
 *   当前认证边界只允许“本机 + 用户级 + 指令选择性预览”。切换源、上下文、正文或目标后，
 *   旧异步响应不得落入新上下文；Apply 与 full-volume 始终不可用。
 *
 * Code Logic（做什么）:
 *   用 context generation + request fingerprint 约束 content/preview 提交；严格核对响应源和目标；
 *   保留旧 controller 形状供调用方渐进迁移，但所有写入/full 方法稳定 fail-closed。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { agentHubApi } from '@/api/agentHub';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubContext } from '../context/agentHubContext';
import { originalFromWorkspace } from '../instructions';
import {
  canRunCrossAgentPreview,
  defaultDestinationsForSource,
  defaultFullDestination,
  destinationCandidates,
  isPeerContextBlocked,
  parseCrossAgentPreview,
  toggleDestinationSelection,
  type CrossAgentAdaptVolumeMode,
  type CrossAgentApplyResult,
  type CrossAgentFullApplyItemResult,
  type CrossAgentFullPlan,
  type CrossAgentPreviewReport,
} from './crossAgentPresentation';

export interface UseCrossAgentAdaptControllerArgs {
  context: AgentHubContext;
  t: TFunction<['agentHub', 'common']>;
  /** 当前三栏已加载的原始正文；上下文变化后按新 generation 注入。 */
  initialSourceMarkdown?: string | null;
}

export interface UseCrossAgentAdaptControllerResult {
  mode: CrossAgentAdaptVolumeMode;
  setMode: (mode: CrossAgentAdaptVolumeMode) => void;
  source: AgentTarget;
  destinations: AgentTarget[];
  destinationOptions: AgentTarget[];
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
  projectOptInNeeded: boolean;
  toggleDestination: (target: AgentTarget) => void;
  toggleFullItemIncluded: (logicalKey: string) => void;
  runPreview: () => Promise<void>;
  runApply: () => Promise<void>;
  refreshSourceContent: () => Promise<void>;
  clearPreview: () => void;
}

/** 把未知错误变成用户可诊断的稳定文本。 */
function formatError(reason: unknown): string {
  if (!reason) return 'unknown_error';
  if (reason instanceof Error) {
    const code = (reason as Error & { code?: unknown }).code;
    if (typeof code === 'string' && code.length > 0) {
      return code === reason.message ? code : `${code}: ${reason.message}`;
    }
    return reason.message || 'unknown_error';
  }
  return String(reason);
}

/** 请求正文和目标的稳定 fingerprint。 */
function previewFingerprint(
  contextFingerprint: string,
  source: AgentTarget,
  destinations: AgentTarget[],
  sourceMarkdown: string,
  scopeConfirmed: boolean,
): string {
  return JSON.stringify({
    contextFingerprint,
    source,
    destinations: [...destinations].sort(),
    sourceMarkdown,
    scope: 'user',
    scopeConfirmed,
  });
}

/** 响应必须精确覆盖本次请求目标，不接受缺项、多项或重复项。 */
function responseMatchesRequest(
  preview: CrossAgentPreviewReport,
  source: AgentTarget,
  destinations: AgentTarget[],
): boolean {
  if (preview.source !== source || preview.destinations.length !== destinations.length) {
    return false;
  }
  const expected = new Set(destinations);
  return preview.destinations.every(
    (row) => expected.has(row.destination) && row.destination !== source && !row.canApply,
  );
}

/**
 * Business Logic（为什么需要）:
 *   用户必须只看到当前 Agent/上下文/正文对应的真实预览。
 *
 * Code Logic（做什么）:
 *   每个影响输入的动作都会递增 generation 并使旧请求失效；full/apply 不调用 API。
 */
export function useCrossAgentAdaptController(
  args: UseCrossAgentAdaptControllerArgs,
): UseCrossAgentAdaptControllerResult {
  const { context, t, initialSourceMarkdown } = args;
  const source = context.agent;
  const peerBlocked = isPeerContextBlocked(context.deviceId);
  const contextFingerprint = JSON.stringify({
    source,
    scope: context.scope,
    deviceId: context.deviceId ?? null,
    projectKey: context.projectKey ?? null,
    tab: context.tab,
    instructionLane: context.instructionLane,
    adaptView: context.adaptView,
  });

  const [destinations, setDestinations] = useState<AgentTarget[]>(() =>
    defaultDestinationsForSource(source),
  );
  const [scopeConfirmed, setScopeConfirmedState] = useState(false);
  const [sourceMarkdown, setSourceMarkdownState] = useState(initialSourceMarkdown ?? '');
  const [contentLoading, setContentLoading] = useState(false);
  const [contentError, setContentError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<CrossAgentPreviewReport | null>(null);
  const currentPreviewFingerprint = useMemo(
    () =>
      previewFingerprint(
        contextFingerprint,
        source,
        destinations,
        sourceMarkdown.trim(),
        scopeConfirmed,
      ),
    [contextFingerprint, destinations, scopeConfirmed, source, sourceMarkdown],
  );

  const mountedRef = useRef(true);
  const generationRef = useRef(0);
  const contentSeqRef = useRef(0);
  const previewSeqRef = useRef(0);
  const activePreviewFingerprintRef = useRef<string | null>(null);
  const latestPreviewFingerprintRef = useRef('');
  const latestContextFingerprintRef = useRef(contextFingerprint);

  useEffect(() => {
    latestContextFingerprintRef.current = contextFingerprint;
    latestPreviewFingerprintRef.current = currentPreviewFingerprint;
  }, [contextFingerprint, currentPreviewFingerprint]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      generationRef.current += 1;
      contentSeqRef.current += 1;
      previewSeqRef.current += 1;
      activePreviewFingerprintRef.current = null;
    };
  }, []);

  /** 只使预览失效；目标/确认变化不得取消独立的 source inspect。 */
  const invalidatePreview = useCallback(() => {
    previewSeqRef.current += 1;
    activePreviewFingerprintRef.current = null;
    setPreview(null);
    setBusy(false);
  }, []);

  /** 从当前本机 Agent 读取原始指令；响应提交受 generation + context 约束。 */
  const refreshSourceContent = useCallback(async () => {
    if (peerBlocked || context.scope !== 'user') {
      setContentError(
        peerBlocked
          ? t('agentHub:crossAgent.errors.peerBlocked')
          : t('agentHub:crossAgent.errors.projectBlocked'),
      );
      return;
    }

    const generation = generationRef.current;
    const seq = ++contentSeqRef.current;
    const requestedContextFingerprint = contextFingerprint;
    invalidatePreview();
    setContentLoading(true);
    setContentError(null);
    setPreview(null);
    setBusy(false);
    try {
      const workspace = await agentHubApi.inspectUserInstructionWorkspace();
      if (
        !mountedRef.current ||
        generation !== generationRef.current ||
        seq !== contentSeqRef.current ||
        latestContextFingerprintRef.current !== requestedContextFingerprint
      ) {
        return;
      }
      const { text } = originalFromWorkspace(workspace, source);
      setSourceMarkdownState(text);
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== generationRef.current ||
        seq !== contentSeqRef.current ||
        latestContextFingerprintRef.current !== requestedContextFingerprint
      ) {
        return;
      }
      setContentError(formatError(reason));
    } finally {
      if (
        mountedRef.current &&
        generation === generationRef.current &&
        seq === contentSeqRef.current &&
        latestContextFingerprintRef.current === requestedContextFingerprint
      ) {
        setContentLoading(false);
      }
    }
  }, [context.scope, contextFingerprint, invalidatePreview, peerBlocked, source, t]);

  // Agent/scope/device/project 或父级 source 变化时，原子开启新 generation。
  useEffect(() => {
    generationRef.current += 1;
    contentSeqRef.current += 1;
    previewSeqRef.current += 1;
    activePreviewFingerprintRef.current = null;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- context generation changes must atomically clear prior UI state.
    setDestinations(defaultDestinationsForSource(source));
    setScopeConfirmedState(false);
    setSourceMarkdownState(initialSourceMarkdown ?? '');
    setContentLoading(false);
    setContentError(null);
    setBusy(false);
    setError(null);
    setPreview(null);

    if (
      !peerBlocked &&
      context.scope === 'user' &&
      (initialSourceMarkdown ?? '').trim().length === 0
    ) {
      const timeoutId = window.setTimeout(() => {
        void refreshSourceContent();
      }, 0);
      return () => {
        window.clearTimeout(timeoutId);
      };
    }
    return undefined;
  }, [
    context.scope,
    contextFingerprint,
    initialSourceMarkdown,
    peerBlocked,
    refreshSourceContent,
    source,
  ]);

  const previewGate = useMemo(
    () =>
      canRunCrossAgentPreview({
        deviceId: context.deviceId,
        source,
        destinations,
        sourceMarkdown,
        busy: busy || contentLoading,
        scope: context.scope,
        projectKey: context.projectKey,
        scopeConfirmed,
      }),
    [
      busy,
      contentLoading,
      context.deviceId,
      context.projectKey,
      context.scope,
      destinations,
      scopeConfirmed,
      source,
      sourceMarkdown,
    ],
  );

  const previewBlockedReason = useMemo(() => {
    if (previewGate.ok) return null;
    switch (previewGate.reason) {
      case 'peerBlocked':
        return t('agentHub:crossAgent.errors.peerBlocked');
      case 'projectBlocked':
        return t('agentHub:crossAgent.errors.projectBlocked');
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
      case 'ok':
      default:
        return null;
    }
  }, [previewGate, t]);

  const setMode = useCallback(
    (nextMode: CrossAgentAdaptVolumeMode) => {
      invalidatePreview();
      setError(
        nextMode === 'full'
          ? t('agentHub:crossAgent.errors.fullUnavailable')
          : null,
      );
    },
    [invalidatePreview, t],
  );

  const setScopeConfirmed = useCallback(
    (value: boolean) => {
      invalidatePreview();
      setScopeConfirmedState(value);
      setError(null);
    },
    [invalidatePreview],
  );

  const setSourceMarkdown = useCallback(
    (value: string) => {
      invalidatePreview();
      contentSeqRef.current += 1;
      setContentLoading(false);
      setSourceMarkdownState(value);
      setError(null);
    },
    [invalidatePreview],
  );

  const toggleDestination = useCallback(
    (target: AgentTarget) => {
      invalidatePreview();
      setDestinations((current) => toggleDestinationSelection(source, current, target));
      setError(null);
    },
    [invalidatePreview, source],
  );

  const clearPreview = useCallback(() => {
    invalidatePreview();
    setError(null);
  }, [invalidatePreview]);

  const runPreview = useCallback(async () => {
    if (!previewGate.ok) {
      setError(previewBlockedReason);
      return;
    }

    const markdown = sourceMarkdown.trim();
    const requestedDestinations = [...destinations];
    const requestFingerprint = currentPreviewFingerprint;
    const generation = generationRef.current;
    const seq = ++previewSeqRef.current;
    activePreviewFingerprintRef.current = requestFingerprint;
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      const raw = await agentHubApi.previewCrossAgentInstruction({
        source,
        destinations: requestedDestinations,
        sourceMarkdown: markdown,
        scope: 'user',
      });
      if (
        !mountedRef.current ||
        generation !== generationRef.current ||
        seq !== previewSeqRef.current ||
        activePreviewFingerprintRef.current !== requestFingerprint ||
        latestPreviewFingerprintRef.current !== requestFingerprint
      ) {
        return;
      }
      const parsed = parseCrossAgentPreview(raw);
      if (!parsed || !responseMatchesRequest(parsed, source, requestedDestinations)) {
        setError(t('agentHub:crossAgent.errors.invalidPreview'));
        return;
      }
      setPreview(parsed);
    } catch (reason) {
      if (
        !mountedRef.current ||
        generation !== generationRef.current ||
        seq !== previewSeqRef.current ||
        latestPreviewFingerprintRef.current !== requestFingerprint
      ) {
        return;
      }
      setError(formatError(reason));
    } finally {
      if (
        mountedRef.current &&
        generation === generationRef.current &&
        seq === previewSeqRef.current
      ) {
        setBusy(false);
      }
    }
  }, [
    currentPreviewFingerprint,
    destinations,
    previewBlockedReason,
    previewGate.ok,
    source,
    sourceMarkdown,
    t,
  ]);

  const runApply = useCallback(async () => {
    setError(t('agentHub:crossAgent.errors.applyUnavailable'));
  }, [t]);

  const setFullDestination = useCallback(() => {
    invalidatePreview();
    setError(t('agentHub:crossAgent.errors.fullUnavailable'));
  }, [invalidatePreview, t]);

  const toggleFullItemIncluded = useCallback(() => {
    setError(t('agentHub:crossAgent.errors.fullUnavailable'));
  }, [t]);

  return {
    mode: 'selective',
    setMode,
    source,
    destinations,
    destinationOptions: destinationCandidates(source),
    fullDestination: defaultFullDestination(source),
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
    applyResults: null,
    fullPlan: null,
    fullApplyResults: null,
    applicableCount: 0,
    canPreview: previewGate.ok && !busy,
    canApply: false,
    previewBlockedReason,
    applyBlockedReason: t('agentHub:crossAgent.errors.applyUnavailable'),
    projectOptInNeeded: context.scope !== 'user',
    toggleDestination,
    toggleFullItemIncluded,
    runPreview,
    runApply,
    refreshSourceContent,
    clearPreview,
  };
}
