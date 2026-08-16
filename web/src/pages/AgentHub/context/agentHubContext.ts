/**
 * Agent Hub URL 上下文纯模型。
 *
 * Business Logic（为什么需要）:
 *   当前发布以 tab × scope × owner × (instructions:lane) × agent 恢复工作台；
 *   本机、远端设备和项目上下文都必须可深链往返，不能因某个写动作暂不可用而丢失导航身份。
 *
 * Code Logic（做什么）:
 *   parse/write URLSearchParams；mapLegacySection 把五段分区映射为 Partial context。
 *   无 React、无 api。
 */

import { allHubTargets } from '@/lib/agentCatalog';
import type { AgentTarget } from '@/lib/types/agentHub';

/** 五 Tab：提示词 + 四类 portable 资产。 */
export type AgentHubTab = 'instructions' | 'skill' | 'command' | 'mcp' | 'plugin';

/** 用户级 / 项目级范围。 */
export type AgentHubScope = 'user' | 'project';

/**
 * 提示词三槽 lane（仅 tab=instructions 有意义）。
 *
 * Business Logic: 公共 / 适配 / 独有 固定槽，禁止再按标题切碎。
 * Code Logic: URL 键 `lane`；默认 exclusive（打开提示词页优先落独有槽）。
 */
export type InstructionLane = 'common' | 'adapted' | 'exclusive';

/**
 * Agent Hub 可深链恢复的导航上下文。
 *
 * Business Logic: 用户级带 deviceId（null=本机）；项目级带 projectKey；adapt 独立全页。
 * Code Logic: 与 query 键 agent/scope/deviceId/project/tab/lane/view 对齐。
 */
export interface AgentHubContext {
  agent: AgentTarget;
  scope: AgentHubScope;
  /** user scope: null = local device */
  deviceId: string | null;
  /** project scope identity; null when scope=user */
  projectKey: string | null;
  tab: AgentHubTab;
  /**
   * 提示词三槽；仅 tab=instructions 时有效，其它 tab 恒为默认 exclusive。
   */
  instructionLane: InstructionLane;
  /** true when view=adapt cross-agent page */
  adaptView: boolean;
}

/**
 * Agent Hub 当前上下文可兑现的能力级别。
 *
 * Business Logic: Shell 导航能力与某个 mutation 是否认证是两回事；远端和项目上下文
 *   必须继续可管理，具体动作再由 inventory/preview/apply 的能力证据决定。
 * Code Logic: local-user=direct；peer 或 remote project=remote；local project=project；
 *   互斥字段混用才 fail-closed 为 unsupported。
 */
export type AgentHubContextCapability = 'direct' | 'remote' | 'project' | 'unsupported';

/** 草稿所属身份；lane 不在其中，因为同一 Agent 的三槽共享一个 Canonical 文档。 */
export interface AgentHubDraftIdentity {
  scope: AgentHubScope;
  deviceId: string | null;
  projectKey: string | null;
  agent: AgentTarget;
}

const AGENT_TARGETS = new Set<AgentTarget>(allHubTargets());
const TABS = new Set<AgentHubTab>(['instructions', 'skill', 'command', 'mcp', 'plugin']);
const LANES = new Set<InstructionLane>(['common', 'adapted', 'exclusive']);
/** 旧 portable kind 可直接映射为 tab（不含 instructions）。 */
const ASSET_KIND_TABS = new Set<AgentHubTab>(['skill', 'command', 'mcp', 'plugin']);

/** 空 URL 的默认上下文。 */
export const DEFAULT_AGENT_HUB_CONTEXT: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  instructionLane: 'exclusive',
  adaptView: false,
};

/**
 * Business Logic: 草稿 lease 必须绑定真实 owner，不能只按当前可见 Tab 或标题判断。
 * Code Logic: 只复制稳定 identity 字段，避免调用方持有可变 context 对象。
 */
export function getAgentHubDraftIdentity(
  context: AgentHubContext,
): AgentHubDraftIdentity {
  return {
    scope: context.scope,
    deviceId: context.deviceId,
    projectKey: context.projectKey,
    agent: context.agent,
  };
}

/**
 * Business Logic: 所有调用方共享同一 owner 分类，防止页面与 API 各自猜测。
 * Code Logic: 只有字段互斥关系非法时 unsupported；合法远端/项目上下文不得被降级成本机。
 */
export function getAgentHubContextCapability(
  context: AgentHubContext,
): AgentHubContextCapability {
  if (context.scope === 'user') {
    if (context.projectKey !== null) return 'unsupported';
    return context.deviceId === null ? 'direct' : 'remote';
  }
  if (context.deviceId !== null) return 'unsupported';
  return context.projectKey?.startsWith('remote:') ? 'remote' : 'project';
}

/**
 * Business Logic: 旧五段 section 书签映射到新 IA 默认字段，避免深链全断。
 * Code Logic: 仅返回 Partial；未知/空 section 返回 {}。
 */
export function mapLegacySection(section: string | null): Partial<AgentHubContext> {
  if (!section) return {};
  switch (section) {
    case 'userInstructions':
      return { scope: 'user', tab: 'instructions' };
    case 'projectInstructions':
      return { scope: 'project', tab: 'instructions' };
    case 'assets':
    case 'portableAssets':
      // 资产区默认落到 skill；kind query 可在 parse 中覆盖
      return { tab: 'skill' };
    case 'syncImport':
    case 'diagnostics':
      // 新 IA 中工具栏/次要入口；不强制改 tab/scope
      return {};
    default:
      return {};
  }
}

/**
 * Business Logic: 从 search params 恢复导航上下文（新键优先，legacy 兜底）。
 * Code Logic: defaults ← mapLegacySection ← legacy target/kind ← 显式 agent/scope/tab/lane/…；
 *   再按 scope 清空互斥的 deviceId/projectKey；非 instructions 时 lane 回默认。
 */
export function parseAgentHubContext(params: URLSearchParams): AgentHubContext {
  const ctx: AgentHubContext = { ...DEFAULT_AGENT_HUB_CONTEXT };

  // 1) legacy section
  Object.assign(ctx, mapLegacySection(params.get('section')));

  // 2) legacy portable filters: target → agent, kind → tab
  const legacyTarget = params.get('target');
  if (legacyTarget && isAgentTarget(legacyTarget)) {
    ctx.agent = legacyTarget;
  }
  const legacyKind = params.get('kind');
  if (legacyKind && isAssetKindTab(legacyKind)) {
    ctx.tab = legacyKind;
  }

  // 3) explicit new params win
  const agent = params.get('agent');
  if (agent && isAgentTarget(agent)) {
    ctx.agent = agent;
  }
  const scope = params.get('scope');
  if (scope === 'user' || scope === 'project') {
    ctx.scope = scope;
  }

  const tab = params.get('tab');
  if (tab && isAgentHubTab(tab)) {
    ctx.tab = tab;
  }

  const lane = params.get('lane');
  if (lane && isInstructionLane(lane)) {
    ctx.instructionLane = lane;
  }

  if (ctx.scope === 'project') {
    const projectKey = params.get('project') ?? params.get('projectKey');
    ctx.projectKey = projectKey?.trim() ? projectKey.trim() : null;
    ctx.deviceId = null;
  } else {
    const deviceId = params.get('deviceId');
    ctx.deviceId = deviceId?.trim() ? deviceId.trim() : null;
    ctx.projectKey = null;
  }

  ctx.adaptView = params.get('view') === 'adapt';

  // 4) lane 仅 instructions 有意义
  if (ctx.tab !== 'instructions') {
    ctx.instructionLane = DEFAULT_AGENT_HUB_CONTEXT.instructionLane;
  }

  return ctx;
}

/**
 * Business Logic: 把上下文写回 URL，保留无关 deep link；默认值删 key 降噪。
 * Code Logic: 写 agent/scope/deviceId/project/tab/lane/view；剥离会干扰 re-parse 的 legacy section/target/kind。
 */
export function writeAgentHubContext(
  params: URLSearchParams,
  ctx: AgentHubContext,
): URLSearchParams {
  const next = new URLSearchParams(params);

  if (ctx.agent === DEFAULT_AGENT_HUB_CONTEXT.agent) next.delete('agent');
  else next.set('agent', ctx.agent);

  if (ctx.scope === 'project') {
    next.set('scope', 'project');
    next.delete('deviceId');
    if (ctx.projectKey) next.set('project', ctx.projectKey);
    else next.delete('project');
  } else {
    next.delete('scope');
    next.delete('project');
    if (ctx.deviceId) next.set('deviceId', ctx.deviceId);
    else next.delete('deviceId');
  }
  next.delete('projectKey');

  if (ctx.tab === DEFAULT_AGENT_HUB_CONTEXT.tab) next.delete('tab');
  else next.set('tab', ctx.tab);

  // lane：仅 instructions 且非 default 时写出
  if (
    ctx.tab === 'instructions' &&
    ctx.instructionLane !== DEFAULT_AGENT_HUB_CONTEXT.instructionLane
  ) {
    next.set('lane', ctx.instructionLane);
  } else {
    next.delete('lane');
  }

  if (ctx.adaptView) next.set('view', 'adapt');
  else next.delete('view');

  // 现代上下文成为权威后，去掉会二次覆盖 parse 的 legacy 导航键
  next.delete('section');
  next.delete('target');
  next.delete('kind');

  return next;
}

function isAgentTarget(value: string): value is AgentTarget {
  return AGENT_TARGETS.has(value as AgentTarget);
}

/**
 * Business Logic: 校验 URL/用户输入是否为合法五 Tab 之一。
 * Code Logic: 对照 TABS 集合。
 */
export function isAgentHubTab(value: string): value is AgentHubTab {
  return TABS.has(value as AgentHubTab);
}

/**
 * Business Logic: skill/command/mcp/plugin 为 portable 资产 tab（非 instructions）。
 * Code Logic: 对照 ASSET_KIND_TABS；供 lane 激活与 bootstrap 共用。
 */
export function isAssetKindTab(value: string): value is AgentHubTab {
  return ASSET_KIND_TABS.has(value as AgentHubTab);
}

/**
 * Business Logic: 校验提示词三槽 lane。
 * Code Logic: 对照 LANES 集合。
 */
export function isInstructionLane(value: string): value is InstructionLane {
  return LANES.has(value as InstructionLane);
}
