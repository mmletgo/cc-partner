/**
 * Cross-agent selective adapt pure helpers.
 *
 * Business Logic（为什么需要）:
 *   独立适配页需要：排除源的目标候选、preview/apply 解析、peer 同机门闩、
 *   预览强制闸门与 mode 展示 tone；规则不得散落在 React 组件里。
 *
 * Code Logic（做什么）:
 *   纯函数：候选列表、解析 DTO、canPreview/canApply gate、mode tone；无 React/API/i18n 实例。
 */

import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubScope } from '../context/agentHubContext';

/** 三 Agent 全集（与壳层一致）。 */
export const ALL_AGENT_TARGETS: AgentTarget[] = ['claude', 'codex', 'opencode'];

/** 适配分类 mode（与后端 CrossAgentAdaptMode camelCase 对齐）。 */
export type CrossAgentAdaptMode = 'shared' | 'adapted' | 'targetOnly' | 'residual';

/** 单目标预览行。 */
export interface CrossAgentTargetPreviewRow {
  destination: AgentTarget;
  mode: CrossAgentAdaptMode;
  path: string;
  renderedHash?: string | null;
  observedHash?: string | null;
  unifiedDiff?: string | null;
  partialBlockers: string[];
  canApply: boolean;
}

/** 跨 Agent preview 报告。 */
export interface CrossAgentPreviewReport {
  source: AgentTarget;
  kind: string;
  destinations: CrossAgentTargetPreviewRow[];
  needsAdaptation: boolean;
  planHash: string;
}

/** 单目标 apply 结果。 */
export interface CrossAgentApplyResult {
  destination: AgentTarget;
  status: string;
  path: string;
  errorCode?: string | null;
}

/** 预览闸门输入。 */
export interface CrossAgentPreviewGateInput {
  deviceId: string | null;
  source: AgentTarget;
  destinations: AgentTarget[];
  sourceMarkdown: string;
  busy: boolean;
  /** project scope 时必须有 projectKey */
  scope: AgentHubScope;
  projectKey: string | null;
  /** 用户已确认当前 scope */
  scopeConfirmed: boolean;
}

/** 预览闸门结果。 */
export interface CrossAgentPreviewGate {
  ok: boolean;
  reason:
    | 'ok'
    | 'peerBlocked'
    | 'projectBlocked'
    | 'busy'
    | 'emptyMarkdown'
    | 'emptyDestinations'
    | 'sourceInDestinations'
    | 'scopeUnconfirmed'
    | 'projectKeyRequired';
}

/** Apply 闸门输入。 */
export interface CrossAgentApplyGateInput {
  deviceId: string | null;
  preview: CrossAgentPreviewReport | null;
  busy: boolean;
}

/** Apply 闸门结果。 */
export interface CrossAgentApplyGate {
  ok: boolean;
  reason: 'applyUnavailable';
  applicableDestinations: AgentTarget[];
}

/**
 * Business Logic: 类型守卫，过滤未知 IPC 值。
 * Code Logic: 三枚举字面量。
 */
export function isAgentTarget(value: unknown): value is AgentTarget {
  return value === 'claude' || value === 'codex' || value === 'opencode';
}

/**
 * Business Logic: 规范化 mode；未知值回落 residual（最保守）。
 * Code Logic: 字面量匹配。
 */
export function normalizeAdaptMode(value: unknown): CrossAgentAdaptMode {
  if (
    value === 'shared' ||
    value === 'adapted' ||
    value === 'targetOnly' ||
    value === 'residual'
  ) {
    return value;
  }
  return 'residual';
}

/**
 * Business Logic: IPC 中未知 mode 不得被伪装成可理解的 residual 结果。
 * Code Logic: 严格识别后端枚举。
 */
function isCrossAgentAdaptMode(value: unknown): value is CrossAgentAdaptMode {
  return (
    value === 'shared' ||
    value === 'adapted' ||
    value === 'targetOnly' ||
    value === 'residual'
  );
}

/**
 * Business Logic: 目标候选永远不含源。
 * Code Logic: 过滤 ALL_AGENT_TARGETS。
 */
export function destinationCandidates(source: AgentTarget): AgentTarget[] {
  return ALL_AGENT_TARGETS.filter((target) => target !== source);
}

/**
 * Business Logic: 禁止把源选为目标。
 * Code Logic: 严格不等。
 */
export function canSelectDestination(source: AgentTarget, destination: AgentTarget): boolean {
  return source !== destination;
}

/**
 * Business Logic: 切换源后剔除非法 destination，避免 source∈destinations。
 * Code Logic: filter + 去重。
 */
export function sanitizeDestinations(
  source: AgentTarget,
  destinations: AgentTarget[],
): AgentTarget[] {
  const allowed = new Set(destinationCandidates(source));
  const out: AgentTarget[] = [];
  for (const dest of destinations) {
    if (!allowed.has(dest)) continue;
    if (out.includes(dest)) continue;
    out.push(dest);
  }
  return out;
}

/**
 * Business Logic: peer 设备上下文禁止跨 Agent（同机 only）。
 * Code Logic: deviceId 非空即 blocked。
 */
export function isPeerContextBlocked(deviceId: string | null | undefined): boolean {
  return deviceId != null && String(deviceId).trim().length > 0;
}

/**
 * Business Logic: 解析 preview IPC 未知载荷。
 * Code Logic: 严格校验完整报告；任一畸形/可写行均使整个响应失效。
 */
export function parseCrossAgentPreview(raw: unknown): CrossAgentPreviewReport | null {
  if (!raw || typeof raw !== 'object') return null;
  const obj = raw as Record<string, unknown>;
  if (
    !isAgentTarget(obj.source) ||
    obj.kind !== 'instruction' ||
    !Array.isArray(obj.destinations) ||
    typeof obj.needsAdaptation !== 'boolean' ||
    typeof obj.planHash !== 'string' ||
    obj.planHash.trim().length === 0
  ) {
    return null;
  }
  const destinations: CrossAgentTargetPreviewRow[] = [];
  const seen = new Set<AgentTarget>();
  for (const row of obj.destinations) {
    if (!row || typeof row !== 'object') return null;
    const r = row as Record<string, unknown>;
    if (
      !isAgentTarget(r.destination) ||
      seen.has(r.destination) ||
      !isCrossAgentAdaptMode(r.mode) ||
      typeof r.path !== 'string' ||
      !Array.isArray(r.partialBlockers) ||
      !r.partialBlockers.every((blocker) => typeof blocker === 'string') ||
      r.canApply !== false ||
      (r.renderedHash != null && typeof r.renderedHash !== 'string') ||
      (r.observedHash != null && typeof r.observedHash !== 'string') ||
      (r.unifiedDiff != null && typeof r.unifiedDiff !== 'string')
    ) {
      return null;
    }
    seen.add(r.destination);
    destinations.push({
      destination: r.destination,
      mode: r.mode,
      path: r.path,
      renderedHash: typeof r.renderedHash === 'string' ? r.renderedHash : null,
      observedHash: typeof r.observedHash === 'string' ? r.observedHash : null,
      unifiedDiff: typeof r.unifiedDiff === 'string' ? r.unifiedDiff : null,
      partialBlockers: r.partialBlockers as string[],
      canApply: false,
    });
  }
  return {
    source: obj.source,
    kind: 'instruction',
    destinations,
    needsAdaptation: obj.needsAdaptation,
    planHash: obj.planHash,
  };
}

/**
 * Business Logic: 解析 apply 结果数组。
 * Code Logic: 行级容错；非数组 → []。
 */
export function parseCrossAgentApplyResults(raw: unknown): CrossAgentApplyResult[] {
  if (!Array.isArray(raw)) return [];
  const out: CrossAgentApplyResult[] = [];
  for (const row of raw) {
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    if (!isAgentTarget(r.destination) || typeof r.path !== 'string') continue;
    out.push({
      destination: r.destination,
      status: typeof r.status === 'string' ? r.status : 'failed',
      path: r.path,
      errorCode: typeof r.errorCode === 'string' ? r.errorCode : null,
    });
  }
  return out;
}

/**
 * Business Logic: mode → Pill tone。
 * Code Logic: shared success / adapted accent / targetOnly warn / residual danger。
 */
export function adaptModeTone(
  mode: CrossAgentAdaptMode,
): 'success' | 'accent' | 'warn' | 'danger' | 'neutral' {
  switch (mode) {
    case 'shared':
      return 'success';
    case 'adapted':
      return 'accent';
    case 'targetOnly':
      return 'warn';
    case 'residual':
      return 'danger';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic: 可自动写入的目标数（残差/空渲染等 canApply=false）。
 * Code Logic: filter canApply。
 */
export function countApplicableDestinations(
  preview: CrossAgentPreviewReport | null | undefined,
): number {
  if (!preview) return 0;
  return preview.destinations.filter((row) => row.canApply).length;
}

/**
 * Business Logic: 预览前硬门闩（peer / 空内容 / 源∈目标 / scope）。
 * Code Logic: 按优先级短路。
 */
export function canRunCrossAgentPreview(input: CrossAgentPreviewGateInput): CrossAgentPreviewGate {
  if (isPeerContextBlocked(input.deviceId)) {
    return { ok: false, reason: 'peerBlocked' };
  }
  if (input.scope !== 'user') {
    return { ok: false, reason: 'projectBlocked' };
  }
  if (input.busy) {
    return { ok: false, reason: 'busy' };
  }
  if (input.sourceMarkdown.trim().length === 0) {
    return { ok: false, reason: 'emptyMarkdown' };
  }
  if (input.destinations.length === 0) {
    return { ok: false, reason: 'emptyDestinations' };
  }
  if (input.destinations.includes(input.source)) {
    return { ok: false, reason: 'sourceInDestinations' };
  }
  if (!input.scopeConfirmed) {
    return { ok: false, reason: 'scopeUnconfirmed' };
  }
  return { ok: true, reason: 'ok' };
}

/**
 * Business Logic: 必须先 preview 且至少一项目可写；peer 再拦一次。
 * Code Logic: 短路 + 收集 applicable destinations。
 */
export function canRunCrossAgentApply(input: CrossAgentApplyGateInput): CrossAgentApplyGate {
  void input;
  return { ok: false, reason: 'applyUnavailable', applicableDestinations: [] };
}

/**
 * Business Logic: 切换 destination 后预览作废。
 * Code Logic: 返回新列表（已 sanitize）。
 */
export function toggleDestinationSelection(
  source: AgentTarget,
  current: AgentTarget[],
  target: AgentTarget,
): AgentTarget[] {
  if (!canSelectDestination(source, target)) {
    return sanitizeDestinations(source, current);
  }
  if (current.includes(target)) {
    return current.filter((d) => d !== target);
  }
  return sanitizeDestinations(source, [...current, target]);
}

/**
 * Business Logic: 源切换时默认选中全部其它目标（空源内容时仍可展示候选）。
 * Code Logic: destinationCandidates。
 */
export function defaultDestinationsForSource(source: AgentTarget): AgentTarget[] {
  return destinationCandidates(source);
}

// ── Full-volume mode (T10) ─────────────────────────────────────────

/** 适配模式：选择性（多目标指令）| 全量（单目标五类）。 */
export type CrossAgentAdaptVolumeMode = 'selective' | 'full';

/** 五类 kind wire token。 */
export type CrossAgentFullKind =
  | 'instruction'
  | 'skill'
  | 'command'
  | 'mcp'
  | 'plugin';

/** 全量 plan 单项。 */
export interface CrossAgentFullPlanItem {
  kind: CrossAgentFullKind;
  logicalKey: string;
  action: string;
  path: string;
  content?: string | null;
  residualReason?: string | null;
  included: boolean;
}

/** 全量适配方案。 */
export interface CrossAgentFullPlan {
  source: AgentTarget;
  destination: AgentTarget;
  scope: string;
  items: CrossAgentFullPlanItem[];
  planHash: string;
  generator: string;
}

/** 全量 apply 单项结果。 */
export interface CrossAgentFullApplyItemResult {
  kind: CrossAgentFullKind;
  logicalKey: string;
  status: string;
  path: string;
  errorCode?: string | null;
}

/** 全量预览闸门输入。 */
export interface CrossAgentFullPreviewGateInput {
  deviceId: string | null;
  source: AgentTarget;
  destination: AgentTarget | null;
  sourceMarkdown: string;
  busy: boolean;
  scope: AgentHubScope;
  projectKey: string | null;
  scopeConfirmed: boolean;
}

/** 全量预览闸门结果。 */
export interface CrossAgentFullPreviewGate {
  ok: boolean;
  reason: 'fullUnavailable';
}

/** 全量 apply 闸门输入。 */
export interface CrossAgentFullApplyGateInput {
  deviceId: string | null;
  plan: CrossAgentFullPlan | null;
  busy: boolean;
}

/** 全量 apply 闸门结果。 */
export interface CrossAgentFullApplyGate {
  ok: boolean;
  reason: 'fullUnavailable';
  includedCount: number;
}

const FULL_KINDS: CrossAgentFullKind[] = [
  'instruction',
  'skill',
  'command',
  'mcp',
  'plugin',
];

/**
 * Business Logic: 规范化 full kind；未知 → skill 保守。
 * Code Logic: 字面量匹配。
 */
export function normalizeFullKind(value: unknown): CrossAgentFullKind {
  if (
    value === 'instruction' ||
    value === 'skill' ||
    value === 'command' ||
    value === 'mcp' ||
    value === 'plugin'
  ) {
    return value;
  }
  return 'skill';
}

/**
 * Business Logic: 解析全量 plan IPC。
 * Code Logic: 结构守卫；items 行级容错；planHash 必填。
 */
export function parseCrossAgentFullPlan(raw: unknown): CrossAgentFullPlan | null {
  if (!raw || typeof raw !== 'object') return null;
  const obj = raw as Record<string, unknown>;
  if (!isAgentTarget(obj.source) || !isAgentTarget(obj.destination)) return null;
  if (typeof obj.planHash !== 'string' || obj.planHash.trim().length === 0) return null;
  if (typeof obj.scope !== 'string') return null;
  if (!Array.isArray(obj.items)) return null;
  const items: CrossAgentFullPlanItem[] = [];
  for (const row of obj.items) {
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    if (typeof r.logicalKey !== 'string' || typeof r.path !== 'string') continue;
    items.push({
      kind: normalizeFullKind(r.kind),
      logicalKey: r.logicalKey,
      action: typeof r.action === 'string' ? r.action : 'skip',
      path: r.path,
      content: typeof r.content === 'string' ? r.content : null,
      residualReason: typeof r.residualReason === 'string' ? r.residualReason : null,
      included: r.included !== false,
    });
  }
  return {
    source: obj.source,
    destination: obj.destination,
    scope: obj.scope,
    items,
    planHash: obj.planHash,
    generator: typeof obj.generator === 'string' ? obj.generator : 'stub',
  };
}

/**
 * Business Logic: 解析全量 apply 结果。
 * Code Logic: 行级容错。
 */
export function parseCrossAgentFullApplyResults(
  raw: unknown,
): CrossAgentFullApplyItemResult[] {
  if (!Array.isArray(raw)) return [];
  const out: CrossAgentFullApplyItemResult[] = [];
  for (const row of raw) {
    if (!row || typeof row !== 'object') continue;
    const r = row as Record<string, unknown>;
    if (typeof r.logicalKey !== 'string' || typeof r.path !== 'string') continue;
    out.push({
      kind: normalizeFullKind(r.kind),
      logicalKey: r.logicalKey,
      status: typeof r.status === 'string' ? r.status : 'failed',
      path: r.path,
      errorCode: typeof r.errorCode === 'string' ? r.errorCode : null,
    });
  }
  return out;
}

/**
 * Business Logic: 可 apply 的全量项 = included 且 action≠skip 且无 residual。
 * Code Logic: filter。
 */
export function countApplicableFullItems(plan: CrossAgentFullPlan | null | undefined): number {
  if (!plan) return 0;
  return plan.items.filter(
    (item) => item.included && item.action !== 'skip' && !item.residualReason,
  ).length;
}

/**
 * Business Logic: 全量 preview 硬门闩（单目标）。
 * Code Logic: 按优先级短路。
 */
export function canRunCrossAgentFullPreview(
  input: CrossAgentFullPreviewGateInput,
): CrossAgentFullPreviewGate {
  void input;
  return { ok: false, reason: 'fullUnavailable' };
}

/**
 * Business Logic: 必须先有 plan_hash 与至少一项可写 included。
 * Code Logic: 短路。
 */
export function canRunCrossAgentFullApply(
  input: CrossAgentFullApplyGateInput,
): CrossAgentFullApplyGate {
  void input;
  return { ok: false, reason: 'fullUnavailable', includedCount: 0 };
}

/**
 * Business Logic: 切换 plan 项 included 后返回新 plan（浅拷贝 items）。
 * Code Logic: map by logicalKey。
 */
export function toggleFullPlanItemIncluded(
  plan: CrossAgentFullPlan,
  logicalKey: string,
): CrossAgentFullPlan {
  return {
    ...plan,
    items: plan.items.map((item) =>
      item.logicalKey === logicalKey ? { ...item, included: !item.included } : item,
    ),
  };
}

/**
 * Business Logic: 全量模式默认单目标 = 第一个候选。
 * Code Logic: destinationCandidates[0]。
 */
export function defaultFullDestination(source: AgentTarget): AgentTarget | null {
  return destinationCandidates(source)[0] ?? null;
}

/**
 * Business Logic: UI 展示五类是否齐全（调试/空态）。
 * Code Logic: Set of kinds。
 */
export function fullPlanHasAllKinds(plan: CrossAgentFullPlan | null | undefined): boolean {
  if (!plan) return false;
  const kinds = new Set(plan.items.map((i) => i.kind));
  return FULL_KINDS.every((k) => kinds.has(k));
}
