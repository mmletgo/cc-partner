/**
 * 项目级原生提示词文件目录（各 CLI 实际加载的仓库根文件）。
 *
 * Business Logic（为什么需要）:
 *   项目 Agent 不走 Hub 三槽投影；用户要直接编辑 Claude/Codex 等真正读取的文件。
 *   Codex / OpenCode / Grok / Cursor / Pi 共用仓库根 AGENTS.md，必须当成同一份文件，
 *   不能按 Agent 复制出多份编辑器。
 *
 * Code Logic（做什么）:
 *   纯数据 + 查询：按 Agent 列出会加载的根文件；切 Agent 时尽量留在共用文件上；
 *   脏稿守卫只在下一上下文再也看不到某份未保存文件时触发。
 */

import type { AgentTarget } from '@/lib/types/agentHub';
import type { AgentHubScope, AgentHubTab } from '../context/agentHubContext';

/** 一份项目根指令文件的稳定身份。 */
export type ProjectInstructionFileId = 'agents' | 'claude' | 'gemini';

/** 仓库根指令文件规格。 */
export interface ProjectInstructionFileSpec {
  id: ProjectInstructionFileId;
  /** 项目根相对路径（规范文件名）。 */
  path: string;
  /** 实际会加载该文件的 Agent（含共用）。 */
  consumers: readonly AgentTarget[];
}

/**
 * 项目根指令文件权威表。
 *
 * Business Logic: 只列 CLI 会读的仓库文件，不含 Hub 独有槽（.grok/rules、.cursor/rules、.pi）。
 * Code Logic: consumers 决定共用关系；Claude 主文件是 CLAUDE.md，Gemini 是 GEMINI.md。
 */
export const PROJECT_INSTRUCTION_FILES: readonly ProjectInstructionFileSpec[] = [
  {
    id: 'agents',
    path: 'AGENTS.md',
    consumers: ['codex', 'opencode', 'grok', 'cursor', 'pi'],
  },
  {
    id: 'claude',
    path: 'CLAUDE.md',
    consumers: ['claude', 'grok', 'cursor', 'pi'],
  },
  {
    id: 'gemini',
    path: 'GEMINI.md',
    consumers: ['gemini'],
  },
];

/**
 * Business Logic: 当前 Agent 只展示它实际会加载的根文件。
 * Code Logic: 按权威表 filter；顺序即默认优先级（AGENTS.md 在 CLAUDE.md 前）。
 */
export function filesForAgent(agent: AgentTarget): ProjectInstructionFileSpec[] {
  return PROJECT_INSTRUCTION_FILES.filter((file) => file.consumers.includes(agent));
}

/**
 * Business Logic: 从 Codex 切到 OpenCode 应留在同一份 AGENTS.md，而不是当成另一份文件。
 * Code Logic: 当前 id 仍被新 Agent 消费则保留，否则落到该 Agent 的第一份文件。
 */
export function resolveActiveFileId(
  agent: AgentTarget,
  currentId: ProjectInstructionFileId | null,
): ProjectInstructionFileId | null {
  const files = filesForAgent(agent);
  if (currentId && files.some((file) => file.id === currentId)) return currentId;
  return files[0]?.id ?? null;
}

/** 脏稿守卫入参（与 URL 上下文对齐的最小字段）。 */
export interface ProjectInstructionGuardInput {
  dirtyFileIds: readonly ProjectInstructionFileId[];
  currentProjectKey: string | null;
  nextTab: AgentHubTab;
  nextAgent: AgentTarget;
  nextScope: AgentHubScope;
  nextProjectKey: string | null;
}

/**
 * Business Logic: 共用 AGENTS.md 的 Agent 之间切换不应打断编辑；离开该文件的可见范围才拦截。
 * Code Logic: 无脏稿放行；换项目/离提示词 tab 拦截；下一 Agent 仍能看到全部脏文件则放行。
 */
export function shouldGuardProjectInstructionContextChange(
  input: ProjectInstructionGuardInput,
): boolean {
  if (input.dirtyFileIds.length === 0) return false;
  if (input.nextScope !== 'project' || input.nextProjectKey !== input.currentProjectKey) {
    return true;
  }
  if (input.nextTab !== 'instructions') return true;
  const nextIds = new Set(filesForAgent(input.nextAgent).map((file) => file.id));
  return input.dirtyFileIds.some((id) => !nextIds.has(id));
}

/**
 * Business Logic: listDir 在大小写不敏感磁盘上可能返回 Claude.md。
 * Code Logic: 先精确匹配 name，再忽略大小写；只接受 file 节点。
 */
export function matchProjectInstructionNodeName(
  names: readonly string[],
  wanted: string,
): string | null {
  const exact = names.find((name) => name === wanted);
  if (exact) return exact;
  const lower = wanted.toLowerCase();
  return names.find((name) => name.toLowerCase() === lower) ?? null;
}
