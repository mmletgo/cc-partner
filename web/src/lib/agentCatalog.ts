/**
 * 多 CLI Agent 身份目录（与 Rust `agent_catalog` 对齐）。
 *
 * Business Logic（为什么需要）:
 *   Hub / Runtime / 会话搜索 / Prompt 历史 / Token 统计 / 优化器
 *   必须共用一份身份表，禁止再写死 claude|codex|opencode。
 *
 * Code Logic（做什么）:
 *   编译期表 + 查询 helper；未知 token 返回 null。
 */

import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentProviderId } from '@/lib/types/orchestrator';
import type { CcHistorySource } from '@/lib/types/core';

/** 产品级 Agent 身份。 */
export type AgentId = 'claude' | 'codex' | 'opencode' | 'grok' | 'gemini' | 'cursor' | 'pi';

/** owning device 无图形剪贴板时 Workbench 贴图的 PTY 注入语法。 */
export type HeadlessImagePasteKind =
  | 'atFileMention'
  | 'bracketedPathPaste'
  | 'typedAbsolutePath';

/** 会话搜索 source（与 history source 当前同形）。 */
export type SessionSearchSource =
  | 'claude'
  | 'codex'
  | 'opencode'
  | 'grok'
  | 'gemini'
  | 'cursor'
  | 'pi';

/** 一条身份登记。 */
export interface AgentIdentity {
  id: AgentId;
  wire: AgentId;
  displayName: string;
  hubTarget: AgentTarget | null;
  runtimeProvider: AgentProviderId | null;
  sessionSource: SessionSearchSource | null;
  historySource: CcHistorySource | null;
  hasUsage: boolean;
  hasHeadless: boolean;
  executableNames: readonly string[];
  /** 无图形剪贴板时的贴图注入；与 Rust `headless_image_paste` 对齐。 */
  headlessImagePaste: HeadlessImagePasteKind;
}

const IDENTITIES: readonly AgentIdentity[] = [
  {
    id: 'claude',
    wire: 'claude',
    displayName: 'Claude Code',
    hubTarget: 'claude',
    runtimeProvider: 'claudeCodeVisible',
    sessionSource: 'claude',
    historySource: 'claude',
    hasUsage: true,
    hasHeadless: true,
    executableNames: ['claude'],
    headlessImagePaste: 'atFileMention',
  },
  {
    id: 'codex',
    wire: 'codex',
    displayName: 'Codex',
    hubTarget: 'codex',
    runtimeProvider: 'codexVisible',
    sessionSource: 'codex',
    historySource: 'codex',
    hasUsage: true,
    hasHeadless: false,
    executableNames: ['codex'],
    headlessImagePaste: 'bracketedPathPaste',
  },
  {
    id: 'opencode',
    wire: 'opencode',
    displayName: 'OpenCode',
    hubTarget: 'opencode',
    runtimeProvider: 'openCodeVisible',
    sessionSource: 'opencode',
    historySource: 'opencode',
    hasUsage: true,
    hasHeadless: false,
    executableNames: ['opencode'],
    headlessImagePaste: 'atFileMention',
  },
  {
    id: 'grok',
    wire: 'grok',
    displayName: 'Grok Build',
    hubTarget: 'grok',
    runtimeProvider: 'grokBuildVisible',
    sessionSource: 'grok',
    historySource: 'grok',
    hasUsage: true,
    hasHeadless: true,
    executableNames: ['grok'],
    headlessImagePaste: 'atFileMention',
  },
  {
    id: 'gemini',
    wire: 'gemini',
    displayName: 'Gemini CLI',
    hubTarget: 'gemini',
    runtimeProvider: 'geminiCliVisible',
    sessionSource: 'gemini',
    historySource: 'gemini',
    hasUsage: true,
    hasHeadless: true,
    executableNames: ['gemini'],
    headlessImagePaste: 'atFileMention',
  },
  {
    id: 'cursor',
    wire: 'cursor',
    displayName: 'Cursor CLI',
    hubTarget: 'cursor',
    runtimeProvider: 'cursorCliVisible',
    sessionSource: 'cursor',
    historySource: 'cursor',
    hasUsage: true,
    hasHeadless: true,
    executableNames: ['agent'],
    headlessImagePaste: 'atFileMention',
  },
  {
    id: 'pi',
    wire: 'pi',
    displayName: 'Pi',
    hubTarget: 'pi',
    runtimeProvider: 'piVisible',
    sessionSource: 'pi',
    historySource: 'pi',
    hasUsage: true,
    hasHeadless: true,
    executableNames: ['pi'],
    headlessImagePaste: 'typedAbsolutePath',
  },
];

/**
 * Business Logic: 页面列表必须读目录，不能本地再维护一份。
 * Code Logic: 返回只读登记表。
 */
export function allAgentIdentities(): readonly AgentIdentity[] {
  return IDENTITIES;
}

/**
 * Business Logic: URL / decoder 只接受已登记身份。
 * Code Logic: 精确匹配 wire。
 */
export function parseAgentId(raw: string): AgentId | null {
  const trimmed = raw.trim();
  return IDENTITIES.some((row) => row.wire === trimmed) ? (trimmed as AgentId) : null;
}

/**
 * Business Logic: Hub 切换器只列出有 hubTarget 的身份。
 * Code Logic: filter map。
 */
export function allHubTargets(): AgentTarget[] {
  return IDENTITIES.flatMap((row) => (row.hubTarget ? [row.hubTarget] : []));
}

/**
 * Business Logic: URL / IPC / 切换器只接受已登记 Hub target。
 * Code Logic: 对照 allHubTargets。
 */
export function isHubTarget(value: unknown): value is AgentTarget {
  return typeof value === 'string' && allHubTargets().includes(value as AgentTarget);
}

/**
 * Business Logic: 会话搜索 tab 列出全部 sessionSource。
 * Code Logic: filter map。
 */
export function allSessionSources(): SessionSearchSource[] {
  return IDENTITIES.flatMap((row) => (row.sessionSource ? [row.sessionSource] : []));
}

/**
 * Business Logic: Prompt 历史筛选列出全部 historySource。
 * Code Logic: filter map。
 */
export function allHistorySources(): CcHistorySource[] {
  return IDENTITIES.flatMap((row) => (row.historySource ? [row.historySource] : []));
}

/**
 * Business Logic: 按 Hub target 取显示名。
 * Code Logic: find。
 */
export function identityByHubTarget(target: AgentTarget): AgentIdentity | null {
  return IDENTITIES.find((row) => row.hubTarget === target) ?? null;
}

/**
 * Business Logic: 按 Runtime provider 取登记；genericTerminal 无产品身份。
 * Code Logic: find。
 */
export function identityByRuntime(provider: string): AgentIdentity | null {
  return IDENTITIES.find((row) => row.runtimeProvider === provider) ?? null;
}

/**
 * Business Logic: Prompt 优化固定使用 Claude Code。
 * Code Logic: 只返回 claude 身份。
 */
export function headlessOptimizerProviders(): AgentIdentity[] {
  return IDENTITIES.filter((row) => row.id === 'claude');
}
