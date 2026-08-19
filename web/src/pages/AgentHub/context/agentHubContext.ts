/**
 * Agent Hub URL 上下文纯模型。
 *
 * Business Logic（为什么需要）:
 *   当前发布以 tab × scope × owner × (instructions:lane | skill/command:assetLane) × agent
 *   恢复工作台；Skill/Command 仓库面与提示词公共槽一样隐藏 Agent 切换。
 *   本机、远端设备和项目上下文都必须可深链往返。
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
 * Skill/Command 存放面（仅 tab=skill|command 有意义）。
 *
 * Business Logic: 「已装备」是当前 Agent 已启用的 native / 已附加 / 运行时借用；
 *   「仓库」是本机一份 portable-store 目录，跨 Agent 列同一份，不按 Agent 切换。
 *   MCP/Plugin 没有这一层。
 * Code Logic: URL 键 `assetLane`；默认 equipped。
 */
export type PortableAssetLane = 'equipped' | 'store';

/**
 * Agent Hub 可深链恢复的导航上下文。
 *
 * Business Logic: 用户级带 deviceId（null=本机）；项目级带 projectKey；adapt 独立全页。
 * Code Logic: 与 query 键 agent/scope/deviceId/project/tab/lane/assetLane/view 对齐。
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
  /**
   * Skill/Command 已装备 / 仓库；仅 skill|command 时有效，其它 tab 恒为 equipped。
   */
  assetLane: PortableAssetLane;
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

/** 用户级三栏 P2P 能力 token（与 health capabilities 精确匹配）。 */
export const AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY = 'agent-hub.user-instructions.v1';

/**
 * Business Logic: 远端用户级三栏只在对端在线且宣告 user-instructions 时挂载，缺能力保持 hint。
 * Code Logic: online + capabilities 精确包含 token；views 不得 import @/api。
 */
export function peerAllowsUserInstructionThreePane(
  peer: { online: boolean; capabilities?: readonly string[] | null } | null | undefined,
): boolean {
  if (!peer?.online) return false;
  return (peer.capabilities ?? []).includes(AGENT_HUB_USER_INSTRUCTIONS_CAPABILITY);
}

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
const ASSET_LANES = new Set<PortableAssetLane>(['equipped', 'store']);
/** 旧 portable kind 可直接映射为 tab（不含 instructions）。 */
const ASSET_KIND_TABS = new Set<AgentHubTab>(['skill', 'command', 'mcp', 'plugin']);
/** Skill/Command 才有 portable-store 存放面。 */
const STORE_TABS = new Set<AgentHubTab>(['skill', 'command']);

/** 空 URL 的默认上下文。 */
export const DEFAULT_AGENT_HUB_CONTEXT: AgentHubContext = {
  agent: 'claude',
  scope: 'user',
  deviceId: null,
  projectKey: null,
  tab: 'instructions',
  instructionLane: 'exclusive',
  assetLane: 'equipped',
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
 *   再按 scope 清空互斥的 deviceId/projectKey；normalize 清非当前 tab 的 lane / assetLane。
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

  ctx.adaptView = params.get('view') === 'adapt' || params.get('adapt') === '1';

  const assetLane = params.get('assetLane');
  if (assetLane && isPortableAssetLane(assetLane)) {
    ctx.assetLane = assetLane;
  }

  return normalizeAgentHubContext(ctx);
}

/**
 * Business Logic: 把上下文写回 URL，保留无关 deep link；默认值删 key 降噪。
 * Code Logic: 写 agent/scope/deviceId/project/tab/lane/assetLane/view；剥离会干扰 re-parse 的 legacy section/target/kind。
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

  if (
    isPortableStoreTab(ctx.tab) &&
    ctx.assetLane !== DEFAULT_AGENT_HUB_CONTEXT.assetLane
  ) {
    next.set('assetLane', ctx.assetLane);
  } else {
    next.delete('assetLane');
  }

  if (ctx.adaptView) next.set('view', 'adapt');
  else next.delete('view');
  next.delete('adapt');

  // 现代上下文成为权威后，去掉会二次覆盖 parse 的 legacy 导航键
  next.delete('section');
  next.delete('target');
  next.delete('kind');

  return next;
}

/**
 * Business Logic: Workbench 托管的项目 Agent 必须冻结当前项目，且不能让 Hub 的 view=adapt
 *   覆盖 workbench 的 view=projectAgent，也不能删掉 Workbench 的 projectId。
 *
 * Code Logic: 先按 user 写出 agent/tab/lane/assetLane；再强制 view=projectAgent、adapt=1 别名；
 *   从不写 scope/project/deviceId。
 */
export function writeWorkbenchHostedAgentHubContext(
  params: URLSearchParams,
  ctx: AgentHubContext,
): URLSearchParams {
  const next = writeAgentHubContext(params, {
    ...ctx,
    scope: 'user',
    projectKey: null,
    deviceId: null,
    adaptView: false,
  });
  next.set('view', 'projectAgent');
  next.delete('scope');
  next.delete('project');
  next.delete('deviceId');
  if (ctx.adaptView) next.set('adapt', '1');
  else next.delete('adapt');
  return next;
}

/**
 * Business Logic: Workbench 项目 Agent 的 URL 只有 Hub 内部导航键，范围由当前项目冻结。
 *
 * Code Logic: parse 普通 Hub 键后强制 scope=project + 传入的 projectKey，并识别 adapt=1。
 */
export function parseWorkbenchHostedAgentHubContext(
  params: URLSearchParams,
  projectKey: string,
): AgentHubContext {
  const parsed = parseAgentHubContext(params);
  return normalizeAgentHubContext({
    ...parsed,
    scope: 'project',
    projectKey,
    deviceId: null,
    adaptView: parsed.adaptView || params.get('adapt') === '1',
  });
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

/**
 * Business Logic: 校验 Skill/Command 存放面。
 * Code Logic: 对照 ASSET_LANES 集合。
 */
export function isPortableAssetLane(value: string): value is PortableAssetLane {
  return ASSET_LANES.has(value as PortableAssetLane);
}

/**
 * Business Logic: 只有 Skill/Command 才有 portable-store 仓库页。
 * Code Logic: 对照 STORE_TABS。
 */
export function isPortableStoreTab(tab: AgentHubTab): boolean {
  return STORE_TABS.has(tab);
}

/**
 * Business Logic: 仓库是本机一份目录，inspect 必须拉全部 Agent；已装备才跟当前 Agent。
 * Code Logic: skill/command + store → `all`；其余返回 context.agent。
 */
export function portableInventoryTargetForHubContext(
  ctx: Pick<AgentHubContext, 'tab' | 'assetLane' | 'agent'>,
): 'all' | AgentTarget {
  if (isPortableStoreTab(ctx.tab) && ctx.assetLane === 'store') return 'all';
  return ctx.agent;
}

/**
 * Business Logic: 离开提示词时清三槽；离开 Skill/Command 时清仓库面，避免 URL 脏状态。
 * Code Logic: 非 instructions → exclusive；非 skill|command → equipped。
 */
export function normalizeAgentHubContext(ctx: AgentHubContext): AgentHubContext {
  return {
    ...ctx,
    instructionLane:
      ctx.tab === 'instructions'
        ? ctx.instructionLane
        : DEFAULT_AGENT_HUB_CONTEXT.instructionLane,
    assetLane: isPortableStoreTab(ctx.tab)
      ? ctx.assetLane
      : DEFAULT_AGENT_HUB_CONTEXT.assetLane,
  };
}
